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

// ---- mask_url_password ----

#[test]
fn mask_url_password_with_credentials() {
    use super::mask_url_password;
    assert_eq!(
        mask_url_password("mysql://sail:secret@127.0.0.1:3306/db"),
        "mysql://sail:***@127.0.0.1:3306/db"
    );
    assert_eq!(
        mask_url_password("postgres://user:p@ssw0rd@host/db"),
        // Only the first `@` after creds is treated as the host separator —
        // best-effort. Any `@` in the password trips this, but it's a
        // diagnostic helper, not security-critical.
        "postgres://user:***@ssw0rd@host/db"
    );
}

#[test]
fn mask_url_password_no_password_no_change() {
    use super::mask_url_password;
    // No `:` in creds → no password to mask.
    assert_eq!(
        mask_url_password("mysql://sail@127.0.0.1/db"),
        "mysql://sail@127.0.0.1/db"
    );
}

#[test]
fn mask_url_password_no_scheme_returns_input() {
    use super::mask_url_password;
    assert_eq!(mask_url_password("not a url"), "not a url");
}

// ---- build_*_candidates (DB_URL / unix_socket / TCP priority) ----

fn make_config_with(url: Option<&str>, socket: Option<&str>, host: &str) -> super::DatabaseConfig {
    super::DatabaseConfig {
        driver: "mysql".to_string(),
        host: host.to_string(),
        port: 3306,
        database: "testdb".to_string(),
        username: "u".to_string(),
        password: "p".to_string(),
        url: url.map(|s| s.to_string()),
        unix_socket: socket.map(|s| s.to_string()),
        charset: None,
        collation: None,
    }
}

#[test]
fn mysql_candidates_db_url_takes_precedence() {
    let provider = DatabaseSchemaProvider::new(std::path::PathBuf::from("/tmp"));
    let cfg = make_config_with(Some("mysql://heroku:abc@db.heroku.com/x"), None, "mysql");
    let candidates = provider.build_mysql_candidates(&cfg);

    // DB_URL must come first.
    assert_eq!(candidates[0].label, "DB_URL");
    assert_eq!(candidates[0].url, "mysql://heroku:abc@db.heroku.com/x");

    // TCP fallbacks should still be there in case DB_URL fails.
    assert!(candidates.iter().any(|c| c.label.starts_with("tcp ")));
}

#[test]
fn mysql_candidates_unix_socket_inserted_before_tcp() {
    let provider = DatabaseSchemaProvider::new(std::path::PathBuf::from("/tmp"));
    let cfg = make_config_with(None, Some("/tmp/mysql.sock"), "localhost");
    let candidates = provider.build_mysql_candidates(&cfg);

    // Socket comes before TCP.
    assert!(candidates[0].label.contains("unix_socket"));
    assert_eq!(candidates[0].label, "unix_socket=/tmp/mysql.sock");
    assert!(candidates[0].url.contains("socket=/tmp/mysql.sock"));
    assert!(candidates[1].label.starts_with("tcp "));
}

#[test]
fn mysql_candidates_sail_host_adds_loopback_fallback() {
    let provider = DatabaseSchemaProvider::new(std::path::PathBuf::from("/tmp"));
    let cfg = make_config_with(None, None, "mysql");
    let candidates = provider.build_mysql_candidates(&cfg);

    // Two TCP candidates: configured host + 127.0.0.1 fallback.
    let tcp: Vec<&str> = candidates
        .iter()
        .filter(|c| c.label.starts_with("tcp "))
        .map(|c| c.label.as_str())
        .collect();
    assert_eq!(tcp, vec!["tcp mysql:3306", "tcp 127.0.0.1:3306"]);
    // The fallback candidate carries the Sail explanation note.
    let fallback = candidates
        .iter()
        .find(|c| c.label == "tcp 127.0.0.1:3306")
        .unwrap();
    assert!(
        fallback
            .success_note
            .as_deref()
            .unwrap_or("")
            .contains("Sail"),
        "expected Sail success_note on the loopback fallback"
    );
}

