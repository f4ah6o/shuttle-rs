use std::path::Path;
use std::sync::{Arc, Mutex};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::core::{Result, ShuttleError};

const MCP_SCOPE: &str = "mcp";

#[derive(Clone)]
pub struct OAuthConfig {
    pub public_url: String,
    /// Owner-approval token for authorization-code issuance.
    ///
    /// CLI public URL mode requires this to be `Some`; `None` is reserved for
    /// programmatic or local-only runtimes that intentionally skip owner
    /// approval.
    pub admin_token: Option<String>,
}

impl OAuthConfig {
    pub fn normalize_public_url(public_url: String) -> String {
        public_url.trim().trim_end_matches('/').to_owned()
    }

    pub fn resource_url(&self) -> String {
        format!("{}/mcp", self.public_url)
    }
}

#[derive(Clone)]
pub struct OAuthStore {
    conn: Arc<Mutex<Connection>>,
}

impl OAuthStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path).map_err(to_store_error)?;
        let store = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        store.init()?;
        Ok(store)
    }

    fn init(&self) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|err| ShuttleError::Store(err.to_string()))?;
        conn.execute_batch(
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

            CREATE TABLE IF NOT EXISTS oauth_tokens (
                token TEXT PRIMARY KEY NOT NULL,
                client_id TEXT NOT NULL,
                scope TEXT NOT NULL,
                expires_at TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            "#,
        )
        .map_err(to_store_error)?;
        purge_expired(&conn)?;
        Ok(())
    }

    pub fn register_client(&self, request: RegisterRequest) -> Result<RegisteredClient> {
        if request.redirect_uris.is_empty() {
            return Err(ShuttleError::Store(
                "redirect_uris must contain at least one URI".to_owned(),
            ));
        }
        let client = RegisteredClient {
            client_id: token(),
            client_secret: None,
            redirect_uris: request.redirect_uris,
            client_name: request.client_name,
        };
        let conn = self
            .conn
            .lock()
            .map_err(|err| ShuttleError::Store(err.to_string()))?;
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
        let scope = normalize_scope(request.scope);
        let code = token();
        let now = Utc::now();
        let conn = self
            .conn
            .lock()
            .map_err(|err| ShuttleError::Store(err.to_string()))?;
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
        let code = request
            .code
            .ok_or_else(|| ShuttleError::Store("missing code".to_owned()))?;
        let verifier = request
            .code_verifier
            .ok_or_else(|| ShuttleError::Store("missing code_verifier".to_owned()))?;
        let conn = self
            .conn
            .lock()
            .map_err(|err| ShuttleError::Store(err.to_string()))?;
        let stored = conn
            .query_row(
                "SELECT client_id, redirect_uri, code_challenge, scope, expires_at, used_at
                 FROM oauth_codes WHERE code = ?1",
                params![code],
                |row| {
                    Ok(StoredCode {
                        client_id: row.get(0)?,
                        redirect_uri: row.get(1)?,
                        code_challenge: row.get(2)?,
                        scope: row.get(3)?,
                        expires_at: row.get(4)?,
                        used_at: row.get(5)?,
                    })
                },
            )
            .optional()
            .map_err(to_store_error)?
            .ok_or_else(|| ShuttleError::Store("invalid code".to_owned()))?;

        if stored.client_id != request.client_id {
            return Err(ShuttleError::Store("invalid client_id".to_owned()));
        }
        if stored.redirect_uri != request.redirect_uri {
            return Err(ShuttleError::Store("invalid redirect_uri".to_owned()));
        }
        if stored.used_at.is_some() {
            return Err(ShuttleError::Store("code already used".to_owned()));
        }
        if parse_time(&stored.expires_at)? < Utc::now() {
            return Err(ShuttleError::Store("code expired".to_owned()));
        }
        if pkce_s256(&verifier) != stored.code_challenge {
            return Err(ShuttleError::Store("invalid code_verifier".to_owned()));
        }

        conn.execute(
            "UPDATE oauth_codes SET used_at = ?1 WHERE code = ?2",
            params![Utc::now().to_rfc3339(), code],
        )
        .map_err(to_store_error)?;
        create_token(&conn, &stored.client_id, &stored.scope)
    }

    pub fn validate_access_token(&self, bearer_token: &str) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|err| ShuttleError::Store(err.to_string()))?;
        let row = conn
            .query_row(
                "SELECT scope, expires_at FROM oauth_tokens WHERE token = ?1",
                params![bearer_token],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(to_store_error)?;
        let Some((scope, expires_at)) = row else {
            return Ok(false);
        };
        Ok(scope.split_whitespace().any(|scope| scope == MCP_SCOPE)
            && parse_time(&expires_at)? > Utc::now())
    }
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

