//! PHP file structural outline via tree-sitter.
//!
//! Returns the top-level classes / interfaces / traits / enums in a PHP file
//! together with their methods and properties, plus any free functions. The
//! types are plain data so consumers (document_symbols.rs) can filter for
//! Laravel-specific shapes (Livewire components, Eloquent models) on top.
//!
//! Tree-sitter is the right tool here — regex-based PHP class parsing trips
//! over heredocs, strings, and comments, and gets ambiguous on multi-class
//! files. The cost is one extra parse per outline request, memoized via Salsa.

use tree_sitter::Node;

/// Parsed structure of a PHP file: top-level class-like declarations plus
/// free-standing functions. Empty default is used as the "parse failed"
/// sentinel.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct PhpFileStructure {
    /// File-level `namespace Foo\Bar;`, when present. Stripped of the
    /// `namespace` keyword and trailing `;` — just the dotted name.
    pub namespace: Option<String>,
    pub structures: Vec<PhpStructure>,
    pub functions: Vec<PhpFunctionInfo>,
}

/// A class/interface/trait/enum declaration with its members.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PhpStructure {
    pub kind: PhpStructureKind,
    pub name: String,
    /// Simplified `extends` target (final `\`-segment), if any.
    pub extends: Option<String>,
    /// Raw `extends` target as it appears in source — preserves
    /// namespace separators and leading `\` for FQCN resolution. `None`
    /// when the class doesn't extend anything.
    pub extends_raw: Option<String>,
    /// Bare names of traits this structure composes via `use TraitName;`
    /// inside the class/trait body. Top-level only — names inside the
    /// conflict-resolution `{ ... }` block are not duplicated.
    pub trait_uses: Vec<String>,
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
    pub methods: Vec<PhpMethodInfo>,
    pub properties: Vec<PhpPropertyInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PhpStructureKind {
    Class,
    Interface,
    Trait,
    Enum,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PhpMethodInfo {
    pub name: String,
    pub visibility: PhpVisibility,
    /// `true` when the method carries a `static` modifier.
    pub is_static: bool,
    /// Simplified return type (final `\`-segment), if any.
    pub return_type: Option<String>,
    /// Raw return type as it appears in source — preserves namespace
    /// separators, generics, union syntax (`?Foo`, `Bar|null`).
    pub return_type_raw: Option<String>,
    pub parameters: Vec<PhpParameter>,
    /// The literal signature line from source, normalised to a single
    /// line and with body braces stripped: `public function with($x = null)`.
    /// Drives the syntax-highlighted code block in completion popups.
    pub raw_signature: String,
    /// PHPDoc body (`/** … */` with markers stripped) immediately
    /// preceding the method declaration, when present.
    pub docblock: Option<String>,
    /// Raw source text of the method's body, including the outer `{}`
    /// braces, when the method has one (abstract / interface methods
    /// return `None`). Lets consumers run focused text/regex extraction
    /// on a method's body without re-walking the tree — e.g.
    /// model_analyzer's relationship detection looks for
    /// `return $this->hasMany(...)` patterns inside method bodies.
    pub body_source: Option<String>,
    /// PHP attributes (`#[Foo]`, `#[Foo(args)]`) applied to the method,
    /// in source order. Each entry is the attribute's name as written —
    /// could be bare (`Scope`) or qualified (`\Illuminate\…\Scope`).
    /// Callers that need an FQCN should resolve through the file's use
    /// aliases. Argument lists are stripped.
    pub attributes: Vec<String>,
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PhpPropertyInfo {
    /// Name without leading `$`.
    pub name: String,
    pub visibility: PhpVisibility,
    /// `true` when the property carries a `static` modifier.
    pub is_static: bool,
    pub property_type: Option<String>,
    /// The default-value expression as it appears in source, e.g.
    /// `"['col' => 'json', 'meta' => 'array']"`. `None` when the property
    /// has no initialiser. Lets specialty parsers (model_analyzer's
    /// `$casts` extraction, for instance) work off the literal text
    /// without re-walking the tree.
    pub default_value: Option<String>,
    /// PHPDoc body immediately preceding the property declaration, with
    /// markers stripped.
    pub docblock: Option<String>,
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PhpFunctionInfo {
    pub name: String,
    pub return_type: Option<String>,
    pub parameters: Vec<PhpParameter>,
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
}

/// A single parameter from a method or function signature. `name` is the
/// variable name without the leading `$`. `param_type` is the simplified
/// type annotation if present.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PhpParameter {
    pub name: String,
    pub param_type: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PhpVisibility {
    Public,
    Protected,
    Private,
}

/// Parse a PHP file and extract its structural outline. Returns an empty
/// `PhpFileStructure` on parse failure (which Salsa caches the same way as a
/// successful empty result — callers shouldn't distinguish).
pub fn extract_php_structure(content: &str) -> PhpFileStructure {
    let Ok(tree) = crate::parser::parse_php(content) else {
        return PhpFileStructure::default();
    };
    let source = content.as_bytes();
    let mut result = PhpFileStructure::default();
    walk_top_level(tree.root_node(), source, &mut result);
    result
}

/// Walk the top-level of a node and collect any class-like declarations and
/// free functions found. Recurses into namespace blocks so declarations inside
/// `namespace Foo { ... }` still surface.
fn walk_top_level(node: Node, source: &[u8], result: &mut PhpFileStructure) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "class_declaration" => {
                if let Some(s) = parse_structure(child, source, PhpStructureKind::Class) {
                    result.structures.push(s);
                }
            }
            "interface_declaration" => {
                if let Some(s) = parse_structure(child, source, PhpStructureKind::Interface) {
                    result.structures.push(s);
                }
            }
            "trait_declaration" => {
                if let Some(s) = parse_structure(child, source, PhpStructureKind::Trait) {
                    result.structures.push(s);
                }
            }
            "enum_declaration" => {
                if let Some(s) = parse_structure(child, source, PhpStructureKind::Enum) {
                    result.structures.push(s);
                }
            }
            "function_definition" => {
                if let Some(f) = parse_function(child, source) {
                    result.functions.push(f);
                }
            }
            // File-level `namespace Foo\Bar;` — capture the name. PHP's
            // bracketed form (`namespace Foo { … }`) ALSO emits a
            // `namespace_definition` node; both are handled here.
            "namespace_definition" => {
                if result.namespace.is_none() {
                    if let Some(name_node) = child
                        .child_by_field_name("name")
                        .or_else(|| find_child_kind(child, &["namespace_name", "qualified_name"]))
                    {
                        if let Ok(text) = name_node.utf8_text(source) {
                            result.namespace = Some(text.to_string());
                        }
                    }
                }
                walk_top_level(child, source, result);
                continue;
            }
            // Recurse into wrappers that commonly contain top-level
            // declarations:
            //   - `compound_statement` for namespace / if-statement bodies
            //   - `if_statement` for the `function_exists` guard pattern
            //     common in Laravel helpers files
            //   - `else_clause` / `else_if_clause` for the same
            "compound_statement"
            | "if_statement"
            | "else_clause"
            | "else_if_clause" => {
                walk_top_level(child, source, result);
            }
            _ => {}
        }
    }
}

