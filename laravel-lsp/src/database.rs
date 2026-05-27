//! Database Schema Provider for Laravel Validation Rules
//!
//! Provides database schema information (tables and columns) for
//! `exists:` and `unique:` validation rule autocomplete.

use regex::Regex;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Mask the password in a database URL for safe logging. Matches the
/// standard shape `driver://user:pass@host:...` and replaces the password
/// segment with `***`. If no password is present (or the URL doesn't match
/// the expected shape), returns the input unchanged.
///
/// This is best-effort — failing gracefully is safer than failing hard,
/// since logging shouldn't crash the LSP.
fn mask_url_password(url: &str) -> String {
    // Find the `://` separator, then the `@` that ends the credentials.
    let Some(scheme_end) = url.find("://") else {
        return url.to_string();
    };
    let creds_start = scheme_end + 3;
    let Some(at_offset) = url[creds_start..].find('@') else {
        return url.to_string();
    };
    let creds_end = creds_start + at_offset;
    let creds = &url[creds_start..creds_end];
    // Credentials are `user[:password]`. Only mask if there's a `:`.
    let Some(colon_offset) = creds.find(':') else {
        return url.to_string();
    };
    let user_end = creds_start + colon_offset;
    let mut masked = String::with_capacity(url.len());
    masked.push_str(&url[..user_end + 1]); // up to and including the `:`
    masked.push_str("***");
    masked.push_str(&url[creds_end..]); // from `@` onwards
    masked
}

/// One thing the connector should attempt: a URL to connect with, a short
/// human-readable label for logs, and an optional explanatory note shown
/// when this candidate is the one that finally succeeded (after earlier
/// ones failed). The label MUST mask any sensitive bits — the full URL is
/// in `url` for the driver, never logged directly.
#[derive(Debug, Clone)]
struct ConnCandidate {
    label: String,
    url: String,
    success_note: Option<String>,
}

/// Cached database schema with expiration
#[derive(Debug, Clone)]
pub struct DatabaseSchema {
    /// List of table names
    pub tables: Vec<String>,
    /// Map of table name to column names
    pub columns: HashMap<String, Vec<String>>,
    /// Map of table name to columns with types (column_name, php_type)
    pub columns_with_types: HashMap<String, Vec<(String, String)>>,
    /// When the cache was last updated
    pub cached_at: Instant,
}

impl DatabaseSchema {
    /// Check if the cache is still valid (default 60 seconds)
    pub fn is_valid(&self) -> bool {
        self.cached_at.elapsed() < Duration::from_secs(60)
    }
}

/// Database connection configuration. Mirrors the keys Laravel's default
/// `config/database.php` exposes for the active connection driver. The
/// LSP reads each key with the same `env(NAME, DEFAULT)` fallback chain
/// Laravel itself uses, so even projects that haven't populated `.env`
/// at all (relying purely on config defaults) connect correctly.
#[derive(Debug, Clone)]
pub struct DatabaseConfig {
    pub driver: String,
    pub host: String,
    pub port: u16,
    pub database: String,
    pub username: String,
    pub password: String,
    /// A full database URL like `mysql://user:pass@host:port/db`. When
    /// present, it takes precedence over the individual host/port/etc.
    /// fields. Laravel's `DB_URL` env var maps here.
    pub url: Option<String>,
    /// Unix socket path (e.g., `/tmp/mysql.sock`, `/var/run/mysqld/mysqld.sock`).
    /// Common on Mac local dev where MySQL/Postgres expose a socket alongside
    /// TCP. When set, drivers should prefer socket over TCP.
    pub unix_socket: Option<String>,
    /// Connection charset (MySQL/Postgres). Defaults to `utf8mb4` for MySQL.
    pub charset: Option<String>,
    /// Connection collation (MySQL). Defaults to `utf8mb4_unicode_ci`.
    pub collation: Option<String>,
}

/// Database connection error information
#[derive(Debug, Clone)]
pub struct DatabaseConnectionError {
    pub message: String,
    pub driver: String,
}

/// Database schema provider with caching
pub struct DatabaseSchemaProvider {
    /// Project root path
    project_root: PathBuf,
    /// Cached schema
    schema_cache: Arc<RwLock<Option<DatabaseSchema>>>,
    /// Cached database config
    config_cache: Arc<RwLock<Option<DatabaseConfig>>>,
    /// Last connection error (if any)
    last_error: Arc<RwLock<Option<DatabaseConnectionError>>>,
    /// Whether we've attempted a connection
    connection_attempted: Arc<RwLock<bool>>,
}

impl DatabaseSchemaProvider {
    /// Create a new schema provider for the given project root
    pub fn new(project_root: PathBuf) -> Self {
        Self {
            project_root,
            schema_cache: Arc::new(RwLock::new(None)),
            config_cache: Arc::new(RwLock::new(None)),
            last_error: Arc::new(RwLock::new(None)),
            connection_attempted: Arc::new(RwLock::new(false)),
        }
    }

    /// Get the last connection error, if any
    pub async fn get_last_error(&self) -> Option<DatabaseConnectionError> {
        self.last_error.read().await.clone()
    }

    /// Check if a connection has been attempted
    pub async fn was_connection_attempted(&self) -> bool {
        *self.connection_attempted.read().await
    }

    /// Set connection error
    async fn set_error(&self, driver: &str, message: &str) {
        *self.last_error.write().await = Some(DatabaseConnectionError {
            message: message.to_string(),
            driver: driver.to_string(),
        });
    }

