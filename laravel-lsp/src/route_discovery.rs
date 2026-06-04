//! Route discovery — find named routes across the project, packages, and framework.
//!
//! Laravel route names are registered via `->name('X')` calls in many places:
//! - `routes/*.php` (project, recursively — catches `auth.php`, custom splits)
//! - `vendor/*/routes/*.php` (packages — Fortify, Telescope, Filament, Horizon, etc.)
//! - Service provider `boot()` methods that call `Route::get(...)->name(...)` directly
//! - Macro definitions in `Route::macro('foo', function () { ... })`
//! - Files registered via `loadRoutesFrom('path')` at non-standard locations
//! - Filament `Panel::routes(fn () => ...)` closures
//!
//! Rather than hard-code well-known files, this module discovers candidates by
//! scanning for files whose content shows route-registration shape (a route
//! facade/router token AND a `->name(` call). This naturally captures every
//! pattern listed above without needing per-package knowledge.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use walkdir::WalkDir;

/// A route group's contribution to its child routes' names. Built once per
/// source file by [`find_route_groups`] and consulted by
/// [`extract_named_routes`] when composing the final name of a `->name('X')`
/// callsite that lives inside one or more enclosing groups.
#[derive(Debug, Clone)]
struct RouteGroupSpan {
    /// Name prefix this group contributes — `"admin."`, `"api.v1."`, etc.
    prefix: String,
    /// Byte offset just past the opening `{` of the group's closure body.
    body_start: usize,
    /// Byte offset of the matching closing `}` of the group's closure body.
    body_end: usize,
}

/// A `->group(...)`/`::group(...)` callsite that loads an *external file*
/// instead of running a closure body (issue #43). Laravel `require`s the file
/// and applies the group's attributes — including its `->as('admin.')` name
/// prefix — to every route declared in it.
#[derive(Debug, Clone)]
struct ExternalGroupLoad {
    /// Byte offset of the `->`/`::` that introduces this `group(...)` call.
    /// Used to locate enclosing closure groups in the same file.
    offset: usize,
    /// This load's OWN name prefix (from a chained `->as(...)`/`->name(...)`
    /// link or an `['as' => ...]` array literal). May be empty.
    own_prefix: String,
    /// Resolved absolute path of the file this group loads.
    target: PathBuf,
}

/// A located route definition. Stored in [`RouteIndex`] keyed by route name.
#[derive(Debug, Clone)]
pub struct RouteDefinition {
    /// Absolute file path containing the `->name('X')` call.
    pub file: PathBuf,
    /// 0-based line of the `->name(` callsite.
    pub line: u32,
    /// 0-based column where the `->name(` callsite begins.
    pub column: u32,
    /// 0-based column where the `->name(` callsite ends (exclusive).
    pub end_column: u32,
    /// Source priority. Higher wins on conflict (app overrides package overrides framework).
    pub priority: u8,
    /// HTTP method extracted from the route declaration (lowercased: "get", "post",
    /// "any", "match", "view", "redirect", etc.). `None` when the verb can't be
    /// resolved statically — e.g. inside a `Route::macro(...)` body or for
    /// programmatically-built routes.
    pub method: Option<String>,
    /// URI extracted from the first string argument of the verb call.
    /// `None` when the first argument isn't a string literal.
    pub uri: Option<String>,
    /// Controller@action extracted from the second argument. Common shapes:
    /// `[UserController::class, 'show']` → `"UserController@show"`,
    /// `'OldController@method'` → `"OldController@method"`,
    /// `UserController::class` (invokable) → `"UserController"`,
    /// `function/fn closure` → `"Closure"`.
    /// `None` when the second argument is missing or unresolvable (e.g. `Route::view`,
    /// `Route::redirect`).
    pub action: Option<String>,
}

/// Priority levels used when multiple files define the same route name.
/// Higher beats lower — if app and Fortify both register `login`, the app's wins.
pub const PRIORITY_FRAMEWORK: u8 = 0;
pub const PRIORITY_PACKAGE: u8 = 1;
pub const PRIORITY_APP: u8 = 2;

/// In-memory map of route name → definition location.
#[derive(Debug, Default, Clone)]
pub struct RouteIndex {
    pub routes: HashMap<String, RouteDefinition>,
    /// Every file that contributed to this index, keyed by normalized
    /// (lexically-cleaned) absolute path. Includes both files found by
    /// [`discover_route_files`] AND files reached transitively through
    /// `->group(<path>)` external loads — even when they live outside `routes/`
    /// (issue #43). Used by `did_save` to decide whether a saved file should
    /// trigger a route-index rebuild.
    pub source_files: std::collections::HashSet<PathBuf>,
}

impl RouteIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a definition. Higher priority replaces lower; equal priority keeps first.
    pub fn insert(&mut self, name: String, def: RouteDefinition) {
        match self.routes.get(&name) {
            Some(existing) if existing.priority >= def.priority => {}
            _ => {
                self.routes.insert(name, def);
            }
        }
    }

    pub fn get(&self, name: &str) -> Option<&RouteDefinition> {
        self.routes.get(name)
    }

    pub fn len(&self) -> usize {
        self.routes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.routes.is_empty()
    }
}

/// A file containing route definitions, paired with its priority tier.
#[derive(Debug, Clone)]
pub struct RouteFile {
    pub path: PathBuf,
    pub priority: u8,
}

/// Walk the project to discover every file likely to define named routes.
///
/// The returned list is deduplicated by path. Order is not significant — the
/// final index resolves conflicts via priority.
pub fn discover_route_files(root: &Path) -> Vec<RouteFile> {
    let mut seen: HashMap<PathBuf, u8> = HashMap::new();

    // Project routes/ — recursive, every *.php
    let project_routes = root.join("routes");
    if project_routes.exists() {
        for path in walk_php_files(&project_routes, 6) {
            promote(&mut seen, path, PRIORITY_APP);
        }
    }

    // Package routes/*.php and any vendor file whose content registers routes.
    let vendor = root.join("vendor");
    if vendor.exists() {
        for entry in WalkDir::new(&vendor)
            .max_depth(8)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            if path.extension().is_none_or(|ext| ext != "php") {
                continue;
            }

            // Anything under a vendor `routes/` subdirectory is a route file
            // by Laravel package convention.
            if is_under_routes_dir(path) {
                promote(
                    &mut seen,
                    path.to_path_buf(),
                    priority_for_vendor_path(path),
                );
                continue;
            }

            // Otherwise content-match: register the file only if it both
            // contains a route-registration token AND a `->name(` call.
            // This is what catches macro bodies (Laravel UI's
            // AuthRouteMethods), service-provider `boot()` registrations,
            // and Filament-style `Panel::routes(fn () => ...)` panels.
            if file_registers_named_routes(path) {
                promote(
                    &mut seen,
                    path.to_path_buf(),
                    priority_for_vendor_path(path),
                );
            }
        }
    }

    // App-level service providers and bootstrap/app.php often register routes
    // directly in `boot()`. Scan with content match to avoid pulling in
    // unrelated *.php files in app/.
    for candidate in app_provider_candidates(root) {
        if candidate.exists() && file_registers_named_routes(&candidate) {
            promote(&mut seen, candidate, PRIORITY_APP);
        }
    }

    seen.into_iter()
        .map(|(path, priority)| RouteFile { path, priority })
        .collect()
}

