//! Sync the repo-local event log with a cloud shuttle-gateway (the Cloudflare
//! Worker in `workers/shuttle-gateway`) over its HTTP API.
//!
//! Push is idempotent server-side: identity is `(project_id, event_id)`, so
//! replaying local UUIDs is a no-op. Pull walks the gateway's keyset cursor
//! (`before=<created_at>|<id>`) and dedups locally via `append_if_absent`,
//! mirroring the mesh sync model.

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::thread;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::core::{Event, EventFilter, EventStore, EventType, Result, ShuttleError};
use crate::store::SqliteEventStore;

/// Page size for pull requests; the gateway clamps `limit` to 500.
const PULL_PAGE_LIMIT: u32 = 500;

/// Resolved connection settings for one gateway project.
#[derive(Debug, Clone)]
pub struct RemoteConfig {
    /// Gateway base URL, e.g. `https://shuttle-gateway.example.workers.dev`.
    pub url: String,
    /// Project id or slug on the gateway.
    pub project: String,
    /// Bearer token with read/write scope for the project.
    pub token: String,
    /// Optional Cloudflare Access service-auth client id.
    pub access_client_id: Option<String>,
    /// Optional Cloudflare Access service-auth client secret.
    pub access_client_secret: Option<String>,
}

/// Persisted remote settings (`.shuttle/remote.json`). The token itself is
/// never stored; only the name of the environment variable that carries it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteSettings {
    pub url: String,
    pub project: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_env: Option<String>,
}

const CHECKPOINT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncCheckpoint {
    pub version: u32,
    pub project: String,
    #[serde(default)]
    pub push_after: Option<String>,
    #[serde(default)]
    pub pull_phase: PullPhase,
    #[serde(default)]
    pub pull_before: Option<String>,
    #[serde(default)]
    pub pull_after: Option<String>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PullPhase {
    #[default]
    Backfill,
    Incremental,
}

impl SyncCheckpoint {
    pub fn new(project: &str) -> Self {
        Self {
            version: CHECKPOINT_VERSION,
            project: project.to_owned(),
            push_after: None,
            pull_phase: PullPhase::Backfill,
            pull_before: None,
            pull_after: None,
            updated_at: Utc::now(),
        }
    }

    pub fn load(path: impl AsRef<Path>, project: &str) -> Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(Self::new(project));
        }
        let contents =
            fs::read_to_string(path).map_err(|err| ShuttleError::Store(err.to_string()))?;
        let checkpoint: Self = serde_json::from_str(&contents)
            .map_err(|err| ShuttleError::Serialization(err.to_string()))?;
        if checkpoint.version != CHECKPOINT_VERSION || checkpoint.project != project {
            return Err(ShuttleError::Store(
                "sync checkpoint version or project does not match".to_owned(),
            ));
        }
        Ok(checkpoint)
    }

    pub fn save(&mut self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|err| ShuttleError::Store(err.to_string()))?;
        }
        self.updated_at = Utc::now();
        let contents = serde_json::to_string_pretty(self)
            .map_err(|err| ShuttleError::Serialization(err.to_string()))?;
        let temporary = path.with_extension("tmp");
        fs::write(&temporary, contents).map_err(|err| ShuttleError::Store(err.to_string()))?;
        fs::rename(&temporary, path).map_err(|err| ShuttleError::Store(err.to_string()))
    }
}

pub fn checkpoint_path(shuttle_dir: impl AsRef<Path>, project: &str) -> std::path::PathBuf {
    let safe_project = project
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    shuttle_dir
        .as_ref()
        .join("sync")
        .join(format!("{safe_project}.json"))
}

