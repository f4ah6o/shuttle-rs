use std::net::SocketAddr;
use std::path::PathBuf;

use crate::core::{Event, Result, ShuttleError};
use crate::store::SqliteEventStore;
use axum::extract::State;
use axum::http::{HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

#[derive(Clone)]
pub struct AppRuntime {
    pub store: SqliteEventStore,
    pub cwd: PathBuf,
    pub workspace_id: String,
    pub agent: String,
    pub session_id: String,
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

async fn mcp_health() -> impl IntoResponse {
    with_cors((StatusCode::OK, "Shuttle MCP server"))
}

async fn mcp_delete() -> impl IntoResponse {
    with_cors((StatusCode::OK, "OK"))
}

async fn mcp_options() -> impl IntoResponse {
    with_cors(StatusCode::NO_CONTENT)
}

async fn mcp_post(
    State(runtime): State<AppRuntime>,
    Json(request): Json<crate::mcp::Request>,
) -> impl IntoResponse {
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

fn with_cors(response: impl IntoResponse) -> impl IntoResponse {
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
        HeaderValue::from_static("content-type,mcp-session-id"),
    );
    parts.headers.insert(
        "access-control-expose-headers",
        HeaderValue::from_static("mcp-session-id"),
    );
    (parts, body)
}
