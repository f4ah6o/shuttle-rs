use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;

use crate::core::{Event, Result, ShuttleError};
use crate::oauth::{self, OAuthConfig, OAuthStore};
use crate::store::SqliteEventStore;
use axum::extract::{Form, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use serde_json::json;

#[derive(Clone)]
pub struct AppRuntime {
    pub store: SqliteEventStore,
    pub cwd: PathBuf,
    pub workspace_id: String,
    pub agent: String,
    pub session_id: String,
    pub oauth: Option<OAuthRuntime>,
}

#[derive(Clone)]
pub struct OAuthRuntime {
    pub config: OAuthConfig,
    pub store: OAuthStore,
}

#[derive(Debug, Serialize)]
struct Dashboard {
    inbox: Vec<Event>,
    tasks: Vec<crate::task::TaskSummary>,
    memories: Vec<Event>,
    context: crate::context::Context,
}

pub async fn serve(runtime: AppRuntime, addr: SocketAddr) -> Result<()> {
    let app = router(runtime);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|err| ShuttleError::Store(err.to_string()))?;
    axum::serve(listener, app)
        .await
        .map_err(|err| ShuttleError::Store(err.to_string()))
}

pub fn router(runtime: AppRuntime) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/dashboard", get(dashboard))
        .route("/api/inbox", get(inbox))
        .route("/api/tasks", get(tasks))
        .route("/api/memories", get(memories))
        .route("/api/context", get(context))
        .route(
            "/mcp",
            get(mcp_health)
                .post(mcp_post)
                .delete(mcp_delete)
                .options(mcp_options),
        )
        .route(
            "/.well-known/oauth-protected-resource",
            get(oauth_protected_resource),
        )
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(oauth_protected_resource),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            get(oauth_authorization_server),
        )
        .route("/oauth/register", post(oauth_register))
        .route(
            "/oauth/authorize",
            get(oauth_authorize_page).post(oauth_authorize_submit),
        )
        .route("/oauth/token", post(oauth_token))
        .with_state(runtime)
}

