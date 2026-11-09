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

#[test]
fn postgres_socket_candidate_url_actually_parses() {
    use sqlx::postgres::PgConnectOptions;
    use std::str::FromStr;

    // `postgres_candidates_socket_uses_libpq_style_url` above pins the `?host=`
    // shape by string match, which a URL the driver cannot parse satisfies just
    // as well — and the authority this branch emitted did exactly that. Hand the
    // candidate to the parser sqlx will actually use.
    let provider = DatabaseSchemaProvider::new(std::path::PathBuf::from("/tmp"));
    let mut cfg = make_config_with(None, Some("/tmp/.s.PGSQL.5432"), "localhost");
    cfg.driver = "pgsql".to_string();
    cfg.port = 5432;
    let socket = provider
        .build_postgres_candidates(&cfg)
        .into_iter()
        .find(|c| c.label.starts_with("unix_socket"))
        .expect("socket candidate");
    let opts = PgConnectOptions::from_str(&socket.url)
        .unwrap_or_else(|e| panic!("`{}` should parse: {e}", socket.url));
    assert_eq!(
        opts.get_socket().map(|p| p.to_string_lossy().into_owned()),
        Some("/tmp/.s.PGSQL.5432".to_string()),
        "the `host=` parameter must still reach sqlx as the socket path"
    );
    assert_eq!(opts.get_username(), "u");
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

// ---- userinfo percent-encoding (issue #362) ------------------------------
//
// Every assertion below is settled by the real driver's own connection-string
// parser rather than by string comparison against a hand-written expectation:
// the defect these tests pin is that `Url::parse` reads a raw delimiter as
// structure, so only a parse can prove the credential survives.

/// Parse `url` with the driver sqlx will actually use, and assert it yields
/// exactly `user` and `password`.
///
/// `MySqlConnectOptions` exposes `get_username()` but no password accessor, so
/// the password is pinned differentially: `to_url_lossy()` of the parsed
/// options against `to_url_lossy()` of the *same* options with the expected
/// password set. Every other field is identical by construction (it is a clone
/// of the parsed value), so the two URLs are equal exactly when the parsed
/// password is the expected one.
fn assert_mysql_credentials(url: &str, user: &str, password: &str) {
    use sqlx::mysql::MySqlConnectOptions;
    use sqlx::ConnectOptions;
    use std::str::FromStr;

    let parsed = MySqlConnectOptions::from_str(url)
        .unwrap_or_else(|e| panic!("`{url}` should parse as a MySQL connection string: {e}"));
    assert_eq!(parsed.get_username(), user, "username parsed from `{url}`");
    assert_eq!(
        parsed.to_url_lossy(),
        parsed.clone().password(password).to_url_lossy(),
        "password parsed from `{url}` is not `{password}`"
    );
}

/// Postgres twin of [`assert_mysql_credentials`], same differential technique.
fn assert_postgres_credentials(url: &str, user: &str, password: &str) {
    use sqlx::postgres::PgConnectOptions;
    use sqlx::ConnectOptions;
    use std::str::FromStr;

    let parsed = PgConnectOptions::from_str(url)
        .unwrap_or_else(|e| panic!("`{url}` should parse as a Postgres connection string: {e}"));
    assert_eq!(parsed.get_username(), user, "username parsed from `{url}`");
    assert_eq!(
        parsed.to_url_lossy(),
        parsed.clone().password(password).to_url_lossy(),
        "password parsed from `{url}` is not `{password}`"
    );
}

/// A config carrying the given credentials, on the loopback host. Defaults to
/// MySQL; the Postgres cases override `driver` and `port`.
fn creds_config(username: &str, password: &str) -> super::DatabaseConfig {
    let mut cfg = make_config_with(None, None, "127.0.0.1");
    cfg.username = username.to_string();
    cfg.password = password.to_string();
    cfg
}

fn mysql_tcp_url(username: &str, password: &str) -> String {
    let provider = DatabaseSchemaProvider::new(std::path::PathBuf::from("/tmp"));
    provider
        .build_mysql_candidates(&creds_config(username, password))
        .into_iter()
        .find(|c| c.label.starts_with("tcp "))
        .expect("tcp candidate")
        .url
}

#[test]
fn userinfo_encodes_reserved_delimiters_in_password() {
    use super::userinfo;
    // The four delimiters issue #362 names: each one ends the authority (or
    // begins the credential) when `Url::parse` meets it unencoded.
    assert_eq!(userinfo("u", "p/ss"), "u:p%2Fss");
    assert_eq!(userinfo("u", "p?ss"), "u:p%3Fss");
    assert_eq!(userinfo("u", "p#ss"), "u:p%23ss");
    assert_eq!(userinfo("u", "p@ss"), "u:p%40ss");
}

#[test]
fn userinfo_encodes_username_symmetrically() {
    use super::userinfo;
    // A username is spliced into the same slot and needs the same treatment.
    // `:` is encoded there too: `userinfo` uses it as its own separator, so a
    // raw one would mis-split the credential.
    assert_eq!(userinfo("foo@bar", "pw"), "foo%40bar:pw");
    assert_eq!(userinfo("a:b", "pw"), "a%3Ab:pw");
    assert_eq!(userinfo("a/b", "pw"), "a%2Fb:pw");
}

#[test]
fn userinfo_leaves_legal_characters_byte_for_byte() {
    use super::userinfo;
    // RFC 3986 `unreserved` + `sub-delims` pass through untouched, so the URLs
    // this server has always built for ordinary credentials do not move.
    let legal = "aZ09-._~!$&'()*+,;=";
    assert_eq!(userinfo(legal, legal), format!("{legal}:{legal}"));
    assert_eq!(userinfo("sail", "password"), "sail:password");
    // …and the assembled URL is byte-for-byte what this server built before.
    assert_eq!(
        mysql_tcp_url("sail", "password"),
        "mysql://sail:password@127.0.0.1:3306/testdb"
    );
    assert_eq!(
        mysql_tcp_url(legal, legal),
        format!("mysql://{legal}:{legal}@127.0.0.1:3306/testdb")
    );
}

#[test]
fn userinfo_encodes_space_control_and_non_ascii_bytes() {
    use super::userinfo;
    assert_eq!(userinfo("u", "a b"), "u:a%20b");
    assert_eq!(userinfo("u", "a\tb"), "u:a%09b");
    assert_eq!(userinfo("u", "a\nb"), "u:a%0Ab");
    assert_eq!(userinfo("u", "[::1]"), "u:%5B%3A%3A1%5D");
    // Each byte of a multi-byte UTF-8 sequence is encoded on its own; sqlx
    // percent-decodes back to the original string.
    assert_eq!(userinfo("u", "pä"), "u:p%C3%A4");
}

#[test]
fn userinfo_encodes_percent_so_hex_pairs_do_not_decode() {
    use super::userinfo;
    // A literal `%` must become `%25`, otherwise `sec%3Dret` would round-trip
    // through sqlx's percent-decode as `sec=ret` — a different password.
    assert_eq!(userinfo("u", "sec%3Dret"), "u:sec%253Dret");
    assert_eq!(userinfo("u", "100%"), "u:100%25");
    assert_mysql_credentials(&mysql_tcp_url("u", "sec%3Dret"), "u", "sec%3Dret");
}

#[test]
fn userinfo_empty_password_still_omits_the_colon() {
    use super::userinfo;
    // Encoding must not disturb the "no password" shape: `root:` tells sqlx an
    // empty password *was* supplied and MySQL answers `using password: YES`.
    assert_eq!(userinfo("root", ""), "root");
    assert_eq!(userinfo("ro@ot", ""), "ro%40ot");
}

#[test]
fn mysql_url_round_trips_reserved_password_characters() {
    for password in ["p/ss", "p?ss", "p#ss", "p@ss", "pa:ss", "p ss", "p%ss"] {
        assert_mysql_credentials(&mysql_tcp_url("sail", password), "sail", password);
    }
}

#[test]
fn mysql_url_round_trips_digit_run_before_a_delimiter() {
    // The silent class from issue #362. The leading run before the delimiter is
    // all digits and fits a `u16`, so the authority parses instead of erroring:
    // every shape below resolved to host `sail`, the digits as the port, and
    // the default `root` user, with no password at all. Nothing surfaces — the
    // connection just goes somewhere else, and a redactor that locates the
    // credential by parsing the URL is told there is nothing to mask.
    for password in [
        "12/34",
        "1234/56",
        "012345/aG9zdG5hbWU=",
        "12?34",
        "1234?56",
        "12#34",
        "1234#56",
    ] {
        assert_mysql_credentials(&mysql_tcp_url("sail", password), "sail", password);
    }
}

#[test]
fn mysql_url_round_trips_reserved_username_characters() {
    for username in ["foo@bar", "a:b", "a/b", "a?b", "a#b"] {
        assert_mysql_credentials(&mysql_tcp_url(username, "pw"), username, "pw");
    }
}

#[test]
fn mysql_socket_candidate_round_trips_credentials_and_socket() {
    let provider = DatabaseSchemaProvider::new(std::path::PathBuf::from("/tmp"));
    let mut cfg = make_config_with(None, Some("/tmp/mysql.sock"), "localhost");
    cfg.username = "sa/il".to_string();
    cfg.password = "012345/aG9zdG5hbWU=".to_string();
    let socket = provider
        .build_mysql_candidates(&cfg)
        .into_iter()
        .find(|c| c.label.starts_with("unix_socket"))
        .expect("socket candidate");
    assert_mysql_credentials(&socket.url, "sa/il", "012345/aG9zdG5hbWU=");
    // The encoded credential must not disturb the trailing `socket=` param.
    let opts = <sqlx::mysql::MySqlConnectOptions as std::str::FromStr>::from_str(&socket.url)
        .expect("socket URL should parse");
    assert_eq!(
        opts.get_socket().map(|p| p.to_string_lossy().into_owned()),
        Some("/tmp/mysql.sock".to_string())
    );
}

#[test]
fn postgres_tcp_candidate_round_trips_credentials() {
    let provider = DatabaseSchemaProvider::new(std::path::PathBuf::from("/tmp"));
    let mut cfg = creds_config("fo@o", "012345/aG9zdG5hbWU=");
    cfg.driver = "pgsql".to_string();
    cfg.port = 5432;
    let tcp = provider
        .build_postgres_candidates(&cfg)
        .into_iter()
        .find(|c| c.label.starts_with("tcp "))
        .expect("tcp candidate");
    assert_postgres_credentials(&tcp.url, "fo@o", "012345/aG9zdG5hbWU=");
}

#[test]
fn postgres_socket_candidate_round_trips_credentials_and_socket() {
    let provider = DatabaseSchemaProvider::new(std::path::PathBuf::from("/tmp"));
    let mut cfg = make_config_with(None, Some("/tmp/.s.PGSQL.5432"), "localhost");
    cfg.driver = "pgsql".to_string();
    cfg.port = 5432;
    cfg.username = "fo@o".to_string();
    cfg.password = "12/34".to_string();
    let socket = provider
        .build_postgres_candidates(&cfg)
        .into_iter()
        .find(|c| c.label.starts_with("unix_socket"))
        .expect("socket candidate");
    assert_postgres_credentials(&socket.url, "fo@o", "12/34");
    // The encoded credential must not disturb the trailing `host=` param.
    let opts = <sqlx::postgres::PgConnectOptions as std::str::FromStr>::from_str(&socket.url)
        .expect("socket URL should parse");
    assert_eq!(
        opts.get_socket().map(|p| p.to_string_lossy().into_owned()),
        Some("/tmp/.s.PGSQL.5432".to_string())
    );
}

#[test]
fn db_url_passthrough_is_never_re_encoded() {
    // `config.url` is a full connection string the user supplied verbatim. It
    // never goes through `userinfo`, and encoding it would corrupt credentials
    // the user already encoded themselves.
    let provider = DatabaseSchemaProvider::new(std::path::PathBuf::from("/tmp"));
    let raw = "mysql://heroku:ab%2Fc@db.heroku.com/x";
    let mut cfg = make_config_with(Some(raw), None, "mysql");
    cfg.username = "ignored@me".to_string();
    cfg.password = "ignored/me".to_string();
    let mysql = provider.build_mysql_candidates(&cfg);
    assert_eq!(mysql[0].label, "DB_URL");
    assert_eq!(mysql[0].url, raw);

    let pg_raw = "postgres://heroku:ab%2Fc@db.heroku.com/x";
    let mut pg_cfg = make_config_with(Some(pg_raw), None, "postgres");
    pg_cfg.driver = "pgsql".to_string();
    pg_cfg.username = "ignored@me".to_string();
    pg_cfg.password = "ignored/me".to_string();
    let pg = provider.build_postgres_candidates(&pg_cfg);
    assert_eq!(pg[0].label, "DB_URL");
    assert_eq!(pg[0].url, pg_raw);
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

// ---------------------------------------------------------------------------
// Circuit breaker state machine (pure, time-injected — no DB, no async)
// ---------------------------------------------------------------------------

const COOLDOWN: Duration = Duration::from_secs(30);

fn breaker() -> CircuitBreaker {
    CircuitBreaker::new(COOLDOWN)
}

#[test]
fn breaker_starts_closed_and_allows_attempts() {
    let mut b = breaker();
    let now = Instant::now();
    assert_eq!(b.allow_attempt(now), Attempt::Closed);
    // Closed → no cooldown pending → loop refreshes at the healthy cadence.
    assert_eq!(b.cooldown_remaining(now), None);
}

#[test]
fn breaker_first_failure_opens_and_signals_new_outage() {
    let mut b = breaker();
    let t0 = Instant::now();
    assert!(
        b.record_failure(t0),
        "Closed→Open is the start of an outage episode"
    );
    assert_eq!(b.allow_attempt(t0 + Duration::from_secs(1)), Attempt::Open);
    // Open → a cooldown is pending (~29s left after 1s).
    assert_eq!(
        b.cooldown_remaining(t0 + Duration::from_secs(1)),
        Some(COOLDOWN - Duration::from_secs(1))
    );
}

#[test]
fn breaker_denies_until_cooldown_elapses() {
    let mut b = breaker();
    let t0 = Instant::now();
    b.record_failure(t0);
    assert_eq!(
        b.allow_attempt(t0 + COOLDOWN - Duration::from_millis(1)),
        Attempt::Open
    );
    assert_eq!(
        b.cooldown_remaining(t0 + COOLDOWN - Duration::from_millis(1)),
        Some(Duration::from_millis(1))
    );
}

#[test]
fn breaker_allows_exactly_one_probe_after_cooldown() {
    let mut b = breaker();
    let t0 = Instant::now();
    b.record_failure(t0);
    assert_eq!(b.allow_attempt(t0 + COOLDOWN), Attempt::HalfOpen);
    // A second caller while the probe is outstanding is denied.
    assert_eq!(
        b.allow_attempt(t0 + COOLDOWN + Duration::from_millis(1)),
        Attempt::Open
    );
}

#[test]
fn breaker_probe_success_closes() {
    let mut b = breaker();
    let t0 = Instant::now();
    b.record_failure(t0);
    assert_eq!(b.allow_attempt(t0 + COOLDOWN), Attempt::HalfOpen);
    b.record_success();
    let after = t0 + COOLDOWN + Duration::from_secs(1);
    assert_eq!(b.allow_attempt(after), Attempt::Closed);
    assert_eq!(b.cooldown_remaining(after), None);
}

#[test]
fn breaker_cooldown_remaining_saturates_to_zero_once_elapsed() {
    let mut b = breaker();
    let t0 = Instant::now();
    b.record_failure(t0);
    // Past the cooldown but before allow_attempt flips it to half-open:
    // the next probe is due now, so zero remaining (loop sleeps 0, retries).
    assert_eq!(
        b.cooldown_remaining(t0 + COOLDOWN + Duration::from_secs(5)),
        Some(Duration::ZERO)
    );
}

#[test]
fn breaker_probe_failure_reopens_without_new_outage_signal() {
    let mut b = breaker();
    let t0 = Instant::now();
    assert!(b.record_failure(t0), "first failure = new outage");
    assert_eq!(b.allow_attempt(t0 + COOLDOWN), Attempt::HalfOpen);
    assert!(
        !b.record_failure(t0 + COOLDOWN),
        "failed probe = same outage, no fresh notification"
    );
    // Re-opened: denied for another full cooldown, then a new probe.
    assert_eq!(
        b.allow_attempt(t0 + COOLDOWN + COOLDOWN - Duration::from_secs(1)),
        Attempt::Open
    );
    assert_eq!(b.allow_attempt(t0 + COOLDOWN + COOLDOWN), Attempt::HalfOpen);
}

#[test]
fn breaker_failure_after_recovery_signals_fresh_outage() {
    let mut b = breaker();
    let t0 = Instant::now();
    assert!(b.record_failure(t0));
    assert_eq!(b.allow_attempt(t0 + COOLDOWN), Attempt::HalfOpen);
    b.record_success(); // reconnected
    assert!(
        b.record_failure(t0 + COOLDOWN + Duration::from_secs(60)),
        "a failure after a successful reconnect is a NEW outage episode"
    );
}

#[test]
fn breaker_stuck_probe_self_heals_after_cooldown() {
    let mut b = breaker();
    let t0 = Instant::now();
    b.record_failure(t0);
    // Probe goes out at t0+30s and never reports back (e.g. a fetch path
    // that bails without recording failure).
    assert_eq!(b.allow_attempt(t0 + COOLDOWN), Attempt::HalfOpen);
    // Still guarded while the probe ages...
    assert_eq!(
        b.allow_attempt(t0 + COOLDOWN + COOLDOWN - Duration::from_secs(1)),
        Attempt::Open
    );
    // ...but after another cooldown the breaker re-arms a fresh probe
    // instead of staying wedged in HalfOpen forever.
    assert_eq!(b.allow_attempt(t0 + COOLDOWN + COOLDOWN), Attempt::HalfOpen);
}

#[test]
fn record_success_reports_recovery_edge_only() {
    let mut b = breaker();
    let t0 = Instant::now();

    // Fresh breaker (already Closed): the first successful fetch is a
    // healthy startup, NOT a recovery — no reconnect toast.
    assert!(
        !b.record_success(),
        "healthy startup must not signal a reconnect"
    );

    // Open → Closed is a genuine recovery edge.
    b.record_failure(t0);
    assert!(b.record_success(), "Open→Closed is a recovery edge");

    // HalfOpen → Closed is a genuine recovery edge (a probe succeeded).
    b.record_failure(t0);
    assert_eq!(b.allow_attempt(t0 + COOLDOWN), Attempt::HalfOpen);
    assert!(b.record_success(), "HalfOpen→Closed is a recovery edge");

    // Already Closed → Closed is a routine steady-state refresh — silent.
    assert!(
        !b.record_success(),
        "an already-healthy refresh must not signal a reconnect"
    );
}

// ---------------------------------------------------------------------------
// Outage classification (scenario 1: unreachable / scenario 2: rejected)
// ---------------------------------------------------------------------------

#[test]
fn outage_class_connection_refused_is_unreachable() {
    assert_eq!(
        outage_class_from_raw("tcp 127.0.0.1:3306: Connection refused (os error 61)"),
        OutageClass::Unreachable
    );
}

#[test]
fn outage_class_pool_timeout_is_unreachable() {
    assert_eq!(
        outage_class_from_raw("pool timed out while waiting for an open connection"),
        OutageClass::Unreachable
    );
}

#[test]
fn outage_class_dns_failure_is_unreachable() {
    assert_eq!(
        outage_class_from_raw(
            "failed to lookup address information: nodename nor servname provided, or not known"
        ),
        OutageClass::Unreachable
    );
}

#[test]
fn outage_class_mysql_access_denied_is_rejected() {
    assert_eq!(
        outage_class_from_raw(
            "error returned from database: 1045 (28000): Access denied for user 'root'@'localhost'"
        ),
        OutageClass::Rejected
    );
}

#[test]
fn outage_class_mysql_unknown_database_is_rejected() {
    assert_eq!(
        outage_class_from_raw("error returned from database: 1049 (42000): Unknown database 'app'"),
        OutageClass::Rejected
    );
}

#[test]
fn outage_class_postgres_bad_password_is_rejected() {
    assert_eq!(
        outage_class_from_raw("28P01: password authentication failed for user \"postgres\""),
        OutageClass::Rejected
    );
}

#[test]
fn outage_class_sqlserver_login_failed_is_rejected() {
    assert_eq!(
        outage_class_from_raw("Login failed for user 'sa'."),
        OutageClass::Rejected
    );
}

#[test]
fn outage_class_unrecognized_error_is_other() {
    assert_eq!(
        outage_class_from_raw("something exploded in a novel way"),
        OutageClass::Other
    );
}

// ---------------------------------------------------------------------------
// Notification text selection (scenario-specific; scenario 0 silent)
// ---------------------------------------------------------------------------

#[test]
fn toast_unreachable_asks_if_db_is_running_and_carries_detail() {
    let msg = outage_toast_message(
        OutageClass::Unreachable,
        "Couldn't reach [tcp 127.0.0.1:3306]",
    )
    .expect("scenario 1 must notify");
    assert!(msg.contains("is it running"), "got: {msg}");
    assert!(
        msg.contains("Couldn't reach [tcp 127.0.0.1:3306]"),
        "got: {msg}"
    );
}

#[test]
fn toast_rejected_points_at_credentials_and_carries_detail() {
    let msg = outage_toast_message(OutageClass::Rejected, "MySQL rejected the credentials")
        .expect("scenario 2 must notify");
    assert!(msg.contains("rejected"), "got: {msg}");
    assert!(msg.contains("credentials"), "got: {msg}");
    assert!(msg.contains("MySQL rejected the credentials"), "got: {msg}");
}

/// Every toast we raise has to name *this* extension — Zed shows toasts
/// from all attached servers in the same place, and "Laravel" alone would
/// be ambiguous next to Laravel's official extension. Covers all three
/// notifying classes, so a prefix missed on one of them fails here.
#[test]
fn every_outage_toast_carries_the_short_brand_prefix() {
    for class in [
        OutageClass::Unreachable,
        OutageClass::Rejected,
        OutageClass::Other,
    ] {
        let msg = outage_toast_message(class, "detail")
            .unwrap_or_else(|| panic!("{class:?} must notify"));
        assert!(
            msg.starts_with("Laravel CE: "),
            "{class:?} toast must be attributable to this extension, got: {msg}"
        );
    }
}

#[test]
fn toast_not_configured_is_silent() {
    assert!(
        outage_toast_message(OutageClass::NotConfigured, "no config").is_none(),
        "a project without a database is a normal state, not an outage"
    );
}

#[test]
fn toast_other_is_generic_but_present() {
    let msg = outage_toast_message(OutageClass::Other, "weird error").expect("must notify");
    assert!(msg.contains("database connection failed"), "got: {msg}");
    assert!(msg.contains("weird error"), "got: {msg}");
}

// ---------------------------------------------------------------------------
// Provider-level notification gating: one Outage per outage episode, one
// Reconnected per recovery. Exercises the set_error/clear_error breaker
// hooks and the event channel end to end — still no real database.
// ---------------------------------------------------------------------------

/// Unwrap a breaker event as an outage, failing loudly on a reconnect.
fn expect_outage(event: DbBreakerEvent) -> DbOutageEvent {
    match event {
        DbBreakerEvent::Outage(o) => o,
        DbBreakerEvent::Reconnected => panic!("expected an Outage event, got Reconnected"),
    }
}

#[tokio::test]
async fn outage_channel_fires_once_per_outage_and_rearms_on_reconnect() {
    let provider = DatabaseSchemaProvider::new(PathBuf::from("/tmp"));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    provider.set_event_channel(tx).await;

    let refused = "tcp 127.0.0.1:3306: Connection refused (os error 61)";
    provider
        .set_error("mysql", refused, OutageClass::Unreachable)
        .await;
    provider
        .set_error("mysql", refused, OutageClass::Unreachable)
        .await;

    let first = expect_outage(rx.try_recv().expect("first failure announces the outage"));
    assert_eq!(first.class, OutageClass::Unreachable);
    assert_eq!(first.message, refused);
    assert!(
        rx.try_recv().is_err(),
        "continuing failures in the same outage must not re-announce"
    );

    provider.clear_error().await; // successful reconnect closes the breaker
    assert!(
        matches!(rx.try_recv(), Ok(DbBreakerEvent::Reconnected)),
        "recovery announces exactly one Reconnected"
    );

    provider
        .set_error("mysql", refused, OutageClass::Unreachable)
        .await;
    let second = expect_outage(
        rx.try_recv()
            .expect("a NEW outage after reconnect announces exactly once more"),
    );
    assert_eq!(second.class, OutageClass::Unreachable);
}

#[tokio::test]
async fn reconnect_event_fires_once_per_recovery_and_not_on_healthy_startup() {
    let provider = DatabaseSchemaProvider::new(PathBuf::from("/tmp"));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    provider.set_event_channel(tx).await;

    // Healthy startup: a first successful fetch from a fresh (Closed)
    // breaker must NOT toast "reconnected".
    provider.clear_error().await;
    assert!(
        rx.try_recv().is_err(),
        "healthy startup emits no reconnect toast"
    );

    // Outage → recovery: exactly one Reconnected on the recovery edge.
    provider
        .set_error("mysql", "Connection refused", OutageClass::Unreachable)
        .await;
    let _ = expect_outage(rx.try_recv().expect("outage announced"));
    provider.clear_error().await;
    assert!(
        matches!(rx.try_recv(), Ok(DbBreakerEvent::Reconnected)),
        "recovery announces exactly one Reconnected"
    );

    // A second successful refresh (already Closed) is steady state — silent.
    provider.clear_error().await;
    assert!(
        rx.try_recv().is_err(),
        "an already-healthy refresh emits no reconnect toast"
    );
}

// ---------------------------------------------------------------------------
// Isolation: interactive accessors are pure cache reads and NEVER connect.
// This is the key regression guard for the freeze bug — a DB-touching LSP
// request handler that blocked on a connect starved unrelated requests.
// ---------------------------------------------------------------------------

fn schema_with(tables: &[&str], cached_at: Instant) -> DatabaseSchema {
    DatabaseSchema {
        tables: tables.iter().map(|t| t.to_string()).collect(),
        columns: HashMap::new(),
        columns_with_types: HashMap::new(),
        cached_at,
    }
}

#[tokio::test]
async fn interactive_accessors_are_cache_only_and_never_connect() {
    // Cache cold, and (a /tmp project with no config/database.php) the DB is
    // effectively "down". Interactive reads must return empty WITHOUT ever
    // attempting a connection.
    let provider = DatabaseSchemaProvider::new(PathBuf::from("/tmp"));

    assert!(provider.get_schema().await.is_none());
    assert!(provider.get_tables().await.is_empty());
    assert!(provider.get_columns("users").await.is_empty());
    assert!(provider.get_columns_with_types("users").await.is_empty());

    assert!(
        !provider.was_connection_attempted().await,
        "interactive reads must NEVER connect — only the background loop does"
    );
}

#[tokio::test]
async fn interactive_get_schema_serves_stale_cache() {
    let provider = DatabaseSchemaProvider::new(PathBuf::from("/tmp"));
    // A schema older than the 60s TTL — is_valid() is false…
    let stale = schema_with(&["users"], Instant::now() - Duration::from_secs(120));
    assert!(!stale.is_valid());
    provider.set_test_schema(stale).await;

    // …but interactive reads serve last-known-good regardless of TTL. The
    // background loop is responsible for refreshing it, not the reader.
    let tables = provider.get_tables().await;
    assert_eq!(tables, vec!["users".to_string()]);
    assert!(!provider.was_connection_attempted().await);
}

// ---------------------------------------------------------------------------
// Background refresh loop: owns connect + breaker + notification, off the
// request path. Exercised without a live DB via the no-config failure path,
// the injected-slow-future timeout path, and the pure breaker cadence.
// ---------------------------------------------------------------------------

/// The background loop's per-tick sleep should be roughly a full cooldown
/// when the breaker is open — within one second of it, allowing for the
/// microseconds of wall-clock drift between opening the breaker and reading
/// the remaining cooldown back.
fn assert_backs_off_on_cooldown(sleep: Duration) {
    assert!(
        sleep <= COOLDOWN && sleep > COOLDOWN - Duration::from_secs(1),
        "expected ~cooldown backoff, got {sleep:?}"
    );
}

#[tokio::test]
async fn refresh_tick_failure_opens_breaker_backs_off_and_notifies_once() {
    // /tmp has no config/database.php, so fetch_schema fails fast.
    let provider = DatabaseSchemaProvider::new(PathBuf::from("/tmp"));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    provider.set_event_channel(tx).await;

    let sleep = provider
        .refresh_tick(Duration::from_secs(1), Duration::from_secs(60))
        .await;

    assert!(
        provider.was_connection_attempted().await,
        "the background tick DOES fetch"
    );
    // Breaker opened → back off on the cooldown, not the healthy interval.
    assert_backs_off_on_cooldown(sleep);
    // Exactly one outage event (no config → NotConfigured → silent toast,
    // but the edge still fires on the channel).
    let event = expect_outage(rx.try_recv().expect("first failure announces the outage"));
    assert_eq!(event.class, OutageClass::NotConfigured);
    assert!(rx.try_recv().is_err(), "only one event per outage episode");

    // A second immediate tick is denied by the open breaker: no re-fetch,
    // no second event, still backing off on the cooldown.
    let sleep2 = provider
        .refresh_tick(Duration::from_secs(1), Duration::from_secs(60))
        .await;
    assert_backs_off_on_cooldown(sleep2);
    assert!(rx.try_recv().is_err(), "open breaker suppresses re-fetch");
}

#[tokio::test]
async fn whole_fetch_timeout_opens_breaker_instead_of_hanging() {
    let provider = DatabaseSchemaProvider::new(PathBuf::from("/tmp"));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    provider.set_event_channel(tx).await;

    // A fetch that "connects" but then stalls far longer than the budget —
    // the connected-but-stalled-server case that used to hang forever
    // (post-connect queries were unbounded). A tiny budget vs a long stall
    // makes the timeout fire near-instantly without hanging the test.
    let budget = Duration::from_millis(20);
    let stalled = async {
        tokio::time::sleep(Duration::from_secs(3600)).await;
        Some(schema_with(&["users"], Instant::now()))
    };
    let stored = provider.run_bounded_fetch(budget, stalled).await;

    assert!(!stored, "a timed-out fetch must not fill the cache");
    assert!(provider.get_schema().await.is_none());
    let event = expect_outage(
        rx.try_recv()
            .expect("timeout opens the breaker and notifies"),
    );
    assert_eq!(
        event.class,
        OutageClass::Unreachable,
        "a stalled server is treated as unreachable"
    );
}

#[tokio::test]
async fn run_bounded_fetch_success_fills_cache() {
    let provider = DatabaseSchemaProvider::new(PathBuf::from("/tmp"));
    let fetched = async { Some(schema_with(&["users", "posts"], Instant::now())) };
    let stored = provider
        .run_bounded_fetch(Duration::from_secs(5), fetched)
        .await;
    assert!(stored);
    assert_eq!(
        provider.get_tables().await,
        vec!["users".to_string(), "posts".to_string()]
    );
}

// ---------------------------------------------------------------------------
// Sail / Docker bind-IP detection: find a DB bound to a non-127.0.0.1
// loopback IP (Mike's Herd setup pins Sail to 127.0.0.2). Temp-dir fixtures
// only — never touches the SQLite test-project/.env. Layered precedence:
// compose override → base compose → APP_PORT → 127.0.0.1 backstop.
// ---------------------------------------------------------------------------

use tempfile::TempDir;

/// TCP candidate labels from a builder, in order.
fn tcp_labels(candidates: &[super::ConnCandidate]) -> Vec<String> {
    candidates
        .iter()
        .filter(|c| c.label.starts_with("tcp "))
        .map(|c| c.label.clone())
        .collect()
}

fn write(dir: &std::path::Path, name: &str, content: &str) {
    std::fs::write(dir.join(name), content).unwrap();
}

/// A mysql config whose host is the Docker service name `mysql`.
fn mysql_service_cfg() -> super::DatabaseConfig {
    make_config_with(None, None, "mysql")
}

#[test]
fn sail_override_bind_ip_becomes_a_candidate() {
    let dir = TempDir::new().unwrap();
    // FORWARD_DB_PORT unset → the `:-3306` default applies.
    write(
        dir.path(),
        "docker-compose.override.yml",
        "services:\n  mysql:\n    image: 'mysql:8'\n    ports:\n      - '127.0.0.2:${FORWARD_DB_PORT:-3306}:3306'\n",
    );
    let provider = DatabaseSchemaProvider::new(dir.path().to_path_buf());
    let cands = provider.build_mysql_candidates(&mysql_service_cfg());
    assert_eq!(
        tcp_labels(&cands),
        vec![
            "tcp mysql:3306".to_string(),
            "tcp 127.0.0.2:3306".to_string(),
            "tcp 127.0.0.1:3306".to_string(),
        ],
        "detected bind IP goes after the service name and before the backstop"
    );
}

#[test]
fn sail_custom_forward_port_uses_host_port_not_container() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), ".env", "FORWARD_DB_PORT=33061\n");
    write(
        dir.path(),
        "docker-compose.override.yml",
        "services:\n  mysql:\n    ports:\n      - '127.0.0.2:${FORWARD_DB_PORT:-3306}:3306'\n",
    );
    let provider = DatabaseSchemaProvider::new(dir.path().to_path_buf());
    let cands = provider.build_mysql_candidates(&mysql_service_cfg());
    assert!(
        tcp_labels(&cands).contains(&"tcp 127.0.0.2:33061".to_string()),
        "the forwarded HOST port (33061) is used, not the container port; got {:?}",
        tcp_labels(&cands)
    );
}