impl RemoteSettings {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let contents =
            fs::read_to_string(path).map_err(|err| ShuttleError::Store(err.to_string()))?;
        serde_json::from_str(&contents).map_err(|err| ShuttleError::Serialization(err.to_string()))
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let contents = serde_json::to_string_pretty(self)
            .map_err(|err| ShuttleError::Serialization(err.to_string()))?;
        fs::write(path, contents).map_err(|err| ShuttleError::Store(err.to_string()))
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushReport {
    pub pushed: usize,
    pub deduplicated: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullReport {
    pub imported: usize,
    pub skipped_duplicates: usize,
    pub pages: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncReport {
    pub push: PushReport,
    pub pull: PullReport,
}

#[derive(Debug, Clone, Copy)]
pub struct AppendOutcome {
    pub deduplicated: bool,
}

#[derive(Debug, Clone, Default)]
pub struct EventsPage {
    pub events: Vec<Value>,
    pub has_more: bool,
    pub next_before: Option<String>,
    pub next_after: Option<String>,
}

/// Gateway operations the sync loops need. Faked in tests; implemented over
/// HTTP by [`HttpRemoteApi`].
pub trait RemoteApi {
    fn append_event(&self, body: &Value) -> Result<AppendOutcome>;
    fn list_events(&self, limit: u32, before: Option<&str>) -> Result<EventsPage>;
    fn list_events_after(&self, limit: u32, after: &str) -> Result<EventsPage> {
        let _ = after;
        self.list_events(limit, None)
    }
}

pub struct HttpRemoteApi {
    config: RemoteConfig,
    agent: ureq::Agent,
}

impl HttpRemoteApi {
    pub fn new(config: RemoteConfig) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(30))
            .build();
        Self { config, agent }
    }

    fn events_url(&self) -> String {
        format!(
            "{}/api/projects/{}/events",
            self.config.url.trim_end_matches('/'),
            self.config.project
        )
    }

    pub fn config(&self) -> &RemoteConfig {
        &self.config
    }

    fn auth(&self, request: ureq::Request) -> ureq::Request {
        let request = request.set("authorization", &format!("Bearer {}", self.config.token));
        let request = if let Some(value) = &self.config.access_client_id {
            request.set("CF-Access-Client-Id", value)
        } else {
            request
        };
        if let Some(value) = &self.config.access_client_secret {
            request.set("CF-Access-Client-Secret", value)
        } else {
            request
        }
    }

    fn handle_error(err: ureq::Error) -> ShuttleError {
        match err {
            ureq::Error::Status(status, response) => {
                let body = response.into_string().unwrap_or_default();
                let snippet: String = body.chars().take(200).collect();
                let hint = if status == 401 || status == 403 {
                    " (check the gateway token, e.g. SHUTTLE_GATEWAY_TOKEN, and its project scope)"
                } else {
                    ""
                };
                ShuttleError::Store(format!("gateway returned {status}{hint}: {snippet}"))
            }
            ureq::Error::Transport(transport) => {
                ShuttleError::Store(format!("gateway request failed: {transport}"))
            }
        }
    }
}

/// EventStore implementation for cloud-first repositories. It never creates
/// or reads a repo-local SQLite database: D1 is the only state store and the
/// Worker remains the authority for atomic task/workflow claims.
pub struct CloudEventStore {
    config: RemoteConfig,
    agent: Arc<ureq::Agent>,
    workspace_id: String,
}

impl CloudEventStore {
    pub fn connect(
        config: RemoteConfig,
        client_instance_id: &str,
        local_path_hint: Option<&str>,
    ) -> Result<Self> {
        let client = Arc::new(
            ureq::AgentBuilder::new()
                .timeout(std::time::Duration::from_secs(30))
                .build(),
        );
        let mut store = Self {
            config,
            agent: client,
            workspace_id: client_instance_id.to_owned(),
        };
        store.workspace_id = store.ensure_workspace(client_instance_id, local_path_hint)?;
        Ok(store)
    }

    fn project_path(&self, suffix: &str) -> String {
        format!(
            "{}/api/projects/{}/{}",
            self.config.url.trim_end_matches('/'),
            self.config.project,
            suffix.trim_start_matches('/')
        )
    }

    fn auth(&self, request: ureq::Request) -> ureq::Request {
        let request = request.set("authorization", &format!("Bearer {}", self.config.token));
        let request = if let Some(value) = &self.config.access_client_id {
            request.set("CF-Access-Client-Id", value)
        } else {
            request
        };
        if let Some(value) = &self.config.access_client_secret {
            request.set("CF-Access-Client-Secret", value)
        } else {
            request
        }
    }

    fn ensure_workspace(
        &self,
        client_instance_id: &str,
        local_path_hint: Option<&str>,
    ) -> Result<String> {
        let response = self
            .auth(self.agent.post(&self.project_path("workspaces")))
            .send_json(json!({
                "client_instance_id": client_instance_id,
                "local_path_hint": local_path_hint,
            }))
            .map_err(cloud_error)?;
        let value: Value = response
            .into_json()
            .map_err(|err| ShuttleError::Serialization(err.to_string()))?;
        value["id"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| ShuttleError::Serialization("workspace response missing id".to_owned()))
    }