/// Build a complete route name → location index from the given files.
///
/// `root` is the project root, used to resolve `base_path(...)` arguments in
/// external-file group loads (issue #43). The working set is BFS-expanded along
/// `->group(<path>)` load edges, so a file referenced via
/// `Route::as('admin.')->group(base_path('app/Custom/admin.php'))` is indexed
/// even when it lives OUTSIDE `routes/` and was never returned by
/// [`discover_route_files`]. Referenced files inherit their loader's priority.
///
/// Files reached by such loads inherit the loading group's name prefix
/// transitively, so their routes are indexed under both their bare and prefixed
/// names. The resulting [`RouteIndex::source_files`] lists every contributing
/// file (discovered + referenced), keyed by normalized path.
pub fn build_route_index(root: &Path, files: &[RouteFile]) -> RouteIndex {
    let expansion = expand_load_graph(root, files);
    let effective = compute_effective_prefixes(root, &expansion.files, &expansion.contents);

    let mut index = RouteIndex::new();
    for file in &expansion.files {
        let key = normalize_path(&file.path);
        index.source_files.insert(key.clone());
        let Some(content) = expansion.contents.get(&key) else {
            continue;
        };
        let inherited = effective.get(&key).cloned().unwrap_or_default();
        for def in extract_named_routes(content, &file.path, file.priority, &inherited) {
            if let Some(name) = def.0 {
                index.insert(name, def.1);
            }
        }
    }
    index
}

/// Return the inherited external-file name prefixes that apply to `file`
/// (issue #43), ALWAYS including `""` and deduplicated.
///
/// A file referenced via `Route::as('admin.')->group(base_path('that.php'))`
/// from somewhere in the project inherits the loading group's name prefix
/// (`"admin."`) transitively across the entire `->group(<path>)` load graph.
/// This is exactly the set [`build_route_index`] applies when indexing the file
/// — exposed standalone so rename / find-references / document-symbols can
/// resolve a route's project-level name without re-running a full index build.
///
/// Runs [`discover_route_files`] + the same BFS load-graph expansion and
/// prefix propagation as [`build_route_index`], then looks up `file`'s
/// normalized key. Returns `["".into()]` when `file` isn't reachable (it's
/// still scanned directly, so the empty prefix always applies).
pub fn external_prefixes_for_file(root: &Path, file: &Path) -> Vec<String> {
    let files = discover_route_files(root);
    let expansion = expand_load_graph(root, &files);
    let effective = compute_effective_prefixes(root, &expansion.files, &expansion.contents);

    let key = normalize_path(file);
    let mut out = vec![String::new()];
    if let Some(prefixes) = effective.get(&key) {
        for p in prefixes {
            if !p.is_empty() && !out.contains(p) {
                out.push(p.clone());
            }
        }
    }
    out
}

/// Like [`external_prefixes_for_file`] but computes the inherited external-load
/// prefixes for EVERY route file in one pass, keyed by normalized path. Callers
/// iterating many files (rename / find-references) build this once instead of
/// re-running the whole project load graph per file (avoids O(files²)).
///
/// Each returned entry always includes `""`. A file with no inherited prefix is
/// simply absent from the map — callers should treat a miss as `["".into()]`.
pub fn external_prefixes_map(root: &Path) -> HashMap<PathBuf, Vec<String>> {
    let files = discover_route_files(root);
    let expansion = expand_load_graph(root, &files);
    let effective = compute_effective_prefixes(root, &expansion.files, &expansion.contents);

    let mut map: HashMap<PathBuf, Vec<String>> = HashMap::new();
    for (key, prefixes) in effective {
        let mut out = vec![String::new()];
        for p in prefixes {
            if !p.is_empty() && !out.contains(&p) {
                out.push(p);
            }
        }
        map.insert(key, out);
    }
    map
}

/// The fully-expanded working set produced by following `->group(<path>)`
/// external loads from a seed file list. Keyed by normalized path so a file
/// reached more than once is read and indexed exactly once.
struct LoadGraphExpansion {
    /// Every contributing file (seed + transitively referenced), with the
    /// highest priority observed for each.
    files: Vec<RouteFile>,
    /// Each file's source text, keyed by normalized path.
    contents: HashMap<PathBuf, String>,
}

/// BFS-expand `files` along external `->group(<path>)` load edges, reading each
/// reachable file's contents. Shared by [`build_route_index`] and
/// [`external_prefixes_for_file`] so both see the identical working set.
fn expand_load_graph(root: &Path, files: &[RouteFile]) -> LoadGraphExpansion {
    // `contents`/`paths`/`priorities` are all keyed by normalized path so a file
    // reached by two routes (discovered + referenced, or via two loaders) is
    // read and indexed exactly once.
    let mut contents: HashMap<PathBuf, String> = HashMap::new();
    // Original (non-normalized) path to use for the RouteDefinition's `file`.
    let mut paths: HashMap<PathBuf, PathBuf> = HashMap::new();
    let mut priorities: HashMap<PathBuf, u8> = HashMap::new();

    // Queue of (path, priority, depth) still to read/expand.
    let mut queue: std::collections::VecDeque<(PathBuf, u8, usize)> =
        std::collections::VecDeque::new();
    for file in files {
        queue.push_back((file.path.clone(), file.priority, 0));
    }

    while let Some((path, priority, depth)) = queue.pop_front() {
        let key = normalize_path(&path);

        // Record the best (highest) priority and remember the path/contents the
        // first time we see this file.
        let already_seen = contents.contains_key(&key);
        priorities
            .entry(key.clone())
            .and_modify(|p| {
                if priority > *p {
                    *p = priority;
                }
            })
            .or_insert(priority);
        if already_seen {
            // Contents already read and this file's edges already expanded;
            // just merging priority above is enough.
            continue;
        }

        let Ok(text) = std::fs::read_to_string(&path) else {
            // Unreadable target (e.g. a referenced file that doesn't exist) —
            // record nothing; it simply contributes no routes.
            continue;
        };

        // Discover this file's external `->group(<path>)` targets and enqueue
        // any not yet read. Depth is capped in the spirit of MAX_LOAD_DEPTH so a
        // pathological chain can't loop forever (the `already_seen` check breaks
        // cycles directly).
        if depth < MAX_LOAD_DEPTH {
            let loader_dir = path.parent().unwrap_or(root).to_path_buf();
            for load in find_external_group_loads(text.as_bytes(), root, &loader_dir) {
                let target_key = normalize_path(&load.target);
                if !contents.contains_key(&target_key) {
                    queue.push_back((load.target, priority, depth + 1));
                }
            }
        }

        paths.insert(key.clone(), path);
        contents.insert(key, text);
    }

    // Build the full expanded file set.
    let expanded: Vec<RouteFile> = paths
        .iter()
        .map(|(key, original)| RouteFile {
            path: original.clone(),
            priority: priorities.get(key).copied().unwrap_or(PRIORITY_APP),
        })
        .collect();

    LoadGraphExpansion {
        files: expanded,
        contents,
    }
}

/// Maximum transitive load depth — a backstop against pathological chains even
/// with the cycle guard in place.
const MAX_LOAD_DEPTH: usize = 10;

