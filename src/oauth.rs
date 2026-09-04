use std::path::Path;
use std::sync::{Arc, Mutex};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use url::Url;
use uuid::Uuid;

use crate::core::{Result, ShuttleError};

const MCP_SCOPE: &str = "mcp";
const CURRENT_OAUTH_SCHEMA_VERSION: u32 = 4;
const RATE_WINDOW: Duration = Duration::minutes(1);
const ACCESS_TOKEN_TTL: Duration = Duration::seconds(3600);
/// Sliding lifetime: every rotation moves the expiry this far into the future,
/// so an actively used connection never needs a second owner approval while an
/// abandoned one disappears on its own.
const REFRESH_TOKEN_TTL: Duration = Duration::days(30);
/// A consumed refresh token replayed inside this window is treated as a
/// concurrent request or a client retry, not as a stolen credential. Beyond it,
/// replay revokes the whole family.
const REFRESH_REPLAY_GRACE: Duration = Duration::seconds(30);
const REFRESH_TOKEN_PREFIX: &str = "stl_rt_";

#[derive(Clone)]
pub struct OAuthConfig {
    pub public_url: String,
    /// Owner-approval token for authorization-code issuance.
    ///
    /// CLI public URL mode requires this to be `Some`; `None` is reserved for
    /// programmatic or local-only runtimes that intentionally skip owner
    /// approval.
    pub admin_token: Option<String>,
    /// Dynamic client registration is disabled for public listeners unless
    /// explicitly opted in by configuration.
    pub allow_dynamic_registration: bool,
}

impl OAuthConfig {
    pub fn normalize_public_url(public_url: String) -> String {
        public_url.trim().trim_end_matches('/').to_owned()
    }

    pub fn resource_url(&self) -> String {
        format!("{}/mcp", self.public_url)
    }

    pub fn dynamic_registration_enabled(&self) -> bool {
        self.allow_dynamic_registration
    }
}

#[derive(Clone)]
pub struct OAuthStore {
    conn: Arc<Mutex<Connection>>,
}