fn parse_structure(node: Node, source: &[u8], kind: PhpStructureKind) -> Option<PhpStructure> {
    let name = field_text(node, "name", source)?;
    let (extends, extends_raw) = parse_extends(node, source);
    let (start_line, start_column) = pos(node.start_position());
    let (end_line, end_column) = pos(node.end_position());

    let mut methods = Vec::new();
    let mut properties = Vec::new();
    let mut trait_uses: Vec<String> = Vec::new();

    if let Some(body) = node.child_by_field_name("body") {
        // Materialise children so each method/property can look back at
        // its preceding `comment` sibling for the PHPDoc.
        let mut body_cursor = body.walk();
        let body_children: Vec<Node> = body.children(&mut body_cursor).collect();

        for (idx, member) in body_children.iter().enumerate() {
            match member.kind() {
                "method_declaration" => {
                    let docblock = previous_docblock(&body_children, idx, source);
                    let attributes = method_attributes(*member, source);
                    if let Some(m) = parse_method(*member, source, docblock, attributes) {
                        methods.push(m);
                    }
                }
                "property_declaration" => {
                    let docblock = previous_docblock(&body_children, idx, source);
                    properties.extend(parse_properties(*member, source, docblock));
                }
                "use_declaration" => {
                    collect_trait_uses(*member, source, &mut trait_uses);
                }
                _ => {}
            }
        }
    }

    Some(PhpStructure {
        kind,
        name,
        extends,
        extends_raw,
        trait_uses,
        start_line,
        start_column,
        end_line,
        end_column,
        methods,
        properties,
    })
}