/// Build the per-file set of inherited name prefixes contributed by
/// external-file group loads, propagated transitively across the load graph.
///
/// Returns a map keyed by normalized file path. Every entry includes `""`
/// (every file is also scanned directly). Cycles are broken by a per-target
/// visited set, and chains are capped at [`MAX_LOAD_DEPTH`].
fn compute_effective_prefixes(
    root: &Path,
    files: &[RouteFile],
    contents: &HashMap<PathBuf, String>,
) -> HashMap<PathBuf, Vec<String>> {
    // Set of files we actually index — only these can receive inherited
    // prefixes, and only loads pointing at one matter.
    let known: std::collections::HashSet<PathBuf> =
        files.iter().map(|f| normalize_path(&f.path)).collect();

    // edges[source] = Vec<(target, edge_prefix)>
    let mut edges: HashMap<PathBuf, Vec<(PathBuf, String)>> = HashMap::new();
    for file in files {
        let source = normalize_path(&file.path);
        let Some(content) = contents.get(&source) else {
            continue;
        };
        let bytes = content.as_bytes();
        let loader_dir = file.path.parent().unwrap_or(root).to_path_buf();
        let loads = find_external_group_loads(bytes, root, &loader_dir);
        if loads.is_empty() {
            continue;
        }
        // Closure-group spans in the SAME file — needed to prepend any enclosing
        // closure prefixes to each load's edge.
        let groups = find_route_groups(bytes);
        for load in loads {
            let target = normalize_path(&load.target);
            if !known.contains(&target) {
                continue;
            }
            let mut edge_prefix = String::new();
            for grp in &groups {
                if grp.body_start <= load.offset && load.offset < grp.body_end {
                    edge_prefix.push_str(&grp.prefix);
                }
            }
            edge_prefix.push_str(&load.own_prefix);
            edges
                .entry(source.clone())
                .or_default()
                .push((target, edge_prefix));
        }
    }

    // Propagate. For each known file, the inherited prefixes are the set of
    // accumulated edge-prefix concatenations along every load path that reaches
    // it. We DFS from each source so cycles are naturally bounded per traversal.
    let mut effective: HashMap<PathBuf, Vec<String>> = HashMap::new();
    for start in &known {
        propagate(start, "", &edges, &mut effective, &mut Vec::new(), 0);
    }
    effective
}

/// Depth-first propagation of accumulated prefixes along load edges. `acc` is
/// the prefix accumulated from the root of this traversal up to (but excluding)
/// `current`. `stack` holds the files on the current path for cycle detection.
fn propagate(
    current: &Path,
    acc: &str,
    edges: &HashMap<PathBuf, Vec<(PathBuf, String)>>,
    effective: &mut HashMap<PathBuf, Vec<String>>,
    stack: &mut Vec<PathBuf>,
    depth: usize,
) {
    if stack.iter().any(|p| p == current) || depth > MAX_LOAD_DEPTH {
        return;
    }
    // Record this accumulated prefix for `current` (skip the empty root case —
    // every file already gets "" implicitly in `extract_named_routes`).
    if !acc.is_empty() {
        let entry = effective.entry(current.to_path_buf()).or_default();
        if !entry.iter().any(|p| p == acc) {
            entry.push(acc.to_string());
        }
    }
    stack.push(current.to_path_buf());
    if let Some(targets) = edges.get(current) {
        for (target, edge_prefix) in targets {
            let next_acc = format!("{}{}", acc, edge_prefix);
            propagate(target, &next_acc, edges, effective, stack, depth + 1);
        }
    }
    stack.pop();
}

/// Extract every `->name('X')` callsite from the given source.
///
/// Returns `(name, RouteDefinition)` pairs. The match is line-based and tolerant
/// of whitespace inside the call: `->name('X')`, `->name ( "X" )`, etc.
///
/// Only matches single-quoted or double-quoted string literals. Variable
/// arguments (`->name($var)`) are skipped because we can't resolve them
/// statically.
///
/// `inherited_prefixes` carries name prefixes contributed by *external-file*
/// group loads that target this file (issue #43). For each callsite, one
/// `RouteDefinition` is emitted per inherited prefix, with the final name being
/// `inherited_prefix + in_file_closure_prefix + leaf`. A file that is loaded
/// both directly and via a prefixed group therefore contributes both its bare
/// and prefixed names. Passing `&[]` (or `&["".into()]`) means "no inherited
/// prefix" and yields byte-identical behavior to a plain direct scan.
pub fn extract_named_routes(
    content: &str,
    file: &Path,
    priority: u8,
    inherited_prefixes: &[String],
) -> Vec<(Option<String>, RouteDefinition)> {
    let mut results = Vec::new();
    let bytes = content.as_bytes();

    // Normalize inherited prefixes to a non-empty set that always contains the
    // empty string (the file is always scanned directly too). Dedupe so a load
    // graph that reaches this file by two equal-prefix paths doesn't duplicate.
    let mut effective: Vec<&str> = vec![""];
    for p in inherited_prefixes {
        if !p.is_empty() && !effective.contains(&p.as_str()) {
            effective.push(p.as_str());
        }
    }

    // Build group-span index once per file. Each span tells us "any
    // `->name('X')` inside body_start..body_end gets `prefix` prepended."
    // Nested groups are handled by accumulating every span that encloses the
    // callsite, ordered outermost-first.
    let groups = find_route_groups(bytes);

    let pattern = b"->name";
    let mut i = 0;

    while i + pattern.len() <= bytes.len() {
        if &bytes[i..i + pattern.len()] != pattern {
            i += 1;
            continue;
        }

        // After "->name", skip whitespace, expect '('.
        let mut j = i + pattern.len();
        while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
            j += 1;
        }
        if j >= bytes.len() || bytes[j] != b'(' {
            i += 1;
            continue;
        }
        j += 1;
        while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
            j += 1;
        }
        if j >= bytes.len() {
            i += 1;
            continue;
        }
        let quote = bytes[j];
        if quote != b'\'' && quote != b'"' {
            i += 1;
            continue;
        }
        j += 1;
        let str_start = j;
        while j < bytes.len() && bytes[j] != quote && bytes[j] != b'\n' {
            // Allow simple escapes — skip the next byte.
            if bytes[j] == b'\\' && j + 1 < bytes.len() {
                j += 2;
            } else {
                j += 1;
            }
        }
        if j >= bytes.len() || bytes[j] != quote {
            i += 1;
            continue;
        }
        let literal_name = match std::str::from_utf8(&bytes[str_start..j]) {
            Ok(s) => s.to_string(),
            Err(_) => {
                i += 1;
                continue;
            }
        };

        // Compose the in-file portion of the name from enclosing closure-group
        // prefixes plus the literal `->name('X')` value. Groups are pre-sorted
        // by body_start so iterating in order yields outermost-first.
        let mut in_file_name = String::new();
        for grp in &groups {
            if grp.body_start <= i && i < grp.body_end {
                in_file_name.push_str(&grp.prefix);
            }
        }
        in_file_name.push_str(&literal_name);

        let (line, column) = byte_to_line_col(bytes, i);
        let end_column = column + (j - i + 1) as u32; // include closing quote

        // Scan backward from the `->name(` callsite to find the verb call that
        // started this route's fluent chain. Captures HTTP method, URI, and the
        // controller@action target for hover display.
        let metadata = extract_route_metadata(bytes, i);

        // Emit one definition per inherited prefix (always including ""), so a
        // file reachable via an external-file group load contributes both its
        // bare and prefixed names.
        for prefix in &effective {
            let mut name = String::with_capacity(prefix.len() + in_file_name.len());
            name.push_str(prefix);
            name.push_str(&in_file_name);
            results.push((
                Some(name),
                RouteDefinition {
                    file: file.to_path_buf(),
                    line,
                    column,
                    end_column,
                    priority,
                    method: metadata.method.clone(),
                    uri: metadata.uri.clone(),
                    action: metadata.action.clone(),
                },
            ));
        }

        i = j + 1;
    }

    // Second pass: derive route names from `Route::resource(...)` /
    // `Route::apiResource(...)` (and their `->resource(...)` / `->apiResource(...)`
    // fluent forms). Laravel synthesizes one named route per CRUD action — e.g.
    // `Route::resource('photos', PhotoController::class)` registers
    // `photos.index`, `photos.create`, ... `photos.destroy`. We compose these
    // leaf names with the same group/inherited prefixes as `->name()` routes.
    extract_resource_routes(bytes, file, priority, &effective, &groups, &mut results);

    results
}

