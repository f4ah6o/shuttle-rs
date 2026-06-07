use std::collections::BTreeMap;
use std::env;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::extract::{Form, Path as AxumPath, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::process::Command;

use crate::core::{Result, ShuttleError};
use crate::oauth::{self, OAuthConfig, OAuthStore};

#[derive(Debug, Clone, Deserialize)]
pub struct GatewayConfig {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub auth: AuthConfig,
    #[serde(default)]
    pub oauth: OAuthGatewayConfig,
    #[serde(default)]
    pub defaults: DefaultsConfig,
    pub projects: BTreeMap<String, ProjectConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_addr")]
    pub addr: SocketAddr,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            addr: default_addr(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct AuthConfig {
    #[serde(default = "default_gateway_token_env")]
    pub bearer_token_env: String,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            bearer_token_env: default_gateway_token_env(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OAuthGatewayConfig {
    #[serde(default)]
    pub public_url: String,
    #[serde(default)]
    pub db_path: Option<PathBuf>,
    #[serde(default = "default_oauth_admin_token_env")]
    pub admin_token_env: String,
}

impl Default for OAuthGatewayConfig {
    fn default() -> Self {
        Self {
            public_url: String::new(),
            db_path: None,
            admin_token_env: default_oauth_admin_token_env(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct DefaultsConfig {
    #[serde(default)]
    pub project: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProjectConfig {
    pub repo: PathBuf,
    #[serde(default)]
    pub db: Option<PathBuf>,
    #[serde(default)]
    pub description: Option<String>,
}

impl GatewayConfig {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let abs_path = path
            .canonicalize()
            .or_else(|_| {
                path.parent()
                    .unwrap_or_else(|| Path::new("."))
                    .canonicalize()
                    .map(|parent| parent.join(path.file_name().unwrap_or_default()))
            })
            .map_err(|err| ShuttleError::Store(err.to_string()))?;
        let raw =
            std::fs::read_to_string(path).map_err(|err| ShuttleError::Store(err.to_string()))?;
        let mut cfg: GatewayConfig =
            toml::from_str(&raw).map_err(|err| ShuttleError::Serialization(err.to_string()))?;

        cfg.oauth.public_url = normalize_public_url(&cfg.oauth.public_url);
        if cfg.projects.is_empty() {
            return Err(ShuttleError::Store(
                "at least one project is required".to_owned(),
            ));
        }
        for (name, project) in &cfg.projects {
            if name.is_empty() {
                return Err(ShuttleError::Store(
                    "project name cannot be empty".to_owned(),
                ));
            }
            if !project.repo.is_absolute() {
                return Err(ShuttleError::Store(format!(
                    "project {name:?} repo must be an absolute path"
                )));
            }
            if let Some(db) = &project.db {
                if !db.is_absolute() {
                    return Err(ShuttleError::Store(format!(
                        "project {name:?} db must be an absolute path when set"
                    )));
                }
            }
        }
        if !cfg.defaults.project.is_empty() && !cfg.projects.contains_key(&cfg.defaults.project) {
            return Err(ShuttleError::Store(format!(
                "default project {:?} is not configured",
                cfg.defaults.project
            )));
        }
        if !cfg.oauth.public_url.is_empty() {
            match &cfg.oauth.db_path {
                Some(path) if !path.is_absolute() => {
                    return Err(ShuttleError::Store(
                        "oauth db_path must be an absolute path when set".to_owned(),
                    ))
                }
                Some(_) => {}
                None => {
                    cfg.oauth.db_path = Some(
                        abs_path
                            .parent()
                            .unwrap_or_else(|| Path::new("."))
                            .join("gateway-oauth.db"),
                    );
                }
            }
        }
        Ok(cfg)
    }
}

#[derive(Clone)]
pub struct GatewayRuntime {
    service: Arc<GatewayService>,
    auth: GatewayAuth,
}

impl GatewayRuntime {
    pub fn from_config(config: GatewayConfig, stl: PathBuf, timeout: Duration) -> Result<Self> {
        let registry = ProjectRegistry::new(config.defaults.project, config.projects)?;
        let service = Arc::new(GatewayService::new(
            registry,
            Arc::new(SubprocessRunner {
                binary: stl,
                timeout,
            }),
        ));
        let auth = if config.oauth.public_url.is_empty() {
            GatewayAuth::Bearer {
                token_env: config.auth.bearer_token_env,
            }
        } else {
            let admin_token = env::var(&config.oauth.admin_token_env).map_err(|_| {
                ShuttleError::Store(format!(
                    "{} is required when oauth public_url is configured",
                    config.oauth.admin_token_env
                ))
            })?;
            if admin_token.is_empty() {
                return Err(ShuttleError::Store(format!(
                    "{} is required when oauth public_url is configured",
                    config.oauth.admin_token_env
                )));
            }
            let db_path = config
                .oauth
                .db_path
                .ok_or_else(|| ShuttleError::Store("oauth db_path is required".to_owned()))?;
            GatewayAuth::OAuth(Arc::new(OAuthRuntime {
                config: OAuthConfig {
                    public_url: config.oauth.public_url,
                    admin_token: Some(admin_token),
                },
                store: OAuthStore::open(db_path)?,
            }))
        };
        Ok(Self { service, auth })
    }
}

#[derive(Clone)]
enum GatewayAuth {
    Bearer { token_env: String },
    OAuth(Arc<OAuthRuntime>),
}

#[derive(Clone)]
struct OAuthRuntime {
    config: OAuthConfig,
    store: OAuthStore,
}

pub async fn serve(runtime: GatewayRuntime, addr: SocketAddr) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|err| ShuttleError::Store(err.to_string()))?;
    axum::serve(listener, router(runtime))
        .await
        .map_err(|err| ShuttleError::Store(err.to_string()))
}

pub fn router(runtime: GatewayRuntime) -> Router {
    Router::new()
        .route("/api/projects", get(api_projects))
        .route("/api/projects/current", get(api_current_project))
        .route("/api/projects/use", post(api_use_project))
        .route("/api/recall", post(api_recall))
        .route("/api/remember", post(api_remember))
        .route("/api/context", get(api_context))
        .route("/api/tasks", get(api_tasks).post(api_create_task))
        .route("/api/tasks/{id}", patch(api_update_task))
        .route("/api/tasks/{id}/done", post(api_done_task))
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

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Project {
    pub name: String,
    pub repo: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub db: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug)]
pub struct ProjectRegistry {
    default_project: String,
    projects: BTreeMap<String, Project>,
}

impl ProjectRegistry {
    pub fn new(default_project: String, configs: BTreeMap<String, ProjectConfig>) -> Result<Self> {
        let projects = configs
            .into_iter()
            .map(|(name, cfg)| {
                let project = Project {
                    name: name.clone(),
                    repo: cfg.repo,
                    db: cfg.db,
                    description: cfg.description,
                };
                (name, project)
            })
            .collect::<BTreeMap<_, _>>();
        if !default_project.is_empty() && !projects.contains_key(&default_project) {
            return Err(ShuttleError::Store(format!(
                "default project {default_project:?} is not configured"
            )));
        }
        Ok(Self {
            default_project,
            projects,
        })
    }

    pub fn list(&self) -> Vec<Project> {
        self.projects.values().cloned().collect()
    }

    pub fn get(&self, name: &str) -> Option<Project> {
        self.projects.get(name).cloned()
    }

    pub fn default(&self) -> Option<Project> {
        (!self.default_project.is_empty())
            .then(|| self.get(&self.default_project))
            .flatten()
    }

    pub fn resolve(&self, project: &str, write: bool) -> Result<Project> {
        if !project.is_empty() {
            return self
                .get(project)
                .ok_or_else(|| ShuttleError::Store(format!("unknown project {project:?}")));
        }
        if write {
            return Err(ShuttleError::Store(
                "project is required for write operations".to_owned(),
            ));
        }
        self.default()
            .ok_or_else(|| ShuttleError::Store("project is required".to_owned()))
    }
}

#[derive(Debug, Serialize)]
pub struct ServiceResponse {
    pub project: String,
    pub result: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stored: Option<bool>,
}

pub struct GatewayService {
    projects: ProjectRegistry,
    runner: Arc<dyn Runner>,
    current: Mutex<String>,
}

impl GatewayService {
    pub fn new(projects: ProjectRegistry, runner: Arc<dyn Runner>) -> Self {
        Self {
            projects,
            runner,
            current: Mutex::new(String::new()),
        }
    }

    pub fn list_projects(&self) -> Vec<Project> {
        self.projects.list()
    }

    pub fn use_project(&self, name: &str) -> Result<Project> {
        let project = self
            .projects
            .get(name)
            .ok_or_else(|| ShuttleError::Store(format!("unknown project {name:?}")))?;
        *self
            .current
            .lock()
            .map_err(|err| ShuttleError::Store(err.to_string()))? = name.to_owned();
        Ok(project)
    }

    pub fn current_project(&self) -> Result<Project> {
        let current = self
            .current
            .lock()
            .map_err(|err| ShuttleError::Store(err.to_string()))?
            .clone();
        if !current.is_empty() {
            if let Some(project) = self.projects.get(&current) {
                return Ok(project);
            }
        }
        self.projects
            .default()
            .ok_or_else(|| ShuttleError::Store("no current or default project".to_owned()))
    }

    pub async fn context(&self, project: &str) -> Result<ServiceResponse> {
        self.run(project, false, &["context"]).await
    }

    pub async fn recall(&self, project: &str, query: &str) -> Result<ServiceResponse> {
        require_non_empty(query, "query is required")?;
        self.run(project, false, &["recall", query]).await
    }

    pub async fn remember(&self, project: &str, kind: &str, text: &str) -> Result<ServiceResponse> {
        require_non_empty(text, "text is required")?;
        let command = match kind {
            "" | "memory" => "remember",
            "decision" => "decide",
            "observation" => "observe",
            "pattern" => "pattern",
            "fact" => "fact",
            "bug" => "bug",
            other => {
                return Err(ShuttleError::Store(format!(
                    "unknown memory kind {other:?}"
                )))
            }
        };
        self.run(project, true, &[command, text]).await
    }

    pub async fn task_list(&self, project: &str) -> Result<ServiceResponse> {
        self.run(project, false, &["task", "list"]).await
    }

    pub async fn task_create(
        &self,
        project: &str,
        title: &str,
        body: &str,
    ) -> Result<ServiceResponse> {
        require_non_empty(title, "title is required")?;
        let content = if body.is_empty() {
            title.to_owned()
        } else {
            format!("{title}\n\n{body}")
        };
        self.run(project, true, &["task", "create", &content]).await
    }

    pub async fn task_update(
        &self,
        project: &str,
        id: &str,
        text: &str,
    ) -> Result<ServiceResponse> {
        require_non_empty(id, "task id is required")?;
        require_non_empty(text, "text is required")?;
        self.run(project, true, &["task", "update", id, text]).await
    }

    pub async fn task_done(&self, project: &str, id: &str) -> Result<ServiceResponse> {
        require_non_empty(id, "task id is required")?;
        self.run(project, true, &["task", "done", id]).await
    }

    async fn run(&self, project: &str, write: bool, args: &[&str]) -> Result<ServiceResponse> {
        let project = self.projects.resolve(project, write)?;
        let result = self.runner.run(&project, args).await.map_err(|err| {
            ShuttleError::Store(format!("stl failed for project {}: {err}", project.name))
        })?;
        Ok(ServiceResponse {
            project: project.name,
            result,
            stored: write.then_some(true),
        })
    }
}

fn require_non_empty(value: &str, message: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(ShuttleError::Store(message.to_owned()));
    }
    Ok(())
}

#[async_trait]
pub trait Runner: Send + Sync {
    async fn run(&self, project: &Project, args: &[&str]) -> std::result::Result<Value, String>;
}

pub struct SubprocessRunner {
    binary: PathBuf,
    timeout: Duration,
}

#[async_trait]
impl Runner for SubprocessRunner {
    async fn run(&self, project: &Project, args: &[&str]) -> std::result::Result<Value, String> {
        let mut command = Command::new(&self.binary);
        command.arg("--json").args(args).current_dir(&project.repo);
        let output = tokio::time::timeout(self.timeout, command.output())
            .await
            .map_err(|_| format!("timed out after {}s", self.timeout.as_secs()))?
            .map_err(|err| err.to_string())?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            return Err(if stderr.is_empty() {
                format!("exit status {}", output.status)
            } else {
                stderr
            });
        }
        serde_json::from_slice(&output.stdout).map_err(|err| err.to_string())
    }
}

async fn api_projects(State(runtime): State<GatewayRuntime>, headers: HeaderMap) -> Response {
    authorize(&runtime, &headers, "/api/projects", false)
        .map(|_| Json(json!({ "projects": runtime.service.list_projects() })).into_response())
        .unwrap_or_else(|response| *response)
}

async fn api_current_project(
    State(runtime): State<GatewayRuntime>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&runtime, &headers, "/api/projects/current", false) {
        return *response;
    }
    match runtime.service.current_project() {
        Ok(project) => Json(project).into_response(),
        Err(err) if err.to_string().contains("no current or default project") => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": err.to_string()})),
        )
            .into_response(),
        Err(err) => error_response(err),
    }
}

#[derive(Deserialize)]
struct ProjectRequest {
    #[serde(default)]
    project: String,
}

async fn api_use_project(
    State(runtime): State<GatewayRuntime>,
    headers: HeaderMap,
    Json(request): Json<ProjectRequest>,
) -> Response {
    if let Err(response) = authorize(&runtime, &headers, "/api/projects/use", false) {
        return *response;
    }
    match runtime.service.use_project(&request.project) {
        Ok(project) => Json(project).into_response(),
        Err(err) => error_response(err),
    }
}

#[derive(Deserialize)]
struct RecallRequest {
    #[serde(default)]
    project: String,
    #[serde(default)]
    query: String,
}

async fn api_recall(
    State(runtime): State<GatewayRuntime>,
    headers: HeaderMap,
    Json(request): Json<RecallRequest>,
) -> Response {
    service_response(
        &runtime,
        &headers,
        "/api/recall",
        runtime
            .service
            .recall(&request.project, &request.query)
            .await,
    )
}

#[derive(Deserialize)]
struct RememberRequest {
    #[serde(default)]
    project: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    text: String,
}

async fn api_remember(
    State(runtime): State<GatewayRuntime>,
    headers: HeaderMap,
    Json(request): Json<RememberRequest>,
) -> Response {
    service_response(
        &runtime,
        &headers,
        "/api/remember",
        runtime
            .service
            .remember(&request.project, &request.kind, &request.text)
            .await,
    )
}

async fn api_context(
    State(runtime): State<GatewayRuntime>,
    headers: HeaderMap,
    Query(request): Query<ProjectRequest>,
) -> Response {
    service_response(
        &runtime,
        &headers,
        "/api/context",
        runtime.service.context(&request.project).await,
    )
}

async fn api_tasks(
    State(runtime): State<GatewayRuntime>,
    headers: HeaderMap,
    Query(request): Query<ProjectRequest>,
) -> Response {
    service_response(
        &runtime,
        &headers,
        "/api/tasks",
        runtime.service.task_list(&request.project).await,
    )
}

#[derive(Deserialize)]
struct CreateTaskRequest {
    #[serde(default)]
    project: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    body: String,
}

async fn api_create_task(
    State(runtime): State<GatewayRuntime>,
    headers: HeaderMap,
    Json(request): Json<CreateTaskRequest>,
) -> Response {
    service_response(
        &runtime,
        &headers,
        "/api/tasks",
        runtime
            .service
            .task_create(&request.project, &request.title, &request.body)
            .await,
    )
}

#[derive(Deserialize)]
struct UpdateTaskRequest {
    #[serde(default)]
    project: String,
    #[serde(default)]
    text: String,
}

async fn api_update_task(
    State(runtime): State<GatewayRuntime>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
    Json(request): Json<UpdateTaskRequest>,
) -> Response {
    service_response(
        &runtime,
        &headers,
        "/api/tasks",
        runtime
            .service
            .task_update(&request.project, &id, &request.text)
            .await,
    )
}

async fn api_done_task(
    State(runtime): State<GatewayRuntime>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
    Json(request): Json<ProjectRequest>,
) -> Response {
    service_response(
        &runtime,
        &headers,
        "/api/tasks",
        runtime.service.task_done(&request.project, &id).await,
    )
}

fn service_response(
    runtime: &GatewayRuntime,
    headers: &HeaderMap,
    path: &str,
    response: Result<ServiceResponse>,
) -> Response {
    if let Err(response) = authorize(runtime, headers, path, false) {
        return *response;
    }
    match response {
        Ok(value) => Json(value).into_response(),
        Err(err) => error_response(err),
    }
}

#[derive(Deserialize)]
struct RpcRequest {
    jsonrpc: Option<String>,
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

async fn mcp_health(State(runtime): State<GatewayRuntime>, headers: HeaderMap) -> Response {
    authorize(&runtime, &headers, "/mcp", true)
        .map(|_| with_cors(Json(json!({ "status": "ok" }))))
        .unwrap_or_else(|response| *response)
}

async fn mcp_delete(State(runtime): State<GatewayRuntime>, headers: HeaderMap) -> Response {
    authorize(&runtime, &headers, "/mcp", true)
        .map(|_| with_cors(StatusCode::OK))
        .unwrap_or_else(|response| *response)
}

async fn mcp_options() -> Response {
    with_cors(StatusCode::NO_CONTENT)
}

async fn mcp_post(
    State(runtime): State<GatewayRuntime>,
    headers: HeaderMap,
    Json(request): Json<RpcRequest>,
) -> Response {
    match authorize(&runtime, &headers, "/mcp", true) {
        Ok(()) if request.method == "notifications/initialized" => {
            with_cors(StatusCode::NO_CONTENT)
        }
        Ok(()) => with_cors(Json(handle_mcp(&runtime.service, request).await)),
        Err(response) => *response,
    }
}

async fn handle_mcp(service: &GatewayService, request: RpcRequest) -> Value {
    let id = request.id.unwrap_or(Value::Null);
    if request.jsonrpc.as_deref() != Some("2.0") {
        return rpc_error(id, -32600, "invalid jsonrpc version");
    }
    match request.method.as_str() {
        "initialize" => rpc_ok(
            id,
            json!({
                "protocolVersion": "2025-11-25",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "shuttle-gateway", "version": env!("CARGO_PKG_VERSION") }
            }),
        ),
        "notifications/initialized" => json!({"jsonrpc": "2.0"}),
        "tools/list" => rpc_ok(id, json!({ "tools": gateway_tools() })),
        "tools/call" => match mcp_call_tool(service, request.params).await {
            Ok(value) => rpc_ok(
                id,
                json!({ "content": [{ "type": "text", "text": value.to_string() }] }),
            ),
            Err(err) => rpc_error(id, -32603, &err.to_string()),
        },
        _ => rpc_error(id, -32601, "method not found"),
    }
}

async fn mcp_call_tool(service: &GatewayService, params: Value) -> Result<Value> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| ShuttleError::Store("missing tool name".to_owned()))?;
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    match name {
        "shuttle_projects" => Ok(json!({ "projects": service.list_projects() })),
        "shuttle_current_project" => serde_json::to_value(service.current_project()?)
            .map_err(|err| ShuttleError::Serialization(err.to_string())),
        "shuttle_use_project" => {
            serde_json::to_value(service.use_project(str_arg(&args, "project")?)?)
                .map_err(|err| ShuttleError::Serialization(err.to_string()))
        }
        "shuttle_context" => service
            .context(optional_str_arg(&args, "project"))
            .await
            .and_then(to_value),
        "shuttle_recall" => service
            .recall(optional_str_arg(&args, "project"), str_arg(&args, "query")?)
            .await
            .and_then(to_value),
        "shuttle_remember" => service
            .remember(
                str_arg(&args, "project")?,
                optional_str_arg(&args, "kind"),
                str_arg(&args, "text")?,
            )
            .await
            .and_then(to_value),
        "shuttle_task_list" => service
            .task_list(optional_str_arg(&args, "project"))
            .await
            .and_then(to_value),
        "shuttle_task_create" => service
            .task_create(
                str_arg(&args, "project")?,
                str_arg(&args, "title")?,
                optional_str_arg(&args, "body"),
            )
            .await
            .and_then(to_value),
        "shuttle_task_update" => service
            .task_update(
                str_arg(&args, "project")?,
                str_arg(&args, "task_id")?,
                str_arg(&args, "text")?,
            )
            .await
            .and_then(to_value),
        "shuttle_task_done" => service
            .task_done(str_arg(&args, "project")?, str_arg(&args, "task_id")?)
            .await
            .and_then(to_value),
        other => Err(ShuttleError::Store(format!("unknown tool: {other}"))),
    }
}

fn to_value(response: ServiceResponse) -> Result<Value> {
    serde_json::to_value(response).map_err(|err| ShuttleError::Serialization(err.to_string()))
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ShuttleError::Store(format!("{key} is required")))
}