#[test]
fn sail_detects_from_base_compose_when_no_override() {
    let dir = TempDir::new().unwrap();
    write(
        dir.path(),
        "docker-compose.yml",
        "services:\n  mysql:\n    ports:\n      - '127.0.0.2:3306:3306'\n",
    );
    let provider = DatabaseSchemaProvider::new(dir.path().to_path_buf());
    let cands = provider.build_mysql_candidates(&mysql_service_cfg());
    assert!(tcp_labels(&cands).contains(&"tcp 127.0.0.2:3306".to_string()));
}

#[test]
fn sail_override_without_match_falls_through_to_base() {
    let dir = TempDir::new().unwrap();
    // Override maps a DIFFERENT container port → no match for DB port 3306.
    write(
        dir.path(),
        "docker-compose.override.yml",
        "services:\n  mysql:\n    ports:\n      - '127.0.0.9:9999:9999'\n",
    );
    write(
        dir.path(),
        "docker-compose.yml",
        "services:\n  mysql:\n    ports:\n      - '127.0.0.3:3306:3306'\n",
    );
    let provider = DatabaseSchemaProvider::new(dir.path().to_path_buf());
    let cands = provider.build_mysql_candidates(&mysql_service_cfg());
    let labels = tcp_labels(&cands);
    assert!(
        labels.contains(&"tcp 127.0.0.3:3306".to_string()),
        "got {labels:?}"
    );
    assert!(
        !labels.iter().any(|l| l.contains("127.0.0.9")),
        "got {labels:?}"
    );
}

