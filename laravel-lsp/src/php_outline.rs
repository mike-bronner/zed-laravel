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
    /// Simplified return type (final `\`-segment), if any.
    pub return_type: Option<String>,
    pub parameters: Vec<PhpParameter>,
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
    pub property_type: Option<String>,
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
            // Recurse into wrappers that commonly contain top-level
            // declarations:
            //   - `namespace_definition` for `namespace Foo { class Bar {} }`
            //   - `compound_statement` for namespace / if-statement bodies
            //   - `if_statement` for the `function_exists` guard pattern
            //     common in Laravel helpers files
            //   - `else_clause` / `else_if_clause` for the same
            "namespace_definition"
            | "compound_statement"
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
    let extends = parse_extends(node, source);
    let (start_line, start_column) = pos(node.start_position());
    let (end_line, end_column) = pos(node.end_position());

    let mut methods = Vec::new();
    let mut properties = Vec::new();

    if let Some(body) = node.child_by_field_name("body") {
        let mut body_cursor = body.walk();
        for member in body.children(&mut body_cursor) {
            match member.kind() {
                "method_declaration" => {
                    if let Some(m) = parse_method(member, source) {
                        methods.push(m);
                    }
                }
                "property_declaration" => {
                    properties.extend(parse_properties(member, source));
                }
                _ => {}
            }
        }
    }

    Some(PhpStructure {
        kind,
        name,
        extends,
        start_line,
        start_column,
        end_line,
        end_column,
        methods,
        properties,
    })
}

fn parse_extends(node: Node, source: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "base_clause" {
            let mut bc_cursor = child.walk();
            for bc_child in child.children(&mut bc_cursor) {
                let kind = bc_child.kind();
                if kind == "name" || kind == "qualified_name" {
                    if let Ok(text) = bc_child.utf8_text(source) {
                        return Some(simple_name(text));
                    }
                }
            }
        }
    }
    None
}

fn parse_method(node: Node, source: &[u8]) -> Option<PhpMethodInfo> {
    let name = field_text(node, "name", source)?;
    let visibility = parse_visibility(node, source);
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

    Some(PhpMethodInfo {
        name,
        visibility,
        return_type,
        parameters,
        start_line,
        start_column,
        end_line,
        end_column,
    })
}

fn parse_properties(node: Node, source: &[u8]) -> Vec<PhpPropertyInfo> {
    let visibility = parse_visibility(node, source);
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
                        property_type: property_type.clone(),
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