#[test]
fn mysql_candidates_localhost_host_no_extra_fallback() {
    let provider = DatabaseSchemaProvider::new(std::path::PathBuf::from("/tmp"));
    let cfg = make_config_with(None, None, "localhost");
    let candidates = provider.build_mysql_candidates(&cfg);

    let tcp_count = candidates
        .iter()
        .filter(|c| c.label.starts_with("tcp "))
        .count();
    assert_eq!(
        tcp_count, 1,
        "localhost host shouldn't add a 127.0.0.1 fallback"
    );
}

#[test]
fn postgres_candidates_socket_uses_libpq_style_url() {
    let provider = DatabaseSchemaProvider::new(std::path::PathBuf::from("/tmp"));
    let mut cfg = make_config_with(None, Some("/tmp/.s.PGSQL.5432"), "localhost");
    cfg.driver = "pgsql".to_string();
    cfg.port = 5432;
    let candidates = provider.build_postgres_candidates(&cfg);

    let socket = candidates
        .iter()
        .find(|c| c.label.starts_with("unix_socket"))
        .expect("expected socket candidate");
    // Postgres socket convention puts the host in a `host=` query param,
    // not a `socket=` one (that's libpq syntax). Pin that here so we
    // don't regress.
    assert!(
        socket.url.contains("?host=/tmp/.s.PGSQL.5432"),
        "got URL: {}",
        socket.url
    );
}

// ---- classify_mysql_error: actionable per-error-code toasts (Phase 5.8b) ---

#[test]
fn classify_mysql_unknown_database_recommends_artisan_migrate() {
    use super::classify_mysql_error;
    let raw = "tcp 127.0.0.1:3306: error returned from database: 1049 (42000): Unknown database 'tru_data'";
    let msg = classify_mysql_error(raw, "tru_data", "tcp 127.0.0.1:3306");
    assert!(
        msg.contains("php artisan migrate"),
        "remediation should be in Laravel terms (artisan migrate), not SQL; got: {msg}"
    );
    assert!(
        msg.contains("sail artisan migrate"),
        "should mention the Sail variant of the artisan command; got: {msg}"
    );
    assert!(
        msg.contains("accepted the connection"),
        "should tell user that auth worked; got: {msg}"
    );
    assert!(
        !msg.contains("CREATE DATABASE"),
        "should NOT include raw SQL commands; got: {msg}"
    );
    assert!(
        !msg.contains("Check DB_URL / DB_HOST"),
        "should NOT show the generic 'check everything' message; got: {msg}"
    );
}

#[test]
fn classify_mysql_missing_table_recommends_artisan_migrate() {
    use super::classify_mysql_error;
    let raw =
        "tcp 127.0.0.1:3306: error returned from database: 1146 (42S02): Table 'tru_data.users' doesn't exist";
    let msg = classify_mysql_error(raw, "tru_data", "tcp 127.0.0.1:3306");
    assert!(
        msg.contains("php artisan migrate"),
        "missing-table case should also point at artisan migrate; got: {msg}"
    );
    assert!(
        msg.contains("table is missing"),
        "should call out that the table specifically is missing; got: {msg}"
    );
}

#[test]
fn classify_mysql_access_denied_calls_out_credentials() {
    use super::classify_mysql_error;
    let raw = "tcp 127.0.0.1:3306: error returned from database: 1045 (28000): Access denied for user 'root'@'localhost' (using password: YES)";
    let msg = classify_mysql_error(raw, "tru_data", "tcp 127.0.0.1:3306");
    assert!(
        msg.contains("DB_USERNAME"),
        "should call out DB_USERNAME/PASSWORD; got: {msg}"
    );
    assert!(
        msg.contains("rejected the credentials"),
        "should say MySQL is reachable but rejected creds; got: {msg}"
    );
}