async fn index() -> Html<&'static str> {
    Html(
        r#"<!doctype html>
<html>
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Shuttle</title>
  <style>
    body { font-family: system-ui, sans-serif; margin: 2rem; color: #1f2937; }
    main { display: grid; gap: 1rem; grid-template-columns: repeat(auto-fit, minmax(260px, 1fr)); }
    section { border: 1px solid #d1d5db; border-radius: 8px; padding: 1rem; }
    h1 { margin-top: 0; }
    pre { white-space: pre-wrap; overflow-wrap: anywhere; }
  </style>
</head>
<body>
  <h1>Shuttle</h1>
  <main id="dashboard"></main>
  <script>
    fetch('/api/dashboard').then(r => r.json()).then(data => {
      const root = document.getElementById('dashboard');
      for (const [name, value] of Object.entries(data)) {
        const section = document.createElement('section');
        section.innerHTML = `<h2>${name}</h2><pre>${JSON.stringify(value, null, 2)}</pre>`;
        root.appendChild(section);
      }
    });
  </script>
</body>
</html>"#,
    )
}

async fn dashboard(State(runtime): State<AppRuntime>) -> impl IntoResponse {
    Json(Dashboard {
        inbox: crate::message::inbox(&runtime.store, &runtime.agent)
            .await
            .unwrap_or_default(),
        tasks: crate::task::open_tasks(&runtime.store, &runtime.workspace_id, Some(20))
            .await
            .unwrap_or_default(),
        memories: crate::memory::memories(&runtime.store)
            .await
            .unwrap_or_default(),
        context: crate::context::assemble_context(
            &runtime.store,
            &runtime.cwd,
            &runtime.workspace_id,
            &runtime.agent,
        )
        .await
        .unwrap_or_else(|_| crate::context::Context {
            repo: runtime.cwd.display().to_string(),
            branch: "unknown".to_owned(),
            commit: "unknown".to_owned(),
            git_remote: None,
            dirty: false,
            dirty_files: Vec::new(),
            open_tasks: Vec::new(),
            claimed_tasks: Vec::new(),
            recent_decisions: Vec::new(),
            related_memories: Vec::new(),
            recent_messages: Vec::new(),
            pending_handoffs: Vec::new(),
            recent_completed_handoffs: Vec::new(),
            inbox: Vec::new(),
        }),
    })
}

async fn inbox(State(runtime): State<AppRuntime>) -> impl IntoResponse {
    Json(
        crate::message::inbox(&runtime.store, &runtime.agent)
            .await
            .unwrap_or_default(),
    )
}

async fn tasks(State(runtime): State<AppRuntime>) -> impl IntoResponse {
    Json(
        crate::task::open_tasks(&runtime.store, &runtime.workspace_id, Some(20))
            .await
            .unwrap_or_default(),
    )
}

async fn memories(State(runtime): State<AppRuntime>) -> impl IntoResponse {
    Json(
        crate::memory::memories(&runtime.store)
            .await
            .unwrap_or_default(),
    )
}

async fn context(State(runtime): State<AppRuntime>) -> impl IntoResponse {
    Json(
        crate::context::assemble_context(
            &runtime.store,
            &runtime.cwd,
            &runtime.workspace_id,
            &runtime.agent,
        )
        .await
        .ok(),
    )
}

async fn mcp_health(headers: HeaderMap, State(runtime): State<AppRuntime>) -> Response {
    if let Some(response) = mcp_unauthorized_response(runtime.oauth.as_ref(), &headers) {
        return response;
    }
    with_cors((StatusCode::OK, "Shuttle MCP server"))
}

async fn mcp_delete(headers: HeaderMap, State(runtime): State<AppRuntime>) -> Response {
    if let Some(response) = mcp_unauthorized_response(runtime.oauth.as_ref(), &headers) {
        return response;
    }
    with_cors((StatusCode::OK, "OK"))
}

async fn mcp_options() -> Response {
    with_cors(StatusCode::NO_CONTENT)
}

async fn mcp_post(
    headers: HeaderMap,
    State(runtime): State<AppRuntime>,
    Json(request): Json<crate::mcp::Request>,
) -> Response {
    if let Some(response) = mcp_unauthorized_response(runtime.oauth.as_ref(), &headers) {
        return response;
    }
    let response = crate::mcp::handle_request(
        &crate::mcp::McpRuntime {
            store: runtime.store,
            cwd: runtime.cwd,
            workspace_id: runtime.workspace_id,
            agent: runtime.agent,
            session_id: runtime.session_id,
        },
        request,
    )
    .await;
    with_cors(Json(response))
}

async fn oauth_protected_resource(State(runtime): State<AppRuntime>) -> Response {
    let Some(oauth) = runtime.oauth else {
        return (StatusCode::NOT_FOUND, "OAuth is not configured").into_response();
    };
    Json(oauth::protected_resource_metadata(&oauth.config)).into_response()
}

async fn oauth_authorization_server(State(runtime): State<AppRuntime>) -> Response {
    let Some(oauth) = runtime.oauth else {
        return (StatusCode::NOT_FOUND, "OAuth is not configured").into_response();
    };
    Json(oauth::authorization_server_metadata(&oauth.config)).into_response()
}

async fn oauth_register(
    State(runtime): State<AppRuntime>,
    Json(request): Json<oauth::RegisterRequest>,
) -> Response {
    let Some(oauth) = runtime.oauth else {
        return (StatusCode::NOT_FOUND, "OAuth is not configured").into_response();
    };
    match oauth.store.register_client(request) {
        Ok(client) => Json(json!({
            "client_id": client.client_id,
            "client_id_issued_at": chrono::Utc::now().timestamp(),
            "redirect_uris": client.redirect_uris,
            "client_name": client.client_name,
            "token_endpoint_auth_method": "none",
        }))
        .into_response(),
        Err(err) => oauth_error(StatusCode::BAD_REQUEST, "invalid_request", &err.to_string()),
    }
}

async fn oauth_authorize_page(
    State(runtime): State<AppRuntime>,
    Query(request): Query<oauth::AuthorizeRequest>,
) -> Response {
    let Some(oauth) = runtime.oauth else {
        return (StatusCode::NOT_FOUND, "OAuth is not configured").into_response();
    };
    if request.response_type != "code" {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "unsupported_response_type",
            "response_type must be code",
        );
    }
    match oauth
        .store
        .client_allows_redirect(&request.client_id, &request.redirect_uri)
    {
        Ok(true) => {
            Html(authorize_html(&request, oauth.config.admin_token.is_some())).into_response()
        }
        Ok(false) => oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "unknown client_id or redirect_uri",
        ),
        Err(_) => oauth_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "server_error",
            "failed to validate OAuth client",
        ),
    }
}