impl OAuthStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path).map_err(to_store_error)?;
        conn.execute_batch(
            "PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000; PRAGMA journal_mode = WAL;",
        )
        .map_err(to_store_error)?;
        let store = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        store.init()?;
        Ok(store)
    }

    fn init(&self) -> Result<()> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|err| ShuttleError::Store(err.to_string()))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS oauth_schema_version (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                version INTEGER NOT NULL
            );",
        )
        .map_err(to_store_error)?;
        let version: u32 = conn
            .query_row(
                "SELECT version FROM oauth_schema_version WHERE id = 1",
                [],
                |row| row.get::<_, u32>(0),
            )
            .optional()
            .map_err(to_store_error)?
            .unwrap_or(0);
        if version > CURRENT_OAUTH_SCHEMA_VERSION {
            return Err(ShuttleError::Store(format!(
                "OAuth schema version {version} is newer than this binary supports"
            )));
        }
        if version == 0 {
            conn.execute(
                "INSERT OR IGNORE INTO oauth_schema_version (id, version) VALUES (1, 0)",
                [],
            )
            .map_err(to_store_error)?;
        }
        for target in (version + 1)..=CURRENT_OAUTH_SCHEMA_VERSION {
            let tx = conn.transaction().map_err(to_store_error)?;
            apply_oauth_migration(&tx, target)?;
            tx.execute(
                "UPDATE oauth_schema_version SET version = ?1 WHERE id = 1",
                [target],
            )
            .map_err(to_store_error)?;
            tx.commit().map_err(to_store_error)?;
        }
        purge_expired(&conn)?;
        Ok(())
    }

    pub fn register_client(&self, request: RegisterRequest) -> Result<RegisteredClient> {
        if request.redirect_uris.is_empty() {
            return Err(ShuttleError::Store(
                "redirect_uris must contain at least one URI".to_owned(),
            ));
        }
        let mut redirect_uris = request.redirect_uris;
        redirect_uris.sort();
        redirect_uris.dedup();
        for uri in &redirect_uris {
            validate_redirect_uri(uri)?;
        }
        let conn = self
            .conn
            .lock()
            .map_err(|err| ShuttleError::Store(err.to_string()))?;
        purge_expired(&conn)?;
        enforce_rate_limit(&conn, "registration", 30)?;
        let client = RegisteredClient {
            client_id: token(),
            client_secret: None,
            redirect_uris,
            client_name: request.client_name,
        };
        conn.execute(
            "INSERT INTO oauth_clients (client_id, client_secret, redirect_uris, client_name, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                client.client_id,
                client.client_secret,
                serde_json::to_string(&client.redirect_uris)
                    .map_err(|err| ShuttleError::Serialization(err.to_string()))?,
                client.client_name,
                Utc::now().to_rfc3339()
            ],
        )
        .map_err(to_store_error)?;
        Ok(client)
    }

    pub fn client_allows_redirect(&self, client_id: &str, redirect_uri: &str) -> Result<bool> {
        validate_redirect_uri(redirect_uri)?;
        let conn = self
            .conn
            .lock()
            .map_err(|err| ShuttleError::Store(err.to_string()))?;
        let redirect_uris = conn
            .query_row(
                "SELECT redirect_uris FROM oauth_clients WHERE client_id = ?1",
                params![client_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(to_store_error)?;
        let Some(redirect_uris) = redirect_uris else {
            return Ok(false);
        };
        let redirect_uris: Vec<String> = serde_json::from_str(&redirect_uris)
            .map_err(|err| ShuttleError::Serialization(err.to_string()))?;
        Ok(redirect_uris.iter().any(|uri| uri == redirect_uri))
    }

    pub fn create_code(&self, request: AuthorizeRequest) -> Result<String> {
        if request.response_type != "code" {
            return Err(ShuttleError::Store("response_type must be code".to_owned()));
        }
        if !self.client_allows_redirect(&request.client_id, &request.redirect_uri)? {
            return Err(ShuttleError::Store(
                "unknown client_id or redirect_uri".to_owned(),
            ));
        }
        if request.code_challenge_method.as_deref() != Some("S256") {
            return Err(ShuttleError::Store(
                "code_challenge_method must be S256".to_owned(),
            ));
        }
        let Some(code_challenge) = request.code_challenge else {
            return Err(ShuttleError::Store("missing code_challenge".to_owned()));
        };
        validate_redirect_uri(&request.redirect_uri)?;
        let scope = normalize_scope(request.scope);
        let code = token();
        let now = Utc::now();
        let conn = self
            .conn
            .lock()
            .map_err(|err| ShuttleError::Store(err.to_string()))?;
        purge_expired(&conn)?;
        enforce_rate_limit(&conn, &format!("authorize:{}", request.client_id), 30)?;
        conn.execute(
            "INSERT INTO oauth_codes (
                code, client_id, redirect_uri, code_challenge, code_challenge_method,
                scope, expires_at, created_at
             ) VALUES (?1, ?2, ?3, ?4, 'S256', ?5, ?6, ?7)",
            params![
                code,
                request.client_id,
                request.redirect_uri,
                code_challenge,
                scope,
                (now + Duration::minutes(10)).to_rfc3339(),
                now.to_rfc3339()
            ],
        )
        .map_err(to_store_error)?;
        Ok(code)
    }

    pub fn exchange_code(&self, request: TokenRequest) -> Result<TokenResponse> {
        if request.grant_type != "authorization_code" {
            return Err(ShuttleError::Store(
                "grant_type must be authorization_code".to_owned(),
            ));
        }
        let code = request
            .code
            .ok_or_else(|| ShuttleError::Store("missing code".to_owned()))?;
        let verifier = request
            .code_verifier
            .ok_or_else(|| ShuttleError::Store("missing code_verifier".to_owned()))?;
        let redirect_uri = request
            .redirect_uri
            .ok_or_else(|| ShuttleError::Store("missing redirect_uri".to_owned()))?;
        let mut conn = self
            .conn
            .lock()
            .map_err(|err| ShuttleError::Store(err.to_string()))?;
        purge_expired(&conn)?;
        enforce_rate_limit(&conn, &format!("token:{}", request.client_id), 20)?;
        let tx = conn.transaction().map_err(to_store_error)?;
        let stored = tx
            .query_row(
                "SELECT client_id, redirect_uri, code_challenge, scope, expires_at
                 FROM oauth_codes WHERE code = ?1 AND used_at IS NULL",
                params![code],
                |row| {
                    Ok(StoredCode {
                        client_id: row.get(0)?,
                        redirect_uri: row.get(1)?,
                        code_challenge: row.get(2)?,
                        scope: row.get(3)?,
                        expires_at: row.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(to_store_error)?;
        let Some(stored) = stored else {
            let exists = tx
                .query_row(
                    "SELECT 1 FROM oauth_codes WHERE code = ?1",
                    params![code],
                    |_| Ok(()),
                )
                .optional()
                .map_err(to_store_error)?
                .is_some();
            return Err(ShuttleError::Store(if exists {
                "code already used".to_owned()
            } else {
                "invalid code".to_owned()
            }));
        };

        if stored.client_id != request.client_id {
            return Err(ShuttleError::Store("invalid client_id".to_owned()));
        }
        if stored.redirect_uri != redirect_uri {
            return Err(ShuttleError::Store("invalid redirect_uri".to_owned()));
        }
        if parse_time(&stored.expires_at)? < Utc::now() {
            return Err(ShuttleError::Store("code expired".to_owned()));
        }
        if pkce_s256(&verifier) != stored.code_challenge {
            return Err(ShuttleError::Store("invalid code_verifier".to_owned()));
        }

        tx.execute(
            "UPDATE oauth_codes SET used_at = ?1 WHERE code = ?2",
            params![Utc::now().to_rfc3339(), code],
        )
        .map_err(to_store_error)?;
        let token = issue_tokens(&tx, &new_family_id(), &stored.client_id, &stored.scope)?;
        tx.commit().map_err(to_store_error)?;
        Ok(token)
    }

    /// Exchange a refresh token for a fresh access token and a rotated refresh
    /// token (RFC 6749 §6).
    ///
    /// Rotation is mandatory here because the token endpoint only serves public
    /// clients, so the refresh token is the whole credential. A consumed token
    /// presented again after [`REFRESH_REPLAY_GRACE`] is treated as evidence of
    /// theft and revokes the entire family.
    pub fn refresh_token(&self, request: TokenRequest) -> Result<TokenResponse> {
        if request.grant_type != "refresh_token" {
            return Err(ShuttleError::Store(
                "grant_type must be refresh_token".to_owned(),
            ));
        }
        let presented = request
            .refresh_token
            .ok_or_else(|| ShuttleError::Store("missing refresh_token".to_owned()))?;
        let mut conn = self
            .conn
            .lock()
            .map_err(|err| ShuttleError::Store(err.to_string()))?;
        purge_expired(&conn)?;
        enforce_rate_limit(&conn, &format!("token:{}", request.client_id), 20)?;
        let tx = conn.transaction().map_err(to_store_error)?;
        let token_hash = hash_token(&presented);
        let stored = tx
            .query_row(
                "SELECT family_id, client_id, scope, expires_at, consumed_at, revoked_at
                 FROM oauth_refresh_tokens WHERE token = ?1",
                params![token_hash],
                |row| {
                    Ok(StoredRefreshToken {
                        family_id: row.get(0)?,
                        client_id: row.get(1)?,
                        scope: row.get(2)?,
                        expires_at: row.get(3)?,
                        consumed_at: row.get(4)?,
                        revoked_at: row.get(5)?,
                    })
                },
            )
            .optional()
            .map_err(to_store_error)?;
        let Some(stored) = stored else {
            return Err(ShuttleError::Store("invalid refresh token".to_owned()));
        };

        let now = Utc::now();
        if stored.revoked_at.is_some() {
            return Err(ShuttleError::Store("refresh token revoked".to_owned()));
        }
        if let Some(consumed_at) = &stored.consumed_at {
            // Concurrent refreshes and client retries land here; only a replay
            // outside the grace window is treated as a compromise.
            if parse_time(consumed_at)? + REFRESH_REPLAY_GRACE >= now {
                return Err(ShuttleError::Store("refresh token already used".to_owned()));
            }
            revoke_family(&tx, &stored.family_id, now)?;
            tx.commit().map_err(to_store_error)?;
            tracing::warn!(
                client_id = %stored.client_id,
                family_id = %stored.family_id,
                "revoked OAuth refresh token family after replay of a consumed token"
            );
            return Err(ShuttleError::Store(
                "refresh token replay detected".to_owned(),
            ));
        }
        if parse_time(&stored.expires_at)? < now {
            return Err(ShuttleError::Store("refresh token expired".to_owned()));
        }
        if stored.client_id != request.client_id {
            return Err(ShuttleError::Store("invalid client_id".to_owned()));
        }
        if let Some(requested) = request.scope.as_deref() {
            if requested.trim() != stored.scope {
                return Err(ShuttleError::Store("invalid scope".to_owned()));
            }
        }

        tx.execute(
            "UPDATE oauth_refresh_tokens SET consumed_at = ?1 WHERE token = ?2",
            params![now.to_rfc3339(), token_hash],
        )
        .map_err(to_store_error)?;
        let token = issue_tokens(&tx, &stored.family_id, &stored.client_id, &stored.scope)?;
        tx.commit().map_err(to_store_error)?;
        Ok(token)
    }

    pub fn validate_access_token(&self, bearer_token: &str) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|err| ShuttleError::Store(err.to_string()))?;
        purge_expired(&conn)?;
        let token_hash = hash_token(bearer_token);
        let row = conn
            .query_row(
                "SELECT scope, expires_at, revoked_at FROM oauth_tokens WHERE token = ?1",
                params![token_hash],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(to_store_error)?;
        let Some((scope, expires_at, revoked_at)) = row else {
            return Ok(false);
        };
        Ok(revoked_at.is_none()
            && scope.split_whitespace().any(|scope| scope == MCP_SCOPE)
            && parse_time(&expires_at)? > Utc::now())
    }

    /// Revoke a presented token (RFC 7009).
    ///
    /// A refresh token takes its whole family with it, including the access
    /// tokens issued from it, as RFC 7009 §2.1 recommends. An access token is
    /// revoked on its own so a client can drop one credential without ending
    /// the session.
    pub fn revoke_token(&self, token: &str, token_type_hint: Option<&str>) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|err| ShuttleError::Store(err.to_string()))?;
        let token_hash = hash_token(token);
        let now = Utc::now();
        // The hint is only a lookup order; RFC 7009 requires the other type to
        // be searched when it misses.
        let refresh_first =
            token.starts_with(REFRESH_TOKEN_PREFIX) || token_type_hint == Some("refresh_token");
        if refresh_first {
            if revoke_refresh_family(&conn, &token_hash, now)? {
                return Ok(true);
            }
            return revoke_access(&conn, &token_hash, now);
        }
        if revoke_access(&conn, &token_hash, now)? {
            return Ok(true);
        }
        revoke_refresh_family(&conn, &token_hash, now)
    }

    pub fn cleanup_expired(&self) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|err| ShuttleError::Store(err.to_string()))?;
        purge_expired(&conn)
    }
}