fn optional_str_arg<'a>(args: &'a Value, key: &str) -> &'a str {
    args.get(key).and_then(Value::as_str).unwrap_or("")
}

fn rpc_ok(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn gateway_tools() -> Vec<Value> {
    vec![
        tool(
            "shuttle_projects",
            "List configured Shuttle projects",
            json!({}),
            vec![],
        ),
        tool(
            "shuttle_current_project",
            "Read the current or default project",
            json!({}),
            vec![],
        ),
        tool(
            "shuttle_use_project",
            "Set the current project",
            json!({"project": string_schema("Configured project name")}),
            vec!["project"],
        ),
        tool(
            "shuttle_context",
            "Read Shuttle context for a project",
            json!({"project": string_schema("Configured project name; optional with default project")}),
            vec![],
        ),
        tool(
            "shuttle_recall",
            "Search Shuttle memories in a project",
            json!({"project": string_schema("Configured project name; optional with default project"), "query": string_schema("Recall query")}),
            vec!["query"],
        ),
        tool(
            "shuttle_remember",
            "Store a Shuttle memory in a project",
            json!({"project": string_schema("Configured project name"), "kind": enum_schema("Memory kind", &["memory", "decision", "observation", "pattern", "fact", "bug"]), "text": string_schema("Memory text")}),
            vec!["project", "text"],
        ),
        tool(
            "shuttle_task_list",
            "List Shuttle tasks in a project",
            json!({"project": string_schema("Configured project name; optional with default project")}),
            vec![],
        ),
        tool(
            "shuttle_task_create",
            "Create a Shuttle task in a project",
            json!({"project": string_schema("Configured project name"), "title": string_schema("Task title"), "body": string_schema("Optional task body")}),
            vec!["project", "title"],
        ),
        tool(
            "shuttle_task_update",
            "Update a Shuttle task in a project",
            json!({"project": string_schema("Configured project name"), "task_id": string_schema("Task UUID"), "text": string_schema("Update text")}),
            vec!["project", "task_id", "text"],
        ),
        tool(
            "shuttle_task_done",
            "Complete a Shuttle task in a project",
            json!({"project": string_schema("Configured project name"), "task_id": string_schema("Task UUID")}),
            vec!["project", "task_id"],
        ),
    ]
}

fn tool(name: &str, description: &str, properties: Value, required: Vec<&str>) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false,
        }
    })
}