#[test]
fn classify_mysql_connection_refused_blames_host() {
    use super::classify_mysql_error;
    let raw = "tcp 127.0.0.1:3306: 2003 Can't connect to MySQL server (Connection refused)";
    let msg = classify_mysql_error(raw, "tru_data", "tcp 127.0.0.1:3306");
    assert!(
        msg.contains("Couldn't reach the MySQL server"),
        "got: {msg}"
    );
    assert!(msg.contains("DB_HOST / DB_PORT"), "got: {msg}");
}

#[test]
fn classify_mysql_unknown_error_falls_through_to_generic() {
    use super::classify_mysql_error;
    let raw = "tcp 127.0.0.1:3306: some weird sqlx-side error we've never seen";
    let msg = classify_mysql_error(raw, "tru_data", "tcp 127.0.0.1:3306");
    assert!(msg.contains("MySQL connection failed"), "got: {msg}");
    assert!(
        msg.contains("Check DB_URL / DB_HOST"),
        "generic message should keep the full .env checklist; got: {msg}"
    );
}

#[test]
fn classify_postgres_unknown_database_recommends_artisan_migrate() {
    use super::classify_postgres_error;
    let raw = "tcp 127.0.0.1:5432: error returned from database: code: \"3D000\" message: \"database \\\"foo\\\" does not exist\"";
    let msg = classify_postgres_error(raw, "foo", "tcp 127.0.0.1:5432");
    assert!(
        msg.contains("php artisan migrate"),
        "Postgres unknown-database should also use Laravel framing; got: {msg}"
    );
    assert!(!msg.contains("CREATE DATABASE"), "no raw SQL; got: {msg}");
}

#[test]
fn classify_postgres_missing_table_recommends_artisan_migrate() {
    use super::classify_postgres_error;
    let raw = "tcp 127.0.0.1:5432: error returned from database: code: \"42P01\" message: \"relation \\\"users\\\" does not exist\"";
    let msg = classify_postgres_error(raw, "foo", "tcp 127.0.0.1:5432");
    assert!(
        msg.contains("php artisan migrate"),
        "Postgres missing-table should point at migrations; got: {msg}"
    );
}

// ---- userinfo / empty-password URL shape (Phase 5.4) --------------------

#[test]
fn userinfo_with_password_uses_colon() {
    use super::userinfo;
    assert_eq!(userinfo("sail", "password"), "sail:password");
}

#[test]
fn userinfo_with_empty_password_omits_colon() {
    use super::userinfo;
    // `user:` would tell sqlx "empty password supplied" and MySQL responds
    // with `using password: YES`. `user` (no colon) tells sqlx "no
    // password" and the handshake omits the password packet — accepted by
    // permissive setups like passwordless `root@localhost`.
    assert_eq!(userinfo("root", ""), "root");
}

#[test]
fn mysql_candidates_empty_password_url_has_no_colon() {
    // The full smoke test: with DB_PASSWORD empty, the resulting connection
    // URL should be `mysql://user@host/...` (no `:` before `@`), not
    // `mysql://user:@host/...`. This makes sqlx skip sending the password
    // packet, which lets passwordless MySQL setups accept the connection.
    let provider = DatabaseSchemaProvider::new(std::path::PathBuf::from("/tmp"));
    let mut cfg = make_config_with(None, None, "127.0.0.1");
    cfg.username = "root".to_string();
    cfg.password = "".to_string();
    let candidates = provider.build_mysql_candidates(&cfg);
    let tcp = candidates
        .iter()
        .find(|c| c.label.starts_with("tcp "))
        .expect("tcp candidate");
    assert!(
        tcp.url.starts_with("mysql://root@"),
        "empty password should produce `user@host`, not `user:@host`; got: {}",
        tcp.url
    );
    assert!(
        !tcp.url.contains(":@"),
        "URL must not contain `:@` (empty-password specifier); got: {}",
        tcp.url
    );
}