fn apply_oauth_migration(conn: &Connection, version: u32) -> Result<()> {
    match version {
        1 => conn
            .execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS oauth_clients (
                    client_id TEXT PRIMARY KEY NOT NULL,
                    client_secret TEXT,
                    redirect_uris TEXT NOT NULL,
                    client_name TEXT,
                    created_at TEXT NOT NULL
                );

                CREATE TABLE IF NOT EXISTS oauth_codes (
                    code TEXT PRIMARY KEY NOT NULL,
                    client_id TEXT NOT NULL,
                    redirect_uri TEXT NOT NULL,
                    code_challenge TEXT NOT NULL,
                    code_challenge_method TEXT NOT NULL,
                    scope TEXT NOT NULL,
                    expires_at TEXT NOT NULL,
                    used_at TEXT,
                    created_at TEXT NOT NULL
                );

                -- The token column contains a SHA-256 digest, never a bearer
                -- token. The column name is retained for old DB compatibility.
                CREATE TABLE IF NOT EXISTS oauth_tokens (
                    token TEXT PRIMARY KEY NOT NULL,
                    client_id TEXT NOT NULL,
                    scope TEXT NOT NULL,
                    expires_at TEXT NOT NULL,
                    created_at TEXT NOT NULL
                );
                "#,
            )
            .map_err(to_store_error),
        2 => {
            ensure_oauth_column(conn, "oauth_tokens", "revoked_at", "TEXT")?;
            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS oauth_rate_limits (
                    key TEXT PRIMARY KEY NOT NULL,
                    window_started TEXT NOT NULL,
                    request_count INTEGER NOT NULL
                );
                "#,
            )
            .map_err(to_store_error)
        }
        3 => {
            // Hashing is idempotent because all newly written values have a
            // fixed URL-safe digest length, while legacy values are stl_... .
            let mut stmt = conn
                .prepare("SELECT token FROM oauth_tokens")
                .map_err(to_store_error)?;
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(to_store_error)?;
            let legacy_tokens = rows
                .filter_map(|row| row.ok())
                .filter(|value| value.starts_with("stl_"))
                .collect::<Vec<_>>();
            drop(stmt);
            for raw in legacy_tokens {
                conn.execute(
                    "UPDATE oauth_tokens SET token = ?1 WHERE token = ?2",
                    params![hash_token(&raw), raw],
                )
                .map_err(to_store_error)?;
            }
            Ok(())
        }
        4 => {
            // Access tokens issued before this migration have no family, so a
            // family revocation cannot reach them. They expire within an hour.
            ensure_oauth_column(conn, "oauth_tokens", "family_id", "TEXT")?;
            conn.execute_batch(
                r#"
                -- The token column contains a SHA-256 digest, never a refresh
                -- token, matching the oauth_tokens convention.
                CREATE TABLE IF NOT EXISTS oauth_refresh_tokens (
                    token TEXT PRIMARY KEY NOT NULL,
                    family_id TEXT NOT NULL,
                    client_id TEXT NOT NULL,
                    scope TEXT NOT NULL,
                    expires_at TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    consumed_at TEXT,
                    revoked_at TEXT
                );

                CREATE INDEX IF NOT EXISTS oauth_refresh_tokens_family
                    ON oauth_refresh_tokens (family_id);

                CREATE INDEX IF NOT EXISTS oauth_tokens_family
                    ON oauth_tokens (family_id);
                "#,
            )
            .map_err(to_store_error)
        }
        _ => Err(ShuttleError::Store(format!(
            "unknown OAuth migration {version}"
        ))),
    }
}