#[test]
fn sail_detects_from_app_port_bind_ip() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), ".env", "APP_PORT=127.0.0.2:80\n");
    let provider = DatabaseSchemaProvider::new(dir.path().to_path_buf());
    let cands = provider.build_mysql_candidates(&mysql_service_cfg());
    // The DB's own port (3306) is paired with the APP_PORT bind IP.
    assert!(tcp_labels(&cands).contains(&"tcp 127.0.0.2:3306".to_string()));
}

#[test]
fn app_port_without_ip_yields_no_extra_candidate() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), ".env", "APP_PORT=80\n");
    let provider = DatabaseSchemaProvider::new(dir.path().to_path_buf());
    let cands = provider.build_mysql_candidates(&mysql_service_cfg());
    assert_eq!(
        tcp_labels(&cands),
        vec![
            "tcp mysql:3306".to_string(),
            "tcp 127.0.0.1:3306".to_string()
        ],
        "APP_PORT with no IP falls straight through to the 127.0.0.1 backstop"
    );
}

#[test]
fn sail_precedence_override_beats_base_and_app_port() {
    let dir = TempDir::new().unwrap();
    write(
        dir.path(),
        "docker-compose.override.yml",
        "services:\n  mysql:\n    ports:\n      - '127.0.0.2:3306:3306'\n",
    );
    write(
        dir.path(),
        "docker-compose.yml",
        "services:\n  mysql:\n    ports:\n      - '127.0.0.3:3306:3306'\n",
    );
    write(dir.path(), ".env", "APP_PORT=127.0.0.4:80\n");
    let provider = DatabaseSchemaProvider::new(dir.path().to_path_buf());
    let labels = tcp_labels(&provider.build_mysql_candidates(&mysql_service_cfg()));
    assert!(
        labels.contains(&"tcp 127.0.0.2:3306".to_string()),
        "override wins: {labels:?}"
    );
    assert!(
        !labels
            .iter()
            .any(|l| l.contains("127.0.0.3") || l.contains("127.0.0.4")),
        "{labels:?}"
    );
}

