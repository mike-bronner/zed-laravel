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
pub fn build_route_index(files: &[RouteFile]) -> RouteIndex {
    let mut index = RouteIndex::new();
    for file in files {
        if let Ok(content) = std::fs::read_to_string(&file.path) {
            for def in extract_named_routes(&content, &file.path, file.priority) {
                if let Some(name) = def.0 {
                    index.insert(name, def.1);
                }
            }
        }
    }
    index
}

/// Extract every `->name('X')` callsite from the given source.
///
/// Returns `(name, RouteDefinition)` pairs. The match is line-based and tolerant
/// of whitespace inside the call: `->name('X')`, `->name ( "X" )`, etc.
///
/// Only matches single-quoted or double-quoted string literals. Variable
/// arguments (`->name($var)`) are skipped because we can't resolve them
/// statically.
pub fn extract_named_routes(
    content: &str,
    file: &Path,
    priority: u8,
) -> Vec<(Option<String>, RouteDefinition)> {
    let mut results = Vec::new();
    let bytes = content.as_bytes();

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

        // Compose the final route name from enclosing group prefixes plus the
        // literal `->name('X')` value. Groups are pre-sorted by body_start so
        // iterating in order yields outermost-first.
        let mut name = String::new();
        for grp in &groups {
            if grp.body_start <= i && i < grp.body_end {
                name.push_str(&grp.prefix);
            }
        }
        name.push_str(&literal_name);

        let (line, column) = byte_to_line_col(bytes, i);
        let end_column = column + (j - i + 1) as u32; // include closing quote

        // Scan backward from the `->name(` callsite to find the verb call that
        // started this route's fluent chain. Captures HTTP method, URI, and the
        // controller@action target for hover display.
        let metadata = extract_route_metadata(bytes, i);

        results.push((
            Some(name),
            RouteDefinition {
                file: file.to_path_buf(),
                line,
                column,
                end_column,
                priority,
                method: metadata.method,
                uri: metadata.uri,
                action: metadata.action,
            },
        ));

        i = j + 1;
    }

    results
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

        if method == "name" {
            // Extract the string literal first-argument from the `()`.
            return read_first_string_literal(bytes, open_paren + 1, close_paren);
        }

        // Not `name` — continue walking back from the connector position.
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