    fn append_json(&self, body: &Value) -> Result<Value> {
        let action = body["metadata"]["action"].as_str();
        let event_type = body["event_type"].as_str();
        let (path, mut payload) = if event_type == Some("task") && action == Some("claimed") {
            let task_id = body["metadata"]["task_id"].as_str().ok_or_else(|| {
                ShuttleError::Serialization("task claim missing task_id".to_owned())
            })?;
            (
                self.project_path(&format!("tasks/{task_id}/claim")),
                body.clone(),
            )
        } else if event_type == Some("workflow")
            && matches!(action, Some("claimed" | "reclaimed" | "taken_over"))
        {
            let run_id = body["metadata"]["run_id"].as_str().ok_or_else(|| {
                ShuttleError::Serialization("workflow claim missing run_id".to_owned())
            })?;
            let step_id = body["metadata"]["step_id"].as_str().ok_or_else(|| {
                ShuttleError::Serialization("workflow claim missing step_id".to_owned())
            })?;
            (
                self.project_path(&format!("workflows/{run_id}/steps/{step_id}/claim")),
                body.clone(),
            )
        } else {
            (self.project_path("events"), body.clone())
        };

        let takeover = payload["metadata"]
            .get("takeover")
            .cloned()
            .or_else(|| payload["metadata"]["value"].get("takeover").cloned());
        let reason = payload["metadata"]
            .get("takeover_reason")
            .cloned()
            .or_else(|| payload["metadata"]["value"].get("takeover_reason").cloned());
        if let Some(takeover) = takeover {
            payload["takeover"] = takeover;
        }
        if let Some(reason) = reason {
            payload["reason"] = reason;
        }
        let response = self
            .auth(self.agent.post(&path))
            .send_json(payload)
            .map_err(cloud_error)?;
        response
            .into_json()
            .map_err(|err| ShuttleError::Serialization(err.to_string()))
    }
}

fn cloud_error(err: ureq::Error) -> ShuttleError {
    match err {
        ureq::Error::Status(status, response) => {
            let body = response.into_string().unwrap_or_default();
            let snippet: String = body.chars().take(300).collect();
            ShuttleError::Store(format!("gateway returned {status}: {snippet}"))
        }
        ureq::Error::Transport(error) => {
            ShuttleError::Store(format!("gateway request failed: {error}"))
        }
    }
}

#[async_trait]
impl EventStore for CloudEventStore {
    async fn append(&self, event: Event) -> Result<Event> {
        let mut body = event_to_push_body(&event);
        body["context"]["workspace_id"] = json!(self.workspace_id);
        let value = self.append_json(&body)?;
        remote_to_event(&value["event"], &self.workspace_id, &self.config.project)
    }

    async fn list(&self, filter: EventFilter) -> Result<Vec<Event>> {
        let mut before: Option<String> = None;
        let mut all = Vec::new();
        loop {
            let mut request = self
                .auth(self.agent.get(&self.project_path("events")))
                .query("limit", "500");
            if let Some(event_type) = filter.event_type {
                request = request.query("event_type", event_type.as_str());
            }
            if let Some(workspace_id) = filter.workspace_id.as_deref() {
                request = request.query("workspace_id", workspace_id);
            }
            if let Some(agent) = filter.agent.as_deref() {
                request = request.query("agent", agent);
            }
            if let Some(recipient) = filter.recipient.as_deref() {
                request = request.query("recipient", recipient);
            }
            if let Some(id) = filter.id {
                request = request.query("id", &id.to_string());
            }
            if let Some(tag) = filter.tag.as_deref() {
                request = request.query("tag", tag);
            }
            if let Some(query) = filter.query.as_deref() {
                request = request.query("query", query);
            }
            if let Some(before) = before.as_deref() {
                request = request.query("before", before);
            }
            let response = request.call().map_err(cloud_error)?;
            let value: Value = response
                .into_json()
                .map_err(|err| ShuttleError::Serialization(err.to_string()))?;
            let events = value["events"].as_array().cloned().unwrap_or_default();
            let page_len = events.len();
            for remote in events {
                let event = remote_to_event(&remote, &self.workspace_id, &self.config.project)?;
                if filter.id.is_some_and(|id| id != event.id)
                    || filter
                        .event_type
                        .is_some_and(|kind| kind != event.event_type)
                    || filter
                        .agent
                        .as_deref()
                        .is_some_and(|agent| agent != event.agent)
                    || filter.recipient.as_deref().is_some_and(|recipient| {
                        event.metadata_json["to"].as_str() != Some(recipient)
                    })
                    || filter
                        .tag
                        .as_deref()
                        .is_some_and(|tag| !event.tags.iter().any(|item| item == tag))
                    || filter
                        .tags
                        .iter()
                        .any(|tag| !event.tags.iter().any(|item| item == tag))
                    || filter.query.as_deref().is_some_and(|query| {
                        let query = query.to_lowercase();
                        !format!(
                            "{} {} {}",
                            event.title.as_deref().unwrap_or_default(),
                            event.content,
                            event.metadata_json
                        )
                        .to_lowercase()
                        .contains(&query)
                    })
                {
                    continue;
                }
                all.push(event);
            }
            if !value["has_more"].as_bool().unwrap_or(false) || page_len == 0 {
                break;
            }
            before = value["next_before"].as_str().map(str::to_owned);
            if before.is_none() {
                break;
            }
        }
        if let Some(limit) = filter.limit {
            all.truncate(limit as usize);
        }
        Ok(all)
    }
}

impl RemoteApi for HttpRemoteApi {
    fn append_event(&self, body: &Value) -> Result<AppendOutcome> {
        let response = retry_request(|| {
            self.auth(self.agent.post(&self.events_url()))
                .send_json(body.clone())
                .map_err(Box::new)
        })?;
        let value: Value = response
            .into_json()
            .map_err(|err| ShuttleError::Serialization(err.to_string()))?;
        Ok(AppendOutcome {
            deduplicated: value["deduplicated"].as_bool().unwrap_or(false),
        })
    }