#[test]
fn sail_dedupes_detected_loopback_against_backstop() {
    let dir = TempDir::new().unwrap();
    write(
        dir.path(),
        "docker-compose.override.yml",
        "services:\n  mysql:\n    ports:\n      - '127.0.0.1:3306:3306'\n",
    );
    let provider = DatabaseSchemaProvider::new(dir.path().to_path_buf());
    let labels = tcp_labels(&provider.build_mysql_candidates(&mysql_service_cfg()));
    assert_eq!(
        labels.iter().filter(|l| *l == "tcp 127.0.0.1:3306").count(),
        1,
        "a detected 127.0.0.1 must not duplicate the backstop: {labels:?}"
    );
    assert_eq!(
        labels,
        vec![
            "tcp mysql:3306".to_string(),
            "tcp 127.0.0.1:3306".to_string()
        ]
    );
}

#[test]
fn detection_skipped_when_host_is_already_an_ip() {
    let dir = TempDir::new().unwrap();
    // Even with a compose file present, an IP host bypasses detection.
    write(
        dir.path(),
        "docker-compose.override.yml",
        "services:\n  mysql:\n    ports:\n      - '127.0.0.9:3306:3306'\n",
    );
    let provider = DatabaseSchemaProvider::new(dir.path().to_path_buf());
    let cfg = make_config_with(None, None, "127.0.0.2");
    let labels = tcp_labels(&provider.build_mysql_candidates(&cfg));
    assert_eq!(
        labels,
        vec!["tcp 127.0.0.2:3306".to_string()],
        "an IP host is used verbatim — no service-name heuristic, no compose scan"
    );
}

#[test]
fn sail_detection_feeds_postgres_candidates_too() {
    let dir = TempDir::new().unwrap();
    write(
        dir.path(),
        "docker-compose.override.yml",
        "services:\n  pgsql:\n    ports:\n      - '127.0.0.2:5432:5432'\n",
    );
    let provider = DatabaseSchemaProvider::new(dir.path().to_path_buf());
    let mut cfg = make_config_with(None, None, "pgsql");
    cfg.driver = "pgsql".to_string();
    cfg.port = 5432;
    let labels = tcp_labels(&provider.build_postgres_candidates(&cfg));
    assert_eq!(
        labels,
        vec![
            "tcp pgsql:5432".to_string(),
            "tcp 127.0.0.2:5432".to_string(),
            "tcp 127.0.0.1:5432".to_string(),
        ]
    );
}

#[test]
fn sail_long_form_ports_mapping_is_parsed() {
    let dir = TempDir::new().unwrap();
    write(
        dir.path(),
        "docker-compose.override.yml",
        "services:\n  mysql:\n    ports:\n      - target: 3306\n        published: 33061\n        host_ip: 127.0.0.2\n",
    );
    let provider = DatabaseSchemaProvider::new(dir.path().to_path_buf());
    let labels = tcp_labels(&provider.build_mysql_candidates(&mysql_service_cfg()));
    assert!(
        labels.contains(&"tcp 127.0.0.2:33061".to_string()),
        "long-form target/published/host_ip mapping: {labels:?}"
    );
}

#[test]
fn normalize_bind_ip_maps_wildcards_to_loopback() {
    assert_eq!(
        DatabaseSchemaProvider::normalize_bind_ip("0.0.0.0").as_deref(),
        Some("127.0.0.1")
    );
    assert_eq!(
        DatabaseSchemaProvider::normalize_bind_ip("::").as_deref(),
        Some("127.0.0.1")
    );
    assert_eq!(
        DatabaseSchemaProvider::normalize_bind_ip("").as_deref(),
        Some("127.0.0.1")
    );
    assert_eq!(
        DatabaseSchemaProvider::normalize_bind_ip("127.0.0.2").as_deref(),
        Some("127.0.0.2")
    );
    // Hostnames / IPv6 literals fail open.
    assert_eq!(
        DatabaseSchemaProvider::normalize_bind_ip("db.example.com"),
        None
    );
}

// ---- nested-`ports:` mis-parse regression + APP_PORT loopback guard ----