fn string_schema(description: &str) -> Value {
    json!({ "type": "string", "description": description })
}

fn enum_schema(description: &str, values: &[&str]) -> Value {
    json!({ "type": "string", "description": description, "enum": values })
}

fn authorize(
    runtime: &GatewayRuntime,
    headers: &HeaderMap,
    path: &str,
    cors: bool,
) -> std::result::Result<(), Box<Response>> {
    if is_oauth_public_route(path) {
        return Ok(());
    }
    match &runtime.auth {
        GatewayAuth::Bearer { token_env } => {
            let Some(token) = env::var(token_env).ok().filter(|token| !token.is_empty()) else {
                return Ok(());
            };
            let expected = format!("Bearer {token}");
            let ok = headers
                .get(header::AUTHORIZATION)
                .and_then(|header| header.to_str().ok())
                .is_some_and(|actual| constant_time_eq(actual.as_bytes(), expected.as_bytes()));
            if ok {
                Ok(())
            } else if cors {
                Err(Box::new(with_cors(StatusCode::UNAUTHORIZED)))
            } else {
                Err(Box::new(
                    (
                        StatusCode::UNAUTHORIZED,
                        Json(json!({"error": "unauthorized"})),
                    )
                        .into_response(),
                ))
            }
        }
        GatewayAuth::OAuth(oauth) => {
            let Some(token) = bearer_token(headers) else {
                return Err(Box::new(unauthorized_oauth(&oauth.config)));
            };
            match oauth.store.validate_access_token(token) {
                Ok(true) => Ok(()),
                Ok(false) => Err(Box::new(unauthorized_oauth(&oauth.config))),
                Err(_) => Err(Box::new(oauth_error(
                    StatusCode::UNAUTHORIZED,
                    "invalid_token",
                    "failed to validate access token",
                ))),
            }
        }
    }
}

