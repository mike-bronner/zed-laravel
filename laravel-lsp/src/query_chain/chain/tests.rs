use super::*;

// ---- parse_table_ref: Laravel `join`/`from` table reference strings ----

#[test]
fn parse_plain_table() {
    let at = parse_table_ref("orders");
    assert_eq!(at.table, "orders");
    assert_eq!(at.alias, None);
    // With no alias the qualifier IS the table name.
    assert_eq!(at.qualifier(), "orders");
}

#[test]
fn parse_aliased_table_lowercase_as() {
    let at = parse_table_ref("users as u");
    assert_eq!(at.table, "users");
    assert_eq!(at.alias.as_deref(), Some("u"));
    assert_eq!(at.qualifier(), "u");
}

#[test]
fn parse_aliased_table_uppercase_as() {
    // The `as` keyword is matched case-insensitively.
    let at = parse_table_ref("users AS u");
    assert_eq!(at.table, "users");
    assert_eq!(at.alias.as_deref(), Some("u"));
}

#[test]
fn parse_implicit_alias() {
    // MySQL-style implicit alias (`table alias`, no `as`).
    let at = parse_table_ref("orders o");
    assert_eq!(at.table, "orders");
    assert_eq!(at.alias.as_deref(), Some("o"));
}

#[test]
fn parse_schema_qualified_table() {
    // Cross-database / schema-qualified name: the dotted table survives whole
    // so column lookup can pass it through; the qualifier is the full name.
    let at = parse_table_ref("mydb.orders");
    assert_eq!(at.table, "mydb.orders");
    assert_eq!(at.alias, None);
    assert_eq!(at.qualifier(), "mydb.orders");
}

#[test]
fn parse_schema_qualified_with_alias() {
    let at = parse_table_ref("mydb.orders as o");
    assert_eq!(at.table, "mydb.orders");
    assert_eq!(at.alias.as_deref(), Some("o"));
    assert_eq!(at.qualifier(), "o");
}

#[test]
fn parse_trims_and_collapses_whitespace() {
    let at = parse_table_ref("  users   as   u  ");
    assert_eq!(at.table, "users");
    assert_eq!(at.alias.as_deref(), Some("u"));
}

#[test]
fn bare_constructor_has_no_alias() {
    let at = AccessibleTable::bare("posts");
    assert_eq!(at.table, "posts");
    assert_eq!(at.alias, None);
    assert_eq!(at.qualifier(), "posts");
}
