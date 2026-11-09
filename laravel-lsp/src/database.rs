//! Database Schema Provider for Laravel Validation Rules
//!
//! Provides database schema information (tables and columns) for
//! `exists:` and `unique:` validation rule autocomplete.

use crate::completion_display::{is_sensitive_env_name, mask_url_credentials};
use regex::Regex;
use std::borrow::Cow;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, info, warn};

/// Read a string column from a sqlx Row, falling back to a `Vec<u8>` +
/// lossy UTF-8 decode if the direct `String` decoder rejects the value.
///
/// This exists because sqlx-mysql sometimes returns result columns with
/// binary collation (especially from `SHOW DATABASES`, `SHOW TABLES`,
/// `SHOW COLUMNS`, and `information_schema.*` against MySQL 8.0). The
/// `String` decoder bails on those, but the bytes are valid UTF-8 — we
/// just need to take the manual path. `from_utf8_lossy` is total, so any
/// stray invalid bytes become U+FFFD rather than a parse error.
///
/// Usage: `read_string(&row, "column_name")` or `read_string(&row, 0)`
/// (by-name overload via the `Idx` trait).
fn read_string<I>(row: &sqlx::mysql::MySqlRow, index: I) -> Option<String>
where
    I: sqlx::ColumnIndex<sqlx::mysql::MySqlRow> + Copy,
{
    use sqlx::Row;
    if let Ok(s) = row.try_get::<String, _>(index) {
        return Some(s);
    }
    if let Ok(bytes) = row.try_get::<Vec<u8>, _>(index) {
        return Some(String::from_utf8_lossy(&bytes).into_owned());
    }
    None
}

/// Same as [`read_string`] but for PostgreSQL rows. Postgres doesn't have
/// the same binary-collation issue MySQL does, but symmetry is easier to
/// maintain across the two drivers.
fn read_string_pg<I>(row: &sqlx::postgres::PgRow, index: I) -> Option<String>
where
    I: sqlx::ColumnIndex<sqlx::postgres::PgRow> + Copy,
{
    use sqlx::Row;
    if let Ok(s) = row.try_get::<String, _>(index) {
        return Some(s);
    }
    if let Ok(bytes) = row.try_get::<Vec<u8>, _>(index) {
        return Some(String::from_utf8_lossy(&bytes).into_owned());
    }
    None
}

/// Turn a raw sqlx-mysql error string into an actionable toast message.
///
/// sqlx surfaces MySQL errors as `... error returned from database: NNNN
/// (SQLSTATE): server-message ...`. We match on the well-known error
/// codes that have specific remediations and produce a focused message;
/// anything else falls through to the generic "check your .env" guidance.
///
/// Why pattern-match the string instead of using `sqlx::Error::Database`
/// fields? The error has already been formatted by the time it reaches
/// this layer (it's a `String`, not a `sqlx::Error`). Re-plumbing the
/// typed error all the way up would touch every candidate iteration
/// site; substring matching is plenty robust for a fixed set of MySQL
/// error codes whose wire format is part of the MySQL protocol.
fn classify_mysql_error(raw_error: &str, db_name: &str, candidates_str: &str) -> String {
    // 1049 (42000): Unknown database — connection + auth succeeded,
    // the named database doesn't exist on this server. Frame the
    // remediation in Laravel terms — once the database exists, the
    // user runs `artisan migrate` to populate it. We deliberately
    // don't dictate HOW to create the database (`CREATE DATABASE`,
    // `mysql -e`, a GUI tool, Sail helper, etc.); that's outside
    // Laravel and varies by setup.
    if raw_error.contains("1049 (42000)") || raw_error.contains("Unknown database") {
        return format!(
            "MySQL accepted the connection and credentials, but database \
             '{db_name}' doesn't exist on this server. Create the database \
             with your usual tool, then run `php artisan migrate` (or \
             `./vendor/bin/sail artisan migrate` for Sail projects) to set \
             up the schema. Or set DB_DATABASE in .env to a database that \
             already exists. (Tried: [{candidates_str}])"
        );
    }
    // 1045 (28000): Access denied for user — host reachable, credentials
    // wrong. Specific enough to call out instead of suggesting host issues.
    if raw_error.contains("1045 (28000)") || raw_error.contains("Access denied for user") {
        return format!(
            "MySQL is reachable but rejected the credentials. Check \
             DB_USERNAME and DB_PASSWORD in .env. The user may need a \
             password, may not exist on this server, or may be restricted \
             to socket-only auth. (Tried: [{candidates_str}]) Error: {raw_error}"
        );
    }
    // 2003 / "Can't connect" / TCP refused — server unreachable.
    if raw_error.contains("2003") || raw_error.contains("Connection refused") {
        return format!(
            "Couldn't reach the MySQL server at [{candidates_str}]. Check \
             DB_HOST / DB_PORT in .env. If using Sail/Docker Compose, ensure \
             the container is running and the port is mapped to your host \
             (run `./vendor/bin/sail up -d`). Error: {raw_error}"
        );
    }
    // 1044 (42000): Access denied for user to database — user exists but
    // doesn't have privileges on the requested database. This is a DB
    // admin task, not Laravel-fixable, so the guidance is generic.
    if raw_error.contains("1044 (42000)") {
        return format!(
            "MySQL accepted the connection but the user has no privileges \
             on database '{db_name}'. Either grant the user access to this \
             database, or set DB_USERNAME / DB_PASSWORD in .env to a user \
             that has access. Error: {raw_error}"
        );
    }
    // 1146 (42S02): Base table or view not found — schema exists but
    // the specific table doesn't. Migrations are the Laravel answer.
    if raw_error.contains("1146 (42S02)") || raw_error.contains("doesn't exist") {
        return format!(
            "MySQL is connected and the database '{db_name}' exists, but a \
             required table is missing. Run `php artisan migrate` (or \
             `./vendor/bin/sail artisan migrate` for Sail projects) to \
             apply pending migrations. Error: {raw_error}"
        );
    }
    // Generic fallback — keeps the original guidance for unknown error
    // codes. Better to over-explain than to miss something.
    format!(
        "MySQL connection failed. Tried candidates: [{candidates_str}]. \
         Last error: {raw_error}. Check DB_URL / DB_HOST / DB_PORT / \
         DB_DATABASE / DB_USERNAME / DB_PASSWORD / DB_SOCKET in .env. \
         If using Sail/Docker Compose, ensure the container is running \
         and the port is mapped to your host (run `./vendor/bin/sail up -d`)."
    )
}

/// Postgres equivalent of [`classify_mysql_error`]. SQLSTATE codes:
/// - `3D000` invalid_catalog_name → database doesn't exist
/// - `28P01` invalid_password → wrong credentials
/// - `28000` invalid_authorization_specification → role/host issue
fn classify_postgres_error(raw_error: &str, db_name: &str, candidates_str: &str) -> String {
    // 3D000 invalid_catalog_name → database doesn't exist. Same Laravel
    // framing as MySQL: create the database, then run `artisan migrate`.
    if raw_error.contains("3D000") {
        return format!(
            "PostgreSQL accepted the connection and credentials, but \
             database '{db_name}' doesn't exist. Create the database with \
             your usual tool, then run `php artisan migrate` (or \
             `./vendor/bin/sail artisan migrate` for Sail projects) to set \
             up the schema. Or set DB_DATABASE in .env to a database that \
             already exists. (Tried: [{candidates_str}])"
        );
    }
    if raw_error.contains("28P01") || raw_error.contains("password authentication failed") {
        return format!(
            "PostgreSQL rejected the credentials. Check DB_USERNAME / \
             DB_PASSWORD in .env. (Tried: [{candidates_str}]) Error: {raw_error}"
        );
    }
    if raw_error.contains("Connection refused") || raw_error.contains("could not connect") {
        return format!(
            "Couldn't reach the PostgreSQL server at [{candidates_str}]. Check \
             DB_HOST / DB_PORT in .env. If using Sail/Docker Compose, ensure \
             the container is running. Error: {raw_error}"
        );
    }
    // 42P01 undefined_table → schema exists but the table doesn't. Run
    // migrations.
    if raw_error.contains("42P01") {
        return format!(
            "PostgreSQL is connected and the database '{db_name}' exists, \
             but a required table is missing. Run `php artisan migrate` (or \
             `./vendor/bin/sail artisan migrate` for Sail projects) to apply \
             pending migrations. Error: {raw_error}"
        );
    }
    format!(
        "PostgreSQL connection failed. Tried candidates: [{candidates_str}]. \
         Last error: {raw_error}. Check DB_URL / DB_HOST / DB_PORT / \
         DB_DATABASE / DB_USERNAME / DB_PASSWORD / DB_SOCKET in .env. \
         If using Sail/Docker Compose, ensure the container is running."
    )
}

/// How long the circuit breaker stays open after a failed schema fetch
/// before allowing a single half-open probe. A fixed cooldown (deliberately
/// NOT exponential): every connection-failure class behaves identically —
/// tune this one constant if 30s proves too eager or too lazy. Also the
/// background loop's retry cadence while the DB is down.
pub const DB_BREAKER_COOLDOWN: Duration = Duration::from_secs(30);

/// Steady-state refresh interval for the background loop when the DB is
/// healthy. Matches [`DatabaseSchema::is_valid`]'s 60s TTL so the served
/// cache is never more than one interval stale.
pub const DB_REFRESH_HEALTHY_INTERVAL: Duration = Duration::from_secs(60);