#[test]
fn mysql_candidates_non_empty_password_keeps_colon() {
    let provider = DatabaseSchemaProvider::new(std::path::PathBuf::from("/tmp"));
    let mut cfg = make_config_with(None, None, "127.0.0.1");
    cfg.username = "sail".to_string();
    cfg.password = "secret".to_string();
    let candidates = provider.build_mysql_candidates(&cfg);
    let tcp = candidates
        .iter()
        .find(|c| c.label.starts_with("tcp "))
        .expect("tcp candidate");
    assert!(
        tcp.url.starts_with("mysql://sail:secret@"),
        "non-empty password should use the user:pass@ shape; got: {}",
        tcp.url
    );
}

// ---- resolve_env: empty value should NOT swallow next line (Phase 5.5) ----

#[test]
fn resolve_env_empty_value_returns_none_not_next_line() {
    use tempfile::TempDir;
    let dir = TempDir::new().unwrap();
    // The exact shape that broke in Mike's tru-data project: an empty
    // DB_PASSWORD followed by other entries. The old regex `\s*=\s*` let
    // the `\s*` after `=` consume the newline and matched the next line
    // as the value.
    std::fs::write(
        dir.path().join(".env"),
        "DB_PASSWORD=\nSESSION_DRIVER=database\nDB_CONNECTION=mysql\n",
    )
    .unwrap();
    let provider = DatabaseSchemaProvider::new(dir.path().to_path_buf());
    let result = provider.resolve_env("DB_PASSWORD");
    assert_eq!(
        result, None,
        "empty value should produce None (filtered by .filter(!is_empty)), \
         not the next line's content"
    );
}

#[test]
fn resolve_env_normal_value_works() {
    use tempfile::TempDir;
    let dir = TempDir::new().unwrap();
    std::fs::write(
        dir.path().join(".env"),
        "DB_PASSWORD=secret\nDB_USERNAME=sail\n",
    )
    .unwrap();
    let provider = DatabaseSchemaProvider::new(dir.path().to_path_buf());
    assert_eq!(
        provider.resolve_env("DB_PASSWORD"),
        Some("secret".to_string())
    );
    assert_eq!(
        provider.resolve_env("DB_USERNAME"),
        Some("sail".to_string())
    );
}

#[test]
fn resolve_env_quoted_value_strips_quotes() {
    use tempfile::TempDir;
    let dir = TempDir::new().unwrap();
    std::fs::write(
        dir.path().join(".env"),
        "DB_PASSWORD=\"s3cr3t!\"\nOTHER='single quoted'\n",
    )
    .unwrap();
    let provider = DatabaseSchemaProvider::new(dir.path().to_path_buf());
    assert_eq!(
        provider.resolve_env("DB_PASSWORD"),
        Some("s3cr3t!".to_string())
    );
    assert_eq!(
        provider.resolve_env("OTHER"),
        Some("single quoted".to_string())
    );
}

#[test]
fn resolve_env_handles_trailing_whitespace_on_key() {
    use tempfile::TempDir;
    let dir = TempDir::new().unwrap();
    // Some editors / templates pad with spaces around `=`. Still single-line.
    std::fs::write(
        dir.path().join(".env"),
        "DB_PASSWORD = padded\nDB_HOST=127.0.0.1\n",
    )
    .unwrap();
    let provider = DatabaseSchemaProvider::new(dir.path().to_path_buf());
    assert_eq!(
        provider.resolve_env("DB_PASSWORD"),
        Some("padded".to_string())
    );
}

#[test]
fn postgres_candidates_empty_password_url_has_no_colon() {
    let provider = DatabaseSchemaProvider::new(std::path::PathBuf::from("/tmp"));
    let mut cfg = make_config_with(None, None, "127.0.0.1");
    cfg.driver = "pgsql".to_string();
    cfg.port = 5432;
    cfg.username = "postgres".to_string();
    cfg.password = "".to_string();
    let candidates = provider.build_postgres_candidates(&cfg);
    let tcp = candidates
        .iter()
        .find(|c| c.label.starts_with("tcp "))
        .expect("tcp candidate");
    assert!(
        tcp.url.starts_with("postgres://postgres@"),
        "got: {}",
        tcp.url
    );
    assert!(!tcp.url.contains(":@"), "got: {}", tcp.url);
}
