use super::*;

#[test]
fn test_default_ports() {
    let provider = DatabaseSchemaProvider::new(PathBuf::from("/tmp"));
    assert_eq!(provider.default_port("mysql"), 3306);
    assert_eq!(provider.default_port("mariadb"), 3306);
    assert_eq!(provider.default_port("pgsql"), 5432);
    assert_eq!(provider.default_port("postgres"), 5432);
    assert_eq!(provider.default_port("sqlsrv"), 1433);
}

#[test]
fn test_schema_cache_validity() {
    let schema = DatabaseSchema {
        tables: vec!["users".to_string()],
        columns: HashMap::new(),
        columns_with_types: HashMap::new(),
        cached_at: Instant::now(),
    };
    assert!(schema.is_valid());
}

#[test]
fn test_map_sql_type_to_php() {
    // Integer types
    assert_eq!(DatabaseSchemaProvider::map_sql_type_to_php("int"), "int");
    assert_eq!(DatabaseSchemaProvider::map_sql_type_to_php("INTEGER"), "int");
    assert_eq!(DatabaseSchemaProvider::map_sql_type_to_php("bigint"), "int");
    assert_eq!(DatabaseSchemaProvider::map_sql_type_to_php("smallint"), "int");
    assert_eq!(DatabaseSchemaProvider::map_sql_type_to_php("tinyint"), "int");
    assert_eq!(DatabaseSchemaProvider::map_sql_type_to_php("serial"), "int");

    // Float types
    assert_eq!(DatabaseSchemaProvider::map_sql_type_to_php("float"), "float");
    assert_eq!(DatabaseSchemaProvider::map_sql_type_to_php("double"), "float");
    assert_eq!(DatabaseSchemaProvider::map_sql_type_to_php("decimal(10,2)"), "float");
    assert_eq!(DatabaseSchemaProvider::map_sql_type_to_php("numeric"), "float");
    assert_eq!(DatabaseSchemaProvider::map_sql_type_to_php("real"), "float");

    // Boolean (PostgreSQL only)
    assert_eq!(DatabaseSchemaProvider::map_sql_type_to_php("boolean"), "bool");
    assert_eq!(DatabaseSchemaProvider::map_sql_type_to_php("bool"), "bool");

    // String types (dates and json are strings without casts!)
    assert_eq!(DatabaseSchemaProvider::map_sql_type_to_php("varchar(255)"), "string");
    assert_eq!(DatabaseSchemaProvider::map_sql_type_to_php("text"), "string");
    assert_eq!(DatabaseSchemaProvider::map_sql_type_to_php("char(10)"), "string");
    assert_eq!(DatabaseSchemaProvider::map_sql_type_to_php("datetime"), "string");
    assert_eq!(DatabaseSchemaProvider::map_sql_type_to_php("timestamp"), "string");
    assert_eq!(DatabaseSchemaProvider::map_sql_type_to_php("date"), "string");
    assert_eq!(DatabaseSchemaProvider::map_sql_type_to_php("time"), "string");
    assert_eq!(DatabaseSchemaProvider::map_sql_type_to_php("json"), "string");
    assert_eq!(DatabaseSchemaProvider::map_sql_type_to_php("jsonb"), "string");
    assert_eq!(DatabaseSchemaProvider::map_sql_type_to_php("blob"), "string");
    assert_eq!(DatabaseSchemaProvider::map_sql_type_to_php("enum('a','b')"), "string");
}
