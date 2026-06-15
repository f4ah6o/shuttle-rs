use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::io::Write;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use async_trait::async_trait;
use axum::extract::{Form, Path as AxumPath, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use futures_executor::block_on;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::process::Command;
use toml_edit::{value, DocumentMut, Item, Table};
use uuid::Uuid;

use crate::context;
use crate::core::{Event, EventStore, EventType, Result, ShuttleError};
use crate::memory;
use crate::oauth::{self, OAuthConfig, OAuthStore};
use crate::store::SqliteEventStore;
use crate::task;

#[derive(Debug, Clone, Deserialize)]
pub struct GatewayConfig {
    #[serde(skip)]
    config_path: Option<PathBuf>,
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub auth: AuthConfig,
    #[serde(default)]
    pub oauth: OAuthGatewayConfig,
    #[serde(default)]
    pub defaults: DefaultsConfig,
    #[serde(default)]
    pub listeners: Vec<ListenerConfig>,
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
    #[serde(default)]
    pub backend: ProjectBackendKind,
    #[serde(default)]
    pub repo: Option<PathBuf>,
    #[serde(default)]
    pub db: Option<PathBuf>,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub token_env: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProjectBackendKind {
    #[default]
    Local,
    Http,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ListenerConfig {
    pub name: String,
    pub addr: SocketAddr,
    pub auth: ListenerAuthKind,
    #[serde(default)]
    pub public_url: String,
    #[serde(default)]
    pub oauth_db_path: Option<PathBuf>,
    #[serde(default = "default_oauth_admin_token_env")]
    pub oauth_admin_token_env: String,
    #[serde(default = "default_gateway_token_env")]
    pub bearer_token_env: String,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ListenerAuthKind {
    OAuth,
    Bearer,
    None,
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
        cfg.config_path = Some(abs_path.clone());

        cfg.oauth.public_url = normalize_public_url(&cfg.oauth.public_url);
        if cfg.projects.is_empty() {
            return Err(ShuttleError::Store(
                "at least one project is required".to_owned(),
            ));
        }
        for (name, project) in &cfg.projects {
            validate_project_config(name, project)?;
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
        let config_dir = abs_path.parent().unwrap_or_else(|| Path::new("."));
        for listener in &mut cfg.listeners {
            listener.public_url = normalize_public_url(&listener.public_url);
            if listener.name.trim().is_empty() {
                return Err(ShuttleError::Store(
                    "listener name cannot be empty".to_owned(),
                ));
            }
            match listener.auth {
                ListenerAuthKind::OAuth => {
                    if listener.public_url.is_empty() {
                        return Err(ShuttleError::Store(format!(
                            "listener {:?} public_url is required for oauth auth",
                            listener.name
                        )));
                    }
                    match &listener.oauth_db_path {
                        Some(path) if !path.is_absolute() => {
                            return Err(ShuttleError::Store(format!(
                                "listener {:?} oauth_db_path must be an absolute path when set",
                                listener.name
                            )));
                        }
                        Some(_) => {}
                        None => {
                            listener.oauth_db_path = Some(
                                config_dir.join(format!("gateway-{}-oauth.db", listener.name)),
                            );
                        }
                    }
                }
                ListenerAuthKind::Bearer => {}
                ListenerAuthKind::None => {
                    if !is_loopback_addr(listener.addr) {
                        return Err(ShuttleError::Store(format!(
                            "listener {:?} auth none is only allowed on loopback addresses",
                            listener.name
                        )));
                    }
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

#[derive(Clone)]
pub struct GatewayListener {
    pub name: String,
    pub addr: SocketAddr,
    pub runtime: GatewayRuntime,
}

impl GatewayRuntime {
    pub fn from_config(
        config: GatewayConfig,
        stl: Option<PathBuf>,
        timeout: Duration,
    ) -> Result<Self> {
        let listeners = Self::listeners_from_config(config, stl, timeout)?;
        if listeners.len() != 1 {
            return Err(ShuttleError::Store(
                "GatewayRuntime::from_config requires exactly one listener".to_owned(),
            ));
        }
        Ok(listeners.into_iter().next().unwrap().runtime)
    }

    pub fn listeners_from_config(
        mut config: GatewayConfig,
        stl: Option<PathBuf>,
        timeout: Duration,
    ) -> Result<Vec<GatewayListener>> {
        let listener_configs = if config.listeners.is_empty() {
            vec![legacy_listener_config(&config)]
        } else {
            std::mem::take(&mut config.listeners)
        };
        let config_path = config.config_path.clone();
        let registry = ProjectRegistry::new(config.defaults.project, config.projects)?;
        // Default to the built-in in-process engine so the gateway runs local
        // projects standalone; an explicit `stl` binary opts back into subprocess
        // execution for compatibility.
        let runner: Arc<dyn Runner> = match stl {
            Some(binary) => Arc::new(SubprocessRunner { binary, timeout }),
            None => Arc::new(LibraryRunner { timeout }),
        };
        let service = Arc::new(GatewayService::new_with_config_path(
            registry,
            runner,
            config_path,
        ));
        listener_configs
            .into_iter()
            .map(|listener| {
                let auth = auth_from_listener(&listener)?;
                Ok(GatewayListener {
                    name: listener.name,
                    addr: listener.addr,
                    runtime: GatewayRuntime {
                        service: service.clone(),
                        auth,
                    },
                })
            })
            .collect()
    }
}

#[derive(Clone)]
enum GatewayAuth {
    Bearer { token_env: String },
    OAuth(Arc<OAuthRuntime>),
    None,
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

pub async fn serve_listeners(listeners: Vec<GatewayListener>) -> Result<()> {
    if listeners.is_empty() {
        return Err(ShuttleError::Store(
            "at least one listener is required".to_owned(),
        ));
    }
    let mut tasks = Vec::new();
    for listener in listeners {
        let addr = listener.addr;
        let runtime = listener.runtime;
        let name = listener.name;
        tasks.push(tokio::spawn(async move {
            let tcp = tokio::net::TcpListener::bind(addr)
                .await
                .map_err(|err| ShuttleError::Store(format!("listener {name}: {err}")))?;
            axum::serve(tcp, router(runtime))
                .await
                .map_err(|err| ShuttleError::Store(format!("listener {name}: {err}")))
        }));
    }
    for task in tasks {
        task.await
            .map_err(|err| ShuttleError::Store(err.to_string()))??;
    }
    Ok(())
}

pub fn router(runtime: GatewayRuntime) -> Router {
    Router::new()
        .route("/api/projects", get(api_projects).post(api_add_project))
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
    pub backend: ProjectBackendKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub db: Option<PathBuf>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub url: String,
    #[serde(skip)]
    pub token_env: Option<String>,
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
        for (name, cfg) in &configs {
            validate_project_config(name, cfg)?;
        }
        let projects = configs
            .into_iter()
            .map(|(name, cfg)| {
                let project = project_from_config(name.clone(), cfg);
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

    pub fn names(&self) -> BTreeSet<String> {
        self.projects.keys().cloned().collect()
    }

    pub fn insert_named(&mut self, name: String, config: ProjectConfig) -> Result<Project> {
        validate_project_config(&name, &config)?;
        if self.projects.contains_key(&name) {
            return Err(ShuttleError::Store(format!(
                "project {name:?} is already configured"
            )));
        }
        let project = project_from_config(name.clone(), config);
        self.projects.insert(name, project.clone());
        Ok(project)
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

fn project_from_config(name: String, cfg: ProjectConfig) -> Project {
    Project {
        name,
        backend: cfg.backend,
        repo: cfg.repo,
        db: cfg.db,
        url: normalize_public_url(&cfg.url),
        token_env: cfg.token_env,
        description: cfg.description,
    }
}

fn validate_project_config(name: &str, project: &ProjectConfig) -> Result<()> {
    validate_project_name(name)?;
    match project.backend {
        ProjectBackendKind::Local => {
            let Some(repo) = &project.repo else {
                return Err(ShuttleError::Store(format!(
                    "project {name:?} repo is required for local backend"
                )));
            };
            if !repo.is_absolute() {
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
        ProjectBackendKind::Http => {
            if project.url.trim().is_empty() {
                return Err(ShuttleError::Store(format!(
                    "project {name:?} url is required for http backend"
                )));
            }
        }
    }
    Ok(())
}

fn validate_project_name(name: &str) -> Result<()> {
    if name.trim().is_empty() {
        return Err(ShuttleError::Store(
            "project name cannot be empty".to_owned(),
        ));
    }
    Ok(())
}

fn normalize_project_name(name: &str) -> Result<String> {
    validate_project_name(name)?;
    Ok(name.trim().to_owned())
}

fn unique_project_name(base_name: &str, existing_names: &BTreeSet<String>) -> String {
    if !existing_names.contains(base_name) {
        return base_name.to_owned();
    }
    for index in 2.. {
        let candidate = format!("{base_name}-{index}");
        if !existing_names.contains(&candidate) {
            return candidate;
        }
    }
    unreachable!("unbounded suffix search always returns")
}

fn persist_project_config(
    config_path: &Path,
    base_name: &str,
    config: &ProjectConfig,
    registry_names: &BTreeSet<String>,
) -> Result<String> {
    let raw =
        std::fs::read_to_string(config_path).map_err(|err| ShuttleError::Store(err.to_string()))?;
    let mut document = raw
        .parse::<DocumentMut>()
        .map_err(|err| ShuttleError::Serialization(err.to_string()))?;
    let mut existing_names = registry_names.clone();
    if let Some(projects) = document.get("projects").and_then(Item::as_table) {
        existing_names.extend(projects.iter().map(|(name, _)| name.to_owned()));
    }
    let name = unique_project_name(base_name, &existing_names);

    let projects = document
        .entry("projects")
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_mut()
        .ok_or_else(|| ShuttleError::Store("projects must be a TOML table".to_owned()))?;
    projects[&name] = project_config_item(config);
    write_config_atomically(config_path, document.to_string().as_bytes())?;
    Ok(name)
}

fn project_config_item(config: &ProjectConfig) -> Item {
    let mut table = Table::new();
    table["backend"] = value(match config.backend {
        ProjectBackendKind::Local => "local",
        ProjectBackendKind::Http => "http",
    });
    if let Some(repo) = &config.repo {
        table["repo"] = value(repo.display().to_string());
    }
    if let Some(db) = &config.db {
        table["db"] = value(db.display().to_string());
    }
    if !config.url.trim().is_empty() {
        table["url"] = value(normalize_public_url(&config.url));
    }
    if let Some(token_env) = &config.token_env {
        if !token_env.trim().is_empty() {
            table["token_env"] = value(token_env.trim());
        }
    }
    if let Some(description) = &config.description {
        if !description.trim().is_empty() {
            table["description"] = value(description.trim());
        }
    }
    Item::Table(table)
}

fn write_config_atomically(path: &Path, contents: &[u8]) -> Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut temp =
        tempfile::NamedTempFile::new_in(dir).map_err(|err| ShuttleError::Store(err.to_string()))?;
    temp.write_all(contents)
        .map_err(|err| ShuttleError::Store(err.to_string()))?;
    temp.flush()
        .map_err(|err| ShuttleError::Store(err.to_string()))?;
    temp.persist(path)
        .map_err(|err| ShuttleError::Store(err.to_string()))?;
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct ServiceResponse {
    pub project: String,
    pub result: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stored: Option<bool>,
}

pub struct GatewayService {
    projects: Mutex<ProjectRegistry>,
    runner: Arc<dyn Runner>,
    config_path: Option<PathBuf>,
    current: Mutex<String>,
}

impl GatewayService {
    pub fn new(projects: ProjectRegistry, runner: Arc<dyn Runner>) -> Self {
        Self::new_with_config_path(projects, runner, None)
    }

    pub fn new_with_config_path(
        projects: ProjectRegistry,
        runner: Arc<dyn Runner>,
        config_path: Option<PathBuf>,
    ) -> Self {
        Self {
            projects: Mutex::new(projects),
            runner,
            config_path,
            current: Mutex::new(String::new()),
        }
    }

    pub fn list_projects(&self) -> Result<Vec<Project>> {
        Ok(self.projects()?.list())
    }

    pub fn add_project(
        &self,
        name: &str,
        config: ProjectConfig,
        make_current: bool,
    ) -> Result<Project> {
        // Keep registry and current-project locks separate so gateway commands never hold
        // mutable registry access while later running backend work.
        let base_name = normalize_project_name(name)?;
        validate_project_config(&base_name, &config)?;
        let project = {
            let mut projects = self.projects()?;
            let name = if let Some(config_path) = &self.config_path {
                persist_project_config(config_path, &base_name, &config, &projects.names())?
            } else {
                unique_project_name(&base_name, &projects.names())
            };
            projects.insert_named(name, config)?
        };
        if make_current {
            *self.current()? = project.name.clone();
        }
        Ok(project)
    }

    pub fn use_project(&self, name: &str) -> Result<Project> {
        // Validate against the registry before updating current; current_project() falls
        // back to the default if future runtime removal leaves this value stale.
        let project = {
            let projects = self.projects()?;
            projects
                .get(name)
                .ok_or_else(|| ShuttleError::Store(format!("unknown project {name:?}")))?
        };
        *self.current()? = name.to_owned();
        Ok(project)
    }

    pub fn current_project(&self) -> Result<Project> {
        let current = self.current()?.clone();
        if !current.is_empty() {
            if let Some(project) = self.projects()?.get(&current) {
                return Ok(project);
            }
        }
        self.projects()?
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
        let project = {
            let projects = self.projects()?;
            projects.resolve(project, write)?
        };
        let result = self.runner.run(&project, args).await.map_err(|err| {
            ShuttleError::Store(format!("stl failed for project {}: {err}", project.name))
        })?;
        Ok(ServiceResponse {
            project: project.name,
            result,
            stored: write.then_some(true),
        })
    }

    fn projects(&self) -> Result<MutexGuard<'_, ProjectRegistry>> {
        self.projects
            .lock()
            .map_err(|err| ShuttleError::Store(err.to_string()))
    }

    fn current(&self) -> Result<MutexGuard<'_, String>> {
        self.current
            .lock()
            .map_err(|err| ShuttleError::Store(err.to_string()))
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
        if project.backend == ProjectBackendKind::Http {
            return run_http_backend(project, args, self.timeout).await;
        }
        let repo = project
            .repo
            .as_ref()
            .ok_or_else(|| "repo is required for local backend".to_owned())?;
        let mut command = Command::new(&self.binary);
        command.arg("--json").args(args).current_dir(repo);
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

/// In-process runner that executes local projects against their Shuttle SQLite
/// database directly, so the gateway works standalone without an external `stl`
/// binary. HTTP-backed projects reuse the shared HTTP call path.
pub struct LibraryRunner {
    timeout: Duration,
}

#[async_trait]
impl Runner for LibraryRunner {
    async fn run(&self, project: &Project, args: &[&str]) -> std::result::Result<Value, String> {
        if project.backend == ProjectBackendKind::Http {
            return run_http_backend(project, args, self.timeout).await;
        }
        let project = project.clone();
        let args = args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
        tokio::task::spawn_blocking(move || local_backend_call(&project, &args))
            .await
            .map_err(|err| err.to_string())?
    }
}

async fn run_http_backend(
    project: &Project,
    args: &[&str],
    timeout: Duration,
) -> std::result::Result<Value, String> {
    let project = project.clone();
    let args = args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
    tokio::task::spawn_blocking(move || http_backend_call(&project, &args, timeout))
        .await
        .map_err(|err| err.to_string())?
}

/// Per-repository runtime derived the same way the `stl` CLI derives it, so
/// in-process execution observes identical workspace, agent, and session state.
struct LocalEnv {
    cwd: PathBuf,
    database_path: PathBuf,
    workspace_id: String,
    agent: String,
    session_id: String,
}

impl LocalEnv {
    fn load(repo: &Path, db: Option<&Path>) -> Result<Self> {
        let shuttle_dir = repo.join(".shuttle");
        let database_path = db
            .map(Path::to_path_buf)
            .unwrap_or_else(|| shuttle_dir.join("shuttle.db"));
        let workspace_id = load_or_create_workspace_id(&shuttle_dir, repo)?;
        let agent = load_agent(&shuttle_dir);
        let session_id = load_or_create_session_id(&shuttle_dir)?;
        Ok(Self {
            cwd: repo.to_path_buf(),
            database_path,
            workspace_id,
            agent,
            session_id,
        })
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct WorkspaceFile {
    workspace_id: String,
    repo_path: String,
    created_at: DateTime<Utc>,
}

fn load_or_create_workspace_id(shuttle_dir: &Path, root: &Path) -> Result<String> {
    let path = shuttle_dir.join("workspace.json");
    if let Ok(contents) = fs::read_to_string(&path) {
        let workspace: WorkspaceFile = serde_json::from_str(&contents)
            .map_err(|err| ShuttleError::Serialization(err.to_string()))?;
        return Ok(workspace.workspace_id);
    }
    fs::create_dir_all(shuttle_dir).map_err(|err| ShuttleError::Store(err.to_string()))?;
    let workspace = WorkspaceFile {
        workspace_id: Uuid::new_v4().to_string(),
        repo_path: root.display().to_string(),
        created_at: Utc::now(),
    };
    let serialized =
        serde_json::to_string_pretty(&workspace).map_err(|err| ShuttleError::Serialization(err.to_string()))?;
    fs::write(&path, serialized).map_err(|err| ShuttleError::Store(err.to_string()))?;
    Ok(workspace.workspace_id)
}

fn load_agent(shuttle_dir: &Path) -> String {
    if let Ok(agent) = env::var("SHUTTLE_AGENT") {
        let agent = agent.trim();
        if !agent.is_empty() {
            return agent.to_owned();
        }
    }
    if let Ok(contents) = fs::read_to_string(shuttle_dir.join("agent")) {
        let agent = contents.trim();
        if !agent.is_empty() {
            return agent.to_owned();
        }
    }
    "unknown".to_owned()
}

fn load_or_create_session_id(shuttle_dir: &Path) -> Result<String> {
    if let Ok(session_id) = env::var("SHUTTLE_SESSION_ID") {
        return Ok(session_id);
    }
    let path = shuttle_dir.join("session");
    if let Ok(contents) = fs::read_to_string(&path) {
        let session_id = contents.trim();
        if !session_id.is_empty() {
            return Ok(session_id.to_owned());
        }
    }
    fs::create_dir_all(shuttle_dir).map_err(|err| ShuttleError::Store(err.to_string()))?;
    let session_id = Uuid::new_v4().to_string();
    fs::write(&path, format!("{session_id}\n")).map_err(|err| ShuttleError::Store(err.to_string()))?;
    Ok(session_id)
}

fn with_local_repo_metadata(mut event: Event, env: &LocalEnv) -> Event {
    if let Ok(status) = context::repo_status(&env.cwd) {
        let repo_id = context::repo_id(&status);
        event.repo_id = Some(repo_id.clone());
        event.repo_path = Some(status.repo_path.clone());
        event.git_remote = status.git_remote.clone();
        event.branch = Some(status.branch.clone());
        event.commit = Some(status.commit.clone());
        event.repo_dirty = Some(status.dirty);
        if let Some(metadata) = event.metadata_json.as_object_mut() {
            metadata.insert("repo_id".to_owned(), json!(repo_id));
            metadata.insert("repo_path".to_owned(), json!(status.repo_path));
            metadata.insert("git_remote".to_owned(), json!(status.git_remote));
            metadata.insert("branch".to_owned(), json!(status.branch));
            metadata.insert("commit".to_owned(), json!(status.commit));
            metadata.insert("repo_dirty".to_owned(), json!(status.dirty));
            metadata.insert("dirty_files".to_owned(), json!(status.dirty_files));
        }
    }
    event
}

fn local_backend_call(project: &Project, args: &[String]) -> std::result::Result<Value, String> {
    let repo = project
        .repo
        .as_ref()
        .ok_or_else(|| "repo is required for local backend".to_owned())?;
    let env = LocalEnv::load(repo, project.db.as_deref()).map_err(|err| err.to_string())?;
    let store = SqliteEventStore::open(&env.database_path).map_err(|err| err.to_string())?;
    let args = args.iter().map(String::as_str).collect::<Vec<_>>();
    let value = match args.as_slice() {
        ["context"] => {
            let context = block_on(context::assemble_context(
                &store,
                &env.cwd,
                &env.workspace_id,
                &env.agent,
            ))
            .map_err(|err| err.to_string())?;
            to_json_value(&context)?
        }
        ["recall", query] => {
            let status = context::repo_status(&env.cwd).ok();
            let repo_id = status.as_ref().map(context::repo_id);
            let branch = status.as_ref().map(|status| status.branch.as_str());
            let results = block_on(memory::ranked_recall(
                &store,
                query,
                None,
                Some(&env.workspace_id),
                repo_id.as_deref(),
                branch,
            ))
            .map_err(|err| err.to_string())?;
            to_json_value(&results)?
        }
        [command, text] if is_memory_command(command) => {
            let event = with_local_repo_metadata(
                memory::new_typed_memory(
                    memory_event_type_for_command(command),
                    env.workspace_id.clone(),
                    env.agent.clone(),
                    env.session_id.clone(),
                    (*text).to_owned(),
                ),
                &env,
            );
            let event = block_on(store.append(event)).map_err(|err| err.to_string())?;
            to_json_value(&event)?
        }
        ["task", "list"] => {
            let tasks = block_on(task::tasks(&store, Some(&env.workspace_id), None))
                .map_err(|err| err.to_string())?;
            to_json_value(&tasks)?
        }
        ["task", "create", content] => {
            let event = with_local_repo_metadata(
                task::new_task(
                    env.workspace_id.clone(),
                    env.agent.clone(),
                    env.session_id.clone(),
                    (*content).to_owned(),
                ),
                &env,
            );
            let event = block_on(store.append(event)).map_err(|err| err.to_string())?;
            to_json_value(&event)?
        }
        ["task", "update", id, text] => {
            let task_id = parse_task_id(id)?;
            block_on(task::ensure_task_exists(&store, &env.workspace_id, task_id))
                .map_err(|err| err.to_string())?;
            let event = with_local_repo_metadata(
                task::new_task_update(
                    env.workspace_id.clone(),
                    env.agent.clone(),
                    env.session_id.clone(),
                    task_id,
                    (*text).to_owned(),
                ),
                &env,
            );
            let event = block_on(store.append(event)).map_err(|err| err.to_string())?;
            to_json_value(&event)?
        }
        ["task", "done", id] => {
            let task_id = parse_task_id(id)?;
            block_on(task::ensure_task_exists(&store, &env.workspace_id, task_id))
                .map_err(|err| err.to_string())?;
            let event = with_local_repo_metadata(
                task::new_task_done(
                    env.workspace_id.clone(),
                    env.agent.clone(),
                    env.session_id.clone(),
                    task_id,
                ),
                &env,
            );
            let event = block_on(store.append(event)).map_err(|err| err.to_string())?;
            to_json_value(&event)?
        }
        _ => {
            return Err(format!(
                "unsupported local backend command: {}",
                args.join(" ")
            ))
        }
    };
    Ok(value)
}

fn to_json_value<T: Serialize>(value: &T) -> std::result::Result<Value, String> {
    serde_json::to_value(value).map_err(|err| err.to_string())
}

fn parse_task_id(id: &str) -> std::result::Result<Uuid, String> {
    Uuid::parse_str(id).map_err(|err| format!("invalid task id {id:?}: {err}"))
}

fn memory_event_type_for_command(command: &str) -> EventType {
    match command {
        "decide" => EventType::Decision,
        "observe" => EventType::Observation,
        "pattern" => EventType::Pattern,
        "fact" => EventType::Fact,
        "bug" => EventType::Bug,
        _ => EventType::Memory,
    }
}

fn http_backend_call(
    project: &Project,
    args: &[String],
    timeout: Duration,
) -> std::result::Result<Value, String> {
    let base = project.url.trim_end_matches('/');
    let agent = ureq::AgentBuilder::new().timeout(timeout).build();
    let request = |method: &str, path: &str| {
        let req = match method {
            "GET" => agent.get(&format!("{base}{path}")),
            "PATCH" => agent.request("PATCH", &format!("{base}{path}")),
            _ => agent.post(&format!("{base}{path}")),
        };
        if let Some(token_env) = &project.token_env {
            if let Ok(token) = env::var(token_env) {
                if !token.is_empty() {
                    return req.set(header::AUTHORIZATION.as_str(), &format!("Bearer {token}"));
                }
            }
        }
        req
    };
    let response = match args {
        [cmd] if cmd == "context" => request("GET", "/api/context").call(),
        [cmd, query] if cmd == "recall" => {
            request("POST", "/api/recall").send_json(json!({ "query": query }))
        }
        [cmd, text] if is_memory_command(cmd) => request("POST", "/api/remember")
            .send_json(json!({ "kind": memory_kind_for_command(cmd), "text": text })),
        [task, cmd] if task == "task" && cmd == "list" => request("GET", "/api/tasks").call(),
        [task, cmd, content] if task == "task" && cmd == "create" => {
            request("POST", "/api/tasks").send_json(json!({ "title": content, "body": "" }))
        }
        [task, cmd, id, text] if task == "task" && cmd == "update" => {
            request("PATCH", &format!("/api/tasks/{id}")).send_json(json!({ "text": text }))
        }
        [task, cmd, id] if task == "task" && cmd == "done" => {
            request("POST", &format!("/api/tasks/{id}/done")).send_json(json!({}))
        }
        _ => {
            return Err(format!(
                "unsupported http backend command: {}",
                args.join(" ")
            ))
        }
    };
    let response = response.map_err(|err| match err {
        ureq::Error::Status(status, response) => {
            let body = response.into_string().unwrap_or_default();
            if body.trim().is_empty() {
                format!("http backend returned status {status}")
            } else {
                format!("http backend returned status {status}: {body}")
            }
        }
        ureq::Error::Transport(err) => err.to_string(),
    })?;
    response.into_json::<Value>().map_err(|err| err.to_string())
}

fn is_memory_command(command: &str) -> bool {
    matches!(
        command,
        "remember" | "decide" | "observe" | "pattern" | "fact" | "bug"
    )
}

fn memory_kind_for_command(command: &str) -> &str {
    match command {
        "decide" => "decision",
        "observe" => "observation",
        "pattern" => "pattern",
        "fact" => "fact",
        "bug" => "bug",
        _ => "memory",
    }
}

async fn api_projects(State(runtime): State<GatewayRuntime>, headers: HeaderMap) -> Response {
    if let Err(response) = authorize(&runtime, &headers, "/api/projects", false) {
        return *response;
    }
    match runtime.service.list_projects() {
        Ok(projects) => Json(json!({ "projects": projects })).into_response(),
        Err(err) => error_response(err),
    }
}

#[derive(Debug, Deserialize)]
struct AddProjectRequest {
    #[serde(default)]
    name: String,
    #[serde(default)]
    backend: ProjectBackendKind,
    #[serde(default)]
    repo: Option<PathBuf>,
    #[serde(default)]
    db: Option<PathBuf>,
    #[serde(default)]
    url: String,
    #[serde(default)]
    token_env: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    make_current: bool,
}

impl AddProjectRequest {
    fn into_parts(self) -> (String, ProjectConfig, bool) {
        (
            self.name,
            ProjectConfig {
                backend: self.backend,
                repo: self.repo,
                db: self.db,
                url: self.url,
                token_env: self.token_env,
                description: self.description,
            },
            self.make_current,
        )
    }
}

async fn api_add_project(
    State(runtime): State<GatewayRuntime>,
    headers: HeaderMap,
    Json(request): Json<AddProjectRequest>,
) -> Response {
    if let Err(response) = authorize(&runtime, &headers, "/api/projects", false) {
        return *response;
    }
    let (name, config, make_current) = request.into_parts();
    match runtime.service.add_project(&name, config, make_current) {
        Ok(project) => (StatusCode::CREATED, Json(project)).into_response(),
        Err(err) => error_response(err),
    }
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
                json!({
                    "content": [{ "type": "text", "text": value.to_string() }],
                    "structuredContent": value,
                }),
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
        "shuttle_projects" => Ok(json!({ "projects": service.list_projects()? })),
        "shuttle_project_add" => {
            let (name, config, make_current) = project_add_args(&args)?;
            serde_json::to_value(service.add_project(&name, config, make_current)?)
                .map_err(|err| ShuttleError::Serialization(err.to_string()))
        }
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

fn project_add_args(args: &Value) -> Result<(String, ProjectConfig, bool)> {
    Ok((
        str_arg(args, "name")?.to_owned(),
        ProjectConfig {
            backend: project_backend_arg(args, "backend")?,
            repo: optional_path_arg(args, "repo"),
            db: optional_path_arg(args, "db"),
            url: optional_string_arg(args, "url").unwrap_or_default(),
            token_env: optional_string_arg(args, "token_env"),
            description: optional_string_arg(args, "description"),
        },
        optional_bool_arg(args, "make_current"),
    ))
}

fn project_backend_arg(args: &Value, key: &str) -> Result<ProjectBackendKind> {
    match optional_str_arg(args, key) {
        "" => Ok(ProjectBackendKind::Local),
        "local" => Ok(ProjectBackendKind::Local),
        "http" => Ok(ProjectBackendKind::Http),
        other => Err(ShuttleError::Store(format!(
            "{key} must be one of: local, http; got {other:?}"
        ))),
    }
}

fn optional_path_arg(args: &Value, key: &str) -> Option<PathBuf> {
    optional_string_arg(args, key).map(PathBuf::from)
}

fn optional_string_arg(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn optional_bool_arg(args: &Value, key: &str) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(false)
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
            projects_output_schema(),
        ),
        tool(
            "shuttle_project_add",
            "Add a Shuttle project to the running gateway",
            json!({
                "name": string_schema("Project name"),
                "backend": enum_schema("Project backend; defaults to local", &["local", "http"]),
                "repo": nullable_string_schema("Absolute local repository path for local backends"),
                "db": nullable_string_schema("Absolute local Shuttle database path"),
                "url": string_schema("HTTP project base URL for http backends"),
                "token_env": nullable_string_schema("Environment variable name containing the backend bearer token"),
                "description": nullable_string_schema("Project description"),
                "make_current": bool_schema("Set the added project as the current project"),
            }),
            vec!["name"],
            project_output_schema(),
        ),
        tool(
            "shuttle_current_project",
            "Read the current or default project",
            json!({}),
            vec![],
            project_output_schema(),
        ),
        tool(
            "shuttle_use_project",
            "Set the current project",
            json!({"project": string_schema("Configured project name")}),
            vec!["project"],
            project_output_schema(),
        ),
        tool(
            "shuttle_context",
            "Read Shuttle context for a project",
            json!({"project": string_schema("Configured project name; optional with default project")}),
            vec![],
            service_response_output_schema(),
        ),
        tool(
            "shuttle_recall",
            "Search Shuttle memories in a project",
            json!({"project": string_schema("Configured project name; optional with default project"), "query": string_schema("Recall query")}),
            vec!["query"],
            service_response_output_schema(),
        ),
        tool(
            "shuttle_remember",
            "Store a Shuttle memory in a project",
            json!({"project": string_schema("Configured project name"), "kind": enum_schema("Memory kind", &["memory", "decision", "observation", "pattern", "fact", "bug"]), "text": string_schema("Memory text")}),
            vec!["project", "text"],
            service_response_output_schema(),
        ),
        tool(
            "shuttle_task_list",
            "List Shuttle tasks in a project",
            json!({"project": string_schema("Configured project name; optional with default project")}),
            vec![],
            service_response_output_schema(),
        ),
        tool(
            "shuttle_task_create",
            "Create a Shuttle task in a project",
            json!({"project": string_schema("Configured project name"), "title": string_schema("Task title"), "body": string_schema("Optional task body")}),
            vec!["project", "title"],
            service_response_output_schema(),
        ),
        tool(
            "shuttle_task_update",
            "Update a Shuttle task in a project",
            json!({"project": string_schema("Configured project name"), "task_id": string_schema("Task UUID"), "text": string_schema("Update text")}),
            vec!["project", "task_id", "text"],
            service_response_output_schema(),
        ),
        tool(
            "shuttle_task_done",
            "Complete a Shuttle task in a project",
            json!({"project": string_schema("Configured project name"), "task_id": string_schema("Task UUID")}),
            vec!["project", "task_id"],
            service_response_output_schema(),
        ),
    ]
}

fn tool(
    name: &str,
    description: &str,
    properties: Value,
    required: Vec<&str>,
    output_schema: Value,
) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false,
        },
        "outputSchema": output_schema,
    })
}

fn projects_output_schema() -> Value {
    object_schema(
        json!({ "projects": { "type": "array", "items": project_schema() } }),
        vec!["projects"],
    )
}

fn project_output_schema() -> Value {
    project_schema()
}

fn service_response_output_schema() -> Value {
    object_schema(
        json!({
            "project": string_schema("Configured project name"),
            "result": json_schema("Tool result from the selected project"),
            "stored": {
                "type": "boolean",
                "description": "Whether the operation stored data",
            },
        }),
        vec!["project", "result"],
    )
}

fn project_schema() -> Value {
    object_schema(
        json!({
            "name": string_schema("Configured project name"),
            "backend": enum_schema("Project backend", &["local", "http"]),
            "repo": nullable_string_schema("Local repository path"),
            "db": nullable_string_schema("Local Shuttle database path"),
            "url": string_schema("HTTP project base URL"),
            "description": nullable_string_schema("Project description"),
        }),
        vec!["name", "backend", "url"],
    )
}

fn object_schema(properties: Value, required: Vec<&str>) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": true,
    })
}

fn string_schema(description: &str) -> Value {
    json!({ "type": "string", "description": description })
}

fn bool_schema(description: &str) -> Value {
    json!({ "type": "boolean", "description": description })
}

fn nullable_string_schema(description: &str) -> Value {
    json!({ "type": ["string", "null"], "description": description })
}

fn json_schema(description: &str) -> Value {
    json!({
        "type": ["object", "array", "string", "number", "integer", "boolean", "null"],
        "description": description,
    })
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
        GatewayAuth::None => Ok(()),
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

fn legacy_listener_config(config: &GatewayConfig) -> ListenerConfig {
    if config.oauth.public_url.is_empty() {
        ListenerConfig {
            name: "default".to_owned(),
            addr: config.server.addr,
            auth: ListenerAuthKind::Bearer,
            public_url: String::new(),
            oauth_db_path: None,
            oauth_admin_token_env: config.oauth.admin_token_env.clone(),
            bearer_token_env: config.auth.bearer_token_env.clone(),
        }
    } else {
        ListenerConfig {
            name: "default".to_owned(),
            addr: config.server.addr,
            auth: ListenerAuthKind::OAuth,
            public_url: config.oauth.public_url.clone(),
            oauth_db_path: config.oauth.db_path.clone(),
            oauth_admin_token_env: config.oauth.admin_token_env.clone(),
            bearer_token_env: config.auth.bearer_token_env.clone(),
        }
    }
}

fn auth_from_listener(listener: &ListenerConfig) -> Result<GatewayAuth> {
    match listener.auth {
        ListenerAuthKind::Bearer => Ok(GatewayAuth::Bearer {
            token_env: listener.bearer_token_env.clone(),
        }),
        ListenerAuthKind::None => Ok(GatewayAuth::None),
        ListenerAuthKind::OAuth => {
            let admin_token = env::var(&listener.oauth_admin_token_env).map_err(|_| {
                ShuttleError::Store(format!(
                    "{} is required when oauth listener {:?} is configured",
                    listener.oauth_admin_token_env, listener.name
                ))
            })?;
            if admin_token.is_empty() {
                return Err(ShuttleError::Store(format!(
                    "{} is required when oauth listener {:?} is configured",
                    listener.oauth_admin_token_env, listener.name
                )));
            }
            let db_path = listener.oauth_db_path.clone().ok_or_else(|| {
                ShuttleError::Store(format!(
                    "oauth_db_path is required for listener {:?}",
                    listener.name
                ))
            })?;
            Ok(GatewayAuth::OAuth(Arc::new(OAuthRuntime {
                config: OAuthConfig {
                    public_url: listener.public_url.clone(),
                    admin_token: Some(admin_token),
                },
                store: OAuthStore::open(db_path)?,
            })))
        }
    }
}

fn is_loopback_addr(addr: SocketAddr) -> bool {
    match addr.ip() {
        IpAddr::V4(ip) => ip.is_loopback(),
        IpAddr::V6(ip) => ip.is_loopback(),
    }
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
                    backend: ProjectBackendKind::Local,
                    repo: Some(PathBuf::from("/tmp/demo")),
                    db: None,
                    url: String::new(),
                    token_env: None,
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

    #[test]
    fn config_accepts_http_projects_without_repo() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("projects.toml");
        std::fs::write(
            &path,
            "[projects.demo]\nbackend = \"http\"\nurl = \"http://127.0.0.1:8787\"\ntoken_env = \"DEMO_TOKEN\"\n",
        )
        .unwrap();
        let cfg = GatewayConfig::load(&path).unwrap();
        let project = cfg.projects.get("demo").unwrap();

        assert_eq!(project.backend, ProjectBackendKind::Http);
        assert!(project.repo.is_none());
        assert_eq!(project.token_env.as_deref(), Some("DEMO_TOKEN"));
    }

    #[test]
    fn config_rejects_http_projects_without_url() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("projects.toml");
        std::fs::write(&path, "[projects.demo]\nbackend = \"http\"\n").unwrap();

        assert!(GatewayConfig::load(&path)
            .unwrap_err()
            .to_string()
            .contains("url is required"));
    }

    #[test]
    fn config_rejects_none_listener_on_non_loopback() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("projects.toml");
        std::fs::write(
            &path,
            "[[listeners]]\nname = \"open\"\naddr = \"0.0.0.0:8787\"\nauth = \"none\"\n\n[projects.demo]\nrepo = \"/tmp/demo\"\n",
        )
        .unwrap();

        assert!(GatewayConfig::load(&path)
            .unwrap_err()
            .to_string()
            .contains("only allowed on loopback"));
    }

    #[tokio::test]
    async fn library_runner_executes_local_project_in_process() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let project = Project {
            name: "demo".to_owned(),
            backend: ProjectBackendKind::Local,
            repo: Some(repo.clone()),
            db: None,
            url: String::new(),
            token_env: None,
            description: None,
        };
        let runner = LibraryRunner {
            timeout: Duration::from_secs(5),
        };

        let stored = runner
            .run(&project, &["remember", "sqlite is the store"])
            .await
            .unwrap();
        assert_eq!(stored["content"], "sqlite is the store");

        let recalled = runner.run(&project, &["recall", "sqlite"]).await.unwrap();
        assert!(recalled
            .as_array()
            .unwrap()
            .iter()
            .any(|result| result["event"]["content"] == "sqlite is the store"));

        let created = runner
            .run(&project, &["task", "create", "ship gateway"])
            .await
            .unwrap();
        let task_id = created["id"].as_str().unwrap().to_owned();

        let listed = runner.run(&project, &["task", "list"]).await.unwrap();
        assert!(listed
            .as_array()
            .unwrap()
            .iter()
            .any(|task| task["id"] == task_id && task["content"] == "ship gateway"));

        // The database is created in-process under the project repo without an
        // external `stl` binary on PATH.
        assert!(repo.join(".shuttle/shuttle.db").exists());
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
    async fn service_add_project_routes_to_runtime_project() {
        let runner = Arc::new(FakeRunner::default());
        let service = GatewayService::new(registry(), runner.clone());
        let project = service
            .add_project(
                " extra ",
                ProjectConfig {
                    backend: ProjectBackendKind::Http,
                    repo: None,
                    db: None,
                    url: "http://127.0.0.1:9999/".to_owned(),
                    token_env: Some("EXTRA_TOKEN".to_owned()),
                    description: Some("extra project".to_owned()),
                },
                true,
            )
            .unwrap();

        assert_eq!(project.name, "extra");
        assert_eq!(project.url, "http://127.0.0.1:9999");
        assert_eq!(service.current_project().unwrap().name, "extra");
        service.task_list("extra").await.unwrap();

        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls[0].0, "extra");
        assert_eq!(calls[0].1, vec!["task", "list"]);
    }

    #[test]
    fn service_add_project_suffixes_duplicates_and_rejects_invalid_config() {
        let service = GatewayService::new(registry(), Arc::new(FakeRunner::default()));
        let duplicate = service
            .add_project(
                "demo",
                ProjectConfig {
                    backend: ProjectBackendKind::Http,
                    repo: None,
                    db: None,
                    url: "http://127.0.0.1:9999".to_owned(),
                    token_env: None,
                    description: None,
                },
                false,
            )
            .unwrap();
        assert_eq!(duplicate.name, "demo-2");
        assert!(service
            .add_project(
                "relative",
                ProjectConfig {
                    backend: ProjectBackendKind::Local,
                    repo: Some(PathBuf::from("relative")),
                    db: None,
                    url: String::new(),
                    token_env: None,
                    description: None,
                },
                false,
            )
            .unwrap_err()
            .to_string()
            .contains("absolute path"));
    }

    #[test]
    fn service_add_project_persists_local_project_config() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = write_gateway_config(dir.path(), false);
        let runtime = file_backed_runtime(&config_path, Arc::new(FakeRunner::default()));
        let repo = dir.path().join("repo");
        let db = dir.path().join("repo/.shuttle/shuttle.db");

        let project = runtime
            .service
            .add_project(
                "local-extra",
                ProjectConfig {
                    backend: ProjectBackendKind::Local,
                    repo: Some(repo.clone()),
                    db: Some(db.clone()),
                    url: String::new(),
                    token_env: None,
                    description: Some("local extra".to_owned()),
                },
                false,
            )
            .unwrap();

        assert_eq!(project.name, "local-extra");
        let reloaded = GatewayConfig::load(&config_path).unwrap();
        let persisted = reloaded.projects.get("local-extra").unwrap();
        assert_eq!(persisted.backend, ProjectBackendKind::Local);
        assert_eq!(persisted.repo.as_ref(), Some(&repo));
        assert_eq!(persisted.db.as_ref(), Some(&db));
        assert_eq!(persisted.description.as_deref(), Some("local extra"));
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
        assert!(tools
            .iter()
            .any(|tool| tool["name"] == "shuttle_project_add"));
        assert!(tools.iter().any(|tool| tool["name"] == "shuttle_remember"));
        assert!(tools
            .iter()
            .all(|tool| tool["inputSchema"]["additionalProperties"] == false));
        assert!(tools
            .iter()
            .all(|tool| tool["outputSchema"]["type"] == "object"));
    }

    #[tokio::test]
    async fn mcp_tool_call_returns_structured_content() {
        let runtime = test_runtime(registry(), Arc::new(FakeRunner::default()));
        env::remove_var("TEST_EMPTY_GATEWAY_TOKEN");
        let response = router(runtime)
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/mcp")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"shuttle_task_create","arguments":{"project":"demo","title":"ship it"}}}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let value: Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(
            value["result"]["structuredContent"],
            json!({"project": "demo", "result": {"ok": true}, "stored": true})
        );
        assert_eq!(
            value["result"]["content"][0]["text"],
            r#"{"project":"demo","result":{"ok":true},"stored":true}"#
        );
    }

    #[tokio::test]
    async fn mcp_project_add_registers_project() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = write_gateway_config(dir.path(), true);
        let runtime = file_backed_runtime(&config_path, Arc::new(FakeRunner::default()));
        env::remove_var("TEST_EMPTY_GATEWAY_TOKEN");
        let app = router(runtime);

        let add = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/mcp")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"shuttle_project_add","arguments":{"name":"extra","backend":"http","url":"http://127.0.0.1:9999/","make_current":true}}}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(add.status(), StatusCode::OK);
        let body = add.into_body().collect().await.unwrap().to_bytes();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["result"]["structuredContent"]["name"], "extra-2");
        assert_eq!(
            value["result"]["structuredContent"]["url"],
            "http://127.0.0.1:9999"
        );
        assert!(GatewayConfig::load(&config_path)
            .unwrap()
            .projects
            .contains_key("extra-2"));

        let current = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/mcp")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"shuttle_current_project","arguments":{}}}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = current.into_body().collect().await.unwrap().to_bytes();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["result"]["structuredContent"]["name"], "extra-2");
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
    async fn http_project_add_updates_project_list_and_current() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = write_gateway_config(dir.path(), true);
        let runtime = file_backed_runtime(&config_path, Arc::new(FakeRunner::default()));
        env::remove_var("TEST_EMPTY_GATEWAY_TOKEN");
        let app = router(runtime);

        let add = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/projects")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"name":"extra","backend":"http","url":"http://127.0.0.1:9999/","description":"extra project","make_current":true}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(add.status(), StatusCode::CREATED);
        let body = add.into_body().collect().await.unwrap().to_bytes();
        let project: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(project["name"], "extra-2");
        assert_eq!(project["url"], "http://127.0.0.1:9999");
        assert!(GatewayConfig::load(&config_path)
            .unwrap()
            .projects
            .contains_key("extra-2"));

        let current = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/projects/current")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = current.into_body().collect().await.unwrap().to_bytes();
        let project: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(project["name"], "extra-2");

        let list = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/projects")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = list.into_body().collect().await.unwrap().to_bytes();
        let projects: Value = serde_json::from_slice(&body).unwrap();
        assert!(projects["projects"]
            .as_array()
            .unwrap()
            .iter()
            .any(|project| project["name"] == "extra-2"));
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

    fn write_gateway_config(dir: &Path, include_extra: bool) -> PathBuf {
        let path = dir.join("projects.toml");
        let extra = if include_extra {
            "\n[projects.extra]\nbackend = \"http\"\nurl = \"http://127.0.0.1:8788\"\n"
        } else {
            ""
        };
        std::fs::write(
            &path,
            format!(
                "[defaults]\nproject = \"demo\"\n\n[projects.demo]\nbackend = \"local\"\nrepo = \"/tmp/demo\"\n{extra}"
            ),
        )
        .unwrap();
        path
    }

    fn file_backed_runtime(path: &Path, runner: Arc<FakeRunner>) -> GatewayRuntime {
        let cfg = GatewayConfig::load(path).unwrap();
        let config_path = cfg.config_path.clone();
        let registry = ProjectRegistry::new(cfg.defaults.project, cfg.projects).unwrap();
        GatewayRuntime {
            service: Arc::new(GatewayService::new_with_config_path(
                registry,
                runner,
                config_path,
            )),
            auth: GatewayAuth::Bearer {
                token_env: "TEST_EMPTY_GATEWAY_TOKEN".to_owned(),
            },
        }
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
                    backend: ProjectBackendKind::Local,
                    repo: Some(PathBuf::from("/tmp/demo")),
                    db: None,
                    url: String::new(),
                    token_env: None,
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