/// Default action set for `Route::resource(...)` — full CRUD.
const RESOURCE_ACTIONS: &[&str] = &[
    "index", "create", "store", "show", "edit", "update", "destroy",
];

/// Default action set for `Route::apiResource(...)` — no `create`/`edit` (those
/// render forms, which an API doesn't serve).
const API_RESOURCE_ACTIONS: &[&str] = &["index", "store", "show", "update", "destroy"];

/// Append resource-derived route definitions to `results`.
///
/// Detects `resource(` / `apiResource(` callsites preceded by `->` or `::`,
/// extracts the first string-literal argument (the resource URI/name), strips
/// leading AND trailing `/`, applies any `->only([...])`/`->except([...])`
/// filter found in the same statement, then emits one [`RouteDefinition`] per
/// surviving action × effective prefix × enclosing closure-group prefix.
///
/// Punted (common case only): `->names([...])` / `->name('…')` overrides on the
/// resource, `Route::resources([...])` plural registration, and shallow/nested
/// resources. These are uncommon and would need substantially more parsing.
fn extract_resource_routes(
    bytes: &[u8],
    file: &Path,
    priority: u8,
    effective: &[&str],
    groups: &[RouteGroupSpan],
    results: &mut Vec<(Option<String>, RouteDefinition)>,
) {
    // Scan for `->`/`::` connectors and read the method identifier that follows.
    // We can't keyword-match on `resource` alone because `apiResource` spells it
    // with a capital `R` (`api` + `Resource`); matching the connector + full
    // identifier handles both spellings cleanly.
    let paren_pairs = build_paren_pairs(bytes);
    let mut i = 0usize;

    while i + 2 < bytes.len() {
        let connector = &bytes[i..i + 2];
        if connector != b"->" && connector != b"::" {
            i += 1;
            continue;
        }

        // Read the identifier immediately after the connector.
        let name_start = i + 2;
        let mut name_end = name_start;
        while name_end < bytes.len() && is_identifier_byte(bytes[name_end]) {
            name_end += 1;
        }
        let method = match std::str::from_utf8(&bytes[name_start..name_end]) {
            Ok(s) => s,
            Err(_) => {
                i += 2;
                continue;
            }
        };
        let is_api = match method {
            "resource" => false,
            "apiResource" => true,
            _ => {
                i += 2;
                continue;
            }
        };

        // Right boundary: an opening `(` (after optional whitespace).
        let mut j = name_end;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if j >= bytes.len() || bytes[j] != b'(' {
            i += 2;
            continue;
        }
        let args_open = j;
        let Some(&args_close) = paren_pairs.get(&args_open) else {
            i = args_open + 1;
            continue;
        };

        // First argument: the resource name/URI. Must be a string literal.
        let stripped = match nth_arg_slice(bytes, args_open + 1, args_close, 0)
            .and_then(parse_string_literal)
        {
            Some(name) => {
                let trimmed = name.trim_matches('/');
                if trimmed.is_empty() {
                    i = args_open + 1;
                    continue;
                }
                trimmed.to_string()
            }
            None => {
                i = args_open + 1;
                continue;
            }
        };

        // Determine the action set, honoring `->only([...])` / `->except([...])`
        // appearing anywhere in the same statement.
        let (_stmt_start, stmt_end) = statement_containing(bytes, name_start);
        let defaults = if is_api {
            API_RESOURCE_ACTIONS
        } else {
            RESOURCE_ACTIONS
        };
        let actions = resource_actions_in_statement(bytes, args_open, stmt_end, defaults);

        // Position of the call (for goto-definition). Point at the start of the
        // method identifier, ending after the closing `)` of the arg list.
        let (line, column) = byte_to_line_col(bytes, name_start);
        let end_column = column + (args_close + 1 - name_start) as u32;

        // Enclosing closure-group prefix(es) for this call's offset.
        let mut in_file_prefix = String::new();
        for grp in groups {
            if grp.body_start <= name_start && name_start < grp.body_end {
                in_file_prefix.push_str(&grp.prefix);
            }
        }

        for action in &actions {
            let leaf = format!("{}.{}", stripped, action);
            for prefix in effective {
                let mut name =
                    String::with_capacity(prefix.len() + in_file_prefix.len() + leaf.len());
                name.push_str(prefix);
                name.push_str(&in_file_prefix);
                name.push_str(&leaf);
                results.push((
                    Some(name),
                    RouteDefinition {
                        file: file.to_path_buf(),
                        line,
                        column,
                        end_column,
                        priority,
                        // Resource routes register multiple verbs; no single
                        // method applies, so leave it unresolved.
                        method: None,
                        uri: Some(stripped.clone()),
                        action: Some((*action).to_string()),
                    },
                ));
            }
        }

        i = args_open + 1;
    }
}

/// Resolve the surviving action set for a resource registration by scanning the
/// statement (`start..end`) for `->only([...])` then `->except([...])`. `only`
/// wins when both are present (matching Laravel's runtime, where the last
/// applied modifier governs, but `only` is the common explicit case). Returns a
/// filtered, owned subset of `defaults` preserving their canonical order.
fn resource_actions_in_statement<'a>(
    bytes: &[u8],
    start: usize,
    end: usize,
    defaults: &'a [&'a str],
) -> Vec<&'a str> {
    if let Some(list) = method_array_arg(bytes, start, end, b"only") {
        return defaults
            .iter()
            .filter(|a| list.contains(&a.to_string()))
            .copied()
            .collect();
    }
    if let Some(list) = method_array_arg(bytes, start, end, b"except") {
        return defaults
            .iter()
            .filter(|a| !list.contains(&a.to_string()))
            .copied()
            .collect();
    }
    defaults.to_vec()
}

/// Find `->{method}([ ... ])` within `start..end` and return the string-literal
/// elements of its array argument. Returns `None` if the method isn't called.
fn method_array_arg(bytes: &[u8], start: usize, end: usize, method: &[u8]) -> Option<Vec<String>> {
    let mut i = start;
    while i + method.len() < end {
        // Match `->{method}` with a `->` connector and a word boundary after.
        if i >= 2
            && &bytes[i - 2..i] == b"->"
            && i + method.len() <= end
            && &bytes[i..i + method.len()] == method
        {
            let after = i + method.len();
            let boundary_ok = after >= end || !is_identifier_byte(bytes[after]);
            if boundary_ok {
                let mut j = after;
                while j < end && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                if j < end && bytes[j] == b'(' {
                    return Some(parse_string_array(bytes, j + 1, end));
                }
            }
        }
        i += 1;
    }
    None
}

/// Collect string-literal elements from an array literal opening at `start`
/// (the byte just after the `(`). Reads up to the matching `]`, returning each
/// quoted element's inner text. Tolerant of a missing `[` (treats the call
/// argument list as the element source).
fn parse_string_array(bytes: &[u8], start: usize, end: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = start;
    while i < end && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i < end && bytes[i] == b'[' {
        i += 1;
    }
    while i < end {
        let b = bytes[i];
        if b == b']' || b == b')' {
            break;
        }
        if b == b'\'' || b == b'"' {
            let quote = b;
            i += 1;
            let lit_start = i;
            while i < end && bytes[i] != quote {
                if bytes[i] == b'\\' && i + 1 < end {
                    i += 2;
                    continue;
                }
                i += 1;
            }
            if i < end {
                if let Ok(s) = std::str::from_utf8(&bytes[lit_start..i]) {
                    out.push(s.to_string());
                }
                i += 1; // skip closing quote
            }
            continue;
        }
        i += 1;
    }
    out
}