/// Returns `(simplified_name, raw_text)` for the parent in `class X extends Y`.
/// `simplified_name` is the basename (`Y` from `Foo\Y`); `raw_text` preserves
/// namespace separators and leading backslash so callers can resolve to FQCN
/// through file-level use aliases.
fn parse_extends(node: Node, source: &[u8]) -> (Option<String>, Option<String>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "base_clause" {
            let mut bc_cursor = child.walk();
            for bc_child in child.children(&mut bc_cursor) {
                let kind = bc_child.kind();
                if kind == "name" || kind == "qualified_name" {
                    if let Ok(text) = bc_child.utf8_text(source) {
                        return (Some(simple_name(text)), Some(text.to_string()));
                    }
                }
            }
        }
    }
    (None, None)
}

/// Walk a class-body `use_declaration` (PHP trait composition syntax) and
/// push each imported trait name onto `out`. Names inside the
/// conflict-resolution `{ ... }` block are skipped — they reference traits
/// already collected at the top level.
fn collect_trait_uses(node: Node, source: &[u8], out: &mut Vec<String>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "name" | "qualified_name" => {
                if let Ok(text) = child.utf8_text(source) {
                    out.push(text.to_string());
                }
            }
            "declaration_list" => break,
            _ => {}
        }
    }
}

/// Collect every PHP attribute applied to a method declaration. In
/// tree-sitter-php, `attribute_list` nodes are CHILDREN of the
/// `method_declaration`, not siblings — we walk the method's direct
/// children for them.
///
/// Returns each attribute's name in source order. `#[Foo, Bar(arg)]`
/// yields `["Foo", "Bar"]`. `#[\Ns\Foo]` yields `"\\Ns\\Foo"` — the
/// caller resolves namespacing via the file's use aliases if it cares.
fn method_attributes(method: Node, source: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut cursor = method.walk();
    for child in method.children(&mut cursor) {
        if child.kind() == "attribute_list" {
            collect_attribute_names(child, source, &mut out);
        }
    }
    out
}

/// Walk an `attribute_list` (`#[Foo, Bar(arg)]`) and append each
/// attribute's name to `out`. Handles both bare names and qualified
/// (`\Ns\Foo`) forms. Argument lists are ignored.
fn collect_attribute_names(node: Node, source: &[u8], out: &mut Vec<String>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        // Tree-sitter-php structures: attribute_list → attribute_group
        // → attribute → name / qualified_name + optional arguments.
        // Recurse one level so we work across grammar versions.
        match child.kind() {
            "attribute" => {
                let mut ac = child.walk();
                for sub in child.children(&mut ac) {
                    if matches!(sub.kind(), "name" | "qualified_name") {
                        if let Ok(text) = sub.utf8_text(source) {
                            out.push(text.to_string());
                            break; // only the name; ignore arguments
                        }
                    }
                }
            }
            "attribute_group" => collect_attribute_names(child, source, out),
            _ => {}
        }
    }
}

/// Look backward from `idx - 1` for a `/** ... */` docblock immediately
/// preceding the member, skipping over `attribute_list` nodes (PHP `#[…]`).
/// Returns the body with `/**`, `*/`, and per-line `*` markers stripped.
fn previous_docblock(children: &[Node], idx: usize, source: &[u8]) -> Option<String> {
    let mut i = idx;
    while i > 0 {
        i -= 1;
        let prev = children[i];
        match prev.kind() {
            "attribute_list" => continue,
            "comment" => {
                let text = prev.utf8_text(source).ok()?;
                if !text.starts_with("/**") {
                    return None;
                }
                return Some(strip_docblock_markers(text));
            }
            _ => return None,
        }
    }
    None
}