/// Connect/acquire timeout for the background schema fetch. Only the
/// detached background loop ever connects — interactive requests read the
/// in-memory cache and never block — so this can afford to be patient.
pub const DB_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Extra budget, on top of [`DB_CONNECT_TIMEOUT`], for the post-connect
/// schema queries (identity probe + SHOW TABLES/COLUMNS and the
/// per-driver equivalents). The whole background fetch is wrapped in a
/// `timeout` of `DB_CONNECT_TIMEOUT + DB_FETCH_QUERY_BUDGET` so a
/// connected-but-stalled server can never hang the loop forever — it
/// always reaches success/failure/timeout and updates the breaker.
/// Sized well above the ~2.5s a 600-table schema costs.
pub const DB_FETCH_QUERY_BUDGET: Duration = Duration::from_secs(30);

/// The framework's default `config/database.php`, which Laravel 11+ merges the
/// app's own `config/database.php` over. Relative to the project root.
const FRAMEWORK_DATABASE_CONFIG: &str = "vendor/laravel/framework/config/database.php";

/// What a caller may do right now, per [`CircuitBreaker::allow_attempt`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attempt {
    /// Breaker closed — normal fetch allowed.
    Closed,
    /// Breaker open (cooling down, or a probe is already in flight) — deny;
    /// return "no schema" without touching the database.
    Open,
    /// Cooldown elapsed — this caller is the single half-open probe.
    HalfOpen,
}

/// Internal breaker state. `Open` and `HalfOpen` both carry the instant the
/// state was entered so time-based transitions are pure functions of the
/// injected `now`.
#[derive(Debug, Clone, Copy)]
enum BreakerState {
    Closed,
    Open { since: Instant },
    HalfOpen { since: Instant },
}

/// Pure, synchronous, time-injected circuit breaker guarding database
/// schema fetches.
///
/// With the database down, every completion/diagnostic/pre-warm used to
/// reconnect and pay the full connect timeout. The breaker makes failure
/// cheap: after any failed fetch it opens for [`DB_BREAKER_COOLDOWN`],
/// during which attempts are denied instantly. After the cooldown exactly
/// one half-open probe is allowed through; success closes the breaker,
/// failure re-opens it for another cooldown.
///
/// All transitions take `now: Instant` as a parameter — no clocks, no
/// async, no I/O — so the whole state machine is unit-testable without a
/// database (see `database/tests.rs`).
#[derive(Debug)]
pub struct CircuitBreaker {
    state: BreakerState,
    cooldown: Duration,
}

impl CircuitBreaker {
    pub fn new(cooldown: Duration) -> Self {
        Self {
            state: BreakerState::Closed,
            cooldown,
        }
    }

    /// Gate a fetch attempt. Returns [`Attempt::HalfOpen`] at most once per
    /// cooldown window: the transition to `HalfOpen` happens here, so a
    /// second caller arriving while the probe is outstanding gets
    /// [`Attempt::Open`].
    ///
    /// A probe that never reports back (a fetch path that returns `None`
    /// without recording failure, or a hung connect) would otherwise wedge
    /// the breaker in `HalfOpen` forever — so an aged-out `HalfOpen` re-arms
    /// into a fresh probe after another cooldown. Self-healing by
    /// construction.
    pub fn allow_attempt(&mut self, now: Instant) -> Attempt {
        match self.state {
            BreakerState::Closed => Attempt::Closed,
            BreakerState::Open { since } | BreakerState::HalfOpen { since } => {
                if now.duration_since(since) >= self.cooldown {
                    self.state = BreakerState::HalfOpen { since: now };
                    Attempt::HalfOpen
                } else {
                    Attempt::Open
                }
            }
        }
    }

    /// Record a failed fetch. Returns `true` only on the Closed→Open
    /// transition — the FIRST failure of a new outage episode, i.e. the
    /// moment to notify the user. A failed half-open probe re-opens the
    /// breaker but returns `false`: same outage, no fresh notification.
    pub fn record_failure(&mut self, now: Instant) -> bool {
        let just_opened = matches!(self.state, BreakerState::Closed);
        self.state = BreakerState::Open { since: now };
        just_opened
    }

    /// Record a successful fetch: close the breaker from any state. Returns
    /// `true` only on a genuine recovery edge — Open/HalfOpen→Closed — the
    /// moment to tell the user the DB is back. Returns `false` when already
    /// Closed (a routine healthy refresh, or the very first successful fetch
    /// from a fresh breaker), so startup and steady-state refreshes stay
    /// silent. Mirrors [`Self::record_failure`]'s Closed→Open edge return.
    /// The next failure after this is a NEW outage episode (fresh toast).
    pub fn record_success(&mut self) -> bool {
        let recovered = matches!(
            self.state,
            BreakerState::Open { .. } | BreakerState::HalfOpen { .. }
        );
        self.state = BreakerState::Closed;
        recovered
    }

    /// How long until the breaker would next allow an attempt, given `now`.
    /// Drives the background refresh loop's sleep so it wakes exactly when
    /// the next half-open probe is due:
    /// - `None` — closed (healthy); the caller sleeps its own steady-state
    ///   refresh interval.
    /// - `Some(d)` — open/half-open; `d` is the remaining cooldown before
    ///   the next probe (saturating to zero once elapsed).
    pub fn cooldown_remaining(&self, now: Instant) -> Option<Duration> {
        match self.state {
            BreakerState::Closed => None,
            BreakerState::Open { since } | BreakerState::HalfOpen { since } => {
                Some(self.cooldown.saturating_sub(now.duration_since(since)))
            }
        }
    }
}

/// Coarse classification of a database connection failure. The breaker's
/// backoff is UNIFORM across classes — only the user-facing notification
/// text differs (see [`outage_toast_message`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutageClass {
    /// Scenario 1: the server couldn't be reached at all — DNS failure,
    /// connection refused, or a connect timeout.
    Unreachable,
    /// Scenario 2: the server was reached but the connection was rejected —
    /// bad credentials, unknown database, missing privileges.
    Rejected,
    /// Scenario 0: no usable database configuration. A normal state for
    /// projects without a DB — never notified.
    NotConfigured,
    /// Anything else — notified with a generic message.
    Other,
}

/// Classify a RAW driver error string (before the per-driver classifiers
/// compose their remediation-rich messages) into an [`OutageClass`].
/// Substring matching on well-known driver/OS error markers, lowercased for
/// robustness — the same pragmatic approach `classify_mysql_error` takes,
/// and for the same reason: by this layer the error is already a `String`.
pub fn outage_class_from_raw(raw_error: &str) -> OutageClass {
    let raw = raw_error.to_lowercase();
    // Auth / unknown-database / privilege markers across MySQL (1045, 1044,
    // 1049), Postgres (28P01, 28000, 3D000), and SQL Server ("Login failed").
    const REJECTED: &[&str] = &[
        "access denied",
        "1045 (28000)",
        "1044 (42000)",
        "1049 (42000)",
        "unknown database",
        "28p01",
        "28000",
        "3d000",
        "password authentication failed",
        "login failed",
        "does not exist",
    ];
    // Network-level markers: TCP refused/reset, DNS lookup failures, and
    // timeouts (sqlx reports a connect that exceeds `acquire_timeout` as
    // "pool timed out while waiting for an open connection").
    const UNREACHABLE: &[&str] = &[
        "connection refused",
        "2003",
        "could not connect",
        "timed out",
        "timeout",
        "failed to lookup address",
        "name or service not known",
        "nodename nor servname",
        "no route to host",
        "network is unreachable",
        "connection reset",
        "broken pipe",
    ];
    if REJECTED.iter().any(|m| raw.contains(m)) {
        return OutageClass::Rejected;
    }
    if UNREACHABLE.iter().any(|m| raw.contains(m)) {
        return OutageClass::Unreachable;
    }
    OutageClass::Other
}

/// The one-per-outage toast text for a breaker Closed→Open transition, or
/// `None` for classes that must stay silent (a project without a database
/// is a normal state, not an outage).
///
/// `detail` is the remediation-rich message the per-driver classifiers
/// composed (it names the exact endpoints tried, which is more truthful
/// than a single host:port when Sail fallbacks are in play).
pub fn outage_toast_message(class: OutageClass, detail: &str) -> Option<String> {
    let retry = DB_BREAKER_COOLDOWN.as_secs();
    match class {
        OutageClass::NotConfigured => None,
        OutageClass::Unreachable => Some(format!(
            "Laravel CE: can't reach the database — is it running? DB-aware \
             completions and diagnostics are disabled until it's reachable \
             (the LSP retries every {retry}s). {detail}"
        )),
        OutageClass::Rejected => Some(format!(
            "Laravel CE: reached the database but the connection was rejected — \
             check the credentials and that the database exists. DB-aware \
             completions and diagnostics are disabled until it connects \
             (the LSP retries every {retry}s). {detail}"
        )),
        OutageClass::Other => Some(format!(
            "Laravel CE: database connection failed, so DB-aware completions \
             and diagnostics are disabled until it recovers (the LSP \
             retries every {retry}s). {detail}"
        )),
    }
}

/// Details of an outage — emitted when the breaker transitions Closed→Open
/// (the start of an outage episode).
#[derive(Debug, Clone)]
pub struct DbOutageEvent {
    pub class: OutageClass,
    /// The classified, remediation-rich error message from the driver layer.
    pub message: String,
}