fn is_oauth_public_route(path: &str) -> bool {
    matches!(
        path,
        "/.well-known/oauth-protected-resource"
            | "/.well-known/oauth-protected-resource/mcp"
            | "/.well-known/oauth-authorization-server"
            | "/oauth/register"
            | "/oauth/token"
            | "/oauth/authorize"
    )
}

async fn oauth_protected_resource(State(runtime): State<GatewayRuntime>) -> Response {
    let GatewayAuth::OAuth(oauth) = &runtime.auth else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "oauth is not configured"})),
        )
            .into_response();
    };
    Json(oauth::protected_resource_metadata(&oauth.config)).into_response()
}

async fn oauth_authorization_server(State(runtime): State<GatewayRuntime>) -> Response {
    let GatewayAuth::OAuth(oauth) = &runtime.auth else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "oauth is not configured"})),
        )
            .into_response();
    };
    Json(oauth::authorization_server_metadata(&oauth.config)).into_response()
}

async fn oauth_register(
    State(runtime): State<GatewayRuntime>,
    Json(request): Json<oauth::RegisterRequest>,
) -> Response {
    let GatewayAuth::OAuth(oauth) = &runtime.auth else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "oauth is not configured"})),
        )
            .into_response();
    };
    match oauth.store.register_client(request) {
        Ok(client) => {
            let mut body = json!({
                "client_id": client.client_id,
                "redirect_uris": client.redirect_uris,
                "client_name": client.client_name,
                "token_endpoint_auth_method": "none",
            });
            if let Some(secret) = client.client_secret {
                body["client_secret"] = json!(secret);
            }
            (StatusCode::CREATED, Json(body)).into_response()
        }
        Err(err) => oauth_error(StatusCode::BAD_REQUEST, "invalid_request", &err.to_string()),
    }
}