/// Strip `/**`, `*/`, and per-line leading `*` from a PHPDoc comment.
fn strip_docblock_markers(docblock: &str) -> String {
    let trimmed = docblock.trim();
    let inner = trimmed
        .strip_prefix("/**")
        .unwrap_or(trimmed)
        .strip_suffix("*/")
        .unwrap_or(trimmed);
    inner
        .lines()
        .map(|l| {
            let l = l.trim();
            l.strip_prefix('*').map(str::trim).unwrap_or(l)
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

fn parse_method(
    node: Node,
    source: &[u8],
    docblock: Option<String>,
    attributes: Vec<String>,
) -> Option<PhpMethodInfo> {
    let name = field_text(node, "name", source)?;
    let visibility = parse_visibility(node, source);
    let is_static = has_static_modifier(node, source);
    let return_type_raw = node
        .child_by_field_name("return_type")
        .and_then(|n| n.utf8_text(source).ok())
        .map(|s| s.trim_start_matches([':', ' ']).trim().to_string());
    let return_type = return_type_raw.as_deref().map(simple_name);
    let parameters = node
        .child_by_field_name("parameters")
        .map(|p| parse_parameters(p, source))
        .unwrap_or_default();
    let raw_signature = extract_raw_signature(node, source);
    let body_source = node.child_by_field_name("body").and_then(|b| {
        std::str::from_utf8(&source[b.start_byte()..b.end_byte()])
            .ok()
            .map(str::to_string)
    });
    let (start_line, start_column) = pos(node.start_position());
    let (end_line, end_column) = pos(node.end_position());

    Some(PhpMethodInfo {
        name,
        visibility,
        is_static,
        return_type,
        return_type_raw,
        parameters,
        raw_signature,
        docblock,
        body_source,
        attributes,
        start_line,
        start_column,
        end_line,
        end_column,
    })
}

/// Extract a method's signature as a single normalised line, ending just
/// before the body's opening `{` (or the trailing `;` for abstract/
/// interface methods).
fn extract_raw_signature(node: Node, source: &[u8]) -> String {
    let start = node.start_byte();
    let end = node
        .child_by_field_name("body")
        .map(|b| b.start_byte())
        .unwrap_or_else(|| node.end_byte());
    let raw = std::str::from_utf8(&source[start..end]).unwrap_or("");
    let normalised = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    normalised.trim_end_matches(';').trim_end().to_string()
}

fn parse_properties(
    node: Node,
    source: &[u8],
    docblock: Option<String>,
) -> Vec<PhpPropertyInfo> {
    let visibility = parse_visibility(node, source);
    let is_static = has_static_modifier(node, source);
    let property_type = node
        .child_by_field_name("type")
        .and_then(|n| n.utf8_text(source).ok())
        .map(|s| simple_name(s.trim()));

    let mut props = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() != "property_element" {
            continue;
        }
        // Capture the default-value expression by source slice — tree-sitter
        // exposes it via the `default_value` field, but the field name
        // varies across grammar versions. Falling back to "everything after
        // the `=`" is robust.
        let default_value = property_default_value(child, source);

        let mut prop_cursor = child.walk();
        for pchild in child.children(&mut prop_cursor) {
            // `variable_name` wraps the `$name` token; some tree-sitter-php
            // versions surface the bare `name` directly. Handle both.
            if pchild.kind() == "variable_name" || pchild.kind() == "name" {
                if let Ok(text) = pchild.utf8_text(source) {
                    let (line, column) = pos(pchild.start_position());
                    let (end_line, end_column) = pos(pchild.end_position());
                    props.push(PhpPropertyInfo {
                        name: text.trim_start_matches('$').to_string(),
                        visibility,
                        is_static,
                        property_type: property_type.clone(),
                        default_value: default_value.clone(),
                        docblock: docblock.clone(),
                        start_line: line,
                        start_column: column,
                        end_line,
                        end_column,
                    });
                }
            }
        }
    }
    props
}

/// First direct child whose node kind matches any of `kinds`. Used as a
/// fallback when a node doesn't expose a named field — tree-sitter-php
/// occasionally varies between versions on whether subexpressions are
/// addressable by field name.
fn find_child_kind<'a>(node: Node<'a>, kinds: &[&str]) -> Option<Node<'a>> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if kinds.contains(&child.kind()) {
            return Some(child);
        }
    }
    None
}