    fn list_events(&self, limit: u32, before: Option<&str>) -> Result<EventsPage> {
        let response = retry_request(|| {
            let mut request = self
                .auth(self.agent.get(&self.events_url()))
                .query("limit", &limit.to_string());
            if let Some(before) = before {
                request = request.query("before", before);
            }
            request.call().map_err(Box::new)
        })?;
        let value: Value = response
            .into_json()
            .map_err(|err| ShuttleError::Serialization(err.to_string()))?;
        let events = value["events"].as_array().cloned().unwrap_or_default();
        Ok(EventsPage {
            has_more: value["has_more"].as_bool().unwrap_or(false),
            next_before: value["next_before"].as_str().map(str::to_owned),
            next_after: value["next_after"].as_str().map(str::to_owned),
            events,
        })
    }

    fn list_events_after(&self, limit: u32, after: &str) -> Result<EventsPage> {
        let response = retry_request(|| {
            self.auth(self.agent.get(&self.events_url()))
                .query("limit", &limit.to_string())
                .query("after", after)
                .call()
                .map_err(Box::new)
        })?;
        let value: Value = response
            .into_json()
            .map_err(|err| ShuttleError::Serialization(err.to_string()))?;
        let events = value["events"].as_array().cloned().unwrap_or_default();
        Ok(EventsPage {
            has_more: value["has_more"].as_bool().unwrap_or(false),
            next_before: value["next_before"].as_str().map(str::to_owned),
            next_after: value["next_after"].as_str().map(str::to_owned),
            events,
        })
    }
}

fn retry_request<F>(mut request: F) -> Result<ureq::Response>
where
    F: FnMut() -> std::result::Result<ureq::Response, Box<ureq::Error>>,
{
    const MAX_ATTEMPTS: usize = 4;
    for attempt in 0..MAX_ATTEMPTS {
        match request() {
            Ok(response) => return Ok(response),
            Err(error) if attempt + 1 < MAX_ATTEMPTS && retryable(error.as_ref()) => {
                let retry_after = match error.as_ref() {
                    ureq::Error::Status(_, response) => response
                        .header("retry-after")
                        .and_then(|value| value.parse::<u64>().ok())
                        .map(std::time::Duration::from_secs),
                    ureq::Error::Transport(_) => None,
                };
                let exponential = 100_u64.saturating_mul(2_u64.saturating_pow(attempt as u32));
                let jitter = (Utc::now().timestamp_subsec_millis() as u64) % 100;
                thread::sleep(
                    retry_after
                        .unwrap_or_else(|| std::time::Duration::from_millis(exponential + jitter)),
                );
            }
            Err(error) => return Err(HttpRemoteApi::handle_error(*error)),
        }
    }
    unreachable!("retry loop returns on every attempt")
}

fn retryable(error: &ureq::Error) -> bool {
    match error {
        ureq::Error::Status(status, _) => *status == 429 || (500..=599).contains(status),
        ureq::Error::Transport(_) => true,
    }
}

/// Map a local event onto the gateway append body. The local UUID becomes the
/// project-scoped `event_id`, repo context travels in the `context` envelope,
/// and columns the gateway does not model (`repo_id`, `repo_path`,
/// `bit_repo_id`) are stashed under `metadata.shuttle_local` so a later pull
/// can restore them.
pub fn event_to_push_body(event: &Event) -> Value {
    let mut metadata = match &event.metadata_json {
        Value::Object(map) => map.clone(),
        _ => serde_json::Map::new(),
    };
    let mut local = serde_json::Map::new();
    if let Some(repo_id) = &event.repo_id {
        local.insert("repo_id".to_owned(), json!(repo_id));
    }
    if let Some(repo_path) = &event.repo_path {
        local.insert("repo_path".to_owned(), json!(repo_path));
    }
    if let Some(bit_repo_id) = &event.bit_repo_id {
        local.insert("bit_repo_id".to_owned(), json!(bit_repo_id));
    }
    if !local.is_empty() {
        metadata.insert("shuttle_local".to_owned(), Value::Object(local));
    }

    json!({
        "event_id": event.id.to_string(),
        "event_type": event.event_type.as_str(),
        "agent": event.agent,
        "session_id": event.session_id,
        "title": event.title,
        "content": event.content,
        "tags": event.tags,
        "context": {
            "workspace_id": event.workspace_id,
            "repo": {
                "git_remote": event.git_remote,
                "branch": event.branch,
                "commit": event.commit,
                "dirty": event.repo_dirty,
                "dirty_files": [],
            },
        },
        "metadata": Value::Object(metadata),
        "created_at": event.created_at.to_rfc3339(),
    })
}

/// Map a gateway event onto the local model. Events become visible in the
/// receiving workspace: `workspace_id` is rewritten and the original stashed
/// in `metadata_json.remote_source_workspace_id` (the mesh sync pattern).
/// Remote ids that are not UUIDs (events appended by non-stl clients) map to
/// a stable UUIDv5 of `(project, remote id)` so repeated pulls dedup.
pub fn remote_to_event(remote: &Value, local_workspace_id: &str, project: &str) -> Result<Event> {
    let remote_id = remote["id"]
        .as_str()
        .ok_or_else(|| ShuttleError::Serialization("remote event missing id".to_owned()))?;
    let id = Uuid::parse_str(remote_id).unwrap_or_else(|_| {
        Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!("shuttle:{project}:{remote_id}").as_bytes(),
        )
    });
    let event_type = EventType::try_from(remote["event_type"].as_str().unwrap_or_default())?;
    let created_at = remote["created_at"]
        .as_str()
        .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
        .map(|parsed| parsed.with_timezone(&Utc))
        .ok_or_else(|| {
            ShuttleError::Serialization(format!("remote event {remote_id} has invalid created_at"))
        })?;