/// A breaker edge worth telling the user about. The provider can't talk
/// LSP, so `main.rs` listens on a channel of these and sends the (single)
/// toast for each edge, no matter which caller — pre-warm, completion,
/// diagnostics, hover — drove the breaker there.
///
/// Both edges fire exactly once per transition: `Outage` on Closed→Open
/// (retry probes inside the same outage are silent), `Reconnected` on
/// Open/HalfOpen→Closed (a routine already-healthy refresh is silent).
#[derive(Debug, Clone)]
pub enum DbBreakerEvent {
    /// The database just became unreachable/unusable.
    Outage(DbOutageEvent),
    /// The database just came back after an outage.
    Reconnected,
}

/// Build the `user[:password]` userinfo segment of a `driver://userinfo@…`
/// connection URL.
///
/// When the password is empty, returns just `user` (no trailing `:`).
/// This matters: `mysql://root:@host/db` (with the `:`) tells sqlx
/// "empty password specified" and sqlx will send the auth handshake
/// including an empty password — MySQL responds with `using password: YES`
/// and may reject the connection (especially against `auth_socket` plugin
/// setups, or `root@localhost` configurations that require socket auth).
/// `mysql://root@host/db` (without the `:`) tells sqlx "no password" and
/// the handshake skips the password packet entirely — accepted by more
/// permissive MySQL configs.
///
/// Both components are percent-encoded by [`encode_userinfo_component`], so a
/// `.env` credential holding a delimiter cannot escape its own slot (issue
/// #362). Splicing raw let it: sqlx routes every connection string through
/// `Url::parse`, which ends the authority at the first `/`, `?` or `#`
/// whether or not that character was meant as a delimiter. A password `p/ss`
/// yields `mysql://user:p/ss@host:3306/db`, whose authority is `user:p` — host
/// `user`, port `p` — and the connection fails with *invalid port number*.
///
/// Worse is the case that does not fail: when the run before the delimiter is
/// all digits and fits a `u16`, the authority parses, so `mysql://user:12/34@host/db`
/// connects to host `user` on port 12 and reports no password at all. Base64
/// passwords (`openssl rand -base64`) mix digits and `/` routinely, so this is
/// an ordinary shape, and any redactor that decides the credential span by
/// parsing the URL is told there is nothing to mask.
fn userinfo(user: &str, password: &str) -> String {
    let user = encode_userinfo_component(user);
    if password.is_empty() {
        user
    } else {
        format!("{user}:{}", encode_userinfo_component(password))
    }
}

/// Percent-encode one userinfo component (a username or a password) for
/// splicing into a connection URL.
///
/// Passes through exactly RFC 3986's `unreserved` (`ALPHA DIGIT - . _ ~`) and
/// `sub-delims` (`! $ & ' ( ) * + , ; =`) sets; every other byte becomes `%XX`,
/// including `:`, `/`, `?`, `#`, `[`, `]`, `@`, `%`, space, control bytes, and
/// each byte of a non-ASCII UTF-8 sequence. A credential built only from the
/// pass-through sets is returned byte-for-byte unchanged, so the URLs this
/// server has always produced for ordinary credentials do not move.
///
/// `:` is encoded even in the *username*, where RFC 3986's userinfo grammar
/// permits it: [`userinfo`] overloads `:` as its own username/password
/// separator, so an unencoded one there mis-splits the credential exactly as an
/// unencoded `/` mis-splits the authority.
///
/// Encoding is safe against double-encoding because it is applied at the single
/// point of construction and the value arrives raw from the `.env` file: `%` is
/// itself encoded to `%25`, so `sec%3Dret` round-trips to the literal
/// `sec%3Dret` rather than decoding to `sec=ret`. sqlx percent-decodes both
/// components on the way back out (`sqlx-mysql/src/options/parse.rs`,
/// `sqlx-postgres/src/options/parse.rs`), so the driver receives the original
/// bytes.
fn encode_userinfo_component(raw: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        match byte {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'.'
            | b'_'
            | b'~'
            | b'!'
            | b'$'
            | b'&'
            | b'\''
            | b'('
            | b')'
            | b'*'
            | b'+'
            | b','
            | b';'
            | b'=' => out.push(byte as char),
            _ => {
                out.push('%');
                out.push(HEX[(byte >> 4) as usize] as char);
                out.push(HEX[(byte & 0x0f) as usize] as char);
            }
        }
    }
    out
}

/// Render a `.env`-sourced value for a log line, through both redaction gates.
///
/// Logs are a display surface. With `RUST_LOG` unset the server installs
/// `EnvFilter::new("info,salsa=warn")` over stderr, which Zed shows in a visible
/// log panel — the same screen-share exposure the completion, hover, `config()`
/// and warm-start-cache redaction closes (issue #344). A value read under a
/// variable name that [`is_sensitive_env_name`] matches therefore never reaches a
/// log in the clear.
///
/// Name-matched values render `(set)`, the spelling
/// [`DatabaseSchemaProvider::parse_database_config`] already prints for the
/// resolved DB password. There is no `(empty)` arm: every call site logs a value
/// that came back from [`crate::config::read_env_value`], which filters an empty
/// value to `None` before it can get here.
///
/// A name the predicate does not match is not therefore safe: Laravel's stock
/// `config/database.php` reads `'url' => env('DATABASE_URL')`, and that value
/// carries the password inside itself while its name matches no segment. So the
/// unmatched arm is not the raw value — it is
/// [`mask_url_credentials`], the same helper `parse_database_config` has always
/// applied to the assembled `url` line. `DB_HOST` and `DB_DATABASE` still log in
/// full: neither gate matches them, and redacting them would spend the whole
/// diagnostic for nothing.
fn mask_env_value_for_log<'a>(name: &str, value: &'a str) -> Cow<'a, str> {
    if is_sensitive_env_name(name) {
        Cow::Borrowed("(set)")
    } else {
        mask_url_credentials(value)
    }
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

/// A Sail/Docker TCP endpoint detected for the database — the host-side bind
/// IP and forwarded port, plus a note on how it was found (compose file or
/// APP_PORT). Produced by [`DatabaseSchemaProvider::detect_sail_endpoint`].
#[derive(Debug, Clone, PartialEq, Eq)]
struct DetectedEndpoint {
    host: String,
    port: u16,
    note: String,
}

/// Count a line's leading spaces (YAML indentation). Tabs are not valid YAML
/// indentation; a line indented with tabs yields 0 here and simply fails the
/// indentation checks — fail-open, never a panic.
fn indent_spaces(line: &str) -> usize {
    line.chars().take_while(|c| *c == ' ').count()
}