async fn oauth_authorize_page(
    State(runtime): State<GatewayRuntime>,
    Query(request): Query<oauth::AuthorizeRequest>,
) -> Response {
    let GatewayAuth::OAuth(oauth) = &runtime.auth else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "oauth is not configured"})),
        )
            .into_response();
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
    State(runtime): State<GatewayRuntime>,
    Form(form): Form<oauth::AuthorizeForm>,
) -> Response {
    let GatewayAuth::OAuth(oauth) = &runtime.auth else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "oauth is not configured"})),
        )
            .into_response();
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
        Ok(code) => Redirect::to(&oauth::authorize_redirect(
            &request.redirect_uri,
            &code,
            request.state.as_deref(),
        ))
        .into_response(),
        Err(err) => oauth_error(StatusCode::BAD_REQUEST, "invalid_request", &err.to_string()),
    }
}

async fn oauth_token(
    State(runtime): State<GatewayRuntime>,
    Form(request): Form<oauth::TokenRequest>,
) -> Response {
    let GatewayAuth::OAuth(oauth) = &runtime.auth else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "oauth is not configured"})),
        )
            .into_response();
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

fn error_response(err: ShuttleError) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error": err.to_string()})),
    )
        .into_response()
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
<head><meta charset="utf-8"><title>Authorize Shuttle Gateway</title></head>
<body>
  <h1>Authorize Shuttle Gateway</h1>
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
        .replace('\'', "&#39;")
}