    /// Clear connection error
    async fn clear_error(&self) {
        *self.last_error.write().await = None;
    }

    /// Get database schema, using cache if valid
    pub async fn get_schema(&self) -> Option<DatabaseSchema> {
        // Check cache first
        {
            let cache = self.schema_cache.read().await;
            if let Some(ref schema) = *cache {
                if schema.is_valid() {
                    debug!("Using cached database schema");
                    return Some(schema.clone());
                }
            }
        }

        // Cache miss or expired, fetch fresh schema
        info!("Fetching fresh database schema");
        let schema = self.fetch_schema().await?;

        // Update cache
        {
            let mut cache = self.schema_cache.write().await;
            *cache = Some(schema.clone());
        }

        Some(schema)
    }

    /// Get list of table names
    pub async fn get_tables(&self) -> Vec<String> {
        self.get_schema()
            .await
            .map(|s| s.tables)
            .unwrap_or_default()
    }

    /// Get columns for a specific table
    pub async fn get_columns(&self, table: &str) -> Vec<String> {
        self.get_schema()
            .await
            .and_then(|s| s.columns.get(table).cloned())
            .unwrap_or_default()
    }

    /// Get columns with their PHP types for a specific table
    /// Returns Vec<(column_name, php_type)>
    pub async fn get_columns_with_types(&self, table: &str) -> Vec<(String, String)> {
        self.get_schema()
            .await
            .and_then(|s| s.columns_with_types.get(table).cloned())
            .unwrap_or_default()
    }

    /// Map SQL data type to PHP type
    /// Note: Without casts, Eloquent returns database values as-is
    /// Dates are strings unless cast, JSON is a string unless cast
    fn map_sql_type_to_php(sql_type: &str) -> String {
        let sql_lower = sql_type.to_lowercase();

        // Integer types
        if sql_lower.contains("int")
            || sql_lower.contains("serial")
            || sql_lower == "integer"
            || sql_lower == "smallint"
            || sql_lower == "bigint"
        {
            return "int".to_string();
        }

        // Float/decimal types
        if sql_lower.contains("float")
            || sql_lower.contains("double")
            || sql_lower.contains("decimal")
            || sql_lower.contains("numeric")
            || sql_lower.contains("real")
            || sql_lower.contains("money")
        {
            return "float".to_string();
        }

        // Boolean (PostgreSQL only - MySQL tinyint(1) is still int without cast)
        if sql_lower == "boolean" || sql_lower == "bool" {
            return "bool".to_string();
        }

        // Everything else is a string in PHP without casts:
        // - varchar, text, char
        // - datetime, timestamp, date, time (strings unless cast to Carbon)
        // - json, jsonb (strings unless cast to array)
        // - blob, binary
        // - enum, set
        "string".to_string()
    }