fn ensure_oauth_column(
    conn: &Connection,
    table: &str,
    column: &str,
    column_type: &str,
) -> Result<()> {
    let exists = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(to_store_error)?
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(to_store_error)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(to_store_error)?
        .iter()
        .any(|name| name == column);
    if !exists {
        conn.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {column_type}"),
            [],
        )
        .map_err(to_store_error)?;
    }
    Ok(())
}

fn enforce_rate_limit(conn: &Connection, key: &str, max_requests: i64) -> Result<()> {
    let now = Utc::now();
    let existing = conn
        .query_row(
            "SELECT window_started, request_count FROM oauth_rate_limits WHERE key = ?1",
            params![key],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()
        .map_err(to_store_error)?;
    let (window_started, request_count) = match existing {
        Some((started, count)) if parse_time(&started)? + RATE_WINDOW > now => (started, count),
        _ => (now.to_rfc3339(), 0),
    };
    if request_count >= max_requests {
        return Err(ShuttleError::Store("rate limit exceeded".to_owned()));
    }
    conn.execute(
        "INSERT INTO oauth_rate_limits (key, window_started, request_count)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET window_started = excluded.window_started,
             request_count = excluded.request_count",
        params![key, window_started, request_count + 1],
    )
    .map_err(to_store_error)?;
    Ok(())
}

fn validate_redirect_uri(value: &str) -> Result<()> {
    if value.contains('*') {
        return Err(ShuttleError::Store(
            "redirect URI wildcards are not allowed".to_owned(),
        ));
    }
    let parsed = Url::parse(value)
        .map_err(|_| ShuttleError::Store("redirect URI is malformed".to_owned()))?;
    if parsed.fragment().is_some() || !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(ShuttleError::Store(
            "redirect URI must not contain a fragment or userinfo".to_owned(),
        ));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| ShuttleError::Store("redirect URI must include a host".to_owned()))?;
    let loopback = matches!(parsed.host(), Some(url::Host::Ipv4(ip)) if ip.is_loopback())
        || matches!(parsed.host(), Some(url::Host::Ipv6(ip)) if ip.is_loopback())
        || host.eq_ignore_ascii_case("localhost");
    if parsed.scheme() != "https" && !(parsed.scheme() == "http" && loopback) {
        return Err(ShuttleError::Store(
            "redirect URI must use HTTPS except for loopback URIs".to_owned(),
        ));
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct RegisterRequest {
    #[serde(default)]
    pub redirect_uris: Vec<String>,
    pub client_name: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RegisteredClient {
    pub client_id: String,
    pub client_secret: Option<String>,
    pub redirect_uris: Vec<String>,
    pub client_name: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct AuthorizeRequest {
    pub response_type: String,
    pub client_id: String,
    pub redirect_uri: String,
    pub state: Option<String>,
    pub scope: Option<String>,
    pub code_challenge: Option<String>,
    pub code_challenge_method: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AuthorizeForm {
    pub admin_token: String,
    pub response_type: String,
    pub client_id: String,
    pub redirect_uri: String,
    pub state: Option<String>,
    pub scope: Option<String>,
    pub code_challenge: Option<String>,
    pub code_challenge_method: Option<String>,
}

impl From<AuthorizeForm> for AuthorizeRequest {
    fn from(form: AuthorizeForm) -> Self {
        Self {
            response_type: form.response_type,
            client_id: form.client_id,
            redirect_uri: form.redirect_uri,
            state: form.state,
            scope: form.scope,
            code_challenge: form.code_challenge,
            code_challenge_method: form.code_challenge_method,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct TokenRequest {
    pub grant_type: String,
    pub client_id: String,
    /// Required by the authorization-code grant and absent from refresh
    /// requests, so it is validated per grant rather than during extraction.
    #[serde(default)]
    pub redirect_uri: Option<String>,
    pub code: Option<String>,
    pub code_verifier: Option<String>,
    pub refresh_token: Option<String>,
    pub scope: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RevokeRequest {
    pub token: String,
    #[serde(default)]
    pub token_type_hint: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub token_type: &'static str,
    pub expires_in: i64,
    pub refresh_token: String,
    pub scope: String,
}

pub fn authorization_server_metadata(config: &OAuthConfig) -> Value {
    json!({
        "schema_version": crate::api::SCHEMA_VERSION,
        "issuer": config.public_url,
        "authorization_endpoint": format!("{}/oauth/authorize", config.public_url),
        "token_endpoint": format!("{}/oauth/token", config.public_url),
        "registration_endpoint": format!("{}/oauth/register", config.public_url),
        "revocation_endpoint": format!("{}/oauth/revoke", config.public_url),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
        "scopes_supported": [MCP_SCOPE],
    })
}

pub fn protected_resource_metadata(config: &OAuthConfig) -> Value {
    json!({
        "schema_version": crate::api::SCHEMA_VERSION,
        "resource": config.resource_url(),
        "authorization_servers": [config.public_url],
        "scopes_supported": [MCP_SCOPE],
        "bearer_methods_supported": ["header"],
    })
}

/// Build the OAuth 2.0 authorization-code redirect URL (RFC 6749 §4.1.2).
///
/// `code` and `state` are serialized as query components. The values are
/// percent-encoded at the redirect boundary so reserved characters in opaque
/// client state cannot change the query structure.
pub fn authorize_redirect(redirect_uri: &str, code: &str, state: Option<&str>) -> String {
    let mut target = format!(
        "{}{}code={}",
        redirect_uri,
        if redirect_uri.contains('?') { "&" } else { "?" },
        query_component(code)
    );
    if let Some(state) = state {
        target.push_str("&state=");
        target.push_str(&query_component(state));
    }
    target
}

fn query_component(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

/// Issue an access token and a refresh token that both belong to `family_id`.
///
/// Every rotation stays in the family that the authorization code created, so
/// revoking the family reaches every credential derived from that one owner
/// approval.
fn issue_tokens(
    conn: &Connection,
    family_id: &str,
    client_id: &str,
    scope: &str,
) -> Result<TokenResponse> {
    let access_token = token();
    let refresh_token = refresh_token();
    let now = Utc::now();
    conn.execute(
        "INSERT INTO oauth_tokens (token, client_id, scope, expires_at, created_at, family_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            hash_token(&access_token),
            client_id,
            scope,
            (now + ACCESS_TOKEN_TTL).to_rfc3339(),
            now.to_rfc3339(),
            family_id
        ],
    )
    .map_err(to_store_error)?;
    conn.execute(
        "INSERT INTO oauth_refresh_tokens (token, family_id, client_id, scope, expires_at, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            hash_token(&refresh_token),
            family_id,
            client_id,
            scope,
            (now + REFRESH_TOKEN_TTL).to_rfc3339(),
            now.to_rfc3339()
        ],
    )
    .map_err(to_store_error)?;
    Ok(TokenResponse {
        access_token,
        token_type: "Bearer",
        expires_in: ACCESS_TOKEN_TTL.num_seconds(),
        refresh_token,
        scope: scope.to_owned(),
    })
}

/// Revoke every unrevoked credential in a family.
fn revoke_family(conn: &Connection, family_id: &str, now: DateTime<Utc>) -> Result<()> {
    let now = now.to_rfc3339();
    conn.execute(
        "UPDATE oauth_refresh_tokens SET revoked_at = ?1
         WHERE family_id = ?2 AND revoked_at IS NULL",
        params![now, family_id],
    )
    .map_err(to_store_error)?;
    conn.execute(
        "UPDATE oauth_tokens SET revoked_at = ?1
         WHERE family_id = ?2 AND revoked_at IS NULL",
        params![now, family_id],
    )
    .map_err(to_store_error)?;
    Ok(())
}

fn revoke_refresh_family(conn: &Connection, token_hash: &str, now: DateTime<Utc>) -> Result<bool> {
    let family_id = conn
        .query_row(
            "SELECT family_id FROM oauth_refresh_tokens WHERE token = ?1",
            params![token_hash],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(to_store_error)?;
    let Some(family_id) = family_id else {
        return Ok(false);
    };
    revoke_family(conn, &family_id, now)?;
    Ok(true)
}

fn revoke_access(conn: &Connection, token_hash: &str, now: DateTime<Utc>) -> Result<bool> {
    let changed = conn
        .execute(
            "UPDATE oauth_tokens SET revoked_at = ?1
             WHERE token = ?2 AND revoked_at IS NULL",
            params![now.to_rfc3339(), token_hash],
        )
        .map_err(to_store_error)?;
    Ok(changed != 0)
}

fn normalize_scope(scope: Option<String>) -> String {
    let scope = scope.unwrap_or_else(|| MCP_SCOPE.to_owned());
    if scope.split_whitespace().any(|scope| scope == MCP_SCOPE) {
        scope
    } else {
        MCP_SCOPE.to_owned()
    }
}

fn token() -> String {
    format!("stl_{}", Uuid::new_v4().simple())
}

/// Refresh tokens carry their own prefix so revocation can pick the right table
/// first and so a value found in a log or a client store is identifiable.
fn refresh_token() -> String {
    format!("{}{}", REFRESH_TOKEN_PREFIX, Uuid::new_v4().simple())
}

fn new_family_id() -> String {
    Uuid::new_v4().simple().to_string()
}

fn hash_token(value: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(value.as_bytes()))
}

fn pkce_s256(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

fn parse_time(value: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|time| time.with_timezone(&Utc))
        .map_err(|err| ShuttleError::Store(err.to_string()))
}

fn to_store_error(err: rusqlite::Error) -> ShuttleError {
    ShuttleError::Store(err.to_string())
}

fn purge_expired(conn: &Connection) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "DELETE FROM oauth_codes WHERE expires_at < ?1",
        params![now],
    )
    .map_err(to_store_error)?;
    conn.execute(
        "DELETE FROM oauth_tokens WHERE expires_at < ?1 OR revoked_at IS NOT NULL",
        params![now],
    )
    .map_err(to_store_error)?;
    // Consumed and revoked refresh tokens are kept until they expire on their
    // own. Deleting them early would turn a replayed stolen token into an
    // unknown token and defeat reuse detection.
    conn.execute(
        "DELETE FROM oauth_refresh_tokens WHERE expires_at < ?1",
        params![now],
    )
    .map_err(to_store_error)?;
    Ok(())
}

struct StoredCode {
    client_id: String,
    redirect_uri: String,
    code_challenge: String,
    scope: String,
    expires_at: String,
}

struct StoredRefreshToken {
    family_id: String,
    client_id: String,
    scope: String,
    expires_at: String,
    consumed_at: Option<String>,
    revoked_at: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_uses_public_url() {
        let config = OAuthConfig {
            public_url: "https://shuttle.example.test".to_owned(),
            admin_token: None,
            allow_dynamic_registration: false,
        };

        assert_eq!(
            protected_resource_metadata(&config)["resource"],
            "https://shuttle.example.test/mcp"
        );
        assert_eq!(
            authorization_server_metadata(&config)["token_endpoint"],
            "https://shuttle.example.test/oauth/token"
        );
    }

    #[test]
    fn authorize_redirect_encodes_state_as_query_component() {
        let url = authorize_redirect(
            "https://claude.ai/api/mcp/auth_callback",
            "stl_abc123",
            Some("opaque=value+with/special&fragment#part"),
        );
        assert_eq!(
            url,
            "https://claude.ai/api/mcp/auth_callback?code=stl_abc123&state=opaque%3Dvalue%2Bwith%2Fspecial%26fragment%23part"
        );
    }

    #[test]
    fn authorize_redirect_omits_state_when_absent() {
        let url = authorize_redirect(
            "https://claude.ai/api/mcp/auth_callback",
            "stl_abc123",
            None,
        );
        assert_eq!(
            url,
            "https://claude.ai/api/mcp/auth_callback?code=stl_abc123"
        );
        assert!(!url.contains("state="));
    }

    const REDIRECT_URI: &str = "https://client.example.test/callback";
    const VERIFIER: &str = "abc123abc123abc123abc123abc123abc123abc123abc123";

    fn register(store: &OAuthStore) -> String {
        store
            .register_client(RegisterRequest {
                redirect_uris: vec![REDIRECT_URI.to_owned()],
                client_name: Some("client".to_owned()),
            })
            .unwrap()
            .client_id
    }

    fn authorize(store: &OAuthStore, client_id: &str) -> String {
        store
            .create_code(AuthorizeRequest {
                response_type: "code".to_owned(),
                client_id: client_id.to_owned(),
                redirect_uri: REDIRECT_URI.to_owned(),
                state: None,
                scope: Some(MCP_SCOPE.to_owned()),
                code_challenge: Some(pkce_s256(VERIFIER)),
                code_challenge_method: Some("S256".to_owned()),
            })
            .unwrap()
    }

    fn code_request(client_id: &str, code: &str) -> TokenRequest {
        TokenRequest {
            grant_type: "authorization_code".to_owned(),
            client_id: client_id.to_owned(),
            redirect_uri: Some(REDIRECT_URI.to_owned()),
            code: Some(code.to_owned()),
            code_verifier: Some(VERIFIER.to_owned()),
            refresh_token: None,
            scope: None,
        }
    }

    fn refresh_request(client_id: &str, refresh_token: &str) -> TokenRequest {
        TokenRequest {
            grant_type: "refresh_token".to_owned(),
            client_id: client_id.to_owned(),
            redirect_uri: None,
            code: None,
            code_verifier: None,
            refresh_token: Some(refresh_token.to_owned()),
            scope: None,
        }
    }

    /// Register a client and walk the authorization-code grant once.
    fn grant(store: &OAuthStore) -> (String, TokenResponse) {
        let client_id = register(store);
        let code = authorize(store, &client_id);
        let response = store
            .exchange_code(code_request(&client_id, &code))
            .unwrap();
        (client_id, response)
    }

    /// Move a refresh token's consumption timestamp into the past so a replay
    /// lands outside the grace window without sleeping in the test.
    fn backdate_consumption(path: &Path, refresh_token: &str, age: Duration) {
        let conn = Connection::open(path).unwrap();
        let changed = conn
            .execute(
                "UPDATE oauth_refresh_tokens SET consumed_at = ?1 WHERE token = ?2",
                params![(Utc::now() - age).to_rfc3339(), hash_token(refresh_token)],
            )
            .unwrap();
        assert_eq!(changed, 1);
    }

    #[test]
    fn code_exchange_validates_pkce() {
        let dir = tempfile::tempdir().unwrap();
        let store = OAuthStore::open(dir.path().join("shuttle.db")).unwrap();
        let (_, response) = grant(&store);

        assert!(store.validate_access_token(&response.access_token).unwrap());
    }

    #[test]
    fn code_exchange_rejects_reused_code() {
        let dir = tempfile::tempdir().unwrap();
        let store = OAuthStore::open(dir.path().join("shuttle.db")).unwrap();
        let client_id = register(&store);
        let code = authorize(&store, &client_id);

        store
            .exchange_code(code_request(&client_id, &code))
            .unwrap();
        let err = store
            .exchange_code(code_request(&client_id, &code))
            .unwrap_err();

        assert!(err.to_string().contains("code already used"));
    }

    #[test]
    fn redirect_uri_policy_rejects_unsafe_forms_and_allows_loopback_http() {
        let dir = tempfile::tempdir().unwrap();
        let store = OAuthStore::open(dir.path().join("oauth.db")).unwrap();
        for redirect_uri in [
            "http://client.example.test/callback",
            "https://client.example.test/callback#fragment",
            "https://user:password@client.example.test/callback",
            "https://client.example.test/*",
            "not a url",
        ] {
            assert!(store
                .register_client(RegisterRequest {
                    redirect_uris: vec![redirect_uri.to_owned()],
                    client_name: None,
                })
                .is_err());
        }
        assert!(store
            .register_client(RegisterRequest {
                redirect_uris: vec!["http://127.0.0.1:3456/callback".to_owned()],
                client_name: None,
            })
            .is_ok());
    }

    #[test]
    fn tokens_are_hashed_and_revocable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("oauth.db");
        let store = OAuthStore::open(&path).unwrap();
        let (_, response) = grant(&store);

        assert!(store.validate_access_token(&response.access_token).unwrap());
        let conn = Connection::open(&path).unwrap();
        let stored: String = conn
            .query_row("SELECT token FROM oauth_tokens", [], |row| row.get(0))
            .unwrap();
        assert_ne!(stored, response.access_token);
        assert!(!stored.starts_with("stl_"));
        assert!(store.revoke_token(&response.access_token, None).unwrap());
        assert!(!store.validate_access_token(&response.access_token).unwrap());
    }

    #[test]
    fn store_validates_oauth_grant_shape() {
        let dir = tempfile::tempdir().unwrap();
        let store = OAuthStore::open(dir.path().join("shuttle.db")).unwrap();
        let client_id = register(&store);

        assert!(store
            .create_code(AuthorizeRequest {
                response_type: "token".to_owned(),
                client_id: client_id.clone(),
                redirect_uri: REDIRECT_URI.to_owned(),
                state: None,
                scope: Some(MCP_SCOPE.to_owned()),
                code_challenge: Some(pkce_s256(VERIFIER)),
                code_challenge_method: Some("S256".to_owned()),
            })
            .unwrap_err()
            .to_string()
            .contains("response_type must be code"));

        assert!(store
            .exchange_code(refresh_request(&client_id, "stl_rt_missing"))
            .unwrap_err()
            .to_string()
            .contains("grant_type must be authorization_code"));

        assert!(store
            .refresh_token(code_request(&client_id, "stl_missing"))
            .unwrap_err()
            .to_string()
            .contains("grant_type must be refresh_token"));
    }

    #[test]
    fn code_exchange_issues_a_hashed_refresh_token() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shuttle.db");
        let store = OAuthStore::open(&path).unwrap();
        let (_, response) = grant(&store);

        assert!(response.refresh_token.starts_with(REFRESH_TOKEN_PREFIX));
        let conn = Connection::open(&path).unwrap();
        let stored: String = conn
            .query_row("SELECT token FROM oauth_refresh_tokens", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_ne!(stored, response.refresh_token);
        assert!(!stored.starts_with(REFRESH_TOKEN_PREFIX));
    }

    #[test]
    fn refresh_rotates_credentials_and_keeps_the_previous_access_token() {
        let dir = tempfile::tempdir().unwrap();
        let store = OAuthStore::open(dir.path().join("shuttle.db")).unwrap();
        let (client_id, first) = grant(&store);

        let second = store
            .refresh_token(refresh_request(&client_id, &first.refresh_token))
            .unwrap();

        assert_ne!(second.access_token, first.access_token);
        assert_ne!(second.refresh_token, first.refresh_token);
        assert_eq!(second.scope, first.scope);
        assert!(store.validate_access_token(&second.access_token).unwrap());
        // In-flight requests holding the previous access token keep working
        // until it expires on its own.
        assert!(store.validate_access_token(&first.access_token).unwrap());
    }

    #[test]
    fn refresh_replay_inside_the_grace_window_keeps_the_family_alive() {
        let dir = tempfile::tempdir().unwrap();
        let store = OAuthStore::open(dir.path().join("shuttle.db")).unwrap();
        let (client_id, first) = grant(&store);
        let second = store
            .refresh_token(refresh_request(&client_id, &first.refresh_token))
            .unwrap();

        let err = store
            .refresh_token(refresh_request(&client_id, &first.refresh_token))
            .unwrap_err();

        assert!(err.to_string().contains("refresh token already used"));
        assert!(store.validate_access_token(&second.access_token).unwrap());
        // The rotated token still works, so a retry or a concurrent request
        // does not cost the owner a re-approval.
        assert!(store
            .refresh_token(refresh_request(&client_id, &second.refresh_token))
            .is_ok());
    }

    #[test]
    fn refresh_replay_after_the_grace_window_revokes_the_family() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shuttle.db");
        let store = OAuthStore::open(&path).unwrap();
        let (client_id, first) = grant(&store);
        let second = store
            .refresh_token(refresh_request(&client_id, &first.refresh_token))
            .unwrap();
        backdate_consumption(&path, &first.refresh_token, Duration::minutes(5));

        let err = store
            .refresh_token(refresh_request(&client_id, &first.refresh_token))
            .unwrap_err();

        assert!(err.to_string().contains("refresh token replay detected"));
        assert!(!store.validate_access_token(&first.access_token).unwrap());
        assert!(!store.validate_access_token(&second.access_token).unwrap());
        assert!(store
            .refresh_token(refresh_request(&client_id, &second.refresh_token))
            .unwrap_err()
            .to_string()
            .contains("refresh token revoked"));
    }

    #[test]
    fn refresh_rejects_a_mismatched_client_id_and_scope() {
        let dir = tempfile::tempdir().unwrap();
        let store = OAuthStore::open(dir.path().join("shuttle.db")).unwrap();
        let (client_id, first) = grant(&store);
        let other_client_id = register(&store);

        assert!(store
            .refresh_token(refresh_request(&other_client_id, &first.refresh_token))
            .unwrap_err()
            .to_string()
            .contains("invalid client_id"));

        let mut widened = refresh_request(&client_id, &first.refresh_token);
        widened.scope = Some("mcp admin".to_owned());
        assert!(store
            .refresh_token(widened)
            .unwrap_err()
            .to_string()
            .contains("invalid scope"));

        // Neither rejection consumed the token.
        let mut echoed = refresh_request(&client_id, &first.refresh_token);
        echoed.scope = Some(MCP_SCOPE.to_owned());
        assert!(store.refresh_token(echoed).is_ok());
    }

    #[test]
    fn refresh_rejects_unknown_and_expired_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shuttle.db");
        let store = OAuthStore::open(&path).unwrap();
        let (client_id, first) = grant(&store);

        assert!(store
            .refresh_token(refresh_request(&client_id, "stl_rt_unknown"))
            .is_err());

        let conn = Connection::open(&path).unwrap();
        conn.execute(
            "UPDATE oauth_refresh_tokens SET expires_at = ?1 WHERE token = ?2",
            params![
                (Utc::now() - Duration::days(1)).to_rfc3339(),
                hash_token(&first.refresh_token)
            ],
        )
        .unwrap();

        assert!(store
            .refresh_token(refresh_request(&client_id, &first.refresh_token))
            .is_err());
    }

    #[test]
    fn revoking_a_refresh_token_ends_the_whole_family() {
        let dir = tempfile::tempdir().unwrap();
        let store = OAuthStore::open(dir.path().join("shuttle.db")).unwrap();
        let (client_id, first) = grant(&store);
        let second = store
            .refresh_token(refresh_request(&client_id, &first.refresh_token))
            .unwrap();

        assert!(store.revoke_token(&second.refresh_token, None).unwrap());

        assert!(!store.validate_access_token(&first.access_token).unwrap());
        assert!(!store.validate_access_token(&second.access_token).unwrap());
        assert!(store
            .refresh_token(refresh_request(&client_id, &second.refresh_token))
            .is_err());
    }

    #[test]
    fn revoking_an_access_token_leaves_the_refresh_token_usable() {
        let dir = tempfile::tempdir().unwrap();
        let store = OAuthStore::open(dir.path().join("shuttle.db")).unwrap();
        let (client_id, first) = grant(&store);

        assert!(store
            .revoke_token(&first.access_token, Some("access_token"))
            .unwrap());

        assert!(!store.validate_access_token(&first.access_token).unwrap());
        let second = store
            .refresh_token(refresh_request(&client_id, &first.refresh_token))
            .unwrap();
        assert!(store.validate_access_token(&second.access_token).unwrap());
    }

    #[test]
    fn revocation_falls_back_to_the_other_token_type_when_the_hint_is_wrong() {
        let dir = tempfile::tempdir().unwrap();
        let store = OAuthStore::open(dir.path().join("shuttle.db")).unwrap();
        let (client_id, first) = grant(&store);

        assert!(store
            .revoke_token(&first.refresh_token, Some("access_token"))
            .unwrap());

        assert!(store
            .refresh_token(refresh_request(&client_id, &first.refresh_token))
            .is_err());
    }

    #[test]
    fn migration_upgrades_a_v3_database_without_losing_access_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shuttle.db");
        let now = Utc::now();
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS oauth_schema_version (
                    id INTEGER PRIMARY KEY CHECK (id = 1),
                    version INTEGER NOT NULL
                 );",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO oauth_schema_version (id, version) VALUES (1, 3)",
                [],
            )
            .unwrap();
            for version in 1..=3 {
                apply_oauth_migration(&conn, version).unwrap();
            }
            conn.execute(
                "INSERT INTO oauth_tokens (token, client_id, scope, expires_at, created_at)
                 VALUES (?1, 'legacy-client', ?2, ?3, ?4)",
                params![
                    hash_token("stl_legacy"),
                    MCP_SCOPE,
                    (now + Duration::hours(1)).to_rfc3339(),
                    now.to_rfc3339()
                ],
            )
            .unwrap();
        }

        let store = OAuthStore::open(&path).unwrap();

        let version: u32 = Connection::open(&path)
            .unwrap()
            .query_row("SELECT version FROM oauth_schema_version", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(version, CURRENT_OAUTH_SCHEMA_VERSION);
        // A token minted before the upgrade keeps working; it simply has no
        // family and expires on its own schedule.
        assert!(store.validate_access_token("stl_legacy").unwrap());
        let (client_id, response) = grant(&store);
        assert!(store
            .refresh_token(refresh_request(&client_id, &response.refresh_token))
            .is_ok());
    }

    #[test]
    fn metadata_advertises_the_refresh_token_grant() {
        let config = OAuthConfig {
            public_url: "https://shuttle.example.test".to_owned(),
            admin_token: None,
            allow_dynamic_registration: false,
        };

        assert_eq!(
            authorization_server_metadata(&config)["grant_types_supported"],
            json!(["authorization_code", "refresh_token"])
        );
    }
}