/// Is `trimmed` a `key:` line that INTRODUCES a nested block (rather than a
/// `key: value` scalar)? Used to match `services:`, a service header, and
/// `ports:`. A block is introduced when what follows the `:` is empty, a
/// `#` comment, or a YAML tag (`!`-prefixed, e.g. `ports: !override` /
/// `!reset` — standard in Sail override files — or `!!seq`): the tag
/// annotates the block on the lines below, so `ports: !override` IS the
/// `ports:` key. A plain inline scalar (`ports: 3306:3306`) is still rejected.
fn line_is_key(trimmed: &str, key: &str) -> bool {
    match trimmed.strip_prefix(key) {
        Some(rest) => {
            let rest = rest.trim_start();
            if let Some(after) = rest.strip_prefix(':') {
                let after = after.trim_start();
                after.is_empty() || after.starts_with('#') || after.starts_with('!')
            } else {
                false
            }
        }
        None => false,
    }
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

/// Database schema provider with caching.
///
/// `Clone` is cheap and share-state: every field is an `Arc` (or a
/// `PathBuf`), so a clone is the same logical provider — same cache, same
/// breaker. This lets callers lift the handle out of an
/// `Arc<RwLock<Option<…>>>` guard and drop the guard before an await.
#[derive(Clone)]
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
    /// Circuit breaker guarding schema fetches. Lives ENTIRELY in the
    /// background refresh loop (`refresh_tick`) — interactive callers never
    /// touch it. After a failed fetch the loop opens the breaker and backs
    /// off; interactive reads just see a stale/absent cache. See
    /// [`CircuitBreaker`].
    breaker: Arc<RwLock<CircuitBreaker>>,
    /// Where breaker edges are announced — one `Outage` per outage episode,
    /// one `Reconnected` per recovery. `None` until `main.rs` wires the
    /// toast listener.
    event_tx: Arc<RwLock<Option<mpsc::UnboundedSender<DbBreakerEvent>>>>,
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
            breaker: Arc::new(RwLock::new(CircuitBreaker::new(DB_BREAKER_COOLDOWN))),
            event_tx: Arc::new(RwLock::new(None)),
        }
    }

    /// Register the channel on which breaker edges (`Outage` / `Reconnected`)
    /// are announced. Called once by the LSP layer when the provider is set up.
    pub async fn set_event_channel(&self, tx: mpsc::UnboundedSender<DbBreakerEvent>) {
        *self.event_tx.write().await = Some(tx);
    }

    /// Test helper: seed the schema cache directly so tests can exercise
    /// completion paths against a known schema without a live MySQL /
    /// Postgres. The cache is the same one `get_schema` reads, so calls
    /// to `get_tables` / `get_columns_with_types` will see this data
    /// immediately. No production caller should be poking the cache manually.
    ///
    /// Exposed as `#[doc(hidden)] pub` rather than `#[cfg(test)]`: the
    /// `main.rs` binary's completion-handler integration tests
    /// (`src/tests/query_chain_completion_handler.rs`) need to seed schema
    /// from a separate crate, and Cargo does not enable `cfg(test)` on a
    /// library consumed as a dependency — a `#[cfg(test)]` seam would be
    /// invisible to that bin test crate. Hidden from docs so it stays off the
    /// public API surface.
    #[doc(hidden)]
    pub async fn set_test_schema(&self, schema: DatabaseSchema) {
        *self.schema_cache.write().await = Some(schema);
    }

    /// Get the last connection error, if any
    pub async fn get_last_error(&self) -> Option<DatabaseConnectionError> {
        self.last_error.read().await.clone()
    }

    /// Check if a connection has been attempted
    pub async fn was_connection_attempted(&self) -> bool {
        *self.connection_attempted.read().await
    }

    /// Record a failed fetch: store the error for `get_last_error`, feed
    /// the circuit breaker, and — exactly when this failure OPENS the
    /// breaker (Closed→Open, the first failure of an outage episode) —
    /// announce it on the outage channel. Failed half-open probes while
    /// the outage continues re-open the breaker silently, so the user
    /// sees ONE notification per outage, not one per retry.
    ///
    /// Every driver's failure path funnels through here, which is what
    /// makes the breaker cover mysql/pgsql/sqlite/sqlsrv with one hook.
    async fn set_error(&self, driver: &str, message: &str, class: OutageClass) {
        *self.last_error.write().await = Some(DatabaseConnectionError {
            message: message.to_string(),
            driver: driver.to_string(),
        });
        let just_opened = self.breaker.write().await.record_failure(Instant::now());
        if just_opened {
            if let Some(tx) = self.event_tx.read().await.as_ref() {
                let _ = tx.send(DbBreakerEvent::Outage(DbOutageEvent {
                    class,
                    message: message.to_string(),
                }));
            }
        }
    }

    /// Record a successful fetch: clear the stored error and close the
    /// breaker. On a genuine recovery edge (Open/HalfOpen→Closed) announce
    /// `Reconnected` on the channel — exactly once per outage→recovery
    /// cycle. A routine already-healthy refresh closes silently. A later
    /// failure then counts as a NEW outage episode (fresh toast).
    async fn clear_error(&self) {
        *self.last_error.write().await = None;
        let recovered = self.breaker.write().await.record_success();
        if recovered {
            if let Some(tx) = self.event_tx.read().await.as_ref() {
                let _ = tx.send(DbBreakerEvent::Reconnected);
            }
        }
    }

    /// Get the database schema **from the in-memory cache only** — the
    /// interactive read path.
    ///
    /// This is called from LSP request handlers (completion, diagnostics,
    /// hover, exists/unique). tower-lsp dispatches at most a handful of
    /// handlers concurrently, so a handler that blocked on a DB connect
    /// would starve unrelated requests (a plain view/route hover froze for
    /// ~30s during an outage — the isolation bug this design fixes).
    /// Therefore this method NEVER connects, NEVER touches the breaker or a
    /// lock, and NEVER blocks: it returns the last-known-good schema
    /// (served stale on purpose — the background loop keeps it fresh) or
    /// `None` on a cold miss. All connecting happens off the request path
    /// in [`Self::refresh_tick`].
    pub async fn get_schema(&self) -> Option<DatabaseSchema> {
        self.schema_cache.read().await.clone()
    }

    /// One iteration of the background refresh loop — the ONLY place that
    /// connects. Returns how long the caller should sleep before the next
    /// tick.
    ///
    /// Flow: consult the breaker; if it allows (closed, or a half-open
    /// probe is due), run a whole-fetch-timeout-bounded fetch. Success
    /// fills the cache and closes the breaker (via `clear_error`); failure
    /// or timeout opens it (via `set_error`) and emits the one-per-outage
    /// event on the Closed→Open edge. The next sleep is derived from the
    /// resulting breaker state: [`DB_BREAKER_COOLDOWN`] while open (the
    /// retry/HalfOpen cadence), `healthy_interval` while closed.
    ///
    /// Detached from any tower-lsp request slot, so a slow or stalled fetch
    /// here can never starve interactive requests.
    pub async fn refresh_tick(
        &self,
        connect_timeout: Duration,
        healthy_interval: Duration,
    ) -> Duration {
        let attempt = self.breaker.write().await.allow_attempt(Instant::now());
        if attempt != Attempt::Open {
            self.fetch_and_store(connect_timeout).await;
        }
        self.breaker
            .read()
            .await
            .cooldown_remaining(Instant::now())
            .unwrap_or(healthy_interval)
    }

    /// Background-only: run one whole-fetch-timeout-bounded fetch and, on
    /// success, publish it to the interactive cache. The timeout bounds the
    /// ENTIRE fetch — connect AND the post-connect identity probe / schema
    /// queries — so a connected-but-stalled server can't wedge the loop; on
    /// timeout we record a failure so the breaker opens and the loop backs
    /// off. `set_error` / `clear_error` inside `fetch_schema` drive the
    /// breaker + notification; the timeout branch records the failure the
    /// cancelled fetch never got to. Returns whether the cache was updated.
    async fn fetch_and_store(&self, connect_timeout: Duration) -> bool {
        let budget = connect_timeout + DB_FETCH_QUERY_BUDGET;
        self.run_bounded_fetch(budget, self.fetch_schema(connect_timeout))
            .await
    }

    /// Apply the whole-fetch timeout and cache/breaker bookkeeping to an
    /// arbitrary fetch future. Split out from [`Self::fetch_and_store`] so
    /// the timeout→breaker-open path is unit-testable with an injected slow
    /// future (no live DB needed).
    async fn run_bounded_fetch<F>(&self, budget: Duration, fetch: F) -> bool
    where
        F: std::future::Future<Output = Option<DatabaseSchema>>,
    {
        match tokio::time::timeout(budget, fetch).await {
            Ok(Some(schema)) => {
                *self.schema_cache.write().await = Some(schema);
                true
            }
            // fetch_schema already recorded the error (set_error) on the way out.
            Ok(None) => false,
            Err(_) => {
                let msg = format!(
                    "Database schema fetch timed out after {budget:?} — the server \
                     accepted a connection but a schema query stalled. Check the \
                     database server's health / load."
                );
                warn!("{}", msg);
                self.set_error("timeout", &msg, OutageClass::Unreachable)
                    .await;
                false
            }
        }
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

    /// Get all available database connection names from the app config and
    /// the framework config Laravel merges it over: the app's own connections
    /// first, then the framework's, each name once.
    pub fn get_connections(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for content in self.database_config_sources() {
            for name in Self::connection_names(&content) {
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
        names
    }

    /// The top-level keys of the `'connections' => [...]` array in one config
    /// file. Nested keys such as `'options' => [` or `'pragmas' => [` belong to
    /// a connection, not to the list of connections, and are skipped.
    fn connection_names(content: &str) -> Vec<String> {
        let Some(connections_start) = Regex::new(r#"['"]connections['"]\s*=>\s*\["#)
            .ok()
            .and_then(|re| re.find(content))
        else {
            return Vec::new();
        };
        let Ok(connection_name_regex) = Regex::new(r#"['"]([a-zA-Z_][a-zA-Z0-9_]*)['"]\s*=>\s*\["#)
        else {
            return Vec::new();
        };

        // An unbalanced file falls back to everything after the opening
        // bracket, as before.
        let connections_block = Self::extract_bracketed_block(content, connections_start.end())
            .unwrap_or_else(|| content[connections_start.end()..].to_string());

        let mut depth = 0i32;
        let mut scanned = 0;
        connection_name_regex
            .captures_iter(&connections_block)
            .filter_map(|caps| {
                let key = caps.get(0)?;
                for c in connections_block[scanned..key.start()].chars() {
                    match c {
                        '[' => depth += 1,
                        ']' => depth -= 1,
                        _ => {}
                    }
                }
                scanned = key.start();
                (depth == 0).then(|| caps[1].to_string())
            })
            .collect()
    }

    /// Invalidate the cache (call when migrations change)
    pub async fn invalidate_cache(&self) {
        let mut cache = self.schema_cache.write().await;
        *cache = None;
        info!("Database schema cache invalidated");
    }

    /// Fetch fresh schema from database. `connect_timeout` bounds
    /// connection establishment for every driver (sqlx `acquire_timeout`;
    /// explicit `tokio::time::timeout` for tiberius, which has no built-in
    /// connect timeout and would otherwise hang on the OS default for an
    /// unreachable host).
    async fn fetch_schema(&self, connect_timeout: Duration) -> Option<DatabaseSchema> {
        // Mark that we've attempted a connection
        *self.connection_attempted.write().await = true;

        let config = match self.get_database_config().await {
            Some(c) => c,
            None => {
                // Scenario 0 — no DB configured. The breaker still opens
                // (don't re-parse config every request) but the class keeps
                // the notification silent: this is a normal state.
                self.set_error(
                    "unknown",
                    "Database configuration not found in .env",
                    OutageClass::NotConfigured,
                )
                .await;
                return None;
            }
        };

        let result = match config.driver.as_str() {
            "mysql" | "mariadb" => self.fetch_mysql_schema(&config, connect_timeout).await,
            "pgsql" | "postgres" => self.fetch_postgres_schema(&config, connect_timeout).await,
            "sqlite" => self.fetch_sqlite_schema(&config, connect_timeout).await,
            "sqlsrv" => self.fetch_sqlserver_schema(&config, connect_timeout).await,
            _ => {
                // Not a connection outage — the LSP simply can't speak this
                // driver. Silent (NotConfigured), the warn! log tells the story.
                self.set_error(
                    &config.driver,
                    &format!("Unsupported database driver: {}", config.driver),
                    OutageClass::NotConfigured,
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
    ///
    /// Since Laravel 11 the framework merges the app's `config/database.php`
    /// over `vendor/laravel/framework/config/database.php`, so an app may only
    /// define its extra connections (or ship no `config/database.php` at all).
    /// Steps 1 and 2 therefore look in the app config first and fall back to
    /// the framework config, mirroring that merge.
    fn parse_database_config(&self) -> Option<DatabaseConfig> {
        let sources = self.database_config_sources();
        if sources.is_empty() {
            warn!(
                "🗄️  Neither config/database.php nor the framework's database config is readable"
            );
            return None;
        }

        // Step 1: Find the `default` key. The first source that sets it
        // decides, so the app's entry wins over the framework's in any form:
        // env('VAR', 'fallback'), env('VAR') or a literal 'name'. A nested
        // array such as redis' 'default' => [...] is not the key.
        let default_regex = Regex::new(
            r#"['"]default['"]\s*=>\s*(?:env\s*\(\s*['"]([^'"]+)['"]\s*(?:,\s*['"]([^'"]+)['"]\s*)?\)|['"]([^'"]+)['"]|[^\[\s])"#,
        )
        .ok()?;

        let default_key = sources
            .iter()
            .find_map(|content| default_regex.captures(content));

        let driver = match &default_key {
            Some(caps) if caps.get(3).is_some() => caps[3].to_string(),
            Some(caps) if caps.get(1).is_some() => self
                .resolve_env(&caps[1])
                .or_else(|| caps.get(2).map(|m| m.as_str().to_string()))
                .unwrap_or_else(|| "mysql".to_string()),
            _ => self
                .resolve_env("DB_CONNECTION")
                .unwrap_or_else(|| "mysql".to_string()),
        };
        info!("🗄️  Resolved default connection: {}", driver);

        // Step 2: Find the connection block for this driver
        // Pattern: 'driver_name' => [...]
        let connection_block = sources
            .iter()
            .find_map(|content| self.extract_connection_block(content, &driver));

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
            info!("🗄️    url: {}", mask_url_credentials(u));
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

    /// Read the database config files in merge-precedence order: the app's
    /// `config/database.php` first, then the framework's defaults it is merged
    /// over. Unreadable files are skipped.
    fn database_config_sources(&self) -> Vec<String> {
        ["config/database.php", FRAMEWORK_DATABASE_CONFIG]
            .iter()
            .filter_map(|relative| {
                let path = self.project_root.join(relative);
                debug!("🗄️  Reading database config from: {:?}", path);
                std::fs::read_to_string(&path)
                    .inspect_err(|e| debug!("🗄️    not readable: {}", e))
                    .ok()
            })
            .collect()
    }

    /// Extract the connection block for a specific connection name from
    /// config/database.php.
    ///
    /// Two forms are supported:
    /// 1. **Inline array** (the common case) — `'mysql' => [ ... ]`.
    /// 2. **Variable reference** — `'mysql' => $mysql`, where the block is
    ///    defined elsewhere as `$mysql = [ ... ]`. Laravel apps (Sail's own
    ///    published `config/database.php` among them) often factor a
    ///    connection into a `$mysql` variable and reference it in
    ///    `'connections' => [ 'mysql' => $mysql ]`. Without this, the block
    ///    came back empty and EVERY setting (host, database, credentials)
    ///    silently fell back to hardcoded defaults, ignoring `.env`.
    ///
    /// Fails open (returns `None`) when neither form matches — the caller
    /// then uses its hardcoded defaults, the prior behaviour.
    fn extract_connection_block(&self, content: &str, connection: &str) -> Option<String> {
        let name = regex::escape(connection);

        // Form 1: inline array — `'mysql' => [`.
        let inline = format!(r#"['"]{name}['"]\s*=>\s*\["#);
        if let Ok(re) = Regex::new(&inline) {
            if let Some(m) = re.find(content) {
                if let Some(block) = Self::extract_bracketed_block(content, m.end()) {
                    return Some(block);
                }
            }
        }

        // Form 2: variable reference — `'mysql' => $mysql` → find `$mysql = [`.
        let var_ref = format!(r#"['"]{name}['"]\s*=>\s*\$([A-Za-z_]\w*)"#);
        let var_name = Regex::new(&var_ref)
            .ok()?
            .captures(content)?
            .get(1)?
            .as_str()
            .to_string();

        // Locate the variable's array definition. Requiring `= [` here means a
        // definition that is itself a bare `$other` (an alias) does NOT match —
        // one level of resolution only, so there's no self-reference loop.
        let def = format!(r#"\${}\s*=\s*\["#, regex::escape(&var_name));
        let def_match = Regex::new(&def).ok()?.find(content)?;
        Self::extract_bracketed_block(content, def_match.end())
    }

    /// Given `content` and a byte offset pointing just PAST an opening `[`,
    /// return the substring up to (not including) the matching `]` via
    /// bracket-depth counting, or `None` if unbalanced. `char_indices` keeps
    /// the slice boundary on a valid char boundary; brackets are ASCII.
    fn extract_bracketed_block(content: &str, after_open_bracket: usize) -> Option<String> {
        let remaining = content.get(after_open_bracket..)?;
        let mut depth = 1;
        for (i, c) in remaining.char_indices() {
            match c {
                '[' => depth += 1,
                ']' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(remaining[..i].to_string());
                    }
                }
                _ => {}
            }
        }
        None
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
                        info!(
                            "🗄️      → resolved from .env: {}",
                            mask_env_value_for_log(&env_var, &env_value)
                        );
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

    /// Resolve an environment variable from .env file.
    ///
    /// The regex uses `[ \t]*` (horizontal whitespace only) around `=`,
    /// NOT `\s*` — `\s` matches newlines in Rust's regex crate, which
    /// meant the value for an empty `KEY=` would consume the newline and
    /// then capture the *next line's content* up to its newline. That
    /// turned a blank `DB_PASSWORD=` into "SESSION_DRIVER=database" (or
    /// whatever line followed), which was sent as the literal password
    /// to MySQL and rejected as bad credentials.
    fn resolve_env(&self, key: &str) -> Option<String> {
        // Delegates to the single hardened reader in `config` — see its doc
        // comment for why this logic must not be duplicated.
        let result = crate::config::read_env_value(&self.project_root, key);
        debug!(
            "🗄️  resolve_env({}): {:?}",
            key,
            result
                .as_deref()
                .map(|value| mask_env_value_for_log(key, value))
        );
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
    async fn fetch_mysql_schema(
        &self,
        config: &DatabaseConfig,
        connect_timeout: Duration,
    ) -> Option<DatabaseSchema> {
        use sqlx::mysql::MySqlPoolOptions;
        use sqlx::Row;

        let candidates = self.build_mysql_candidates(config);
        let mut last_err: Option<String> = None;
        let mut pool_opt = None;
        let primary_label = candidates.first().map(|c| c.label.clone());

        for cand in &candidates {
            // Log the exact URL shape we're handing to sqlx (with the
            // password masked). This makes "is the new binary loaded?"
            // and "did the empty-password URL change actually take
            // effect?" trivially answerable from the LSP log.
            info!(
                "MySQL: trying candidate '{}' with url={}",
                cand.label,
                mask_url_credentials(&cand.url)
            );
            match MySqlPoolOptions::new()
                .max_connections(1)
                .acquire_timeout(connect_timeout)
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
                let candidates_str = candidates
                    .iter()
                    .map(|c| c.label.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                let raw_err = last_err.unwrap_or_else(|| "(no error captured)".to_string());
                let msg = classify_mysql_error(&raw_err, &config.database, &candidates_str);
                warn!("{}", msg);
                self.set_error("mysql", &msg, outage_class_from_raw(&raw_err))
                    .await;
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
                // Use read_string everywhere so binary-collation columns
                // from MySQL 8.0's SHOW/information_schema responses decode
                // cleanly. Without this, every "string" column came back as
                // empty and silently dropped — the bug we just isolated.
                let db_name = read_string(&row, "db").unwrap_or_default();
                let hostname = read_string(&row, "hostname").unwrap_or_default();
                let user = read_string(&row, "user").unwrap_or_default();
                let version = read_string(&row, "version").unwrap_or_default();
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
                let row_count = rows.len();
                // Try by column name first (`Database` is the standard for
                // SHOW DATABASES output) then fall back to positional index.
                // The two-tier helps when sqlx and the server disagree on
                // the column type or name.
                let dbs: Vec<String> = rows
                    .into_iter()
                    .filter_map(|r| read_string(&r, "Database").or_else(|| read_string(&r, 0)))
                    .collect();
                info!(
                    "MySQL probe — SHOW DATABASES returned {} rows, parsed {}: {:?}",
                    row_count,
                    dbs.len(),
                    dbs
                );
            }
            Err(e) => {
                warn!("MySQL probe (SHOW DATABASES) failed: {}", e);
            }
        }

        // What grants does the connected user actually have? If the LSP user
        // turns out to be different from the app's user (e.g., wildcard vs
        // host-specific user shadowing), this output makes it obvious.
        match sqlx::query("SHOW GRANTS FOR CURRENT_USER()")
            .fetch_all(&pool)
            .await
        {
            Ok(rows) => {
                let grants: Vec<String> = rows
                    .into_iter()
                    .filter_map(|r| read_string(&r, 0))
                    .collect();
                info!(
                    "MySQL probe — SHOW GRANTS for current user ({} grants): {:?}",
                    grants.len(),
                    grants
                );
            }
            Err(e) => {
                warn!("MySQL probe (SHOW GRANTS) failed: {}", e);
            }
        }

        // information_schema cross-check probe — bypasses SHOW commands
        // entirely. If this returns a non-zero count but SHOW TABLES below
        // returns zero, we know the problem is specific to SHOW (driver
        // quirk, connection state, etc.) and not actual visibility.
        // Dynamic SQL: the schema name is interpolated as a quoted string
        // literal with `'` doubled. Audited safe; sqlx 0.9 requires the
        // explicit AssertSqlSafe opt-in for non-'static query strings.
        match sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT COUNT(*) AS n FROM information_schema.tables WHERE table_schema = '{}'",
            config.database.replace('\'', "''")
        )))
        .fetch_one(&pool)
        .await
        {
            Ok(row) => {
                let n: i64 = row.try_get("n").unwrap_or(-1);
                info!(
                    "MySQL probe — information_schema.tables count for {:?} = {}",
                    config.database, n
                );
            }
            Err(e) => {
                warn!("MySQL probe (information_schema count) failed: {}", e);
            }
        }
        // Also try listing them via information_schema, so we can compare
        // shape against SHOW TABLES below.
        // Dynamic SQL: schema name as a quoted string literal, `'` doubled.
        // Audited safe; AssertSqlSafe satisfies the sqlx 0.9 guard.
        match sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT table_name FROM information_schema.tables WHERE table_schema = '{}' LIMIT 5",
            config.database.replace('\'', "''")
        )))
        .fetch_all(&pool)
        .await
        {
            Ok(rows) => {
                let names: Vec<String> = rows
                    .into_iter()
                    .filter_map(|r| read_string(&r, "table_name").or_else(|| read_string(&r, 0)))
                    .collect();
                info!(
                    "MySQL probe — first 5 tables via information_schema = {:?}",
                    names
                );
            }
            Err(e) => {
                warn!("MySQL probe (information_schema sample) failed: {}", e);
            }
        }

        // Get tables. Log row count + parsed count separately so we can
        // tell "MySQL returned 0 rows" (privilege issue / empty DB) from
        // "we failed to parse the column" (sqlx / driver weirdness).
        let table_rows = match sqlx::query("SHOW TABLES").fetch_all(&pool).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!("MySQL: SHOW TABLES failed: {}", e);
                return None;
            }
        };
        let row_count = table_rows.len();
        let tables: Vec<String> = table_rows
            .into_iter()
            .filter_map(|row| read_string(&row, 0))
            .collect();
        info!(
            "MySQL: SHOW TABLES returned {} rows, parsed {} table names",
            row_count,
            tables.len()
        );

        // Get columns for each table (with types)
        let mut columns = HashMap::new();
        let mut columns_with_types = HashMap::new();
        for table in &tables {
            // Dynamic SQL: `table` is a backtick-quoted identifier from
            // SHOW TABLES (the DB's own schema). Escape embedded backticks
            // and assert safe for the sqlx 0.9 guard.
            let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
                "SHOW COLUMNS FROM `{}`",
                table.replace('`', "``")
            )))
            .fetch_all(&pool)
            .await
            .ok()?;

            let mut col_names = Vec::new();
            let mut col_types = Vec::new();

            for row in rows {
                if let Some(field) = read_string(&row, "Field") {
                    let sql_type = read_string(&row, "Type").unwrap_or_default();
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

    /// Does `host` look like a Docker Compose service name (rather than a
    /// real hostname or IP)? Service names are the trigger for the Sail
    /// fallbacks: bare word, no dots, not `localhost`, not `127.0.0.1`.
    /// From the LSP's vantage point (outside the Docker network) a service
    /// name doesn't resolve, so we look for a mapped host port instead.
    fn is_docker_service_name(host: &str) -> bool {
        !host.is_empty()
            && !host.contains('.')
            && !host.eq_ignore_ascii_case("localhost")
            && host != "127.0.0.1"
    }

    /// Should Sail/Docker bind-IP detection run for this host?
    ///
    /// A superset of [`Self::is_docker_service_name`] that ALSO covers a
    /// plain loopback primary — `127.0.0.1` or `localhost`. Those are the
    /// default when `DB_HOST` is unset, yet the DB may actually live on
    /// another loopback IP (Herd owns `127.0.0.1`, so Sail is pinned to
    /// `127.0.0.2` via `APP_PORT`). Detection then adds the real
    /// `127.0.0.2` endpoint after the literal primary.
    ///
    /// Deliberately does NOT run for an explicit non-loopback host — a real
    /// IP (`192.168.x`, a public IP), a domain, or an explicit `127.0.0.2`:
    /// the user named the endpoint precisely, so we honour it verbatim
    /// (`is_docker_service_name` already rejects dotted hosts).
    fn should_attempt_sail_detection(host: &str) -> bool {
        Self::is_docker_service_name(host)
            || host.eq_ignore_ascii_case("localhost")
            || host == "127.0.0.1"
    }

    /// Build the ordered list of host candidates to try. The primary
    /// (configured) host is always first; if it looks like a Docker Compose
    /// service name we add `127.0.0.1` as a backstop so Sail / Docker
    /// Compose setups work without the LSP needing to be inside the Docker
    /// network.
    fn host_candidates(primary: &str) -> Vec<String> {
        let mut candidates = vec![primary.to_string()];
        if Self::is_docker_service_name(primary) {
            candidates.push("127.0.0.1".to_string());
        }
        candidates
    }

    /// The ordered `(host, port, success_note)` TCP endpoints to try, shared
    /// by the MySQL and Postgres candidate builders:
    /// 1. the configured `host:port` (primary),
    /// 2. any Sail/Docker endpoint detected from a compose file or APP_PORT
    ///    (see [`Self::detect_sail_endpoint`]) — this is what finds a DB
    ///    bound to a non-`127.0.0.1` loopback IP like `127.0.0.2`,
    /// 3. the `127.0.0.1` backstop (present only for service-name hosts).
    ///
    /// Deduplicated on `(host, port)` so a detected `127.0.0.1:<port>`
    /// doesn't also emit the backstop twice.
    fn tcp_endpoints(&self, config: &DatabaseConfig) -> Vec<(String, u16, Option<String>)> {
        const SAIL_BACKSTOP_NOTE: &str =
            "Looks like a Sail / Docker Compose setup — the LSP runs outside Docker, so the \
             service hostname doesn't resolve, but the mapped host port on 127.0.0.1 does.";

        let hosts = Self::host_candidates(&config.host);
        let mut eps: Vec<(String, u16, Option<String>)> = Vec::new();

        // 1. Primary configured host:port.
        eps.push((hosts[0].clone(), config.port, None));

        // 2. Detected Sail/Docker endpoint (carries its OWN host + port — the
        //    forwarded host port may differ from the driver's internal port).
        if let Some(ep) = self.detect_sail_endpoint(config) {
            if !eps.iter().any(|(h, p, _)| *h == ep.host && *p == ep.port) {
                eps.push((ep.host, ep.port, Some(ep.note)));
            }
        }

        // 3. 127.0.0.1 backstop — only present in `hosts` for service names.
        for h in hosts.iter().skip(1) {
            if !eps
                .iter()
                .any(|(host, p, _)| host == h && *p == config.port)
            {
                eps.push((h.clone(), config.port, Some(SAIL_BACKSTOP_NOTE.to_string())));
            }
        }

        eps
    }

    /// Detect a Sail/Docker TCP endpoint for the database when `config.host`
    /// is a Docker service name. Mike's setup is the motivating case: Herd
    /// owns `127.0.0.1`, so Docker/Sail confines every service to
    /// `127.0.0.2`, and the DB is at `127.0.0.2:3306` — a host the old
    /// `mysql` service-name + hardcoded `127.0.0.1` candidates never tried.
    ///
    /// Layered, first non-empty wins:
    /// 1. compose override (`docker-compose.override.yml` etc.),
    /// 2. base compose (`docker-compose.yml` etc.),
    /// 3. `APP_PORT` in `.env` (e.g. `127.0.0.2:80` → bind IP `127.0.0.2`).
    ///
    /// Fails open at every step (returns `None`) — never panics, never
    /// guesses — so a malformed compose file just falls through to the next
    /// layer, then to the `127.0.0.1` backstop.
    fn detect_sail_endpoint(&self, config: &DatabaseConfig) -> Option<DetectedEndpoint> {
        if !Self::should_attempt_sail_detection(&config.host) {
            return None;
        }

        const OVERRIDE: &[&str] = &[
            "docker-compose.override.yml",
            "docker-compose.override.yaml",
            "compose.override.yaml",
            "compose.override.yml",
        ];
        const BASE: &[&str] = &[
            "docker-compose.yml",
            "docker-compose.yaml",
            "compose.yaml",
            "compose.yml",
        ];

        self.detect_from_compose_files(config, OVERRIDE)
            .or_else(|| self.detect_from_compose_files(config, BASE))
            .or_else(|| self.detect_from_app_port(config))
    }

    /// Try each compose filename in order; the first that yields a forwarded
    /// endpoint for the DB service wins.
    fn detect_from_compose_files(
        &self,
        config: &DatabaseConfig,
        filenames: &[&str],
    ) -> Option<DetectedEndpoint> {
        for filename in filenames {
            let path = crate::config::resolve_worktree_fallback(&self.project_root, filename);
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            if let Some((host, port)) = self.parse_compose_ports(&content, config) {
                debug!("🗄️  Sail detection: {host}:{port} for DB service from {filename}");
                return Some(DetectedEndpoint {
                    note: format!(
                        "Sail / Docker Compose: DB port forwarded to {host}:{port}, \
                         detected from {filename}."
                    ),
                    host,
                    port,
                });
            }
        }
        None
    }

    /// `APP_PORT` (Sail's web-container bind) often carries the loopback IP
    /// the whole project is pinned to — e.g. `APP_PORT=127.0.0.2:80`. If it
    /// has an `IP:PORT` shape with a valid IPv4, reuse that IP and pair it
    /// with the DB's configured port.
    fn detect_from_app_port(&self, config: &DatabaseConfig) -> Option<DetectedEndpoint> {
        let app_port = self.resolve_env("APP_PORT")?;
        let (ip, _web_port) = app_port.split_once(':')?;
        let host = Self::normalize_bind_ip(ip.trim())?;
        // A plain 127.0.0.1 bind (or a wildcard that normalises to it) is
        // already covered by the 127.0.0.1 backstop — which, for a
        // service-name host, is always present and carries the correct
        // SAIL_BACKSTOP_NOTE. Emitting it here too would only add a
        // misattributed "from APP_PORT" note that dedupe then drops. Only a
        // non-loopback-default bind (e.g. 127.0.0.2) is worth a candidate.
        if host == "127.0.0.1" {
            return None;
        }
        Some(DetectedEndpoint {
            note: format!(
                "Sail / Docker Compose: DB host {host} taken from the APP_PORT bind IP in .env."
            ),
            host,
            port: config.port,
        })
    }

    /// Parse a compose file's YAML for the DB service's forwarded host
    /// endpoint. Targeted, dependency-free, and fail-open: locate
    /// `services:` → the DB service block (by `config.host`, else the known
    /// driver service names) → its `ports:` list → the entry whose CONTAINER
    /// (target) port equals `config.port`, returning that entry's host-side
    /// `(bind_ip, host_port)`. Returns `None` on any ambiguity.
    fn parse_compose_ports(&self, content: &str, config: &DatabaseConfig) -> Option<(String, u16)> {
        let lines: Vec<&str> = content.lines().collect();
        for name in self.compose_service_names(config) {
            if let Some(items) = Self::service_ports_items(&lines, &name) {
                if let Some(ep) = self.select_forwarded_endpoint(&items, config) {
                    return Some(ep);
                }
            }
        }
        None
    }

    /// The service names to look for, in precedence order: the configured
    /// host first, then the conventional service names for the driver.
    fn compose_service_names(&self, config: &DatabaseConfig) -> Vec<String> {
        let mut names = vec![config.host.clone()];
        let extra: &[&str] = match config.driver.as_str() {
            "mysql" | "mariadb" => &["mysql", "mariadb"],
            "pgsql" | "postgres" => &["pgsql", "postgres", "postgresql"],
            _ => &[],
        };
        for e in extra {
            if !names.iter().any(|n| n == e) {
                names.push((*e).to_string());
            }
        }
        names
    }

    /// Extract the raw `(indent, content)` lines of a named service's
    /// `ports:` list. Indentation-aware, comment/blank tolerant.
    fn service_ports_items(lines: &[&str], service: &str) -> Option<Vec<(usize, String)>> {
        let is_skippable = |l: &str| l.trim().is_empty() || l.trim_start().starts_with('#');

        // 1. Top-level `services:` key.
        let services_idx = lines
            .iter()
            .position(|l| indent_spaces(l) == 0 && line_is_key(l.trim_start(), "services"))?;

        // 2. The indent of `services:`'s direct children (first real child).
        let child_indent = lines[services_idx + 1..]
            .iter()
            .find(|l| !is_skippable(l))
            .map(|l| indent_spaces(l))?;
        if child_indent == 0 {
            return None;
        }

        // 3. Find the service header at `child_indent`.
        let mut i = services_idx + 1;
        let mut body_start = None;
        while i < lines.len() {
            let line = lines[i];
            if is_skippable(line) {
                i += 1;
                continue;
            }
            let ind = indent_spaces(line);
            if ind < child_indent {
                break; // left the services block
            }
            if ind == child_indent && line_is_key(line.trim_start(), service) {
                body_start = Some(i + 1);
                break;
            }
            i += 1;
        }
        let body_start = body_start?;

        // The service body's direct-child indent (first real line of the
        // body). `ports:` must live here — matching a bare `ports:` at ANY
        // deeper indent would read a NESTED one (e.g. under an `x-*` compose
        // extension field) that textually precedes the real service-level
        // list, returning a confident wrong binding. The first non-skippable
        // line after the header is either a body child (deeper) or a sibling/
        // dedent (<= child_indent, i.e. no body of its own) → None.
        let body_indent = lines[body_start..]
            .iter()
            .find(|l| !is_skippable(l))
            .map(|l| indent_spaces(l))?;
        if body_indent <= child_indent {
            return None;
        }

        // 4. Find `ports:` at the service body's direct-child indent only.
        let mut j = body_start;
        let mut ports = None;
        while j < lines.len() {
            let line = lines[j];
            if is_skippable(line) {
                j += 1;
                continue;
            }
            let ind = indent_spaces(line);
            if ind <= child_indent {
                break; // left the service body
            }
            // Skip nested content (deeper than a direct child, e.g. an
            // `x-meta:` block's own `ports:`); only a direct child counts.
            if ind == body_indent && line_is_key(line.trim_start(), "ports") {
                ports = Some((j + 1, ind));
                break;
            }
            j += 1;
        }
        let (ports_start, ports_indent) = ports?;

        // 5. Collect the ports list lines (deeper than `ports:`).
        let mut items = Vec::new();
        let mut k = ports_start;
        while k < lines.len() {
            let line = lines[k];
            if is_skippable(line) {
                k += 1;
                continue;
            }
            let ind = indent_spaces(line);
            if ind <= ports_indent {
                break;
            }
            items.push((ind, line.trim_start().to_string()));
            k += 1;
        }
        (!items.is_empty()).then_some(items)
    }

    /// Group the ports lines into entries (`-` starts an entry; deeper lines
    /// continue it) and return the first entry whose container port matches
    /// `config.port`, as `(bind_ip, host_port)`.
    fn select_forwarded_endpoint(
        &self,
        items: &[(usize, String)],
        config: &DatabaseConfig,
    ) -> Option<(String, u16)> {
        let mut groups: Vec<Vec<String>> = Vec::new();
        for (_, content) in items {
            if content.starts_with('-') {
                groups.push(vec![content.clone()]);
            } else {
                // Continuation line: attach to the current entry, or bail if
                // it appears before any `-` (malformed).
                groups.last_mut()?.push(content.clone());
            }
        }
        groups
            .iter()
            .find_map(|group| self.endpoint_from_group(group, config))
    }

    /// Parse a single ports entry — short form (`ip:host:container`) or long
    /// form (`target:`/`published:`/`host_ip:`) — into `(bind_ip, host_port)`
    /// if its container port matches `config.port`.
    fn endpoint_from_group(
        &self,
        group: &[String],
        config: &DatabaseConfig,
    ) -> Option<(String, u16)> {
        let head = group[0].strip_prefix('-')?.trim();
        let head_is_field = head
            .split_once(':')
            .map(|(k, _)| {
                matches!(
                    k.trim(),
                    "target" | "published" | "host_ip" | "protocol" | "mode" | "name"
                )
            })
            .unwrap_or(false);

        if group.len() > 1 || head.is_empty() || head_is_field {
            // Long-form mapping: build a small key→value map.
            let mut map: HashMap<String, String> = HashMap::new();
            if head_is_field {
                if let Some((k, v)) = head.split_once(':') {
                    map.insert(k.trim().to_string(), v.trim().to_string());
                }
            }
            for cont in &group[1..] {
                if let Some((k, v)) = cont.split_once(':') {
                    map.insert(k.trim().to_string(), v.trim().to_string());
                }
            }
            self.endpoint_from_longform(&map, config)
        } else {
            self.endpoint_from_shortform(head, config)
        }
    }

    /// Parse a short-form ports string: `container`, `host:container`, or
    /// `ip:host:container`, with an optional `/proto` suffix. `${VAR}` and
    /// `${VAR:-default}` are resolved BEFORE splitting (the `:-` default
    /// syntax itself contains a colon).
    fn endpoint_from_shortform(&self, raw: &str, config: &DatabaseConfig) -> Option<(String, u16)> {
        let unquoted = raw
            .trim()
            .trim_matches(|c: char| c == '\'' || c == '"')
            .trim();
        let resolved = self.resolve_compose_vars(unquoted);
        let no_proto = resolved.split('/').next().unwrap_or(&resolved).trim();
        let parts: Vec<&str> = no_proto.split(':').map(|p| p.trim()).collect();

        let (bind_ip, host_port_str, container_str) = match parts.as_slice() {
            // A bare container port maps to a RANDOM host port — unusable.
            [_container] => return None,
            [host_port, container] => ("", *host_port, *container),
            [ip, host_port, container] => (*ip, *host_port, *container),
            _ => return None,
        };

        let container: u16 = container_str.parse().ok()?;
        if container != config.port {
            return None;
        }
        let host_port: u16 = host_port_str.parse().ok()?;
        let host = Self::normalize_bind_ip(bind_ip)?;
        Some((host, host_port))
    }

    /// Parse a long-form ports mapping (`target`/`published`/`host_ip`) into
    /// `(bind_ip, host_port)` if `target` matches `config.port`.
    fn endpoint_from_longform(
        &self,
        map: &HashMap<String, String>,
        config: &DatabaseConfig,
    ) -> Option<(String, u16)> {
        let clean = |s: &str| -> String {
            self.resolve_compose_vars(
                s.trim()
                    .trim_matches(|c: char| c == '\'' || c == '"')
                    .trim(),
            )
        };
        let target: u16 = clean(map.get("target")?).trim().parse().ok()?;
        if target != config.port {
            return None;
        }
        let host_port: u16 = clean(map.get("published")?).trim().parse().ok()?;
        let host = match map.get("host_ip") {
            Some(ip) => Self::normalize_bind_ip(clean(ip).trim())?,
            None => "127.0.0.1".to_string(),
        };
        Some((host, host_port))
    }

    /// Resolve `${VAR}` and `${VAR:-default}` in a compose value via
    /// [`Self::resolve_env`], falling back to the default (or empty) when the
    /// var is unset. Bare `$VAR` is not handled (Sail uses the braced form).
    fn resolve_compose_vars(&self, s: &str) -> String {
        let Ok(re) = Regex::new(r"\$\{([A-Za-z_][A-Za-z0-9_]*)(?::-([^}]*))?\}") else {
            return s.to_string();
        };
        re.replace_all(s, |caps: &regex::Captures| {
            let var = &caps[1];
            let default = caps.get(2).map(|m| m.as_str()).unwrap_or("");
            self.resolve_env(var).unwrap_or_else(|| default.to_string())
        })
        .into_owned()
    }

    /// Normalise a compose/APP_PORT bind IP into a URL host:
    /// - empty → `127.0.0.1` (the compose default when no bind IP is given),
    /// - `0.0.0.0` / `::` / `*` → `127.0.0.1` (wildcard binds are reachable
    ///   on loopback),
    /// - a valid IPv4 literal → itself,
    /// - anything else (hostnames, IPv6 literals) → `None` (fail open).
    fn normalize_bind_ip(ip: &str) -> Option<String> {
        let ip = ip.trim();
        if ip.is_empty() || ip == "0.0.0.0" || ip == "::" || ip == "[::]" || ip == "*" {
            return Some("127.0.0.1".to_string());
        }
        ip.parse::<std::net::Ipv4Addr>()
            .ok()
            .map(|_| ip.to_string())
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
                    "mysql://{}@localhost/{}?socket={}",
                    userinfo(&config.username, &config.password),
                    config.database,
                    socket
                ),
                success_note: Some(
                    "Configured via unix_socket — bypasses TCP entirely.".to_string(),
                ),
            });
        }

        // TCP candidates. Always added — these are the fallback path when
        // neither URL nor socket are configured, OR when those fail. The
        // endpoint list is: configured host → any detected Sail/Docker bind
        // (e.g. 127.0.0.2, or a custom forwarded port) → 127.0.0.1 backstop.
        for (host, port, note) in self.tcp_endpoints(config) {
            out.push(ConnCandidate {
                label: format!("tcp {host}:{port}"),
                url: format!(
                    "mysql://{}@{}:{}/{}",
                    userinfo(&config.username, &config.password),
                    host,
                    port,
                    config.database
                ),
                success_note: note,
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
            // libpq-style socket connection: the socket path rides in a `host=`
            // query parameter, not the authority.
            //
            // The authority still needs a literal `localhost`, the same
            // placeholder the MySQL socket branch uses. The WHATWG URL rules
            // sqlx's `Url::parse` implements reject an empty host that follows
            // a userinfo component, so the shorter
            // `postgres://user@/db?host=/path` this branch used to emit came
            // back as *empty host* and the candidate could never connect —
            // independent of the credential, and true for every username. sqlx
            // reads `host=` after the authority and stores it as the socket, so
            // `localhost` is inert: verified against sqlx 0.9, the parsed
            // options carry socket `/tmp/.s.PGSQL.5432` with host `localhost`.
            out.push(ConnCandidate {
                label: format!("unix_socket={socket}"),
                url: format!(
                    "postgres://{}@localhost/{}?host={}",
                    userinfo(&config.username, &config.password),
                    config.database,
                    socket
                ),
                success_note: Some("Configured via unix_socket.".to_string()),
            });
        }

        for (host, port, note) in self.tcp_endpoints(config) {
            out.push(ConnCandidate {
                label: format!("tcp {host}:{port}"),
                url: format!(
                    "postgres://{}@{}:{}/{}",
                    userinfo(&config.username, &config.password),
                    host,
                    port,
                    config.database
                ),
                success_note: note,
            });
        }

        out
    }

    /// Fetch schema from PostgreSQL. Same candidate priority as
    /// `fetch_mysql_schema`: DB_URL → unix_socket → TCP with Sail fallback.
    async fn fetch_postgres_schema(
        &self,
        config: &DatabaseConfig,
        connect_timeout: Duration,
    ) -> Option<DatabaseSchema> {
        use sqlx::postgres::PgPoolOptions;

        let candidates = self.build_postgres_candidates(config);
        let mut last_err: Option<String> = None;
        let mut pool_opt = None;
        let primary_label = candidates.first().map(|c| c.label.clone());

        for cand in &candidates {
            match PgPoolOptions::new()
                .max_connections(1)
                .acquire_timeout(connect_timeout)
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
                let candidates_str = candidates
                    .iter()
                    .map(|c| c.label.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                let raw_err = last_err.unwrap_or_else(|| "(no error captured)".to_string());
                let msg = classify_postgres_error(&raw_err, &config.database, &candidates_str);
                warn!("{}", msg);
                self.set_error("pgsql", &msg, outage_class_from_raw(&raw_err))
                    .await;
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
        .filter_map(|row| read_string_pg(&row, "table_name"))
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
                if let Some(col_name) = read_string_pg(&row, "column_name") {
                    let sql_type = read_string_pg(&row, "data_type").unwrap_or_default();
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
    async fn fetch_sqlite_schema(
        &self,
        config: &DatabaseConfig,
        connect_timeout: Duration,
    ) -> Option<DatabaseSchema> {
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
            // Scenario 2-shaped: the "server" (filesystem) is fine, the
            // configured database itself is missing — same remediation
            // family as an unknown database ("check that it exists").
            self.set_error("sqlite", &msg, OutageClass::Rejected).await;
            return None;
        }

        let url = format!("sqlite:{}", db_path.display());

        let pool = match SqlitePoolOptions::new()
            .max_connections(1)
            .acquire_timeout(connect_timeout)
            .connect(&url)
            .await
        {
            Ok(p) => p,
            Err(e) => {
                let raw_err = e.to_string();
                let msg = format!("SQLite connection failed: {}. Check DB_DATABASE in .env", e);
                warn!("{}", msg);
                self.set_error("sqlite", &msg, outage_class_from_raw(&raw_err))
                    .await;
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
            // Dynamic SQL: `table` is a quoted string literal from
            // sqlite_master (the DB's own schema). Escape embedded quotes
            // and assert safe for the sqlx 0.9 guard.
            let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
                "PRAGMA table_info('{}')",
                table.replace('\'', "''")
            )))
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

    /// Fetch schema from SQL Server. tiberius has no built-in connect
    /// timeout, so both the raw TCP connect and the TDS handshake are
    /// wrapped in `tokio::time::timeout` — an unreachable sqlsrv host
    /// otherwise hangs on the OS default (minutes).
    async fn fetch_sqlserver_schema(
        &self,
        config: &DatabaseConfig,
        connect_timeout: Duration,
    ) -> Option<DatabaseSchema> {
        use tiberius::{AuthMethod, Client, Config};
        use tokio::net::TcpStream;
        use tokio_util::compat::TokioAsyncWriteCompatExt;

        let mut tib_config = Config::new();
        tib_config.host(&config.host);
        tib_config.port(config.port);
        tib_config.database(&config.database);
        tib_config.authentication(AuthMethod::sql_server(&config.username, &config.password));
        tib_config.trust_cert();

        let tcp = match tokio::time::timeout(
            connect_timeout,
            TcpStream::connect(tib_config.get_addr()),
        )
        .await
        {
            Ok(Ok(t)) => t,
            Ok(Err(e)) => {
                let msg = format!(
                    "SQL Server TCP connection failed: {}. Check DB_HOST, DB_PORT in .env",
                    e
                );
                warn!("{}", msg);
                self.set_error("sqlsrv", &msg, OutageClass::Unreachable)
                    .await;
                return None;
            }
            Err(_) => {
                let msg = format!(
                    "SQL Server TCP connection timed out after {:?}. Check DB_HOST, DB_PORT in .env",
                    connect_timeout
                );
                warn!("{}", msg);
                self.set_error("sqlsrv", &msg, OutageClass::Unreachable)
                    .await;
                return None;
            }
        };

        tcp.set_nodelay(true).ok();

        let mut client = match tokio::time::timeout(
            connect_timeout,
            Client::connect(tib_config, tcp.compat_write()),
        )
        .await
        {
            Ok(Ok(c)) => c,
            Ok(Err(e)) => {
                let raw_err = e.to_string();
                let msg = format!("SQL Server connection failed: {}. Check DB_DATABASE, DB_USERNAME, DB_PASSWORD in .env", e);
                warn!("{}", msg);
                self.set_error("sqlsrv", &msg, outage_class_from_raw(&raw_err))
                    .await;
                return None;
            }
            Err(_) => {
                let msg = format!(
                    "SQL Server handshake timed out after {:?}. Check DB_HOST, DB_PORT in .env",
                    connect_timeout
                );
                warn!("{}", msg);
                self.set_error("sqlsrv", &msg, OutageClass::Unreachable)
                    .await;
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