#[test]
fn nested_ports_under_extension_field_is_ignored() {
    // `x-meta` is a legal compose extension field; its nested `ports:` list
    // appears textually BEFORE the real service-level one. The parser must
    // read the service-level binding, not the nested decoy.
    let dir = TempDir::new().unwrap();
    write(
        dir.path(),
        "docker-compose.override.yml",
        "services:\n  mysql:\n    x-meta:\n      ports:\n        - '10.0.0.1:9999:3306'\n    ports:\n      - '127.0.0.2:33061:3306'\n",
    );
    let provider = DatabaseSchemaProvider::new(dir.path().to_path_buf());
    let labels = tcp_labels(&provider.build_mysql_candidates(&mysql_service_cfg()));
    assert!(
        labels.contains(&"tcp 127.0.0.2:33061".to_string()),
        "must read the REAL service-level ports, got {labels:?}"
    );
    assert!(
        !labels
            .iter()
            .any(|l| l.contains("10.0.0.1") || l.contains(":9999")),
        "must NOT read the nested x-meta ports, got {labels:?}"
    );
}

#[test]
fn nested_ports_only_yields_no_detection() {
    // The service has ONLY a nested `x-meta.ports:` and no direct `ports:`.
    // Ambiguity → None (never read the nested one); fall to the backstop.
    let dir = TempDir::new().unwrap();
    write(
        dir.path(),
        "docker-compose.override.yml",
        "services:\n  mysql:\n    x-meta:\n      ports:\n        - '10.0.0.1:9999:3306'\n    image: 'mysql:8'\n",
    );
    let provider = DatabaseSchemaProvider::new(dir.path().to_path_buf());
    let labels = tcp_labels(&provider.build_mysql_candidates(&mysql_service_cfg()));
    assert_eq!(
        labels,
        vec![
            "tcp mysql:3306".to_string(),
            "tcp 127.0.0.1:3306".to_string()
        ],
        "a nested-only ports list must not be read; got {labels:?}"
    );
}

#[test]
fn service_level_ports_before_nested_still_wins() {
    // Order-independence: the real `ports:` appears BEFORE a later nested one.
    let dir = TempDir::new().unwrap();
    write(
        dir.path(),
        "docker-compose.override.yml",
        "services:\n  mysql:\n    ports:\n      - '127.0.0.2:33061:3306'\n    x-meta:\n      ports:\n        - '10.0.0.1:9999:3306'\n",
    );
    let provider = DatabaseSchemaProvider::new(dir.path().to_path_buf());
    let labels = tcp_labels(&provider.build_mysql_candidates(&mysql_service_cfg()));
    assert!(
        labels.contains(&"tcp 127.0.0.2:33061".to_string()),
        "got {labels:?}"
    );
    assert!(
        !labels.iter().any(|l| l.contains("10.0.0.1")),
        "got {labels:?}"
    );
}

#[test]
fn app_port_loopback_bind_yields_no_app_port_candidate() {
    // APP_PORT=127.0.0.1:80 → the backstop already covers 127.0.0.1, so
    // detect_from_app_port returns None and the sole 127.0.0.1 candidate
    // carries the backstop note (not a misattributed APP_PORT note).
    let dir = TempDir::new().unwrap();
    write(dir.path(), ".env", "APP_PORT=127.0.0.1:80\n");
    let provider = DatabaseSchemaProvider::new(dir.path().to_path_buf());
    let cfg = mysql_service_cfg();
    assert!(
        provider.detect_from_app_port(&cfg).is_none(),
        "a 127.0.0.1 APP_PORT bind must not produce an APP_PORT-attributed candidate"
    );
    let cands = provider.build_mysql_candidates(&cfg);
    let backstop = cands
        .iter()
        .find(|c| c.label == "tcp 127.0.0.1:3306")
        .expect("backstop present");
    assert!(
        backstop
            .success_note
            .as_deref()
            .unwrap_or("")
            .contains("Sail"),
        "the 127.0.0.1 candidate carries the backstop note, not APP_PORT attribution"
    );
    assert_eq!(
        tcp_labels(&cands),
        vec![
            "tcp mysql:3306".to_string(),
            "tcp 127.0.0.1:3306".to_string()
        ]
    );
}

// ---- broadened gate: detection also runs for a plain-loopback primary ----

#[test]
fn should_attempt_sail_detection_covers_loopback_but_not_explicit_ips() {
    assert!(DatabaseSchemaProvider::should_attempt_sail_detection(
        "mysql"
    ));
    assert!(DatabaseSchemaProvider::should_attempt_sail_detection(
        "127.0.0.1"
    ));
    assert!(DatabaseSchemaProvider::should_attempt_sail_detection(
        "localhost"
    ));
    assert!(DatabaseSchemaProvider::should_attempt_sail_detection(
        "Localhost"
    ));
    // Explicit, precisely-named hosts are honoured verbatim — no detection.
    assert!(!DatabaseSchemaProvider::should_attempt_sail_detection(
        "127.0.0.2"
    ));
    assert!(!DatabaseSchemaProvider::should_attempt_sail_detection(
        "192.168.1.50"
    ));
    assert!(!DatabaseSchemaProvider::should_attempt_sail_detection(
        "db.example.com"
    ));
}

#[test]
fn loopback_host_with_app_port_detects_second_loopback_ip() {
    // The Decision Cloud case: DB_HOST=127.0.0.1 but Sail is pinned to
    // 127.0.0.2 via APP_PORT. Literal primary first, detected second.
    let dir = TempDir::new().unwrap();
    write(dir.path(), ".env", "APP_PORT=127.0.0.2:80\n");
    let provider = DatabaseSchemaProvider::new(dir.path().to_path_buf());
    let cfg = make_config_with(None, None, "127.0.0.1");
    assert_eq!(
        tcp_labels(&provider.build_mysql_candidates(&cfg)),
        vec![
            "tcp 127.0.0.1:3306".to_string(),
            "tcp 127.0.0.2:3306".to_string()
        ],
        "literal 127.0.0.1 primary first, detected 127.0.0.2 second"
    );
}

#[test]
fn localhost_host_with_app_port_detects_loopback_ip() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), ".env", "APP_PORT=127.0.0.2:80\n");
    let provider = DatabaseSchemaProvider::new(dir.path().to_path_buf());
    let cfg = make_config_with(None, None, "localhost");
    assert_eq!(
        tcp_labels(&provider.build_mysql_candidates(&cfg)),
        vec![
            "tcp localhost:3306".to_string(),
            "tcp 127.0.0.2:3306".to_string()
        ],
        "the literal 'localhost' primary is kept; 127.0.0.2 is added"
    );
}

#[test]
fn loopback_host_with_compose_detects_bind_ip() {
    let dir = TempDir::new().unwrap();
    write(
        dir.path(),
        "docker-compose.override.yml",
        "services:\n  mysql:\n    ports:\n      - '127.0.0.2:3306:3306'\n",
    );
    let provider = DatabaseSchemaProvider::new(dir.path().to_path_buf());
    let cfg = make_config_with(None, None, "127.0.0.1");
    assert_eq!(
        tcp_labels(&provider.build_mysql_candidates(&cfg)),
        vec![
            "tcp 127.0.0.1:3306".to_string(),
            "tcp 127.0.0.2:3306".to_string()
        ]
    );
}

#[test]
fn loopback_host_with_matching_app_port_yields_only_primary() {
    // APP_PORT bound to 127.0.0.1 → the 127.0.0.1 guard returns None, and a
    // loopback primary has no backstop, so just the single primary remains.
    let dir = TempDir::new().unwrap();
    write(dir.path(), ".env", "APP_PORT=127.0.0.1:80\n");
    let provider = DatabaseSchemaProvider::new(dir.path().to_path_buf());
    let cfg = make_config_with(None, None, "127.0.0.1");
    assert_eq!(
        tcp_labels(&provider.build_mysql_candidates(&cfg)),
        vec!["tcp 127.0.0.1:3306".to_string()]
    );
}

#[test]
fn explicit_non_loopback_host_skips_detection_even_with_app_port() {
    // A precisely-named host must be honoured verbatim — no APP_PORT/compose
    // second-guessing, even when a signal is present.
    for host in ["192.168.1.50", "db.example.com", "127.0.0.2"] {
        let dir = TempDir::new().unwrap();
        write(dir.path(), ".env", "APP_PORT=127.0.0.9:80\n");
        write(
            dir.path(),
            "docker-compose.override.yml",
            "services:\n  mysql:\n    ports:\n      - '127.0.0.8:3306:3306'\n",
        );
        let provider = DatabaseSchemaProvider::new(dir.path().to_path_buf());
        let cfg = make_config_with(None, None, host);
        assert_eq!(
            tcp_labels(&provider.build_mysql_candidates(&cfg)),
            vec![format!("tcp {host}:3306")],
            "explicit host {host} must skip detection"
        );
    }
}

// ---- YAML merge tags (`!override` / `!reset`) — the real Sail override shape ----

#[test]
fn line_is_key_accepts_yaml_tag_but_not_inline_scalar() {
    use super::line_is_key;
    // A `!`-tag introduces the block below the key.
    assert!(line_is_key("ports: !override", "ports"));
    assert!(line_is_key("ports: !reset", "ports"));
    assert!(line_is_key("ports:  !override  # note", "ports"));
    // Still a key when bare or comment-only.
    assert!(line_is_key("ports:", "ports"));
    assert!(line_is_key("ports: # inline list follows", "ports"));
    // A real inline scalar is NOT a block-introducing key.
    assert!(!line_is_key("ports: 3306:3306", "ports"));
    assert!(!line_is_key("image: mysql:8", "ports"));
}

/// The exact Decision Cloud override: `ports: !override` binding the DB to
/// 127.0.0.2, layered over a base compose that binds to 127.0.0.1.
const DC_OVERRIDE: &str = "services:\n  mysql:\n    ports: !override\n      - '127.0.0.2:3306:3306'\n  pgsql:\n    ports: !override\n      - '127.0.0.2:5432:5432'\n";
const DC_BASE: &str = "services:\n  mysql:\n    ports:\n      - '${FORWARD_DB_PORT:-3306}:3306'\n";

#[test]
fn sail_override_with_merge_tag_wins_over_base() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), "docker-compose.override.yml", DC_OVERRIDE);
    write(dir.path(), "docker-compose.yml", DC_BASE);
    let provider = DatabaseSchemaProvider::new(dir.path().to_path_buf());
    let labels = tcp_labels(&provider.build_mysql_candidates(&mysql_service_cfg()));
    assert_eq!(
        labels,
        vec![
            "tcp mysql:3306".to_string(),
            "tcp 127.0.0.2:3306".to_string(),
            "tcp 127.0.0.1:3306".to_string(),
        ],
        "the `!override` block must be read (127.0.0.2), beating base's 127.0.0.1"
    );
}