    let mut metadata = match &remote["metadata_json"] {
        Value::Object(map) => map.clone(),
        _ => serde_json::Map::new(),
    };
    let local = metadata.remove("shuttle_local");
    let local_field = |key: &str| -> Option<String> {
        local
            .as_ref()
            .and_then(|value| value[key].as_str())
            .map(str::to_owned)
    };

    let remote_workspace = remote["workspace_id"].as_str();
    if remote_workspace.is_some_and(|workspace| workspace != local_workspace_id) {
        metadata.insert(
            "remote_source_workspace_id".to_owned(),
            json!(remote_workspace),
        );
    }

    Ok(Event {
        id,
        event_type,
        workspace_id: local_workspace_id.to_owned(),
        repo_id: local_field("repo_id"),
        repo_path: local_field("repo_path"),
        git_remote: remote["git_remote"].as_str().map(str::to_owned),
        bit_repo_id: local_field("bit_repo_id"),
        branch: remote["branch"].as_str().map(str::to_owned),
        commit: remote["commit_hash"].as_str().map(str::to_owned),
        repo_dirty: remote["repo_dirty"].as_bool(),
        agent: remote["agent"].as_str().unwrap_or("unknown").to_owned(),
        session_id: remote["session_id"].as_str().unwrap_or_default().to_owned(),
        title: remote["title"].as_str().map(str::to_owned),
        content: remote["content"].as_str().unwrap_or_default().to_owned(),
        tags: remote["tags"]
            .as_array()
            .map(|tags| {
                tags.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        metadata_json: Value::Object(metadata),
        created_at,
    })
}

/// Push every local event to the gateway, oldest first so server insertion
/// order roughly follows history. Idempotent: replays report as deduplicated.
pub async fn push(store: &SqliteEventStore, api: &dyn RemoteApi) -> Result<PushReport> {
    push_impl(store, api, None).await
}

async fn push_impl(
    store: &SqliteEventStore,
    api: &dyn RemoteApi,
    checkpoint: Option<(&std::path::Path, &str)>,
) -> Result<PushReport> {
    let mut events = store
        .list(EventFilter {
            limit: Some(u32::MAX),
            ..EventFilter::default()
        })
        .await?;
    events.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then(left.id.cmp(&right.id))
    });

    let mut state = if let Some((path, project)) = checkpoint {
        Some(SyncCheckpoint::load(path, project)?)
    } else {
        None
    };
    if let Some(state) = &state {
        if let Some(after) = state.push_after.as_deref() {
            events.retain(|event| event_cursor(event).as_str() > after);
        }
    }
    let mut report = PushReport::default();
    for event in &events {
        let outcome = api.append_event(&event_to_push_body(event))?;
        if outcome.deduplicated {
            report.deduplicated += 1;
        } else {
            report.pushed += 1;
        }
        if let (Some((path, _)), Some(state)) = (checkpoint, state.as_mut()) {
            state.push_after = Some(event_cursor(event));
            state.save(path)?;
        }
    }
    Ok(report)
}

pub async fn push_with_checkpoint(
    store: &SqliteEventStore,
    api: &dyn RemoteApi,
    project: &str,
    checkpoint_path: &std::path::Path,
) -> Result<PushReport> {
    push_impl(store, api, Some((checkpoint_path, project))).await
}

/// Pull every gateway event into the local store, following the keyset cursor
/// until the last page. Duplicates are skipped via `append_if_absent`.
pub async fn pull(
    store: &SqliteEventStore,
    api: &dyn RemoteApi,
    local_workspace_id: &str,
    project: &str,
) -> Result<PullReport> {
    pull_impl(store, api, local_workspace_id, project, None).await
}