fn quoted_header_value(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn default_addr() -> SocketAddr {
    "127.0.0.1:8787".parse().expect("valid default address")
}

fn default_gateway_token_env() -> String {
    "SHUTTLE_GATEWAY_TOKEN".to_owned()
}

fn default_oauth_admin_token_env() -> String {
    "SHUTTLE_OAUTH_ADMIN_TOKEN".to_owned()
}

fn normalize_public_url(url: &str) -> String {
    url.trim().trim_end_matches('/').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Method, Request};
    use http_body_util::BodyExt;
    use std::sync::Mutex;
    use tower::ServiceExt;

    #[derive(Default)]
    struct FakeRunner {
        calls: Mutex<Vec<(String, Vec<String>)>>,
    }

    #[async_trait]
    impl Runner for FakeRunner {
        async fn run(
            &self,
            project: &Project,
            args: &[&str],
        ) -> std::result::Result<Value, String> {
            self.calls.lock().unwrap().push((
                project.name.clone(),
                args.iter().map(|arg| (*arg).to_owned()).collect(),
            ));
            Ok(json!({"ok": true}))
        }
    }

    fn registry() -> ProjectRegistry {
        ProjectRegistry::new(
            "demo".to_owned(),
            BTreeMap::from([(
                "demo".to_owned(),
                ProjectConfig {
                    repo: PathBuf::from("/tmp/demo"),
                    db: None,
                    description: None,
                },
            )]),
        )
        .unwrap()
    }

    #[test]
    fn config_rejects_relative_repo_and_applies_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("projects.toml");
        std::fs::write(&path, "[projects.demo]\nrepo = \"relative\"\n").unwrap();
        assert!(GatewayConfig::load(&path).is_err());

        std::fs::write(&path, "[projects.demo]\nrepo = \"/tmp/demo\"\n").unwrap();
        let cfg = GatewayConfig::load(&path).unwrap();
        assert_eq!(cfg.server.addr, default_addr());
        assert_eq!(cfg.auth.bearer_token_env, "SHUTTLE_GATEWAY_TOKEN");
        assert_eq!(cfg.oauth.admin_token_env, "SHUTTLE_OAUTH_ADMIN_TOKEN");
    }

    #[test]
    fn config_normalizes_oauth_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("projects.toml");
        std::fs::write(
            &path,
            "[oauth]\npublic_url = \"https://shuttle.example.test/\"\n\n[projects.demo]\nrepo = \"/tmp/demo\"\n",
        )
        .unwrap();
        let cfg = GatewayConfig::load(&path).unwrap();
        assert_eq!(cfg.oauth.public_url, "https://shuttle.example.test");
        assert_eq!(
            cfg.oauth.db_path.unwrap().file_name().unwrap(),
            "gateway-oauth.db"
        );
    }

    #[tokio::test]
    async fn remember_requires_explicit_project_and_maps_kind() {
        let runner = Arc::new(FakeRunner::default());
        let service = GatewayService::new(registry(), runner.clone());
        assert!(service.remember("", "decision", "ship it").await.is_err());
        let response = service
            .remember("demo", "decision", "ship it")
            .await
            .unwrap();
        assert_eq!(response.project, "demo");
        assert_eq!(response.stored, Some(true));
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0].1, vec!["decide", "ship it"]);
    }

    #[tokio::test]
    async fn task_create_combines_title_and_body() {
        let runner = Arc::new(FakeRunner::default());
        let service = GatewayService::new(registry(), runner.clone());
        service.task_create("demo", "title", "body").await.unwrap();
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0].1, vec!["task", "create", "title\n\nbody"]);
    }

    #[tokio::test]
    async fn mcp_tools_list_includes_gateway_tools() {
        let runtime = test_runtime(registry(), Arc::new(FakeRunner::default()));
        env::remove_var("TEST_EMPTY_GATEWAY_TOKEN");
        let response = router(runtime)
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/mcp")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let value: Value = serde_json::from_slice(&body).unwrap();
        let tools = value["result"]["tools"].as_array().unwrap();
        assert!(tools.iter().any(|tool| tool["name"] == "shuttle_projects"));
        assert!(tools.iter().any(|tool| tool["name"] == "shuttle_remember"));
        assert!(tools
            .iter()
            .all(|tool| tool["inputSchema"]["additionalProperties"] == false));
    }

    #[tokio::test]
    async fn mcp_initialized_notification_returns_no_content() {
        let runtime = test_runtime(registry(), Arc::new(FakeRunner::default()));
        env::remove_var("TEST_EMPTY_GATEWAY_TOKEN");
        let response = router(runtime)
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/mcp")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn http_remember_requires_project_and_recall_uses_default() {
        let runner = Arc::new(FakeRunner::default());
        let runtime = test_runtime(registry(), runner.clone());
        env::remove_var("TEST_EMPTY_GATEWAY_TOKEN");

        let remember = router(runtime.clone())
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/remember")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"text":"note"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(remember.status(), StatusCode::BAD_REQUEST);

        let recall = router(runtime)
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/recall")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"query":"sqlite"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(recall.status(), StatusCode::OK);
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0].1, vec!["recall", "sqlite"]);
    }

    #[tokio::test]
    async fn current_project_without_default_returns_not_found() {
        let runtime = test_runtime(registry_without_default(), Arc::new(FakeRunner::default()));
        env::remove_var("TEST_EMPTY_GATEWAY_TOKEN");
        let response = router(runtime)
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/projects/current")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn oauth_routes_issue_token_that_authorizes_protected_routes() {
        let oauth_dir = tempfile::tempdir().unwrap();
        let oauth = Arc::new(OAuthRuntime {
            config: OAuthConfig {
                public_url: "https://shuttle.example.test".to_owned(),
                admin_token: Some("admin-token".to_owned()),
            },
            store: OAuthStore::open(oauth_dir.path().join("oauth.db")).unwrap(),
        });
        let runtime = GatewayRuntime {
            service: Arc::new(GatewayService::new(
                registry(),
                Arc::new(FakeRunner::default()),
            )),
            auth: GatewayAuth::OAuth(oauth),
        };
        let app = router(runtime);

        let register = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/oauth/register")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"redirect_uris":["https://client.example.test/callback"],"client_name":"client"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(register.status(), StatusCode::CREATED);
        let body = register.into_body().collect().await.unwrap().to_bytes();
        let registered: Value = serde_json::from_slice(&body).unwrap();
        assert!(registered.get("client_secret").is_none());
        let client_id = registered["client_id"].as_str().unwrap();
        let verifier = "abc123abc123abc123abc123abc123abc123abc123abc123";
        let challenge = pkce_s256(verifier);
        let form = format!(
            "admin_token=admin-token&response_type=code&client_id={client_id}&redirect_uri=https%3A%2F%2Fclient.example.test%2Fcallback&state=state-123&scope=mcp&code_challenge={challenge}&code_challenge_method=S256"
        );
        let authorize = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/oauth/authorize")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from(form))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(authorize.status(), StatusCode::SEE_OTHER);
        let location = authorize
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .unwrap();
        assert!(location.contains("&state=state-123"));
        let code = location
            .split("code=")
            .nth(1)
            .unwrap()
            .split('&')
            .next()
            .unwrap();
        let token_form = format!(
            "grant_type=authorization_code&client_id={client_id}&redirect_uri=https%3A%2F%2Fclient.example.test%2Fcallback&code={code}&code_verifier={verifier}"
        );
        let token = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/oauth/token")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from(token_form))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(token.status(), StatusCode::OK);
        let body = token.into_body().collect().await.unwrap().to_bytes();
        let token: Value = serde_json::from_slice(&body).unwrap();
        let access_token = token["access_token"].as_str().unwrap();

        let unauthorized = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/projects")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

        let authorized = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/projects")
                    .header(header::AUTHORIZATION, format!("Bearer {access_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(authorized.status(), StatusCode::OK);
    }

    fn test_runtime(registry: ProjectRegistry, runner: Arc<FakeRunner>) -> GatewayRuntime {
        GatewayRuntime {
            service: Arc::new(GatewayService::new(registry, runner)),
            auth: GatewayAuth::Bearer {
                token_env: "TEST_EMPTY_GATEWAY_TOKEN".to_owned(),
            },
        }
    }

    fn registry_without_default() -> ProjectRegistry {
        ProjectRegistry::new(
            String::new(),
            BTreeMap::from([(
                "demo".to_owned(),
                ProjectConfig {
                    repo: PathBuf::from("/tmp/demo"),
                    db: None,
                    description: None,
                },
            )]),
        )
        .unwrap()
    }

    fn pkce_s256(verifier: &str) -> String {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine;
        use sha2::{Digest, Sha256};

        URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
    }
}