/// Resolved data describing the verb call that opens a route's fluent chain.
/// Each field can be `None` when static extraction can't pin it down — callers
/// must handle missing data gracefully (typically by omitting it from display).
#[derive(Debug, Default, Clone)]
pub struct RouteMetadata {
    pub method: Option<String>,
    pub uri: Option<String>,
    pub action: Option<String>,
}

/// HTTP verbs (and verb-shaped methods like `view`, `redirect`) that Laravel's
/// router exposes for registering routes. Matched against `Route::<verb>(` and
/// `->name`-chained `<receiver>-><verb>(` callsites when reconstructing route
/// metadata. Lowercased — comparisons are case-sensitive against PHP source.
const HTTP_VERBS: &[&str] = &[
    "get",
    "post",
    "put",
    "patch",
    "delete",
    "options",
    "any",
    "match",
    "view",
    "redirect",
    "permanentRedirect",
];

/// Walk forward from the start of the statement that contains `name_callsite_offset`
/// to find the verb call that opens the fluent chain, then parse its first two
/// arguments (URI + action).
///
/// Statement boundaries are detected with full string-literal and depth tracking
/// (parens, brackets, braces) so `{user}` route parameters and closure bodies
/// don't fool the scan. Returns a partial result whenever a piece is missing —
/// a closure-based route still resolves method + URI.
pub fn extract_route_metadata(bytes: &[u8], name_callsite_offset: usize) -> RouteMetadata {
    let (stmt_start, _stmt_end) = statement_containing(bytes, name_callsite_offset);
    let Some((method, args_start)) = find_verb_call(bytes, stmt_start, name_callsite_offset) else {
        return RouteMetadata::default();
    };

    // Walk forward from the verb's opening `(` to grab arg 1 (URI) and arg 2
    // (action). Both are optional — `Route::view('/x', 'view.name')` has no
    // action, `Route::redirect(...)` has only a URI.
    let (uri_slice, after_uri) = read_arg(bytes, args_start);
    let uri = uri_slice.and_then(parse_string_literal);

    let action = if let Some(after_uri_idx) = after_uri {
        let (action_slice, _) = read_arg(bytes, after_uri_idx);
        action_slice.and_then(parse_action_argument)
    } else {
        None
    };

    RouteMetadata {
        method: Some(method),
        uri,
        action,
    }
}

/// Return `(start, end)` of the PHP statement that contains `offset`. Walks
/// forward from byte 0, tracking string literals and combined brace/paren/bracket
/// depth so that `;` is only treated as a separator when depth is zero and we're
/// not inside a string.
fn statement_containing(bytes: &[u8], offset: usize) -> (usize, usize) {
    let mut stmt_start = 0usize;
    let mut depth = 0i32;
    let mut in_string: Option<u8> = None;
    let mut i = 0usize;

    while i < bytes.len() {
        let b = bytes[i];
        if let Some(quote) = in_string {
            if b == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if b == quote {
                in_string = None;
            }
            i += 1;
            continue;
        }
        match b {
            b'\'' | b'"' => in_string = Some(b),
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b';' if depth == 0 => {
                if i >= offset {
                    return (stmt_start, i);
                }
                stmt_start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    (stmt_start, bytes.len())
}

/// Find the first `<receiver>-><verb>(` or `Route::<verb>(` whose verb appears
/// in [`HTTP_VERBS`]. Returns the verb name and the byte offset of the first
/// character inside the `(`.
fn find_verb_call(bytes: &[u8], start: usize, end: usize) -> Option<(String, usize)> {
    let mut i = start;
    while i < end {
        for &verb in HTTP_VERBS {
            let vb = verb.as_bytes();
            if i + vb.len() > end || &bytes[i..i + vb.len()] != vb {
                continue;
            }
            // Verb must be preceded by `->` or `::` — this is the left
            // word-boundary check (the `>` / `:` is not an identifier byte).
            let prefix_ok = i >= 2 && (&bytes[i - 2..i] == b"->" || &bytes[i - 2..i] == b"::");
            if !prefix_ok {
                continue;
            }
            // Verb must not be followed by an identifier byte (otherwise
            // `->getUser(` would match `get`).
            let after_verb = i + vb.len();
            if after_verb < end && is_identifier_byte(bytes[after_verb]) {
                continue;
            }
            let mut j = after_verb;
            while j < end && (bytes[j] == b' ' || bytes[j] == b'\t') {
                j += 1;
            }
            if j < end && bytes[j] == b'(' {
                return Some((verb.to_string(), j + 1));
            }
        }
        i += 1;
    }
    None
}

fn is_identifier_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Read a single argument starting at `start` from a (paren-balanced) argument
/// list. Returns (`Some(slice)` of the argument bytes, `Some(idx)` of the byte
/// after the separating comma — or `None` if the closing `)` was reached).
fn read_arg(bytes: &[u8], start: usize) -> (Option<&[u8]>, Option<usize>) {
    let mut depth_paren = 0i32;
    let mut depth_bracket = 0i32;
    let mut depth_brace = 0i32;
    let mut in_string: Option<u8> = None;
    let mut i = start;
    let arg_start = i;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(quote) = in_string {
            if b == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if b == quote {
                in_string = None;
            }
            i += 1;
            continue;
        }
        match b {
            b'\'' | b'"' => in_string = Some(b),
            b'(' => depth_paren += 1,
            b')' => {
                if depth_paren == 0 && depth_bracket == 0 && depth_brace == 0 {
                    let slice = trimmed_slice(bytes, arg_start, i);
                    return (slice, None);
                }
                depth_paren -= 1;
            }
            b'[' => depth_bracket += 1,
            b']' => depth_bracket -= 1,
            b'{' => depth_brace += 1,
            b'}' => depth_brace -= 1,
            b',' if depth_paren == 0 && depth_bracket == 0 && depth_brace == 0 => {
                let slice = trimmed_slice(bytes, arg_start, i);
                return (slice, Some(i + 1));
            }
            _ => {}
        }
        i += 1;
    }
    (None, None)
}

fn trimmed_slice(bytes: &[u8], start: usize, end: usize) -> Option<&[u8]> {
    let mut s = start;
    let mut e = end;
    while s < e && bytes[s].is_ascii_whitespace() {
        s += 1;
    }
    while e > s && bytes[e - 1].is_ascii_whitespace() {
        e -= 1;
    }
    if s >= e {
        None
    } else {
        Some(&bytes[s..e])
    }
}

/// Decode a single-quoted or double-quoted string literal into the raw inner text.
/// Returns `None` for non-string-literal arguments (variables, expressions, etc.).
fn parse_string_literal(slice: &[u8]) -> Option<String> {
    if slice.len() < 2 {
        return None;
    }
    let quote = slice[0];
    if quote != b'\'' && quote != b'"' {
        return None;
    }
    if *slice.last()? != quote {
        return None;
    }
    let inner = &slice[1..slice.len() - 1];
    let mut out = String::with_capacity(inner.len());
    let mut i = 0;
    while i < inner.len() {
        let b = inner[i];
        if b == b'\\' && i + 1 < inner.len() {
            out.push(inner[i + 1] as char);
            i += 2;
            continue;
        }
        out.push(b as char);
        i += 1;
    }
    Some(out)
}

/// Parse the second argument of a route registration into a human-readable
/// `Controller@action` string. Handles the four shapes Laravel accepts plus
/// the closure case. Returns `None` when the argument is something we can't
/// statically resolve (e.g. a variable).
fn parse_action_argument(slice: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(slice).ok()?.trim();
    if text.is_empty() {
        return None;
    }

    // Shape 1: closure literal — `function (...) { ... }` or `fn(...) => ...`
    if text.starts_with("function") || text.starts_with("fn(") || text.starts_with("fn ") {
        return Some("Closure".to_string());
    }

    // Shape 2: `[Controller::class, 'method']`
    if text.starts_with('[') && text.ends_with(']') {
        let inner = &text[1..text.len() - 1];
        let parts: Vec<&str> = inner.splitn(2, ',').map(str::trim).collect();
        if parts.len() == 2 {
            let class = parts[0].trim_end_matches("::class");
            let class_short = short_class_name(class);
            if let Some(method) = parse_string_literal(parts[1].as_bytes()) {
                return Some(format!("{}@{}", class_short, method));
            }
        }
        return None;
    }

    // Shape 3: `Controller::class` — single-action / invokable controller
    if let Some(class) = text.strip_suffix("::class") {
        return Some(short_class_name(class).to_string());
    }

    // Shape 4: `'Controller@method'` — legacy string syntax
    if let Some(s) = parse_string_literal(text.as_bytes()) {
        if s.contains('@') {
            // Render short class name in the @method form.
            let mut parts = s.splitn(2, '@');
            if let (Some(class), Some(method)) = (parts.next(), parts.next()) {
                return Some(format!("{}@{}", short_class_name(class), method));
            }
        }
        return Some(s);
    }

    None
}

/// Take the last `\`-separated segment of a PHP FQN. `App\Http\UserController` → `UserController`.
fn short_class_name(fqn: &str) -> &str {
    fqn.rsplit('\\').next().unwrap_or(fqn)
}

/// Quick content check — does this file likely register named routes?
///
/// Looks for both a route-registration token and a `->name(` call. False
/// positives are tolerable (the index lookup just won't find an entry); false
/// negatives are not (we'd miss valid route definitions).
///
/// Registration shape can be any of:
/// - `Route::` / `Router::` / `$router->` static or facade call
/// - `Route::macro(...)` or `RouteRegistrar` references
/// - An HTTP-verb method invocation (`->get(`, `->post(`, etc.) — covers
///   route macro bodies that bind via `$this->get(...)` (e.g., Laravel UI's
///   `AuthRouteMethods`).
fn file_registers_named_routes(path: &Path) -> bool {
    let Ok(content) = std::fs::read_to_string(path) else {
        return false;
    };
    content_registers_named_routes(&content)
}

/// Content-only variant for testability — same logic as
/// [`file_registers_named_routes`] but operates on a string.
fn content_registers_named_routes(content: &str) -> bool {
    if !content.contains("->name(") {
        return false;
    }
    if content.contains("Route::")
        || content.contains("Router::")
        || content.contains("$router->")
        || content.contains("$this->router->")
        || content.contains("RouteRegistrar")
    {
        return true;
    }

    // HTTP verb invocations also imply route registration shape.
    // Laravel's router/registrar exposes these methods, so finding any of
    // them paired with `->name(` strongly indicates a route definition.
    const VERB_CALLS: &[&str] = &[
        "->get(",
        "->post(",
        "->put(",
        "->patch(",
        "->delete(",
        "->options(",
        "->any(",
        "->match(",
        "->redirect(",
        "->view(",
        "->resource(",
        "->apiResource(",
        "->fallback(",
    ];
    VERB_CALLS.iter().any(|verb| content.contains(verb))
}

fn walk_php_files(dir: &Path, max_depth: usize) -> Vec<PathBuf> {
    WalkDir::new(dir)
        .max_depth(max_depth)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "php"))
        .map(|e| e.path().to_path_buf())
        .collect()
}