async fn oauth_authorize_submit(
    State(runtime): State<AppRuntime>,
    Form(form): Form<oauth::AuthorizeForm>,
) -> Response {
    let Some(oauth) = runtime.oauth else {
        return (StatusCode::NOT_FOUND, "OAuth is not configured").into_response();
    };
    if let Some(expected) = oauth.config.admin_token.as_deref() {
        if !constant_time_eq(form.admin_token.as_bytes(), expected.as_bytes()) {
            return oauth_error(
                StatusCode::UNAUTHORIZED,
                "access_denied",
                "invalid admin token",
            );
        }
    }
    let request = oauth::AuthorizeRequest::from(form);
    if request.response_type != "code" {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "unsupported_response_type",
            "response_type must be code",
        );
    }
    match oauth.store.create_code(request.clone()) {
        Ok(code) => Redirect::temporary(&oauth::authorize_redirect(
            &request.redirect_uri,
            &code,
            request.state.as_deref(),
        ))
        .into_response(),
        Err(err) => oauth_error(StatusCode::BAD_REQUEST, "invalid_request", &err.to_string()),
    }
}

async fn oauth_token(
    State(runtime): State<AppRuntime>,
    Form(request): Form<oauth::TokenRequest>,
) -> Response {
    let Some(oauth) = runtime.oauth else {
        return (StatusCode::NOT_FOUND, "OAuth is not configured").into_response();
    };
    if request.grant_type != "authorization_code" {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "unsupported_grant_type",
            "grant_type must be authorization_code",
        );
    }
    match oauth.store.exchange_code(request) {
        Ok(token) => Json(token).into_response(),
        Err(err) => oauth_error(StatusCode::BAD_REQUEST, "invalid_grant", &err.to_string()),
    }
}

fn mcp_unauthorized_response(
    oauth: Option<&OAuthRuntime>,
    headers: &HeaderMap,
) -> Option<Response> {
    if let Some(oauth) = oauth {
        let Some(token) = bearer_token(headers) else {
            return Some(unauthorized_oauth(&oauth.config));
        };
        return match oauth.store.validate_access_token(token) {
            Ok(true) => None,
            Ok(false) => Some(unauthorized_oauth(&oauth.config)),
            Err(_) => Some(oauth_error(
                StatusCode::UNAUTHORIZED,
                "invalid_token",
                "failed to validate access token",
            )),
        };
    }

    let token = env::var("SHUTTLE_MCP_BEARER_TOKEN")
        .ok()
        .filter(|token| !token.is_empty())?;
    let expected = format!("Bearer {token}");
    if headers
        .get("authorization")
        .and_then(|header| header.to_str().ok())
        .is_some_and(|actual| constant_time_eq(actual.as_bytes(), expected.as_bytes()))
    {
        None
    } else {
        Some(with_cors(StatusCode::UNAUTHORIZED))
    }
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|header| header.to_str().ok())
        .and_then(|value| {
            let (scheme, token) = value.split_once(' ')?;
            scheme.eq_ignore_ascii_case("Bearer").then_some(token)
        })
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut diff = left.len() ^ right.len();
    for index in 0..left.len().max(right.len()) {
        let left = *left.get(index).unwrap_or(&0);
        let right = *right.get(index).unwrap_or(&0);
        diff |= (left ^ right) as usize;
    }
    diff == 0
}

