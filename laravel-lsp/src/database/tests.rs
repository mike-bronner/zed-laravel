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
    assert_eq!(
        DatabaseSchemaProvider::map_sql_type_to_php("INTEGER"),
        "int"
    );
    assert_eq!(DatabaseSchemaProvider::map_sql_type_to_php("bigint"), "int");
    assert_eq!(
        DatabaseSchemaProvider::map_sql_type_to_php("smallint"),
        "int"
    );
    assert_eq!(
        DatabaseSchemaProvider::map_sql_type_to_php("tinyint"),
        "int"
    );
    assert_eq!(DatabaseSchemaProvider::map_sql_type_to_php("serial"), "int");

    // Float types
    assert_eq!(
        DatabaseSchemaProvider::map_sql_type_to_php("float"),
        "float"
    );
    assert_eq!(
        DatabaseSchemaProvider::map_sql_type_to_php("double"),
        "float"
    );
    assert_eq!(
        DatabaseSchemaProvider::map_sql_type_to_php("decimal(10,2)"),
        "float"
    );
    assert_eq!(
        DatabaseSchemaProvider::map_sql_type_to_php("numeric"),
        "float"
    );
    assert_eq!(DatabaseSchemaProvider::map_sql_type_to_php("real"), "float");

    // Boolean (PostgreSQL only)
    assert_eq!(
        DatabaseSchemaProvider::map_sql_type_to_php("boolean"),
        "bool"
    );
    assert_eq!(DatabaseSchemaProvider::map_sql_type_to_php("bool"), "bool");

    // String types (dates and json are strings without casts!)
    assert_eq!(
        DatabaseSchemaProvider::map_sql_type_to_php("varchar(255)"),
        "string"
    );
    assert_eq!(
        DatabaseSchemaProvider::map_sql_type_to_php("text"),
        "string"
    );
    assert_eq!(
        DatabaseSchemaProvider::map_sql_type_to_php("char(10)"),
        "string"
    );
    assert_eq!(
        DatabaseSchemaProvider::map_sql_type_to_php("datetime"),
        "string"
    );
    assert_eq!(
        DatabaseSchemaProvider::map_sql_type_to_php("timestamp"),
        "string"
    );
    assert_eq!(
        DatabaseSchemaProvider::map_sql_type_to_php("date"),
        "string"
    );
    assert_eq!(
        DatabaseSchemaProvider::map_sql_type_to_php("time"),
        "string"
    );
    assert_eq!(
        DatabaseSchemaProvider::map_sql_type_to_php("json"),
        "string"
    );
    assert_eq!(
        DatabaseSchemaProvider::map_sql_type_to_php("jsonb"),
        "string"
    );
    assert_eq!(
        DatabaseSchemaProvider::map_sql_type_to_php("blob"),
        "string"
    );
    assert_eq!(
        DatabaseSchemaProvider::map_sql_type_to_php("enum('a','b')"),
        "string"
    );
}

// ---- host_candidates (Sail / Docker Compose fallback) ----

#[test]
fn host_candidates_docker_service_name_adds_localhost_fallback() {
    // The canonical Sail case — DB_HOST=mysql (the container name).
    assert_eq!(
        DatabaseSchemaProvider::host_candidates("mysql"),
        vec!["mysql".to_string(), "127.0.0.1".to_string()]
    );
}

#[test]
fn host_candidates_postgres_service_name_too() {
    assert_eq!(
        DatabaseSchemaProvider::host_candidates("pgsql"),
        vec!["pgsql".to_string(), "127.0.0.1".to_string()]
    );
}

#[test]
fn host_candidates_localhost_no_fallback() {
    // Already localhost — no point retrying with itself.
    assert_eq!(
        DatabaseSchemaProvider::host_candidates("localhost"),
        vec!["localhost".to_string()]
    );
    assert_eq!(
        DatabaseSchemaProvider::host_candidates("Localhost"),
        vec!["Localhost".to_string()]
    );
}

#[test]
fn host_candidates_ip_no_fallback() {
    // Already an IP — no service-name heuristic.
    assert_eq!(
        DatabaseSchemaProvider::host_candidates("127.0.0.1"),
        vec!["127.0.0.1".to_string()]
    );
    assert_eq!(
        DatabaseSchemaProvider::host_candidates("10.0.5.4"),
        vec!["10.0.5.4".to_string()]
    );
}

#[test]
fn host_candidates_fqdn_no_fallback() {
    // Dotted hostname is a real DNS name; don't second-guess it.
    assert_eq!(
        DatabaseSchemaProvider::host_candidates("db.internal.example.com"),
        vec!["db.internal.example.com".to_string()]
    );
}

#[test]
fn host_candidates_empty_no_fallback() {
    // Defensive — don't add `127.0.0.1` when the input is junk.
    assert_eq!(
        DatabaseSchemaProvider::host_candidates(""),
        vec!["".to_string()]
    );
}