#[test]
fn sail_override_merge_tag_with_loopback_host() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), "docker-compose.override.yml", DC_OVERRIDE);
    write(dir.path(), "docker-compose.yml", DC_BASE);
    let provider = DatabaseSchemaProvider::new(dir.path().to_path_buf());
    let cfg = make_config_with(None, None, "127.0.0.1");
    assert_eq!(
        tcp_labels(&provider.build_mysql_candidates(&cfg)),
        vec![
            "tcp 127.0.0.1:3306".to_string(),
            "tcp 127.0.0.2:3306".to_string()
        ]
    );
}

#[test]
fn sail_override_merge_tag_custom_host_port() {
    let dir = TempDir::new().unwrap();
    write(
        dir.path(),
        "docker-compose.override.yml",
        "services:\n  mysql:\n    ports: !override\n      - '127.0.0.2:33061:3306'\n",
    );
    let provider = DatabaseSchemaProvider::new(dir.path().to_path_buf());
    let labels = tcp_labels(&provider.build_mysql_candidates(&mysql_service_cfg()));
    assert!(
        labels.contains(&"tcp 127.0.0.2:33061".to_string()),
        "the forwarded host port under a `!override` tag is honoured; got {labels:?}"
    );
}

#[test]
fn sail_override_merge_tag_feeds_postgres_too() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), "docker-compose.override.yml", DC_OVERRIDE);
    let provider = DatabaseSchemaProvider::new(dir.path().to_path_buf());
    let mut cfg = make_config_with(None, None, "pgsql");
    cfg.driver = "pgsql".to_string();
    cfg.port = 5432;
    assert_eq!(
        tcp_labels(&provider.build_postgres_candidates(&cfg)),
        vec![
            "tcp pgsql:5432".to_string(),
            "tcp 127.0.0.2:5432".to_string(),
            "tcp 127.0.0.1:5432".to_string(),
        ]
    );
}

// ---- variable-referenced connection block (Sail's `'mysql' => $mysql`) ----

/// A Sail-style config where the connection is factored into a `$mysql`
/// variable and referenced from `connections` (the real Decision Cloud shape).
const CONFIG_VAR_FORM: &str = r#"<?php
$mysql = [
    'driver' => 'mysql',
    'host' => env('DB_HOST', '127.0.0.1'),
    'port' => env('DB_PORT', '3306'),
    'database' => env('DB_DATABASE', 'laravel'),
    'username' => env('DB_USERNAME', 'root'),
    'password' => env('DB_PASSWORD', ''),
];
$mysqlUnbuffered = $mysql;
return [
    'default' => env('DB_CONNECTION', 'mysql'),
    'connections' => [
        'mysql' => $mysql,
        'mysql_unbuffered' => $mysqlUnbuffered,
    ],
];
"#;

fn write_config_php(dir: &std::path::Path, content: &str) {
    std::fs::create_dir_all(dir.join("config")).unwrap();
    std::fs::write(dir.join("config").join("database.php"), content).unwrap();
}

const DC_ENV: &str =
    "DB_CONNECTION=mysql\nDB_HOST=mysql\nDB_DATABASE=cashlender\nDB_USERNAME=sail\nDB_PASSWORD=password\n";

#[test]
fn extract_connection_block_inline_form_still_works() {
    let provider = DatabaseSchemaProvider::new(PathBuf::from("/tmp"));
    let content = "'connections' => [\n  'mysql' => [\n    'driver' => 'mysql',\n    'host' => env('DB_HOST', '127.0.0.1'),\n  ],\n]";
    let block = provider
        .extract_connection_block(content, "mysql")
        .expect("inline array must resolve");
    assert!(block.contains("'host' => env('DB_HOST'"), "got: {block}");
}

#[test]
fn extract_connection_block_variable_form_resolves() {
    let provider = DatabaseSchemaProvider::new(PathBuf::from("/tmp"));
    let block = provider
        .extract_connection_block(CONFIG_VAR_FORM, "mysql")
        .expect("`'mysql' => $mysql` must resolve to the $mysql array");
    assert!(block.contains("'host' => env('DB_HOST'"), "got: {block}");
    assert!(
        block.contains("'database' => env('DB_DATABASE'"),
        "got: {block}"
    );
}

#[test]
fn extract_connection_block_undefined_variable_is_none() {
    let provider = DatabaseSchemaProvider::new(PathBuf::from("/tmp"));
    let content = "'connections' => [\n  'mysql' => $missing,\n]";
    assert!(
        provider
            .extract_connection_block(content, "mysql")
            .is_none(),
        "an undefined variable reference must fail open to None (→ defaults)"
    );
}

#[tokio::test]
async fn variable_referenced_connection_resolves_env_not_defaults() {
    let dir = TempDir::new().unwrap();
    write_config_php(dir.path(), CONFIG_VAR_FORM);
    write(dir.path(), ".env", DC_ENV);
    let provider = DatabaseSchemaProvider::new(dir.path().to_path_buf());
    let cfg = provider.get_database_config().await.expect("config parsed");
    // Before the fix these were all hardcoded defaults (127.0.0.1 / laravel /
    // root / ""), silently ignoring .env.
    assert_eq!(cfg.host, "mysql");
    assert_eq!(cfg.database, "cashlender");
    assert_eq!(cfg.username, "sail");
    assert_eq!(cfg.password, "password");
    assert_eq!(cfg.port, 3306);
}

// ---- framework config fallback (Laravel 11+ merged config, issue #398) ----

/// A verbatim copy of `vendor/laravel/framework/config/database.php` from
/// laravel/framework v12.70.1. Laravel 11+ merges the app config over it.
const FRAMEWORK_CONFIG: &str = include_str!("fixtures/laravel-12-framework-database.php");

/// An app config that only adds its own connection and relies on the
/// framework merge for `default` and the standard connections.
const APP_CONFIG_CUSTOM_ONLY: &str = r#"<?php
return [
    'connections' => [
        'legacy' => [
            'driver' => 'mysql',
            'database' => env('DB_DATABASE_LEGACY', 'legacy'),
        ],
    ],
];
"#;

/// Every value differs from the hard-coded fallbacks (127.0.0.1, 3306,
/// laravel, root, empty), so a match can only come from `.env`.
const CUSTOM_ENV: &str = "DB_CONNECTION=mysql\nDB_HOST=db.internal\nDB_PORT=3307\nDB_DATABASE=app_db\nDB_USERNAME=app_user\nDB_PASSWORD=app_secret\n";

fn write_framework_config_php(dir: &std::path::Path) {
    let config_dir = dir.join("vendor/laravel/framework/config");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(config_dir.join("database.php"), FRAMEWORK_CONFIG).unwrap();
}

/// Build a provider over a temp project with the given files.
fn merged_config_provider(
    app_config: Option<&str>,
    with_framework_config: bool,
    env: &str,
) -> (TempDir, DatabaseSchemaProvider) {
    let dir = TempDir::new().unwrap();
    if let Some(content) = app_config {
        write_config_php(dir.path(), content);
    }
    if with_framework_config {
        write_framework_config_php(dir.path());
    }
    write(dir.path(), ".env", env);
    let provider = DatabaseSchemaProvider::new(dir.path().to_path_buf());
    (dir, provider)
}

fn assert_custom_env_values(cfg: &super::DatabaseConfig) {
    assert_eq!(cfg.driver, "mysql");
    assert_eq!(cfg.host, "db.internal");
    assert_eq!(cfg.port, 3307);
    assert_eq!(cfg.database, "app_db");
    assert_eq!(cfg.username, "app_user");
    assert_eq!(cfg.password, "app_secret");
}

#[tokio::test]
async fn custom_only_app_config_resolves_env_through_framework_block() {
    let (_dir, provider) = merged_config_provider(Some(APP_CONFIG_CUSTOM_ONLY), true, CUSTOM_ENV);
    let cfg = provider.get_database_config().await.expect("config parsed");
    assert_custom_env_values(&cfg);
}

#[tokio::test]
async fn missing_app_config_resolves_env_through_framework_block() {
    let (_dir, provider) = merged_config_provider(None, true, CUSTOM_ENV);
    let cfg = provider.get_database_config().await.expect("config parsed");
    assert_custom_env_values(&cfg);
}

#[tokio::test]
async fn framework_block_env_default_applies_when_env_is_silent() {
    let (_dir, provider) = merged_config_provider(None, true, "DB_CONNECTION=sqlite\n");
    let cfg = provider.get_database_config().await.expect("config parsed");
    assert_eq!(cfg.driver, "sqlite");
    assert_eq!(cfg.database, "database/database.sqlite");
}

#[tokio::test]
async fn framework_default_key_follows_db_connection_in_env() {
    let (_dir, provider) = merged_config_provider(
        Some(APP_CONFIG_CUSTOM_ONLY),
        true,
        "DB_CONNECTION=pgsql\nDB_DATABASE=pg_app\n",
    );
    let cfg = provider.get_database_config().await.expect("config parsed");
    assert_eq!(cfg.driver, "pgsql");
    assert_eq!(cfg.database, "pg_app");
}

#[tokio::test]
async fn framework_default_key_falls_back_to_sqlite_when_db_connection_is_unset() {
    let (_dir, provider) = merged_config_provider(Some(APP_CONFIG_CUSTOM_ONLY), true, "");
    let cfg = provider.get_database_config().await.expect("config parsed");
    assert_eq!(cfg.driver, "sqlite");
}

#[tokio::test]
async fn app_default_key_wins_over_framework_default_key() {
    let app_config = r#"<?php
return [
    'default' => env('DB_CONNECTION', 'mysql'),
    'connections' => [],
];
"#;
    let (_dir, provider) = merged_config_provider(Some(app_config), true, "");
    let cfg = provider.get_database_config().await.expect("config parsed");
    assert_eq!(cfg.driver, "mysql");
}

#[tokio::test]
async fn literal_app_default_wins_over_framework_default_key() {
    // `pgsql` is neither the hard-coded fallback (`mysql`) nor the
    // framework's (`sqlite`), so it can only come from the app's literal.
    let app_config = r#"<?php
return [
    'default' => 'pgsql',
    'connections' => [],
];
"#;
    let (_dir, provider) = merged_config_provider(Some(app_config), true, "");
    let cfg = provider.get_database_config().await.expect("config parsed");
    assert_eq!(cfg.driver, "pgsql");

    let (_dir, provider) = merged_config_provider(Some(app_config), true, "DB_CONNECTION=mysql\n");
    let cfg = provider.get_database_config().await.expect("config parsed");
    assert_eq!(cfg.driver, "pgsql");
}