/// Look for a `default_value:` field on a `property_element`; if missing,
/// fall back to "everything after the first `=` in the element's source".
/// Returns `None` when the property has no initialiser.
fn property_default_value(element: Node, source: &[u8]) -> Option<String> {
    if let Some(dv) = element.child_by_field_name("default_value") {
        if let Ok(text) = dv.utf8_text(source) {
            return Some(text.trim().to_string());
        }
    }
    // Fallback: slice the element's text after the first `=`.
    let text = element.utf8_text(source).ok()?;
    let eq_idx = text.find('=')?;
    let after = text[eq_idx + 1..].trim().trim_end_matches(';').trim();
    if after.is_empty() {
        None
    } else {
        Some(after.to_string())
    }
}

/// `true` when the declaration carries a `static` modifier. Checks both
/// the typed `static_modifier` node and bare `static` tokens for
/// tree-sitter-php version compatibility.
fn has_static_modifier(node: Node, source: &[u8]) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "static_modifier" {
            return true;
        }
        if let Ok(text) = child.utf8_text(source) {
            if text == "static" {
                return true;
            }
        }
    }
    false
}

/// Look for a `visibility_modifier` child and read its keyword. PHP defaults
/// to `public` when no modifier is present (matches the language spec).
fn parse_visibility(node: Node, source: &[u8]) -> PhpVisibility {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "visibility_modifier" {
            if let Ok(text) = child.utf8_text(source) {
                return match text {
                    "private" => PhpVisibility::Private,
                    "protected" => PhpVisibility::Protected,
                    _ => PhpVisibility::Public,
                };
            }
        }
    }
    PhpVisibility::Public
}

fn parse_function(node: Node, source: &[u8]) -> Option<PhpFunctionInfo> {
    let name = field_text(node, "name", source)?;
    let return_type = node
        .child_by_field_name("return_type")
        .and_then(|n| n.utf8_text(source).ok())
        .map(|s| simple_name(s.trim_start_matches([':', ' ']).trim()));
    let parameters = node
        .child_by_field_name("parameters")
        .map(|p| parse_parameters(p, source))
        .unwrap_or_default();
    let (start_line, start_column) = pos(node.start_position());
    let (end_line, end_column) = pos(node.end_position());

    Some(PhpFunctionInfo {
        name,
        return_type,
        parameters,
        start_line,
        start_column,
        end_line,
        end_column,
    })
}

/// Walk a `formal_parameters` node and extract each parameter's name and
/// type annotation. Skips defaults (the symbol label has finite room and
/// defaults add noise without aiding navigation). Handles simple
/// parameters, constructor property promotion, and variadic parameters
/// uniformly — we just extract `(type, $name)` for each.
fn parse_parameters(node: Node, source: &[u8]) -> Vec<PhpParameter> {
    let mut params = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "simple_parameter" | "property_promotion_parameter" | "variadic_parameter" => {
                if let Some(param) = parse_one_parameter(child, source) {
                    params.push(param);
                }
            }
            _ => {}
        }
    }
    params
}

fn parse_one_parameter(node: Node, source: &[u8]) -> Option<PhpParameter> {
    // Type may appear as a `type:` field or as a child node of various
    // type-related kinds depending on the tree-sitter-php version.
    let param_type = node
        .child_by_field_name("type")
        .and_then(|n| n.utf8_text(source).ok())
        .map(|s| simple_name(s.trim()));

    // Name comes from a `variable_name` child (or `name` field directly).
    let mut name: Option<String> = None;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "variable_name" {
            if let Ok(text) = child.utf8_text(source) {
                name = Some(text.trim_start_matches('$').to_string());
                break;
            }
        }
    }
    let name = name?;

    Some(PhpParameter { name, param_type })
}

fn field_text(node: Node, field: &str, source: &[u8]) -> Option<String> {
    node.child_by_field_name(field)?
        .utf8_text(source)
        .ok()
        .map(|s| s.to_string())
}

fn pos(point: tree_sitter::Point) -> (u32, u32) {
    (point.row as u32, point.column as u32)
}

/// Strip namespace prefix from a fully-qualified name. `\App\Models\User` →
/// `User`. Leading `?` (nullable) is preserved as part of the name.
fn simple_name(fqn: &str) -> String {
    let trimmed = fqn.trim();
    let nullable = trimmed.starts_with('?');
    let body = trimmed.trim_start_matches('?').trim_start_matches('\\');
    let simple = body.rsplit('\\').next().unwrap_or(body);
    if nullable {
        format!("?{simple}")
    } else {
        simple.to_string()
    }
}

#[cfg(test)]
mod tests;