fn with_cors(response: impl IntoResponse) -> Response {
    let (mut parts, body) = response.into_response().into_parts();
    parts
        .headers
        .insert("access-control-allow-origin", HeaderValue::from_static("*"));
    parts.headers.insert(
        "access-control-allow-methods",
        HeaderValue::from_static("GET,POST,DELETE,OPTIONS"),
    );
    parts.headers.insert(
        "access-control-allow-headers",
        HeaderValue::from_static(
            "accept,authorization,content-type,mcp-protocol-version,mcp-session-id",
        ),
    );
    parts.headers.insert(
        "access-control-expose-headers",
        HeaderValue::from_static("mcp-session-id"),
    );
    Response::from_parts(parts, body)
}

fn unauthorized_oauth(config: &OAuthConfig) -> Response {
    let mut response = with_cors(StatusCode::UNAUTHORIZED);
    let header_value = format!(
        r#"Bearer resource_metadata="{}/.well-known/oauth-protected-resource/mcp", scope="mcp""#,
        quoted_header_value(&config.public_url)
    );
    if let Ok(value) = HeaderValue::from_str(&header_value) {
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, value);
    }
    response
}

fn oauth_error(status: StatusCode, code: &str, description: &str) -> Response {
    (
        status,
        Json(json!({ "error": code, "error_description": description })),
    )
        .into_response()
}

fn authorize_html(request: &oauth::AuthorizeRequest, requires_admin_token: bool) -> String {
    let admin = if requires_admin_token {
        r#"<label>Admin token <input name="admin_token" type="password" autocomplete="current-password" required></label>"#
    } else {
        r#"<input name="admin_token" type="hidden" value="">"#
    };
    format!(
        r#"<!doctype html>
<html>
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Authorize Shuttle</title>
  <style>
    body {{ font-family: system-ui, sans-serif; margin: 2rem; color: #1f2937; }}
    form {{ display: grid; gap: 1rem; max-width: 32rem; }}
    input, button {{ font: inherit; padding: .6rem; }}
    label {{ display: grid; gap: .35rem; }}
  </style>
</head>
<body>
  <h1>Authorize Shuttle MCP</h1>
  <p>{client_id} is requesting access to Shuttle MCP.</p>
  <form method="post" action="/oauth/authorize">
    {admin}
    <input type="hidden" name="response_type" value="{response_type}">
    <input type="hidden" name="client_id" value="{client_id}">
    <input type="hidden" name="redirect_uri" value="{redirect_uri}">
    <input type="hidden" name="state" value="{state}">
    <input type="hidden" name="scope" value="{scope}">
    <input type="hidden" name="code_challenge" value="{code_challenge}">
    <input type="hidden" name="code_challenge_method" value="{code_challenge_method}">
    <button type="submit">Authorize</button>
  </form>
</body>
</html>"#,
        admin = admin,
        response_type = html_escape(&request.response_type),
        client_id = html_escape(&request.client_id),
        redirect_uri = html_escape(&request.redirect_uri),
        state = html_escape(request.state.as_deref().unwrap_or("")),
        scope = html_escape(request.scope.as_deref().unwrap_or("mcp")),
        code_challenge = html_escape(request.code_challenge.as_deref().unwrap_or("")),
        code_challenge_method =
            html_escape(request.code_challenge_method.as_deref().unwrap_or("S256")),
    )
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn quoted_header_value(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}