fn app_provider_candidates(root: &Path) -> Vec<PathBuf> {
    let mut paths = vec![
        root.join("bootstrap/app.php"),
        root.join("app/Http/Kernel.php"),
    ];
    let providers = root.join("app/Providers");
    if providers.exists() {
        paths.extend(walk_php_files(&providers, 4));
    }
    paths
}

fn is_under_routes_dir(path: &Path) -> bool {
    path.components()
        .any(|c| c.as_os_str().eq_ignore_ascii_case("routes"))
}

fn priority_for_vendor_path(path: &Path) -> u8 {
    let s = path.to_string_lossy();
    if s.contains("/laravel/framework/") || s.contains("\\laravel\\framework\\") {
        PRIORITY_FRAMEWORK
    } else {
        PRIORITY_PACKAGE
    }
}

fn promote(seen: &mut HashMap<PathBuf, u8>, path: PathBuf, priority: u8) {
    seen.entry(path)
        .and_modify(|p| {
            if priority > *p {
                *p = priority;
            }
        })
        .or_insert(priority);
}

fn byte_to_line_col(bytes: &[u8], byte_offset: usize) -> (u32, u32) {
    let mut line = 0u32;
    let mut last_newline: i64 = -1;
    for (idx, b) in bytes.iter().enumerate().take(byte_offset) {
        if *b == b'\n' {
            line += 1;
            last_newline = idx as i64;
        }
    }
    let column = (byte_offset as i64 - last_newline - 1) as u32;
    (line, column)
}

// ============================================================================
// Route-group resolution — composing route names across nested groups
// ============================================================================
//
// Laravel route groups contribute a name prefix to every route declared inside
// the group's closure body:
//
//   Route::name('admin.')->group(function () {
//       Route::get('/users', ...)->name('users.index');  // → "admin.users.index"
//   });
//
// Groups also accept attributes in array form:
//
//   Route::group(['as' => 'api.'], function () {
//       Route::get('/users', ...)->name('users.index');  // → "api.users.index"
//   });
//
// Groups can nest arbitrarily. To compose the final name we:
//   1. Find every `->group(...)` callsite, extracting its prefix contribution
//      and the byte range of its closure body.
//   2. When `extract_named_routes` sees a `->name('X')` callsite at offset O,
//      concatenate every group's prefix whose body range encloses O.

/// Scan `bytes` for every `->group(...)` callsite, returning the spans of
/// those that contribute a name prefix. Forward scan produces spans ordered
/// by `body_start` ascending, so iterating in order is outermost-first when
/// resolving an enclosed `->name(...)`.
fn find_route_groups(bytes: &[u8]) -> Vec<RouteGroupSpan> {
    let paren_pairs = build_paren_pairs(bytes);
    let mut groups = Vec::new();
    // Look for the `group` keyword preceded by either `->` (chained: `->group(`)
    // or `::` (facade: `Route::group(`). Both forms register route groups in
    // Laravel; restricting to `->group` (as the first draft did) missed every
    // `Route::group(['as' => ...], ...)` callsite.
    let keyword = b"group";
    let mut i = 0usize;

    while i + keyword.len() < bytes.len() {
        if &bytes[i..i + keyword.len()] != keyword {
            i += 1;
            continue;
        }
        // Word boundary BEFORE: must be `->` or `::`.
        if i < 2 {
            i += 1;
            continue;
        }
        let connector = &bytes[i - 2..i];
        if connector != b"->" && connector != b"::" {
            i += 1;
            continue;
        }
        // Word boundary AFTER: must not be `groupBy`, `groupName`, etc.
        let after_g = i + keyword.len();
        if after_g < bytes.len() && is_identifier_byte(bytes[after_g]) {
            i = after_g;
            continue;
        }
        // Find the `(` opening the group's arg list.
        let mut j = after_g;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if j >= bytes.len() || bytes[j] != b'(' {
            i = j.max(i + 1);
            continue;
        }
        let args_open = j;
        let Some(&args_close) = paren_pairs.get(&args_open) else {
            i = args_open + 1;
            continue;
        };

        // Prefix can come from one of:
        //   (a) a `->name('X')` link earlier in the same fluent chain
        //   (b) an `['as' => 'X', ...]` array literal as the first arg
        //
        // Use the connector position (start of `->`/`::`) so the chain walker
        // sees this call's left edge correctly.
        let group_offset = i - 2;
        let chain_prefix = extract_chain_name_prefix(bytes, group_offset, &paren_pairs);
        let array_prefix = extract_array_as_prefix(bytes, args_open + 1, args_close);
        let prefix = chain_prefix.or(array_prefix);

        // Closure body: `function (...) { ... }` between args_open+1 and args_close.
        let body = find_function_body(bytes, args_open + 1, args_close);

        if let (Some(prefix), Some((body_start, body_end))) = (prefix, body) {
            groups.push(RouteGroupSpan {
                prefix,
                body_start,
                body_end,
            });
        }

        // Continue scanning forward — including INSIDE this group's body, so
        // nested groups get picked up. (Skipping to args_close + 1 would lose
        // every nested `->group(...)`.)
        i += 1;
    }

    groups
}