async fn pull_impl(
    store: &SqliteEventStore,
    api: &dyn RemoteApi,
    local_workspace_id: &str,
    project: &str,
    checkpoint: Option<&std::path::Path>,
) -> Result<PullReport> {
    let mut state = checkpoint
        .map(|path| SyncCheckpoint::load(path, project))
        .transpose()?;
    if let Some(state) = &state {
        if state.pull_phase == PullPhase::Incremental {
            if let Some(after) = state.pull_after.as_deref() {
                return pull_incremental(
                    store,
                    api,
                    local_workspace_id,
                    project,
                    after,
                    state,
                    checkpoint,
                )
                .await;
            }
        }
    }

    let mut report = PullReport::default();
    let mut before = state.as_ref().and_then(|state| state.pull_before.clone());
    let mut latest = state.as_ref().and_then(|state| state.pull_after.clone());
    loop {
        let page = api.list_events(PULL_PAGE_LIMIT, before.as_deref())?;
        if page.events.is_empty() {
            break;
        }
        report.pages += 1;
        if latest.is_none() {
            latest = page.events.first().and_then(remote_cursor);
        }
        for remote in &page.events {
            let event = remote_to_event(remote, local_workspace_id, project)?;
            if store.append_if_absent(event)? {
                report.imported += 1;
            } else {
                report.skipped_duplicates += 1;
            }
        }
        if !page.has_more {
            if let (Some(path), Some(state)) = (checkpoint, state.as_mut()) {
                state.pull_phase = PullPhase::Incremental;
                state.pull_before = None;
                state.pull_after = latest;
                state.save(path)?;
            }
            break;
        }
        match page.next_before.clone() {
            Some(next) => {
                before = Some(next);
                if let (Some(path), Some(state)) = (checkpoint, state.as_mut()) {
                    state.pull_before = before.clone();
                    state.pull_after = latest.clone();
                    state.save(path)?;
                }
            }
            None => {
                if let (Some(path), Some(state)) = (checkpoint, state.as_mut()) {
                    state.pull_phase = PullPhase::Incremental;
                    state.pull_before = None;
                    state.pull_after = latest;
                    state.save(path)?;
                }
                break;
            }
        }
    }
    Ok(report)
}

pub async fn pull_with_checkpoint(
    store: &SqliteEventStore,
    api: &dyn RemoteApi,
    local_workspace_id: &str,
    project: &str,
    checkpoint_path: &std::path::Path,
) -> Result<PullReport> {
    pull_impl(
        store,
        api,
        local_workspace_id,
        project,
        Some(checkpoint_path),
    )
    .await
}

async fn pull_incremental(
    store: &SqliteEventStore,
    api: &dyn RemoteApi,
    local_workspace_id: &str,
    project: &str,
    after: &str,
    state: &SyncCheckpoint,
    checkpoint: Option<&std::path::Path>,
) -> Result<PullReport> {
    let mut report = PullReport::default();
    let mut cursor = after.to_owned();
    loop {
        let page = api.list_events_after(PULL_PAGE_LIMIT, &cursor)?;
        if page.events.is_empty() {
            break;
        }
        report.pages += 1;
        for remote in &page.events {
            let event = remote_to_event(remote, local_workspace_id, project)?;
            if store.append_if_absent(event)? {
                report.imported += 1;
            } else {
                report.skipped_duplicates += 1;
            }
        }
        let Some(next) = page
            .next_after
            .clone()
            .or_else(|| page.events.last().and_then(remote_cursor))
        else {
            break;
        };
        cursor = next;
        if let Some(path) = checkpoint {
            let mut next_state = state.clone();
            next_state.pull_after = Some(cursor.clone());
            next_state.save(path)?;
        }
        if !page.has_more {
            break;
        }
    }
    Ok(report)
}

/// Bidirectional sync: push local history, then pull the gateway's log.
pub async fn sync(
    store: &SqliteEventStore,
    api: &dyn RemoteApi,
    local_workspace_id: &str,
    project: &str,
) -> Result<SyncReport> {
    let push = push(store, api).await?;
    let pull = pull(store, api, local_workspace_id, project).await?;
    Ok(SyncReport { push, pull })
}

pub async fn sync_with_checkpoint(
    store: &SqliteEventStore,
    api: &dyn RemoteApi,
    local_workspace_id: &str,
    project: &str,
    checkpoint_path: &std::path::Path,
) -> Result<SyncReport> {
    let push = push_impl(store, api, Some((checkpoint_path, project))).await?;
    let pull = pull_impl(
        store,
        api,
        local_workspace_id,
        project,
        Some(checkpoint_path),
    )
    .await?;
    Ok(SyncReport { push, pull })
}

fn event_cursor(event: &Event) -> String {
    format!("{}|{}", event.created_at.to_rfc3339(), event.id)
}