    /// Get all available database connection names from config/database.php
    pub fn get_connections(&self) -> Vec<String> {
        let config_path = self.project_root.join("config/database.php");

        let content = match std::fs::read_to_string(&config_path) {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };

        // Find the 'connections' => [ block
        let connections_regex = match Regex::new(r#"['"]connections['"]\s*=>\s*\["#) {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };

        let match_start = match connections_regex.find(&content) {
            Some(m) => m.end(),
            None => return Vec::new(),
        };

        // Find all connection names: 'name' => [
        let connection_name_regex =
            match Regex::new(r#"['"]([a-zA-Z_][a-zA-Z0-9_]*)['"]\s*=>\s*\["#) {
                Ok(r) => r,
                Err(_) => return Vec::new(),
            };

        let remaining = &content[match_start..];

        // Find the end of the connections block (matching bracket)
        let mut depth = 1;
        let mut end_pos = remaining.len();
        for (i, c) in remaining.chars().enumerate() {
            match c {
                '[' => depth += 1,
                ']' => {
                    depth -= 1;
                    if depth == 0 {
                        end_pos = i;
                        break;
                    }
                }
                _ => {}
            }
        }

        let connections_block = &remaining[..end_pos];

        // Extract connection names
        connection_name_regex
            .captures_iter(connections_block)
            .filter_map(|caps| caps.get(1).map(|m| m.as_str().to_string()))
            .collect()
    }

    /// Invalidate the cache (call when migrations change)
    pub async fn invalidate_cache(&self) {
        let mut cache = self.schema_cache.write().await;
        *cache = None;
        info!("Database schema cache invalidated");
    }

    /// Fetch fresh schema from database
    async fn fetch_schema(&self) -> Option<DatabaseSchema> {
        // Mark that we've attempted a connection
        *self.connection_attempted.write().await = true;

        let config = match self.get_database_config().await {
            Some(c) => c,
            None => {
                self.set_error("unknown", "Database configuration not found in .env")
                    .await;
                return None;
            }
        };

        let result = match config.driver.as_str() {
            "mysql" | "mariadb" => self.fetch_mysql_schema(&config).await,
            "pgsql" | "postgres" => self.fetch_postgres_schema(&config).await,
            "sqlite" => self.fetch_sqlite_schema(&config).await,
            "sqlsrv" => self.fetch_sqlserver_schema(&config).await,
            _ => {
                self.set_error(
                    &config.driver,
                    &format!("Unsupported database driver: {}", config.driver),
                )
                .await;
                warn!("Unsupported database driver: {}", config.driver);
                return None;
            }
        };

        if result.is_some() {
            self.clear_error().await;
        }

        result
    }

    /// Get database configuration from Laravel config
    pub async fn get_database_config(&self) -> Option<DatabaseConfig> {
        // Check cache first
        {
            let cache = self.config_cache.read().await;
            if cache.is_some() {
                return cache.clone();
            }
        }

        // Parse config/database.php
        let config = self.parse_database_config()?;

        // Update cache
        {
            let mut cache = self.config_cache.write().await;
            *cache = Some(config.clone());
        }

        Some(config)
    }

    /// Parse config/database.php to extract connection settings
    ///
    /// This properly parses the Laravel config file:
    /// 1. Find 'default' => env('DB_CONNECTION', 'fallback') to get connection name
    /// 2. Find the connection block for that driver
    /// 3. Parse env('VAR', 'default') patterns from the connection block
    /// 4. Resolve env vars from .env, falling back to parsed defaults
    fn parse_database_config(&self) -> Option<DatabaseConfig> {
        let config_path = self.project_root.join("config/database.php");
        info!("🗄️  Parsing database config from: {:?}", config_path);

        let content = match std::fs::read_to_string(&config_path) {
            Ok(c) => c,
            Err(e) => {
                warn!("🗄️  Failed to read config/database.php: {}", e);
                return None;
            }
        };

        // Step 1: Parse 'default' => env('DB_CONNECTION', 'fallback')
        let default_regex = Regex::new(
            r#"['"]default['"]\s*=>\s*env\s*\(\s*['"]([^'"]+)['"]\s*,\s*['"]([^'"]+)['"]\s*\)"#,
        )
        .ok()?;

        let (default_env_var, default_fallback) = default_regex
            .captures(&content)
            .map(|caps| {
                let var = caps
                    .get(1)
                    .map(|m| m.as_str().to_string())
                    .unwrap_or_else(|| "DB_CONNECTION".to_string());
                let fallback = caps
                    .get(2)
                    .map(|m| m.as_str().to_string())
                    .unwrap_or_else(|| "mysql".to_string());
                (var, fallback)
            })
            .unwrap_or_else(|| ("DB_CONNECTION".to_string(), "mysql".to_string()));

        info!(
            "🗄️  default => env('{}', '{}')",
            default_env_var, default_fallback
        );

        // Resolve the default connection name
        let driver = self
            .resolve_env(&default_env_var)
            .unwrap_or(default_fallback.clone());
        info!(
            "🗄️  Resolved driver: {} (from .env: {}, fallback: {})",
            driver,
            self.resolve_env(&default_env_var).is_some(),
            default_fallback
        );

        // Step 2: Find the connection block for this driver
        // Pattern: 'driver_name' => [...]
        let connection_block = self.extract_connection_block(&content, &driver);

        if connection_block.is_none() {
            warn!("🗄️  Could not find connection block for driver: {}", driver);
        }

        let block = connection_block.unwrap_or_default();
        info!("🗄️  Found connection block ({} chars)", block.len());

        // Step 3: Parse settings from the connection block. Each call honors
        // Laravel's `env(NAME, DEFAULT)` chain: if the env var is set we use
        // it, otherwise the default in `config/database.php`, otherwise the
        // hard-coded fallback below.
        let host = self.parse_env_setting(&block, "host", "127.0.0.1");
        let port_str =
            self.parse_env_setting(&block, "port", &self.default_port(&driver).to_string());
        let port = port_str.parse().unwrap_or(self.default_port(&driver));
        let database = self.parse_env_setting(&block, "database", "laravel");
        let username = self.parse_env_setting(&block, "username", "root");
        let password = self.parse_env_setting(&block, "password", "");

        // Optional / less common settings. Empty / unset → None so the
        // connection logic can skip them rather than send empty strings.
        let url = self.parse_optional_setting(&block, "url");
        let unix_socket = self.parse_optional_setting(&block, "unix_socket");
        let charset = self.parse_optional_setting(&block, "charset");
        let collation = self.parse_optional_setting(&block, "collation");

        info!("🗄️  Parsed database config:");
        info!("🗄️    driver: {}", driver);
        info!("🗄️    host: {}", host);
        info!("🗄️    port: {}", port);
        info!("🗄️    database: {}", database);
        info!("🗄️    username: {}", username);
        info!(
            "🗄️    password: {}",
            if password.is_empty() {
                "(empty)"
            } else {
                "(set)"
            }
        );
        if let Some(u) = &url {
            // Mask the password in the URL when logging — common shape is
            // `driver://user:pass@host:port/db`. Best-effort, fail-open.
            info!("🗄️    url: {}", mask_url_password(u));
        }
        if let Some(s) = &unix_socket {
            info!("🗄️    unix_socket: {}", s);
        }
        if let Some(c) = &charset {
            info!("🗄️    charset: {}", c);
        }
        if let Some(c) = &collation {
            info!("🗄️    collation: {}", c);
        }

        // For SQLite, check if file exists
        if driver == "sqlite" {
            let db_path = if database.starts_with('/') {
                std::path::PathBuf::from(&database)
            } else {
                self.project_root.join(&database)
            };
            info!(
                "🗄️    SQLite path resolved to: {:?} (exists: {})",
                db_path,
                db_path.exists()
            );
        }

        Some(DatabaseConfig {
            driver,
            host,
            port,
            database,
            username,
            password,
            url,
            unix_socket,
            charset,
            collation,
        })
    }

    /// Extract the connection block for a specific driver from config/database.php
    fn extract_connection_block(&self, content: &str, driver: &str) -> Option<String> {
        // Find 'driver_name' => [
        let pattern = format!(r#"['"]{driver}['"]\s*=>\s*\["#);
        let regex = Regex::new(&pattern).ok()?;

        let match_start = regex.find(content)?.end();

        // Find the matching closing bracket
        let remaining = &content[match_start..];
        let mut depth = 1;
        let mut end_pos = 0;

        for (i, c) in remaining.chars().enumerate() {
            match c {
                '[' => depth += 1,
                ']' => {
                    depth -= 1;
                    if depth == 0 {
                        end_pos = i;
                        break;
                    }
                }
                _ => {}
            }
        }

        if end_pos > 0 {
            Some(remaining[..end_pos].to_string())
        } else {
            None
        }
    }

    /// Parse an optional setting from the connection block. Same env() chain
    /// as [`Self::parse_env_setting`] but returns `None` when the resolved
    /// value is empty (no env, empty default, or empty string literal). Use
    /// for settings that shouldn't be passed to the driver when missing —
    /// e.g., empty `unix_socket` should NOT trigger socket-mode.
    fn parse_optional_setting(&self, block: &str, key: &str) -> Option<String> {
        let value = self.parse_env_setting(block, key, "");
        if value.is_empty() {
            None
        } else {
            Some(value)
        }
    }

    /// Parse an env() setting from a connection block
    /// Handles: 'key' => env('VAR', 'default') or 'key' => env('VAR', default_func())
    fn parse_env_setting(&self, block: &str, key: &str, fallback: &str) -> String {
        // First find 'key' => env(
        let key_pattern = format!(r#"['"]{key}['"]\s*=>\s*env\s*\("#);

        if let Ok(key_regex) = Regex::new(&key_pattern) {
            if let Some(key_match) = key_regex.find(block) {
                // Found the start of env(), now extract the contents with balanced parens
                let after_env = &block[key_match.end()..];

                if let Some((env_var, default_value)) = self.extract_env_args(after_env) {
                    info!("🗄️    {} => env('{}', {})", key, env_var, default_value);

                    // Try to resolve from .env first
                    if let Some(env_value) = self.resolve_env(&env_var) {
                        info!("🗄️      → resolved from .env: {}", env_value);
                        return env_value;
                    }

                    // Fall back to the default from config
                    let resolved_default = self.resolve_php_value(&default_value);
                    info!(
                        "🗄️      → using default: {} → {}",
                        default_value, resolved_default
                    );
                    return resolved_default;
                }
            }
        }

        // Key not found in block, return the fallback
        info!(
            "🗄️    {} not found in block, using fallback: {}",
            key, fallback
        );
        fallback.to_string()
    }

    /// Extract env() arguments handling nested parentheses
    /// Input: "'VAR', default_func('arg'))" - everything after "env("
    /// Returns: (env_var, default_value)
    fn extract_env_args(&self, input: &str) -> Option<(String, String)> {
        let mut chars = input.chars().peekable();
        let mut env_var = String::new();
        let mut default_value = String::new();

        // Skip whitespace
        while chars.peek() == Some(&' ')
            || chars.peek() == Some(&'\n')
            || chars.peek() == Some(&'\t')
        {
            chars.next();
        }

        // Extract env var name (in quotes)
        let quote_char = chars.next()?;
        if quote_char != '\'' && quote_char != '"' {
            return None;
        }

        // Read until closing quote
        for c in chars.by_ref() {
            if c == quote_char {
                break;
            }
            env_var.push(c);
        }

        // Skip whitespace and comma
        while let Some(&c) = chars.peek() {
            if c == ' ' || c == '\n' || c == '\t' || c == ',' {
                chars.next();
            } else {
                break;
            }
        }

        // Check if there's a default value or just closing paren
        if chars.peek() == Some(&')') {
            // No default value
            return Some((env_var, String::new()));
        }

        // Extract default value with balanced parentheses
        let mut paren_depth = 0;
        for c in chars.by_ref() {
            match c {
                '(' => {
                    paren_depth += 1;
                    default_value.push(c);
                }
                ')' => {
                    if paren_depth == 0 {
                        // This is the closing paren of env()
                        break;
                    }
                    paren_depth -= 1;
                    default_value.push(c);
                }
                _ => default_value.push(c),
            }
        }

        Some((env_var, default_value.trim().to_string()))
    }

    /// Resolve PHP values/functions to actual values
    fn resolve_php_value(&self, value: &str) -> String {
        let trimmed = value.trim();

        // Handle string literals: 'value' or "value"
        if (trimmed.starts_with('\'') && trimmed.ends_with('\''))
            || (trimmed.starts_with('"') && trimmed.ends_with('"'))
        {
            return trimmed[1..trimmed.len() - 1].to_string();
        }

        // Handle database_path('file.sqlite') -> database/file.sqlite
        if let Some(caps) = Regex::new(r#"database_path\s*\(\s*['"]([^'"]+)['"]\s*\)"#)
            .ok()
            .and_then(|r| r.captures(trimmed))
        {
            let path = caps.get(1).map(|m| m.as_str()).unwrap_or("database.sqlite");
            return format!("database/{}", path);
        }

        // Handle storage_path('file') -> storage/file
        if let Some(caps) = Regex::new(r#"storage_path\s*\(\s*['"]([^'"]+)['"]\s*\)"#)
            .ok()
            .and_then(|r| r.captures(trimmed))
        {
            let path = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            return format!("storage/{}", path);
        }

        // Handle boolean true/false
        if trimmed == "true" {
            return "true".to_string();
        }
        if trimmed == "false" {
            return "false".to_string();
        }

        // Handle null
        if trimmed == "null" {
            return String::new();
        }

        // Handle numeric values
        if trimmed.parse::<i64>().is_ok() || trimmed.parse::<f64>().is_ok() {
            return trimmed.to_string();
        }

        // Unknown, return as-is (stripped of quotes if any)
        trimmed.trim_matches(|c| c == '\'' || c == '"').to_string()
    }

    /// Get default port for a database driver
    fn default_port(&self, driver: &str) -> u16 {
        match driver {
            "mysql" | "mariadb" => 3306,
            "pgsql" | "postgres" => 5432,
            "sqlsrv" => 1433,
            _ => 3306,
        }
    }

    /// Resolve an environment variable from .env file
    fn resolve_env(&self, key: &str) -> Option<String> {
        let env_path = self.project_root.join(".env");
        let content = match std::fs::read_to_string(&env_path) {
            Ok(c) => c,
            Err(e) => {
                debug!("🗄️  resolve_env({}): Failed to read .env: {}", key, e);
                return None;
            }
        };

        // Pattern: KEY=value or KEY="value" or KEY='value'
        let pattern = format!(r#"(?m)^{}\s*=\s*['"]?([^'"\n]*)['"]?"#, regex::escape(key));
        let regex = match Regex::new(&pattern) {
            Ok(r) => r,
            Err(e) => {
                debug!("🗄️  resolve_env({}): Invalid regex: {}", key, e);
                return None;
            }
        };

        let result = regex
            .captures(&content)
            .and_then(|caps| caps.get(1))
            .map(|m| m.as_str().trim().to_string())
            .filter(|s| !s.is_empty());

        debug!("🗄️  resolve_env({}): {:?}", key, result);
        result
    }

    /// Fetch schema from MySQL/MariaDB. Tries connection candidates in
    /// priority order:
    /// 1. **`DB_URL`** — managed cloud providers (Heroku, Render, AWS)
    ///    deliver a full connection string. When set, this overrides
    ///    everything else, exactly as Laravel's `ConfigurationUrlParser`
    ///    does.
    /// 2. **`unix_socket`** — common on Mac local dev (Homebrew MySQL),
    ///    where the daemon exposes both TCP and a `.sock` file.
    /// 3. **TCP** with the configured host, plus a `127.0.0.1` fallback
    ///    for Sail / Docker Compose setups where the configured host is a
    ///    container service name unresolvable from outside Docker.
    async fn fetch_mysql_schema(&self, config: &DatabaseConfig) -> Option<DatabaseSchema> {
        use sqlx::mysql::MySqlPoolOptions;
        use sqlx::Row;

        let candidates = self.build_mysql_candidates(config);
        let mut last_err: Option<String> = None;
        let mut pool_opt = None;
        let primary_label = candidates.first().map(|c| c.label.clone());

        for cand in &candidates {
            match MySqlPoolOptions::new()
                .max_connections(1)
                .acquire_timeout(Duration::from_secs(5))
                .connect(&cand.url)
                .await
            {
                Ok(p) => {
                    if Some(&cand.label) != primary_label.as_ref() {
                        info!(
                            "MySQL: connected via fallback '{}' (primary '{}' failed). \
                             {}",
                            cand.label,
                            primary_label.as_deref().unwrap_or("?"),
                            cand.success_note.as_deref().unwrap_or("")
                        );
                    } else {
                        info!("MySQL: connected via {}", cand.label);
                    }
                    pool_opt = Some(p);
                    break;
                }
                Err(e) => {
                    if candidates.len() > 1 && Some(&cand.label) == primary_label.as_ref() {
                        info!(
                            "MySQL: primary candidate '{}' didn't connect ({}). Trying fallback...",
                            cand.label, e
                        );
                    }
                    last_err = Some(format!("{}: {}", cand.label, e));
                }
            }
        }

        let pool = match pool_opt {
            Some(p) => p,
            None => {
                let msg = format!(
                    "MySQL connection failed. Tried candidates: [{}]. Last error: {}. \
                     Check DB_URL / DB_HOST / DB_PORT / DB_DATABASE / DB_USERNAME / DB_PASSWORD / \
                     DB_SOCKET in .env. If using Sail/Docker Compose, ensure the container is \
                     running and the port is mapped to your host (run `./vendor/bin/sail up -d`).",
                    candidates
                        .iter()
                        .map(|c| c.label.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    last_err.unwrap_or_else(|| "(no error captured)".to_string())
                );
                warn!("{}", msg);
                self.set_error("mysql", &msg).await;
                return None;
            }
        };

        // Diagnostic identity probe — when SHOW TABLES returns 0 rows but
        // the user knows the DB has tables, the connection probably landed
        // on the wrong MySQL instance (e.g., Homebrew MySQL on 127.0.0.1:3306
        // intercepting before Sail). Log the server identity so the user can
        // see what they're actually connected to.
        //
        // Use match (not if-let) so any error from these probe queries gets
        // surfaced — silent failure here is what prevented the previous
        // diagnostic round from telling us anything.
        match sqlx::query(
            "SELECT DATABASE() AS db, @@hostname AS hostname, USER() AS user, @@version AS version",
        )
        .fetch_one(&pool)
        .await
        {
            Ok(row) => {
                let db_name: String = row.try_get("db").unwrap_or_default();
                let hostname: String = row.try_get("hostname").unwrap_or_default();
                let user: String = row.try_get("user").unwrap_or_default();
                let version: String = row.try_get("version").unwrap_or_default();
                info!(
                    "MySQL probe — db={:?} server_hostname={:?} user={:?} version={:?}",
                    db_name, hostname, user, version
                );
            }
            Err(e) => {
                warn!("MySQL probe (identity query) failed: {}", e);
            }
        }
        match sqlx::query("SHOW DATABASES").fetch_all(&pool).await {
            Ok(rows) => {
                let dbs: Vec<String> = rows
                    .into_iter()
                    .filter_map(|r| r.try_get::<String, _>(0).ok())
                    .collect();
                info!("MySQL probe — visible databases = {:?}", dbs);
            }
            Err(e) => {
                warn!("MySQL probe (SHOW DATABASES) failed: {}", e);
            }
        }

        // Get tables
        let tables: Vec<String> = sqlx::query("SHOW TABLES")
            .fetch_all(&pool)
            .await
            .ok()?
            .into_iter()
            .filter_map(|row| row.try_get::<String, _>(0).ok())
            .collect();

        // Get columns for each table (with types)
        let mut columns = HashMap::new();
        let mut columns_with_types = HashMap::new();
        for table in &tables {
            let rows = sqlx::query(&format!("SHOW COLUMNS FROM `{}`", table))
                .fetch_all(&pool)
                .await
                .ok()?;

            let mut col_names = Vec::new();
            let mut col_types = Vec::new();

            for row in rows {
                if let Ok(field) = row.try_get::<String, _>("Field") {
                    let sql_type = row.try_get::<String, _>("Type").unwrap_or_default();
                    let php_type = Self::map_sql_type_to_php(&sql_type);
                    col_names.push(field.clone());
                    col_types.push((field, php_type));
                }
            }

            columns.insert(table.clone(), col_names);
            columns_with_types.insert(table.clone(), col_types);
        }

        info!("MySQL schema loaded: {} tables", tables.len());

        Some(DatabaseSchema {
            tables,
            columns,
            columns_with_types,
            cached_at: Instant::now(),
        })
    }

    /// Build the ordered list of host candidates to try. The primary
    /// (configured) host is always first; if it looks like a Docker Compose
    /// service name (no dots, not `localhost`) we add `127.0.0.1` as a
    /// fallback so Sail / Docker Compose setups work without the LSP needing
    /// to be inside the Docker network.
    fn host_candidates(primary: &str) -> Vec<String> {
        let mut candidates = vec![primary.to_string()];
        if !primary.is_empty()
            && !primary.contains('.')
            && !primary.eq_ignore_ascii_case("localhost")
            && primary != "127.0.0.1"
        {
            candidates.push("127.0.0.1".to_string());
        }
        candidates
    }

    /// Build the ordered list of MySQL connection candidates. Priority:
    /// 1. `DB_URL` (full connection string, used by managed cloud providers)
    /// 2. `unix_socket` (local dev, e.g. Homebrew MySQL exposing `.sock`)
    /// 3. TCP via configured host + 127.0.0.1 Sail/Docker fallback
    ///
    /// All sources of credentials/database come from `config` — the URL/socket
    /// don't carry their own credentials; we splice them in.
    fn build_mysql_candidates(&self, config: &DatabaseConfig) -> Vec<ConnCandidate> {
        let mut out = Vec::new();

        if let Some(url) = &config.url {
            // Pass DB_URL through verbatim — managed providers (Heroku, Render,
            // AWS RDS proxy, etc.) bake credentials AND host into the URL and
            // expect the driver to honor it as-is.
            out.push(ConnCandidate {
                label: "DB_URL".to_string(),
                url: url.clone(),
                success_note: Some(
                    "Configured via DB_URL (typical for managed cloud providers).".to_string(),
                ),
            });
        }

        if let Some(socket) = &config.unix_socket {
            // sqlx-mysql honors the `socket` query parameter — point host at
            // `localhost` (ignored when socket is present, but required for
            // URL syntax) and tack the socket on. Real-world socket paths
            // (`/tmp/mysql.sock`, `/var/run/mysqld/mysqld.sock`) have no
            // characters that need URL-encoding, so we splice raw.
            out.push(ConnCandidate {
                label: format!("unix_socket={socket}"),
                url: format!(
                    "mysql://{}:{}@localhost/{}?socket={}",
                    config.username, config.password, config.database, socket
                ),
                success_note: Some(
                    "Configured via unix_socket — bypasses TCP entirely.".to_string(),
                ),
            });
        }

        // TCP candidates. Always added — these are the fallback path when
        // neither URL nor socket are configured, OR when those fail.
        for host in Self::host_candidates(&config.host) {
            let is_sail_fallback = host == "127.0.0.1" && host != config.host;
            out.push(ConnCandidate {
                label: format!("tcp {}:{}", host, config.port),
                url: format!(
                    "mysql://{}:{}@{}:{}/{}",
                    config.username, config.password, host, config.port, config.database
                ),
                success_note: if is_sail_fallback {
                    Some(
                        "Looks like a Sail / Docker Compose setup — the LSP runs outside Docker, \
                         so the service hostname doesn't work, but the mapped host port does."
                            .to_string(),
                    )
                } else {
                    None
                },
            });
        }

        out
    }

    /// Build the ordered list of PostgreSQL connection candidates. Same
    /// priority as MySQL: DB_URL → unix_socket → TCP with host fallback.
    fn build_postgres_candidates(&self, config: &DatabaseConfig) -> Vec<ConnCandidate> {
        let mut out = Vec::new();

        if let Some(url) = &config.url {
            out.push(ConnCandidate {
                label: "DB_URL".to_string(),
                url: url.clone(),
                success_note: Some("Configured via DB_URL.".to_string()),
            });
        }

        if let Some(socket) = &config.unix_socket {
            // libpq-style socket connection: `postgres://user:pass@/db?host=/path`.
            out.push(ConnCandidate {
                label: format!("unix_socket={socket}"),
                url: format!(
                    "postgres://{}:{}@/{}?host={}",
                    config.username, config.password, config.database, socket
                ),
                success_note: Some("Configured via unix_socket.".to_string()),
            });
        }

        for host in Self::host_candidates(&config.host) {
            let is_sail_fallback = host == "127.0.0.1" && host != config.host;
            out.push(ConnCandidate {
                label: format!("tcp {}:{}", host, config.port),
                url: format!(
                    "postgres://{}:{}@{}:{}/{}",
                    config.username, config.password, host, config.port, config.database
                ),
                success_note: if is_sail_fallback {
                    Some("Sail / Docker Compose fallback to 127.0.0.1.".to_string())
                } else {
                    None
                },
            });
        }

        out
    }

    /// Fetch schema from PostgreSQL. Same candidate priority as
    /// `fetch_mysql_schema`: DB_URL → unix_socket → TCP with Sail fallback.
    async fn fetch_postgres_schema(&self, config: &DatabaseConfig) -> Option<DatabaseSchema> {
        use sqlx::postgres::PgPoolOptions;
        use sqlx::Row;

        let candidates = self.build_postgres_candidates(config);
        let mut last_err: Option<String> = None;
        let mut pool_opt = None;
        let primary_label = candidates.first().map(|c| c.label.clone());

        for cand in &candidates {
            match PgPoolOptions::new()
                .max_connections(1)
                .acquire_timeout(Duration::from_secs(5))
                .connect(&cand.url)
                .await
            {
                Ok(p) => {
                    if Some(&cand.label) != primary_label.as_ref() {
                        info!(
                            "PostgreSQL: connected via fallback '{}' (primary '{}' failed). {}",
                            cand.label,
                            primary_label.as_deref().unwrap_or("?"),
                            cand.success_note.as_deref().unwrap_or("")
                        );
                    } else {
                        info!("PostgreSQL: connected via {}", cand.label);
                    }
                    pool_opt = Some(p);
                    break;
                }
                Err(e) => {
                    last_err = Some(format!("{}: {}", cand.label, e));
                }
            }
        }

        let pool = match pool_opt {
            Some(p) => p,
            None => {
                let msg = format!(
                    "PostgreSQL connection failed. Tried candidates: [{}]. Last error: {}. \
                     Check DB_URL / DB_HOST / DB_PORT / DB_DATABASE / DB_USERNAME / DB_PASSWORD / \
                     DB_SOCKET in .env. If using Sail/Docker Compose, ensure the container is \
                     running.",
                    candidates
                        .iter()
                        .map(|c| c.label.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    last_err.unwrap_or_else(|| "(no error captured)".to_string())
                );
                warn!("{}", msg);
                self.set_error("pgsql", &msg).await;
                return None;
            }
        };

        // Get tables from public schema
        let tables: Vec<String> = sqlx::query(
            "SELECT table_name FROM information_schema.tables WHERE table_schema = 'public'",
        )
        .fetch_all(&pool)
        .await
        .ok()?
        .into_iter()
        .filter_map(|row| row.try_get::<String, _>("table_name").ok())
        .collect();

        // Get columns for each table (with types)
        let mut columns = HashMap::new();
        let mut columns_with_types = HashMap::new();
        for table in &tables {
            let rows = sqlx::query(
                "SELECT column_name, data_type FROM information_schema.columns WHERE table_schema = 'public' AND table_name = $1"
            )
                .bind(table)
                .fetch_all(&pool)
                .await
                .ok()?;

            let mut col_names = Vec::new();
            let mut col_types = Vec::new();

            for row in rows {
                if let Ok(col_name) = row.try_get::<String, _>("column_name") {
                    let sql_type = row.try_get::<String, _>("data_type").unwrap_or_default();
                    let php_type = Self::map_sql_type_to_php(&sql_type);
                    col_names.push(col_name.clone());
                    col_types.push((col_name, php_type));
                }
            }

            columns.insert(table.clone(), col_names);
            columns_with_types.insert(table.clone(), col_types);
        }

        info!("PostgreSQL schema loaded: {} tables", tables.len());

        Some(DatabaseSchema {
            tables,
            columns,
            columns_with_types,
            cached_at: Instant::now(),
        })
    }

    /// Fetch schema from SQLite
    async fn fetch_sqlite_schema(&self, config: &DatabaseConfig) -> Option<DatabaseSchema> {
        use sqlx::sqlite::SqlitePoolOptions;
        use sqlx::Row;

        // SQLite database path - could be absolute or relative to project
        let db_path = if config.database.starts_with('/') {
            PathBuf::from(&config.database)
        } else {
            self.project_root.join(&config.database)
        };

        if !db_path.exists() {
            let msg = format!(
                "SQLite database not found: {:?}. Check DB_DATABASE in .env",
                db_path
            );
            warn!("{}", msg);
            self.set_error("sqlite", &msg).await;
            return None;
        }

        let url = format!("sqlite:{}", db_path.display());

        let pool = match SqlitePoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(5))
            .connect(&url)
            .await
        {
            Ok(p) => p,
            Err(e) => {
                let msg = format!("SQLite connection failed: {}. Check DB_DATABASE in .env", e);
                warn!("{}", msg);
                self.set_error("sqlite", &msg).await;
                return None;
            }
        };

        // Get tables
        let tables: Vec<String> = sqlx::query(
            "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
        )
        .fetch_all(&pool)
        .await
        .ok()?
        .into_iter()
        .filter_map(|row| row.try_get::<String, _>("name").ok())
        .collect();

        // Get columns for each table (with types)
        let mut columns = HashMap::new();
        let mut columns_with_types = HashMap::new();
        for table in &tables {
            let rows = sqlx::query(&format!("PRAGMA table_info('{}')", table))
                .fetch_all(&pool)
                .await
                .ok()?;

            let mut col_names = Vec::new();
            let mut col_types = Vec::new();

            for row in rows {
                if let Ok(col_name) = row.try_get::<String, _>("name") {
                    let sql_type = row.try_get::<String, _>("type").unwrap_or_default();
                    let php_type = Self::map_sql_type_to_php(&sql_type);
                    col_names.push(col_name.clone());
                    col_types.push((col_name, php_type));
                }
            }

            columns.insert(table.clone(), col_names);
            columns_with_types.insert(table.clone(), col_types);
        }

        info!("SQLite schema loaded: {} tables", tables.len());

        Some(DatabaseSchema {
            tables,
            columns,
            columns_with_types,
            cached_at: Instant::now(),
        })
    }

    /// Fetch schema from SQL Server
    async fn fetch_sqlserver_schema(&self, config: &DatabaseConfig) -> Option<DatabaseSchema> {
        use tiberius::{AuthMethod, Client, Config};
        use tokio::net::TcpStream;
        use tokio_util::compat::TokioAsyncWriteCompatExt;

        let mut tib_config = Config::new();
        tib_config.host(&config.host);
        tib_config.port(config.port);
        tib_config.database(&config.database);
        tib_config.authentication(AuthMethod::sql_server(&config.username, &config.password));
        tib_config.trust_cert();

        let tcp = match TcpStream::connect(tib_config.get_addr()).await {
            Ok(t) => t,
            Err(e) => {
                let msg = format!(
                    "SQL Server TCP connection failed: {}. Check DB_HOST, DB_PORT in .env",
                    e
                );
                warn!("{}", msg);
                self.set_error("sqlsrv", &msg).await;
                return None;
            }
        };

        tcp.set_nodelay(true).ok();

        let mut client = match Client::connect(tib_config, tcp.compat_write()).await {
            Ok(c) => c,
            Err(e) => {
                let msg = format!("SQL Server connection failed: {}. Check DB_DATABASE, DB_USERNAME, DB_PASSWORD in .env", e);
                warn!("{}", msg);
                self.set_error("sqlsrv", &msg).await;
                return None;
            }
        };

        // Get tables
        let stream = client
            .query(
                "SELECT TABLE_NAME FROM INFORMATION_SCHEMA.TABLES WHERE TABLE_TYPE = 'BASE TABLE'",
                &[],
            )
            .await
            .ok()?;

        let tables: Vec<String> = stream
            .into_first_result()
            .await
            .ok()?
            .into_iter()
            .filter_map(|row| row.get::<&str, _>("TABLE_NAME").map(|s| s.to_string()))
            .collect();

        // Get columns for each table (with types)
        let mut columns = HashMap::new();
        let mut columns_with_types = HashMap::new();
        for table in &tables {
            let stream = client.query(
                "SELECT COLUMN_NAME, DATA_TYPE FROM INFORMATION_SCHEMA.COLUMNS WHERE TABLE_NAME = @P1",
                &[&table.as_str()]
            ).await.ok()?;

            let rows = stream.into_first_result().await.ok()?;

            let mut col_names = Vec::new();
            let mut col_types = Vec::new();

            for row in rows {
                if let Some(col_name) = row.get::<&str, _>("COLUMN_NAME") {
                    let sql_type = row.get::<&str, _>("DATA_TYPE").unwrap_or("");
                    let php_type = Self::map_sql_type_to_php(sql_type);
                    col_names.push(col_name.to_string());
                    col_types.push((col_name.to_string(), php_type));
                }
            }

            columns.insert(table.clone(), col_names);
            columns_with_types.insert(table.clone(), col_types);
        }

        info!("SQL Server schema loaded: {} tables", tables.len());

        Some(DatabaseSchema {
            tables,
            columns,
            columns_with_types,
            cached_at: Instant::now(),
        })
    }
}

#[cfg(test)]
mod tests;