/// Scan `bytes` for every `->group(...)`/`::group(...)` callsite that loads an
/// *external file* rather than running a closure body (issue #43). A load is
/// recorded only when the call has NO closure body and its (path) argument
/// resolves to a file path. The own-prefix (which may be empty) is captured the
/// same way closure groups capture theirs.
///
/// `root` is the project root (for `base_path(...)`); `loader_dir` is the
/// directory of the file being scanned (for `__DIR__ . '...'`).
fn find_external_group_loads(
    bytes: &[u8],
    root: &Path,
    loader_dir: &Path,
) -> Vec<ExternalGroupLoad> {
    let paren_pairs = build_paren_pairs(bytes);
    let mut loads = Vec::new();
    let keyword = b"group";
    let mut i = 0usize;

    while i + keyword.len() < bytes.len() {
        if &bytes[i..i + keyword.len()] != keyword {
            i += 1;
            continue;
        }
        if i < 2 {
            i += 1;
            continue;
        }
        let connector = &bytes[i - 2..i];
        if connector != b"->" && connector != b"::" {
            i += 1;
            continue;
        }
        let after_g = i + keyword.len();
        if after_g < bytes.len() && is_identifier_byte(bytes[after_g]) {
            i = after_g;
            continue;
        }
        let mut j = after_g;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if j >= bytes.len() || bytes[j] != b'(' {
            i = j.max(i + 1);
            continue;
        }
        let args_open = j;
        let Some(&args_close) = paren_pairs.get(&args_open) else {
            i = args_open + 1;
            continue;
        };

        // An external load never has a closure body — skip closure groups so we
        // don't double-handle them (they're owned by `find_route_groups`).
        if find_function_body(bytes, args_open + 1, args_close).is_some() {
            i += 1;
            continue;
        }

        let group_offset = i - 2;
        let chain_prefix = extract_chain_name_prefix(bytes, group_offset, &paren_pairs);
        let array_prefix = extract_array_as_prefix(bytes, args_open + 1, args_close);

        // Determine which argument holds the path. With an array `['as' => ...]`
        // first arg (array form), the path is the SECOND argument; otherwise the
        // fluent `->as(...)->group($path)` form puts it FIRST.
        let array_first = first_arg_is_array_literal(bytes, args_open + 1, args_close);
        let path_slice = if array_first {
            nth_arg_slice(bytes, args_open + 1, args_close, 1)
        } else {
            nth_arg_slice(bytes, args_open + 1, args_close, 0)
        };

        if let Some(slice) = path_slice {
            if let Some(target) = resolve_path_argument(slice, root, loader_dir) {
                loads.push(ExternalGroupLoad {
                    offset: group_offset,
                    own_prefix: chain_prefix.or(array_prefix).unwrap_or_default(),
                    target,
                });
            }
        }

        i += 1;
    }

    loads
}

/// Does the first argument in `[start..end]` begin with a `[` array literal?
fn first_arg_is_array_literal(bytes: &[u8], start: usize, end: usize) -> bool {
    let mut i = start;
    while i < end && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    i < end && bytes[i] == b'['
}