fn remote_cursor(value: &Value) -> Option<String> {
    Some(format!(
        "{}|{}",
        value["created_at"].as_str()?,
        value["id"].as_str()?
    ))
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use futures_executor::block_on;

    /// In-memory gateway double: project-scoped id dedup and keyset paging,
    /// matching the Worker's contract.
    struct FakeRemoteApi {
        events: RefCell<Vec<Value>>,
    }

    impl FakeRemoteApi {
        fn new() -> Self {
            Self {
                events: RefCell::new(Vec::new()),
            }
        }
    }

    impl RemoteApi for FakeRemoteApi {
        fn append_event(&self, body: &Value) -> Result<AppendOutcome> {
            let id = body["event_id"].as_str().unwrap().to_owned();
            let mut events = self.events.borrow_mut();
            if events.iter().any(|event| event["id"] == json!(id)) {
                return Ok(AppendOutcome { deduplicated: true });
            }
            let repo = &body["context"]["repo"];
            let mut metadata = body["metadata"].clone();
            if !repo.is_null() {
                metadata["repo"] = repo.clone();
            }
            events.push(json!({
                "id": id,
                "project_id": "project-1",
                "workspace_id": body["context"]["workspace_id"],
                "event_type": body["event_type"],
                "agent": body["agent"],
                "session_id": body["session_id"],
                "title": body["title"],
                "content": body["content"],
                "git_remote": repo["git_remote"],
                "branch": repo["branch"],
                "commit_hash": repo["commit"],
                "repo_dirty": repo["dirty"],
                "metadata_json": metadata,
                "tags": body["tags"],
                "created_at": body["created_at"],
            }));
            Ok(AppendOutcome {
                deduplicated: false,
            })
        }

        fn list_events(&self, limit: u32, before: Option<&str>) -> Result<EventsPage> {
            let mut ordered = self.events.borrow().clone();
            ordered.sort_by(|left, right| {
                right["created_at"]
                    .as_str()
                    .cmp(&left["created_at"].as_str())
                    .then(right["id"].as_str().cmp(&left["id"].as_str()))
            });
            if let Some(before) = before {
                let (created_at, id) = before.split_once('|').unwrap();
                ordered.retain(|event| {
                    let event_created = event["created_at"].as_str().unwrap();
                    event_created < created_at
                        || (event_created == created_at && event["id"].as_str().unwrap() < id)
                });
            }
            let page: Vec<Value> = ordered.into_iter().take(limit as usize).collect();
            let next_before = page.last().map(|event| {
                format!(
                    "{}|{}",
                    event["created_at"].as_str().unwrap(),
                    event["id"].as_str().unwrap()
                )
            });
            Ok(EventsPage {
                has_more: page.len() == limit as usize,
                next_before,
                next_after: page.last().map(|event| {
                    format!(
                        "{}|{}",
                        event["created_at"].as_str().unwrap(),
                        event["id"].as_str().unwrap()
                    )
                }),
                events: page,
            })
        }

        fn list_events_after(&self, limit: u32, after: &str) -> Result<EventsPage> {
            let (created_at, id) = after.split_once('|').unwrap();
            let mut ordered = self.events.borrow().clone();
            ordered.sort_by(|left, right| {
                left["created_at"]
                    .as_str()
                    .cmp(&right["created_at"].as_str())
                    .then(left["id"].as_str().cmp(&right["id"].as_str()))
            });
            ordered.retain(|event| {
                let event_created = event["created_at"].as_str().unwrap();
                event_created > created_at
                    || (event_created == created_at && event["id"].as_str().unwrap() > id)
            });
            let page: Vec<Value> = ordered.into_iter().take(limit as usize).collect();
            let next_after = page.last().map(|event| {
                format!(
                    "{}|{}",
                    event["created_at"].as_str().unwrap(),
                    event["id"].as_str().unwrap()
                )
            });
            Ok(EventsPage {
                has_more: page.len() == limit as usize,
                next_before: None,
                next_after,
                events: page,
            })
        }
    }

    fn open_store(dir: &tempfile::TempDir) -> SqliteEventStore {
        SqliteEventStore::open(dir.path().join("shuttle.db")).unwrap()
    }

    fn local_event(content: &str) -> Event {
        let mut event = crate::memory::new_memory(
            "local-workspace".into(),
            "codex".into(),
            "session".into(),
            content.into(),
        );
        event.repo_id = Some("repo-1".into());
        event.repo_path = Some("/repo".into());
        event.branch = Some("main".into());
        event.commit = Some("abc123".into());
        event.repo_dirty = Some(false);
        event
    }

    #[test]
    fn push_is_idempotent_against_the_gateway() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(&dir);
        block_on(store.append(local_event("first"))).unwrap();
        block_on(store.append(local_event("second"))).unwrap();
        let api = FakeRemoteApi::new();

        let first = block_on(push(&store, &api)).unwrap();
        assert_eq!(first.pushed, 2);
        assert_eq!(first.deduplicated, 0);

        let second = block_on(push(&store, &api)).unwrap();
        assert_eq!(second.pushed, 0);
        assert_eq!(second.deduplicated, 2);
        assert_eq!(api.events.borrow().len(), 2);
    }

    #[test]
    fn mapping_round_trips_through_the_gateway_shape() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(&dir);
        let original = local_event("round trip");
        block_on(store.append(original.clone())).unwrap();
        let api = FakeRemoteApi::new();
        block_on(push(&store, &api)).unwrap();

        let target_dir = tempfile::tempdir().unwrap();
        let target = open_store(&target_dir);
        let report = block_on(pull(&target, &api, "target-workspace", "demo")).unwrap();
        assert_eq!(report.imported, 1);

        let events = block_on(target.list(EventFilter::default())).unwrap();
        assert_eq!(events.len(), 1);
        let pulled = &events[0];
        assert_eq!(pulled.id, original.id);
        assert_eq!(pulled.content, original.content);
        assert_eq!(pulled.created_at, original.created_at);
        assert_eq!(pulled.workspace_id, "target-workspace");
        assert_eq!(
            pulled.metadata_json["remote_source_workspace_id"],
            "local-workspace"
        );
        assert_eq!(pulled.repo_id.as_deref(), Some("repo-1"));
        assert_eq!(pulled.repo_path.as_deref(), Some("/repo"));
        assert_eq!(pulled.commit.as_deref(), Some("abc123"));
        assert!(pulled.metadata_json.get("shuttle_local").is_none());

        // A second pull skips everything.
        let again = block_on(pull(&target, &api, "target-workspace", "demo")).unwrap();
        assert_eq!(again.imported, 0);
        assert_eq!(again.skipped_duplicates, 1);
    }

    #[test]
    fn pull_walks_all_pages() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(&dir);
        let api = FakeRemoteApi::new();
        for i in 0..7 {
            let mut event = local_event(&format!("event {i}"));
            event.created_at = DateTime::parse_from_rfc3339("2024-06-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc)
                + chrono::Duration::seconds(i);
            api.append_event(&event_to_push_body(&event)).unwrap();
        }

        // Force paging by listing three at a time through the pull loop.
        struct SmallPages<'a>(&'a FakeRemoteApi);
        impl RemoteApi for SmallPages<'_> {
            fn append_event(&self, body: &Value) -> Result<AppendOutcome> {
                self.0.append_event(body)
            }
            fn list_events(&self, _limit: u32, before: Option<&str>) -> Result<EventsPage> {
                self.0.list_events(3, before)
            }
        }

        let report = block_on(pull(&store, &SmallPages(&api), "workspace", "demo")).unwrap();
        assert_eq!(report.imported, 7);
        assert_eq!(report.pages, 3);
        let events = block_on(store.list(EventFilter {
            limit: Some(u32::MAX),
            ..EventFilter::default()
        }))
        .unwrap();
        assert_eq!(events.len(), 7);
    }

    #[test]
    fn non_uuid_remote_ids_map_to_stable_uuids() {
        let remote = json!({
            "id": "srv-generated-1",
            "workspace_id": null,
            "event_type": "memory",
            "agent": "web",
            "session_id": "s",
            "title": null,
            "content": "from another client",
            "git_remote": null,
            "branch": null,
            "commit_hash": null,
            "repo_dirty": null,
            "metadata_json": {},
            "tags": [],
            "created_at": "2024-06-01T00:00:00Z",
        });
        let first = remote_to_event(&remote, "workspace", "demo").unwrap();
        let second = remote_to_event(&remote, "workspace", "demo").unwrap();
        assert_eq!(first.id, second.id);
        let other_project = remote_to_event(&remote, "workspace", "other").unwrap();
        assert_ne!(first.id, other_project.id);
    }

    #[test]
    fn remote_settings_round_trip_without_persisting_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("remote.json");
        let settings = RemoteSettings {
            url: "https://gateway.example".into(),
            project: "demo".into(),
            token_env: Some("SHUTTLE_GATEWAY_TOKEN".into()),
        };
        settings.save(&path).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(!contents.to_lowercase().contains("stl_"));
        assert_eq!(RemoteSettings::load(&path).unwrap(), settings);
    }

    #[test]
    fn checkpointed_pull_switches_from_backfill_to_incremental_sync() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(&dir);
        let api = FakeRemoteApi::new();
        for i in 0..3 {
            let mut event = local_event(&format!("checkpoint event {i}"));
            event.created_at = DateTime::parse_from_rfc3339("2024-06-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc)
                + chrono::Duration::seconds(i);
            api.append_event(&event_to_push_body(&event)).unwrap();
        }
        let checkpoint = checkpoint_path(dir.path(), "demo");

        let first = block_on(pull_with_checkpoint(
            &store,
            &api,
            "workspace",
            "demo",
            &checkpoint,
        ))
        .unwrap();
        assert_eq!(first.imported, 3);
        let saved = SyncCheckpoint::load(&checkpoint, "demo").unwrap();
        assert_eq!(saved.pull_phase, PullPhase::Incremental);

        let second = block_on(pull_with_checkpoint(
            &store,
            &api,
            "workspace",
            "demo",
            &checkpoint,
        ))
        .unwrap();
        assert_eq!(second.imported, 0);
        assert_eq!(second.skipped_duplicates, 0);

        let new_event = local_event("checkpoint event new");
        api.append_event(&event_to_push_body(&new_event)).unwrap();
        let third = block_on(pull_with_checkpoint(
            &store,
            &api,
            "workspace",
            "demo",
            &checkpoint,
        ))
        .unwrap();
        assert_eq!(third.imported, 1);
    }
}