#[derive(Debug, Deserialize)]
pub struct TokenRequest {
    pub grant_type: String,
    pub client_id: String,
    pub redirect_uri: String,
    pub code: Option<String>,
    pub code_verifier: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub token_type: &'static str,
    pub expires_in: i64,
    pub scope: String,
}

pub fn authorization_server_metadata(config: &OAuthConfig) -> Value {
    json!({
        "issuer": config.public_url,
        "authorization_endpoint": format!("{}/oauth/authorize", config.public_url),
        "token_endpoint": format!("{}/oauth/token", config.public_url),
        "registration_endpoint": format!("{}/oauth/register", config.public_url),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
        "scopes_supported": [MCP_SCOPE],
    })
}

pub fn protected_resource_metadata(config: &OAuthConfig) -> Value {
    json!({
        "resource": config.resource_url(),
        "authorization_servers": [config.public_url],
        "scopes_supported": [MCP_SCOPE],
        "bearer_methods_supported": ["header"],
    })
}

/// Build the OAuth 2.0 authorization-code redirect URL (RFC 6749 §4.1.2).
///
/// `code` and `state` are appended **verbatim** without percent-encoding. The
/// `code` we issue is `stl_<UUID simple>` (URL-safe alphanumerics), and `state`
/// is whatever the client originally provided — the OAuth spec requires us to
/// echo it back unchanged. Re-encoding values that already came through axum's
/// `Query` decoder would silently mutate the bytes claude.ai expects to receive,
/// which is one known cause of the hosted-Claude callback returning 405.
pub fn authorize_redirect(redirect_uri: &str, code: &str, state: Option<&str>) -> String {
    let mut target = format!(
        "{}{}code={}",
        redirect_uri,
        if redirect_uri.contains('?') { "&" } else { "?" },
        code
    );
    if let Some(state) = state {
        target.push_str("&state=");
        target.push_str(state);
    }
    target
}

fn create_token(conn: &Connection, client_id: &str, scope: &str) -> Result<TokenResponse> {
    let access_token = token();
    let now = Utc::now();
    let expires_in = 3600;
    conn.execute(
        "INSERT INTO oauth_tokens (token, client_id, scope, expires_at, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            access_token,
            client_id,
            scope,
            (now + Duration::seconds(expires_in)).to_rfc3339(),
            now.to_rfc3339()
        ],
    )
    .map_err(to_store_error)?;
    Ok(TokenResponse {
        access_token,
        token_type: "Bearer",
        expires_in,
        scope: scope.to_owned(),
    })
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
        "DELETE FROM oauth_codes WHERE expires_at < ?1 OR used_at IS NOT NULL",
        params![now],
    )
    .map_err(to_store_error)?;
    conn.execute(
        "DELETE FROM oauth_tokens WHERE expires_at < ?1",
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
    used_at: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_uses_public_url() {
        let config = OAuthConfig {
            public_url: "https://shuttle.example.test".to_owned(),
            admin_token: None,
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
    fn authorize_redirect_echoes_state_verbatim() {
        let url = authorize_redirect(
            "https://claude.ai/api/mcp/auth_callback",
            "stl_abc123",
            Some("opaque=value+with/special"),
        );
        assert_eq!(
            url,
            "https://claude.ai/api/mcp/auth_callback?code=stl_abc123&state=opaque=value+with/special"
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

    #[test]
    fn code_exchange_validates_pkce() {
        let dir = tempfile::tempdir().unwrap();
        let store = OAuthStore::open(dir.path().join("shuttle.db")).unwrap();
        let client = store
            .register_client(RegisterRequest {
                redirect_uris: vec!["https://client.example.test/callback".to_owned()],
                client_name: Some("client".to_owned()),
            })
            .unwrap();
        let verifier = "abc123abc123abc123abc123abc123abc123abc123abc123";
        let code = store
            .create_code(AuthorizeRequest {
                response_type: "code".to_owned(),
                client_id: client.client_id.clone(),
                redirect_uri: "https://client.example.test/callback".to_owned(),
                state: None,
                scope: Some("mcp".to_owned()),
                code_challenge: Some(pkce_s256(verifier)),
                code_challenge_method: Some("S256".to_owned()),
            })
            .unwrap();

        let token = store
            .exchange_code(TokenRequest {
                grant_type: "authorization_code".to_owned(),
                client_id: client.client_id,
                redirect_uri: "https://client.example.test/callback".to_owned(),
                code: Some(code),
                code_verifier: Some(verifier.to_owned()),
            })
            .unwrap();

        assert!(store.validate_access_token(&token.access_token).unwrap());
    }
}
