//! Sync the repo-local event log with a cloud shuttle-gateway (the Cloudflare
//! Worker in `workers/shuttle-gateway`) over its HTTP API.
//!
//! Push is idempotent server-side: identity is `(project_id, event_id)`, so
//! replaying local UUIDs is a no-op. Pull walks the gateway's keyset cursor
//! (`before=<created_at>|<id>`) and dedups locally via `append_if_absent`,
//! mirroring the mesh sync model.

use std::fs;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use shuttle_core::{Event, EventFilter, EventStore, EventType, Result, ShuttleError};
use shuttle_store::SqliteEventStore;

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
}

/// Gateway operations the sync loops need. Faked in tests; implemented over
/// HTTP by [`HttpRemoteApi`].
pub trait RemoteApi {
    fn append_event(&self, body: &Value) -> Result<AppendOutcome>;
    fn list_events(&self, limit: u32, before: Option<&str>) -> Result<EventsPage>;
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

impl RemoteApi for HttpRemoteApi {
    fn append_event(&self, body: &Value) -> Result<AppendOutcome> {
        let response = self
            .agent
            .post(&self.events_url())
            .set("authorization", &format!("Bearer {}", self.config.token))
            .send_json(body.clone())
            .map_err(Self::handle_error)?;
        let value: Value = response
            .into_json()
            .map_err(|err| ShuttleError::Serialization(err.to_string()))?;
        Ok(AppendOutcome {
            deduplicated: value["deduplicated"].as_bool().unwrap_or(false),
        })
    }

    fn list_events(&self, limit: u32, before: Option<&str>) -> Result<EventsPage> {
        let mut request = self
            .agent
            .get(&self.events_url())
            .set("authorization", &format!("Bearer {}", self.config.token))
            .query("limit", &limit.to_string());
        if let Some(before) = before {
            request = request.query("before", before);
        }
        let response = request.call().map_err(Self::handle_error)?;
        let value: Value = response
            .into_json()
            .map_err(|err| ShuttleError::Serialization(err.to_string()))?;
        let events = value["events"].as_array().cloned().unwrap_or_default();
        Ok(EventsPage {
            has_more: value["has_more"].as_bool().unwrap_or(false),
            next_before: value["next_before"].as_str().map(str::to_owned),
            events,
        })
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

    let mut report = PushReport::default();
    for event in &events {
        let outcome = api.append_event(&event_to_push_body(event))?;
        if outcome.deduplicated {
            report.deduplicated += 1;
        } else {
            report.pushed += 1;
        }
    }
    Ok(report)
}

/// Pull every gateway event into the local store, following the keyset cursor
/// until the last page. Duplicates are skipped via `append_if_absent`.
pub async fn pull(
    store: &SqliteEventStore,
    api: &dyn RemoteApi,
    local_workspace_id: &str,
    project: &str,
) -> Result<PullReport> {
    let mut report = PullReport::default();
    let mut before: Option<String> = None;
    loop {
        let page = api.list_events(PULL_PAGE_LIMIT, before.as_deref())?;
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
        if !page.has_more {
            break;
        }
        match page.next_before {
            Some(next) => before = Some(next),
            None => break,
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
                events: page,
            })
        }
    }

    fn open_store(dir: &tempfile::TempDir) -> SqliteEventStore {
        SqliteEventStore::open(dir.path().join("shuttle.db")).unwrap()
    }

    fn local_event(content: &str) -> Event {
        let mut event = shuttle_memory::new_memory(
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
}