/// Return the trimmed byte slice of the `n`-th (0-based) comma-separated
/// argument inside `[start..end)`, respecting nested parens/brackets/braces and
/// string literals. `None` if there is no such argument.
fn nth_arg_slice(bytes: &[u8], start: usize, end: usize, n: usize) -> Option<&[u8]> {
    let mut depth_paren = 0i32;
    let mut depth_bracket = 0i32;
    let mut depth_brace = 0i32;
    let mut in_string: Option<u8> = None;
    let mut arg_index = 0usize;
    let mut arg_start = start;
    let mut i = start;
    while i < end {
        let b = bytes[i];
        if let Some(quote) = in_string {
            if b == b'\\' && i + 1 < end {
                i += 2;
                continue;
            }
            if b == quote {
                in_string = None;
            }
            i += 1;
            continue;
        }
        match b {
            b'\'' | b'"' => in_string = Some(b),
            b'(' => depth_paren += 1,
            b')' => depth_paren -= 1,
            b'[' => depth_bracket += 1,
            b']' => depth_bracket -= 1,
            b'{' => depth_brace += 1,
            b'}' => depth_brace -= 1,
            b',' if depth_paren == 0 && depth_bracket == 0 && depth_brace == 0 => {
                if arg_index == n {
                    return trimmed_slice(bytes, arg_start, i);
                }
                arg_index += 1;
                arg_start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    if arg_index == n {
        trimmed_slice(bytes, arg_start, end)
    } else {
        None
    }
}

/// Resolve a `->group(...)` path argument to an absolute path. Recognized
/// forms (the file need not exist — `.`/`..` are normalized lexically):
/// - `base_path('sub/dir')` → `root/sub/dir`; `base_path()` → `root`
/// - `__DIR__ . '/sub'` / `__DIR__.'sub'` → `loader_dir/sub`
/// - bare string literal `'x'` → absolute as-is, else `root/x`
/// - anything else → `None` (skip).
fn resolve_path_argument(slice: &[u8], root: &Path, loader_dir: &Path) -> Option<PathBuf> {
    let text = std::str::from_utf8(slice).ok()?.trim();
    if text.is_empty() {
        return None;
    }

    // base_path('...') / base_path()
    if let Some(rest) = text.strip_prefix("base_path") {
        let rest = rest.trim_start();
        let inner = rest.strip_prefix('(')?.strip_suffix(')')?.trim();
        if inner.is_empty() {
            return Some(normalize_path(root));
        }
        let sub = parse_string_literal(inner.as_bytes())?;
        return Some(normalize_path(&root.join(sub)));
    }

    // __DIR__ . '...'  (the dot and surrounding whitespace are optional spacing)
    if let Some(rest) = text.strip_prefix("__DIR__") {
        let rest = rest.trim_start().strip_prefix('.')?.trim_start();
        let sub = parse_string_literal(rest.as_bytes())?;
        let sub = sub.trim_start_matches('/');
        return Some(normalize_path(&loader_dir.join(sub)));
    }

    // Bare string literal.
    if let Some(s) = parse_string_literal(text.as_bytes()) {
        let p = Path::new(&s);
        if p.is_absolute() {
            return Some(normalize_path(p));
        }
        return Some(normalize_path(&root.join(s)));
    }

    None
}

/// Lexically normalize a path: collapse `.` and resolve `..` against prior
/// components without touching the filesystem (the target may not exist).
///
/// Public so callers (e.g. `did_save` in `main.rs`) can normalize a path the
/// same way before comparing it against [`RouteIndex::source_files`].
pub fn normalize_path(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                // Pop only a real directory segment; preserve root/prefix and
                // any leading `..` that can't be resolved lexically.
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                } else {
                    out.push(comp.as_os_str());
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Walk backward from a `->group(` offset through prior chain links looking
/// for a `->name('X')` whose string argument should prefix routes in this
/// group. Returns the prefix string (e.g. `"admin."`).
///
/// The walk skips over each intervening `->method(...)` link by paren-balancing
/// — that's why we need the pre-computed paren_pairs map.
fn extract_chain_name_prefix(
    bytes: &[u8],
    group_offset: usize,
    paren_pairs: &HashMap<usize, usize>,
) -> Option<String> {
    let mut cursor = group_offset;
    loop {
        if cursor < 2 {
            return None;
        }
        // The byte just before `->` should be `)` closing the previous link.
        let mut k = cursor;
        while k > 0 && bytes[k - 1].is_ascii_whitespace() {
            k -= 1;
        }
        if k == 0 || bytes[k - 1] != b')' {
            return None;
        }
        let close_paren = k - 1;
        let open_paren = *paren_pairs
            .iter()
            .find(|(_, &v)| v == close_paren)
            .map(|(k, _)| k)?;

        // Walk back from `(` over the method name.
        let mut name_end = open_paren;
        while name_end > 0 && bytes[name_end - 1].is_ascii_whitespace() {
            name_end -= 1;
        }
        let mut name_start = name_end;
        while name_start > 0 && is_identifier_byte(bytes[name_start - 1]) {
            name_start -= 1;
        }
        if name_start == name_end {
            return None;
        }
        let method = std::str::from_utf8(&bytes[name_start..name_end]).ok()?;

        // The 2 bytes before the method name must be `->` or `::`.
        if name_start < 2 {
            return None;
        }
        let connector = &bytes[name_start - 2..name_start];
        if connector != b"->" && connector != b"::" {
            return None;
        }

        // `Route::name('admin.')` and its alias `Route::as('admin.')` both set
        // a group's name prefix.
        if method == "name" || method == "as" {
            // Extract the string literal first-argument from the `()`.
            return read_first_string_literal(bytes, open_paren + 1, close_paren);
        }

        // Not a name-setter — continue walking back from the connector position.
        cursor = name_start - 2;
    }
}

/// Read the first string literal in `[start..end]`. Returns the unescaped
/// inner text. Skips leading whitespace; ignores subsequent arguments.
fn read_first_string_literal(bytes: &[u8], start: usize, end: usize) -> Option<String> {
    let mut i = start;
    while i < end && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= end {
        return None;
    }
    let quote = bytes[i];
    if quote != b'\'' && quote != b'"' {
        return None;
    }
    i += 1;
    let mut out = String::new();
    while i < end && bytes[i] != quote {
        if bytes[i] == b'\\' && i + 1 < end {
            out.push(bytes[i + 1] as char);
            i += 2;
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    if i >= end {
        return None;
    }
    Some(out)
}

/// Look for `['as' => 'X', ...]` in the group's first argument. Returns the
/// value of the `as` key when found.
fn extract_array_as_prefix(bytes: &[u8], start: usize, end: usize) -> Option<String> {
    // First arg must start with `[`.
    let mut i = start;
    while i < end && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= end || bytes[i] != b'[' {
        return None;
    }

    // Scan within the array literal for `'as'` (or `"as"`) followed by `=>`
    // and a string. Operates within the args range, ignoring nested arrays
    // and strings via depth-tracking.
    let patterns: [&[u8]; 2] = [b"'as'", b"\"as\""];
    for pat in patterns {
        let mut j = i;
        let mut in_string: Option<u8> = None;
        while j + pat.len() < end {
            let b = bytes[j];
            if let Some(q) = in_string {
                if b == b'\\' && j + 1 < end {
                    j += 2;
                    continue;
                }
                if b == q {
                    in_string = None;
                }
                j += 1;
                continue;
            }
            if b == b'\'' || b == b'"' {
                // Check if this is the start of the pattern we want
                if j + pat.len() <= end && &bytes[j..j + pat.len()] == pat {
                    // Look past the pattern for `=>` then a string literal.
                    let mut k = j + pat.len();
                    while k < end && bytes[k].is_ascii_whitespace() {
                        k += 1;
                    }
                    if k + 2 <= end && &bytes[k..k + 2] == b"=>" {
                        let value = read_first_string_literal(bytes, k + 2, end);
                        if value.is_some() {
                            return value;
                        }
                    }
                }
                in_string = Some(b);
                j += 1;
                continue;
            }
            j += 1;
        }
    }
    None
}

/// Find the byte range of a `function (...) { ... }` body inside the group's
/// arg list. Returns `(body_start, body_end)` — body_start points to the byte
/// right after the opening `{`, body_end to the matching `}` byte.
fn find_function_body(bytes: &[u8], start: usize, end: usize) -> Option<(usize, usize)> {
    let kw = b"function";
    let mut i = start;
    let mut in_string: Option<u8> = None;
    while i + kw.len() < end {
        let b = bytes[i];
        if let Some(q) = in_string {
            if b == b'\\' && i + 1 < end {
                i += 2;
                continue;
            }
            if b == q {
                in_string = None;
            }
            i += 1;
            continue;
        }
        if b == b'\'' || b == b'"' {
            in_string = Some(b);
            i += 1;
            continue;
        }
        if i + kw.len() <= end && &bytes[i..i + kw.len()] == kw {
            let before_ok = i == 0 || !is_identifier_byte(bytes[i - 1]);
            let after = i + kw.len();
            let after_ok = after >= bytes.len() || !is_identifier_byte(bytes[after]);
            if before_ok && after_ok {
                // Walk forward to the `{`. PHP's `function () use ($x) {`
                // and other variants all end with a `{`.
                let mut j = after;
                while j < end && bytes[j] != b'{' {
                    j += 1;
                }
                if j >= end {
                    return None;
                }
                let body_start = j + 1;
                // Brace-balance from j to find the close.
                let mut depth = 1i32;
                let mut k = body_start;
                let mut in_s: Option<u8> = None;
                while k < bytes.len() {
                    let bc = bytes[k];
                    if let Some(q) = in_s {
                        if bc == b'\\' && k + 1 < bytes.len() {
                            k += 2;
                            continue;
                        }
                        if bc == q {
                            in_s = None;
                        }
                        k += 1;
                        continue;
                    }
                    match bc {
                        b'\'' | b'"' => in_s = Some(bc),
                        b'{' => depth += 1,
                        b'}' => {
                            depth -= 1;
                            if depth == 0 {
                                return Some((body_start, k));
                            }
                        }
                        _ => {}
                    }
                    k += 1;
                }
                return None;
            }
        }
        i += 1;
    }
    None
}

/// Pre-compute open `(` → close `)` pairs across the entire file. Skips
/// quoted strings so parens inside string literals don't mis-balance. Used
/// for backward chain walks where naive byte scanning would get confused by
/// nested calls.
fn build_paren_pairs(bytes: &[u8]) -> HashMap<usize, usize> {
    let mut pairs = HashMap::new();
    let mut stack: Vec<usize> = Vec::new();
    let mut in_string: Option<u8> = None;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = in_string {
            if b == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if b == q {
                in_string = None;
            }
            i += 1;
            continue;
        }
        match b {
            b'\'' | b'"' => in_string = Some(b),
            b'(' => stack.push(i),
            b')' => {
                if let Some(open) = stack.pop() {
                    pairs.insert(open, i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    pairs
}

#[cfg(test)]
mod tests;