#[tokio::test]
async fn single_argument_env_default_keeps_hard_coded_fallback() {
    let app_config = r#"<?php
return [
    'default' => env('APP_DB_CONNECTION'),
    'connections' => [],
];
"#;
    let (_dir, provider) = merged_config_provider(Some(app_config), true, "");
    let cfg = provider.get_database_config().await.expect("config parsed");
    assert_eq!(cfg.driver, "mysql");

    let (_dir, provider) =
        merged_config_provider(Some(app_config), true, "APP_DB_CONNECTION=pgsql\n");
    let cfg = provider.get_database_config().await.expect("config parsed");
    assert_eq!(cfg.driver, "pgsql");
}

#[tokio::test]
async fn redis_default_block_in_app_config_does_not_mask_framework_default() {
    let app_config = r#"<?php
return [
    'connections' => [
        'legacy' => [
            'driver' => 'mysql',
        ],
    ],
    'redis' => [
        'default' => [
            'host' => env('REDIS_HOST', '127.0.0.1'),
        ],
    ],
];
"#;
    let (_dir, provider) = merged_config_provider(Some(app_config), true, "");
    let cfg = provider.get_database_config().await.expect("config parsed");
    assert_eq!(cfg.driver, "sqlite");
}

#[tokio::test]
async fn app_block_replaces_framework_block_whole() {
    // The partial app block has no `database` key. Laravel's merge replaces
    // the framework block whole, so the value is the hard-coded fallback,
    // never the framework block's `env('DB_DATABASE', ...)` → `from_env`.
    let app_config = r#"<?php
return [
    'connections' => [
        'mysql' => [
            'driver' => 'mysql',
            'host' => env('DB_HOST', '127.0.0.1'),
        ],
    ],
];
"#;
    let (_dir, provider) = merged_config_provider(
        Some(app_config),
        true,
        "DB_CONNECTION=mysql\nDB_DATABASE=from_env\n",
    );
    let cfg = provider.get_database_config().await.expect("config parsed");
    assert_eq!(cfg.database, "laravel");
}

#[tokio::test]
async fn full_app_config_ignores_framework_config() {
    // A Laravel 10-style app config holding its own `default` and `mysql`
    // block, with defaults that differ from the framework file's.
    let app_config = r#"<?php
return [
    'default' => env('DB_CONNECTION', 'mysql'),
    'connections' => [
        'mysql' => [
            'driver' => 'mysql',
            'host' => env('DB_HOST', 'app-host'),
            'port' => env('DB_PORT', '3310'),
            'database' => env('DB_DATABASE', 'app_default_db'),
            'username' => env('DB_USERNAME', 'app_default_user'),
            'password' => env('DB_PASSWORD', ''),
        ],
    ],
];
"#;
    let (_dir, provider) = merged_config_provider(Some(app_config), true, "");
    let cfg = provider.get_database_config().await.expect("config parsed");
    assert_eq!(cfg.driver, "mysql");
    assert_eq!(cfg.host, "app-host");
    assert_eq!(cfg.port, 3310);
    assert_eq!(cfg.database, "app_default_db");
    assert_eq!(cfg.username, "app_default_user");
}

#[tokio::test]
async fn missing_framework_config_keeps_app_only_resolution() {
    let (_dir, provider) = merged_config_provider(Some(CONFIG_VAR_FORM), false, DC_ENV);
    let cfg = provider.get_database_config().await.expect("config parsed");
    assert_eq!(cfg.host, "mysql");
    assert_eq!(cfg.database, "cashlender");

    let (_dir, provider) = merged_config_provider(Some(APP_CONFIG_CUSTOM_ONLY), false, "");
    assert_eq!(provider.get_connections(), vec!["legacy".to_string()]);
}

#[tokio::test]
async fn no_database_config_at_all_is_none() {
    let (_dir, provider) = merged_config_provider(None, false, CUSTOM_ENV);
    assert!(provider.get_database_config().await.is_none());
    assert!(provider.get_connections().is_empty());
}

#[test]
fn connection_names_merge_app_and_framework_top_level_keys() {
    let (_dir, provider) = merged_config_provider(Some(APP_CONFIG_CUSTOM_ONLY), true, "");
    // `pragmas` (sqlite) is a nested key in the framework file and must not
    // be offered as a connection.
    assert_eq!(
        provider.get_connections(),
        ["legacy", "sqlite", "mysql", "mariadb", "pgsql", "sqlsrv"]
            .map(String::from)
            .to_vec()
    );
}

#[test]
fn connection_names_list_a_redefined_framework_connection_once() {
    let app_config = r#"<?php
return [
    'connections' => [
        'mysql' => [
            'driver' => 'mysql',
            'options' => [
                'timeout' => 5,
            ],
        ],
    ],
];
"#;
    let (_dir, provider) = merged_config_provider(Some(app_config), true, "");
    assert_eq!(
        provider.get_connections(),
        ["mysql", "sqlite", "mariadb", "pgsql", "sqlsrv"]
            .map(String::from)
            .to_vec()
    );
}

#[test]
fn connection_names_come_from_framework_when_app_config_is_missing() {
    let (_dir, provider) = merged_config_provider(None, true, "");
    assert_eq!(
        provider.get_connections(),
        ["sqlite", "mysql", "mariadb", "pgsql", "sqlsrv"]
            .map(String::from)
            .to_vec()
    );
}

#[tokio::test]
async fn decision_cloud_end_to_end_variable_config_plus_sail_override() {
    let dir = TempDir::new().unwrap();
    write_config_php(dir.path(), CONFIG_VAR_FORM);
    write(dir.path(), ".env", DC_ENV);
    write(
        dir.path(),
        "docker-compose.override.yml",
        "services:\n  mysql:\n    ports: !override\n      - '127.0.0.2:3306:3306'\n",
    );
    let provider = DatabaseSchemaProvider::new(dir.path().to_path_buf());
    let cfg = provider.get_database_config().await.expect("config parsed");
    assert_eq!(cfg.host, "mysql", "the real DB_HOST feeds detection");

    let labels = tcp_labels(&provider.build_mysql_candidates(&cfg));
    assert!(
        labels.contains(&"tcp mysql:3306".to_string()),
        "primary service-name candidate present: {labels:?}"
    );
    assert!(
        labels.contains(&"tcp 127.0.0.2:3306".to_string()),
        "Sail override 127.0.0.2 detected once the host is correct: {labels:?}"
    );
}

// ---- logs are a display surface (issue #344) ----
//
// The four surfaces #344 enumerated all render into the client. Logs are the
// fifth: with `RUST_LOG` unset the server logs at `info` to stderr, which Zed
// shows in a visible panel, and `docs/troubleshooting.md` asks users to paste
// that panel into bug reports. A `.env` value read under a secret-bearing name
// must therefore be masked there too, by the same predicate the other four use.

/// A plaintext distinctive enough that finding it in a log cannot be a
/// coincidence.
const LOG_SECRET: &str = "hunter2-log-issue-344";
/// A second matched keyword category. One category alone cannot tell the shared
/// predicate from a hard-coded `contains("PASSWORD")`.
const LOG_TOKEN: &str = "tok-log-issue-344";
/// The unmatched control: an ordinary setting whose value must still be logged
/// in full, or the masking has bought security by deleting the diagnostic.
const LOG_PLAIN_HOST: &str = "db.internal.example";

/// The credential the *name* gate cannot see. `DATABASE_URL` matches no
/// segment of `SENSITIVE_ENV_SEGMENTS`, and Laravel's stock
/// `config/database.php` ships `'url' => env('DATABASE_URL')`, so this value
/// reaches the log on the default configuration of an ordinary project.
const LOG_URL_SECRET: &str = "url-hunter2@tail-355";
/// The half of `LOG_URL_SECRET` the old first-`@` parse left in the log
/// (issue #355). Asserted on its own: a whole-secret check passes vacuously on
/// a partially masked line, which is the defect this fixture now pins.
const LOG_URL_SECRET_TAIL: &str = "tail-355";

/// The credential *neither* gate could see. `JDBC_URL` matches no sensitive
/// segment, and a `jdbc:` value parses to an opaque path with no authority, so
/// `url` reports no password and the shape gate returned the whole line
/// untouched — into an `info!` that renders in Zed's log panel by default.
const LOG_JDBC_SECRET: &str = "jdbc-hunter2@tail-358";
/// `LOG_JDBC_SECRET`'s surviving half, for the same reason as
/// `LOG_URL_SECRET_TAIL`.
const LOG_JDBC_SECRET_TAIL: &str = "tail-358";

fn log_fixture_db_url() -> String {
    format!("mysql://sail:{LOG_URL_SECRET}@{LOG_PLAIN_HOST}:3306/laravel")
}

/// The same fixture with the credential masked — the positive control. Without
/// it the leak assertion would also pass on a log line that dropped the URL
/// altogether, which would be a lost diagnostic rather than a fix.
fn log_fixture_db_url_masked() -> String {
    format!("mysql://sail:***@{LOG_PLAIN_HOST}:3306/laravel")
}

fn log_fixture_jdbc_url() -> String {
    format!("jdbc:mysql://sail:{LOG_JDBC_SECRET}@{LOG_PLAIN_HOST}:3306/laravel")
}

fn log_fixture_jdbc_url_masked() -> String {
    format!("jdbc:mysql://sail:***@{LOG_PLAIN_HOST}:3306/laravel")
}

/// `config/database.php` with the `url` setting Laravel ships by default.
const CONFIG_WITH_URL: &str = r#"<?php
return [
    'default' => env('DB_CONNECTION', 'mysql'),
    'connections' => [
        'mysql' => [
            'driver' => 'mysql',
            'url' => env('DATABASE_URL'),
            'host' => env('DB_HOST', '127.0.0.1'),
            'port' => env('DB_PORT', '3306'),
            'database' => env('DB_DATABASE', 'laravel'),
            'username' => env('DB_USERNAME', 'root'),
            'password' => env('DB_PASSWORD', ''),
        ],
    ],
];
"#;

fn log_fixture_env() -> String {
    format!(
        "DB_CONNECTION=mysql\nDB_HOST={LOG_PLAIN_HOST}\nDB_DATABASE=laravel\n\
         DB_USERNAME=sail\nDB_PASSWORD={LOG_SECRET}\nMAIL_API_TOKEN={LOG_TOKEN}\n"
    )
}

thread_local! {
    /// Everything logged on this thread while a capture is running. `None`
    /// means this thread is not capturing, and the subscriber's output is
    /// dropped — which is what every other test in the binary wants.
    static CAPTURED_LOG: std::cell::RefCell<Option<Vec<u8>>> =
        const { std::cell::RefCell::new(None) };
}

/// The writer behind the one subscriber the test binary installs. It routes
/// each event to the capturing thread's buffer, or to nowhere.
#[derive(Clone, Copy, Default)]
struct ThreadLocalLogWriter;

impl std::io::Write for ThreadLocalLogWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        CAPTURED_LOG.with(|slot| {
            if let Some(buffer) = slot.borrow_mut().as_mut() {
                buffer.extend_from_slice(bytes);
            }
        });
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for ThreadLocalLogWriter {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        *self
    }
}

/// Run `f` and return its result plus everything it logged.
///
/// The subscriber is **global and installed once**, rather than scoped per
/// call with `tracing::subscriber::with_default`. Scoping looks tidier and is
/// wrong here: `tracing` caches each callsite's `Interest` the first time that
/// callsite is reached, so an ordinary test touching `resolve_env` on another
/// thread — with no subscriber in place — caches "never" for that macro and
/// every later capture silently misses it. That is not theory: it turned this
/// suite red on one CI runner out of three while passing locally, with the
/// resolver's own lines absent from a capture that held its neighbours.
///
/// A global subscriber makes every callsite register against a real subscriber
/// (and `set_global_default` rebuilds the interest cache), so the answer is
/// "yes" for good. Isolation moves to the buffer, which is per-thread: a test
/// that is not capturing writes into `None` and drops its output, exactly as it
/// did when no subscriber existed at all.
fn capture_logs<T>(f: impl FnOnce() -> T) -> (T, String) {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| {
        let subscriber = tracing_subscriber::fmt()
            .with_writer(ThreadLocalLogWriter)
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .finish();
        tracing::subscriber::set_global_default(subscriber)
            .expect("no other global subscriber in the test binary");
    });

    CAPTURED_LOG.with(|slot| *slot.borrow_mut() = Some(Vec::new()));
    let result = f();
    let captured = CAPTURED_LOG.with(|slot| {
        String::from_utf8(slot.borrow_mut().take().unwrap_or_default())
            .expect("log output is utf-8")
    });
    (result, captured)
}

#[test]
fn parsing_the_database_config_never_logs_the_dotenv_password() {
    let dir = TempDir::new().unwrap();
    write_config_php(dir.path(), CONFIG_VAR_FORM);
    write(dir.path(), ".env", &log_fixture_env());
    let provider = DatabaseSchemaProvider::new(dir.path().to_path_buf());

    let (config, logs) = capture_logs(|| provider.parse_database_config().expect("config parsed"));

    assert_eq!(
        config.password, LOG_SECRET,
        "the parse still resolves the real password — masking is a log concern only"
    );
    assert!(
        !logs.contains(LOG_SECRET),
        "the .env password reached the log in plaintext:\n{logs}"
    );
    // Pinned to the line that owns the value. A bare `(set)` search would pass
    // on the neighbouring `password: (set)` summary, which was already masked
    // before this change and proves nothing about these sites.
    assert!(
        logs.contains("resolved from .env: (set)"),
        "the resolver's info line carries the masked rendering:\n{logs}"
    );
    assert!(
        logs.contains(r#"resolve_env(DB_PASSWORD): Some("(set)")"#),
        "the reader's debug line carries the masked rendering:\n{logs}"
    );
    assert!(
        logs.contains(LOG_PLAIN_HOST),
        "an ordinary DB_HOST value is still logged in full:\n{logs}"
    );
}

/// The name gate's blind spot, closed by the shape gate.
///
/// `DATABASE_URL` matches no sensitive segment, so `mask_env_value_for_log`
/// takes its *unmatched* arm — which is why that arm is `mask_url_credentials`
/// and not the raw value. The resolver's `info!` fires under the default
/// `EnvFilter("info,salsa=warn")`, so an unmasked password here is on screen in
/// Zed's log panel out of the box.
#[test]
fn parsing_the_database_config_never_logs_a_credential_inside_a_url_value() {
    let dir = TempDir::new().unwrap();
    write_config_php(dir.path(), CONFIG_WITH_URL);
    write(
        dir.path(),
        ".env",
        &format!(
            "{}DATABASE_URL={}\n",
            log_fixture_env(),
            log_fixture_db_url()
        ),
    );
    let provider = DatabaseSchemaProvider::new(dir.path().to_path_buf());

    let (config, logs) = capture_logs(|| provider.parse_database_config().expect("config parsed"));

    assert_eq!(
        config.url.as_deref(),
        Some(log_fixture_db_url().as_str()),
        "the parse still hands the driver the real URL — masking is a display concern only"
    );
    for secret in [LOG_URL_SECRET, LOG_URL_SECRET_TAIL] {
        assert!(
            !logs.contains(secret),
            "the password inside DATABASE_URL reached the log in plaintext:\n{logs}"
        );
    }
    // Two lines print this value, and both must mask it: the resolver's
    // `resolved from .env:` line and the config summary's `url:` line. Pinned
    // separately, because masking one and not its sibling is precisely the
    // shape this fix exists to close.
    assert!(
        logs.contains(&format!(
            "resolved from .env: {}",
            log_fixture_db_url_masked()
        )),
        "the resolver's line must carry the masked URL, not nothing and not the secret:\n{logs}"
    );
    assert!(
        logs.contains(&format!("url: {}", log_fixture_db_url_masked())),
        "the config summary's url line must carry the masked URL:\n{logs}"
    );
    // The unmatched control still logs in full, so the fix did not buy safety
    // by blanking the diagnostic.
    assert!(
        logs.contains(&format!("host: {LOG_PLAIN_HOST}")),
        "an ordinary DB_HOST value is still logged in full:\n{logs}"
    );
}

/// The same gate on the reader itself, one `RUST_LOG=debug` away from the
/// default filter — the level an ordinary troubleshooting session turns on, and
/// whose output gets pasted into bug reports.
#[test]
fn resolve_env_masks_a_credential_inside_a_url_value_in_its_debug_log() {
    // Both routes through the shape gate. `DATABASE_URL` parses with an
    // authority and `url` reports its password; `JDBC_URL` parses to an opaque
    // path with no authority, where `url` reports none however many the value
    // holds. Masking the first says nothing about the second.
    for (name, value, masked, secrets) in [
        (
            "DATABASE_URL",
            log_fixture_db_url(),
            log_fixture_db_url_masked(),
            [LOG_URL_SECRET, LOG_URL_SECRET_TAIL],
        ),
        (
            "JDBC_URL",
            log_fixture_jdbc_url(),
            log_fixture_jdbc_url_masked(),
            [LOG_JDBC_SECRET, LOG_JDBC_SECRET_TAIL],
        ),
    ] {
        let dir = TempDir::new().unwrap();
        write(dir.path(), ".env", &format!("{name}={value}\n"));
        let provider = DatabaseSchemaProvider::new(dir.path().to_path_buf());

        let (resolved, logs) = capture_logs(|| provider.resolve_env(name));

        assert_eq!(
            resolved.as_deref(),
            Some(value.as_str()),
            "caller still gets the real URL"
        );
        for secret in secrets {
            assert!(
                !logs.contains(secret),
                "the password inside {name} reached the debug log in plaintext:\n{logs}"
            );
        }
        assert!(
            logs.contains(&format!(r#"resolve_env({name}): Some("{masked}")"#)),
            "the debug line must carry the masked URL:\n{logs}"
        );
    }
}

#[test]
fn resolve_env_masks_every_matched_keyword_category_in_its_debug_log() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), ".env", &log_fixture_env());
    let provider = DatabaseSchemaProvider::new(dir.path().to_path_buf());

    let ((password, token, host), logs) = capture_logs(|| {
        (
            provider.resolve_env("DB_PASSWORD"),
            provider.resolve_env("MAIL_API_TOKEN"),
            provider.resolve_env("DB_HOST"),
        )
    });

    assert_eq!(
        password.as_deref(),
        Some(LOG_SECRET),
        "caller still gets it"
    );
    assert_eq!(token.as_deref(), Some(LOG_TOKEN), "caller still gets it");
    assert_eq!(
        host.as_deref(),
        Some(LOG_PLAIN_HOST),
        "caller still gets it"
    );

    assert!(
        !logs.contains(LOG_SECRET) && !logs.contains(LOG_TOKEN),
        "a secret-named value reached the debug log in plaintext:\n{logs}"
    );
    assert!(
        logs.contains(r#"resolve_env(DB_PASSWORD): Some("(set)")"#),
        "PASSWORD is masked:\n{logs}"
    );
    assert!(
        logs.contains(r#"resolve_env(MAIL_API_TOKEN): Some("(set)")"#),
        "TOKEN is masked by the same gate, not by a PASSWORD-only check:\n{logs}"
    );
    assert!(
        logs.contains(&format!(
            r#"resolve_env(DB_HOST): Some("{LOG_PLAIN_HOST}")"#
        )),
        "an unmatched name still logs its value in full:\n{logs}"
    );
}

#[test]
fn parse_env_setting_masks_by_the_env_var_name_not_the_config_key() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), ".env", &log_fixture_env());
    let provider = DatabaseSchemaProvider::new(dir.path().to_path_buf());
    // `host` is an innocuous config key fed by a secret-named variable, and
    // `password` a secret-sounding key fed by an ordinary one: the gate reads
    // the env var's name, which is the only name the policy is defined over.
    let block = "'host' => env('MAIL_API_TOKEN', '127.0.0.1'),\n\
                 'password' => env('DB_HOST', ''),";

    let ((host, password), logs) = capture_logs(|| {
        (
            provider.parse_env_setting(block, "host", "127.0.0.1"),
            provider.parse_env_setting(block, "password", ""),
        )
    });

    assert_eq!(host, LOG_TOKEN, "the resolved value is unchanged");
    assert_eq!(password, LOG_PLAIN_HOST, "the resolved value is unchanged");
    assert!(
        !logs.contains(LOG_TOKEN),
        "the secret-named value leaked through an innocuous config key:\n{logs}"
    );
    assert!(
        logs.contains("resolved from .env: (set)"),
        "the secret-named value is masked:\n{logs}"
    );
    assert!(
        logs.contains(&format!("resolved from .env: {LOG_PLAIN_HOST}")),
        "the ordinary value is logged in full even under a `password` key:\n{logs}"
    );
}
