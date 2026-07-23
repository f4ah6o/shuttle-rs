use crate::core::{Event, EventFilter, EventStore, EventType, NewEvent, Result, ShuttleError};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use uuid::Uuid;

pub const TAG_OPEN: &str = "task:open";
pub const TAG_CLAIMED: &str = "task:claimed";
pub const TAG_DONE: &str = "task:done";
pub const TAG_HANDOFF_PENDING: &str = "handoff:pending";
pub const TAG_HANDOFF_ACCEPTED: &str = "handoff:accepted";
pub const TAG_HANDOFF_DONE: &str = "handoff:done";

pub fn claim_tag(agent: &str) -> String {
    format!("claimed_by:{agent}")
}

pub fn task_ref_tag(id: Uuid) -> String {
    format!("task_ref:{id}")
}

pub fn handoff_ref_tag(id: Uuid) -> String {
    format!("handoff_ref:{id}")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Open,
    Claimed,
    Completed,
}

impl TaskStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Claimed => "claimed",
            Self::Completed => "completed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSummary {
    pub id: Uuid,
    pub status: TaskStatus,
    pub content: String,
    pub created_by: String,
    pub claimed_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub source_event_ids: Vec<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffStatus {
    Pending,
    Accepted,
    Completed,
}

impl HandoffStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Accepted => "accepted",
            Self::Completed => "completed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffSummary {
    pub id: Uuid,
    pub status: HandoffStatus,
    pub content: String,
    pub from_agent: String,
    pub to_agent: String,
    pub accepted_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub source_event_ids: Vec<Uuid>,
}

pub fn new_task(workspace_id: String, agent: String, session_id: String, content: String) -> Event {
    Event::new(NewEvent {
        event_type: EventType::Task,
        workspace_id,
        repo_id: None,
        repo_path: None,
        git_remote: None,
        bit_repo_id: None,
        branch: None,
        commit: None,
        repo_dirty: None,
        agent,
        session_id,
        title: Some("task".to_owned()),
        content,
        tags: vec![TAG_OPEN.to_owned()],
        metadata_json: json!({ "action": "created", "status": "open" }),
    })
}

pub fn new_claim(workspace_id: String, agent: String, session_id: String, task_id: Uuid) -> Event {
    Event::new(NewEvent {
        event_type: EventType::Task,
        workspace_id,
        repo_id: None,
        repo_path: None,
        git_remote: None,
        bit_repo_id: None,
        branch: None,
        commit: None,
        repo_dirty: None,
        agent: agent.clone(),
        session_id,
        title: Some("task claim".to_owned()),
        content: format!("claimed task {task_id}"),
        tags: vec![
            TAG_CLAIMED.to_owned(),
            claim_tag(&agent),
            task_ref_tag(task_id),
        ],
        metadata_json: json!({
            "action": "claimed",
            "status": "claimed",
            "task_id": task_id,
            "claimed_by": agent,
        }),
    })
}

pub fn new_task_update(
    workspace_id: String,
    agent: String,
    session_id: String,
    task_id: Uuid,
    content: String,
) -> Event {
    Event::new(NewEvent {
        event_type: EventType::Task,
        workspace_id,
        repo_id: None,
        repo_path: None,
        git_remote: None,
        bit_repo_id: None,
        branch: None,
        commit: None,
        repo_dirty: None,
        agent,
        session_id,
        title: Some("task update".to_owned()),
        content,
        tags: vec![task_ref_tag(task_id)],
        metadata_json: json!({ "action": "updated", "task_id": task_id }),
    })
}

pub fn new_task_done(
    workspace_id: String,
    agent: String,
    session_id: String,
    task_id: Uuid,
) -> Event {
    Event::new(NewEvent {
        event_type: EventType::Task,
        workspace_id,
        repo_id: None,
        repo_path: None,
        git_remote: None,
        bit_repo_id: None,
        branch: None,
        commit: None,
        repo_dirty: None,
        agent,
        session_id,
        title: Some("task completed".to_owned()),
        content: format!("completed task {task_id}"),
        tags: vec![TAG_DONE.to_owned(), task_ref_tag(task_id)],
        metadata_json: json!({
            "action": "completed",
            "status": "completed",
            "task_id": task_id,
        }),
    })
}

pub fn new_handoff(
    workspace_id: String,
    from_agent: String,
    session_id: String,
    to_agent: String,
    content: String,
) -> Event {
    Event::new(NewEvent {
        event_type: EventType::Handoff,
        workspace_id,
        repo_id: None,
        repo_path: None,
        git_remote: None,
        bit_repo_id: None,
        branch: None,
        commit: None,
        repo_dirty: None,
        agent: from_agent.clone(),
        session_id,
        title: Some("handoff".to_owned()),
        content,
        tags: vec![TAG_HANDOFF_PENDING.to_owned()],
        metadata_json: json!({
            "action": "requested",
            "status": "pending",
            "from": from_agent,
            "to": to_agent,
        }),
    })
}

pub fn new_handoff_accept(
    workspace_id: String,
    agent: String,
    session_id: String,
    handoff_id: Uuid,
) -> Event {
    Event::new(NewEvent {
        event_type: EventType::Handoff,
        workspace_id,
        repo_id: None,
        repo_path: None,
        git_remote: None,
        bit_repo_id: None,
        branch: None,
        commit: None,
        repo_dirty: None,
        agent: agent.clone(),
        session_id,
        title: Some("handoff accepted".to_owned()),
        content: format!("accepted handoff {handoff_id}"),
        tags: vec![
            TAG_HANDOFF_ACCEPTED.to_owned(),
            handoff_ref_tag(handoff_id),
            claim_tag(&agent),
        ],
        metadata_json: json!({
            "action": "accepted",
            "status": "accepted",
            "handoff_id": handoff_id,
            "accepted_by": agent,
        }),
    })
}

pub fn new_handoff_done(
    workspace_id: String,
    agent: String,
    session_id: String,
    handoff_id: Uuid,
) -> Event {
    Event::new(NewEvent {
        event_type: EventType::Handoff,
        workspace_id,
        repo_id: None,
        repo_path: None,
        git_remote: None,
        bit_repo_id: None,
        branch: None,
        commit: None,
        repo_dirty: None,
        agent,
        session_id,
        title: Some("handoff completed".to_owned()),
        content: format!("completed handoff {handoff_id}"),
        tags: vec![TAG_HANDOFF_DONE.to_owned(), handoff_ref_tag(handoff_id)],
        metadata_json: json!({
            "action": "completed",
            "status": "completed",
            "handoff_id": handoff_id,
        }),
    })
}

pub async fn list(store: &impl EventStore) -> Result<Vec<TaskSummary>> {
    tasks(store, None, None).await
}

pub async fn tasks(
    store: &impl EventStore,
    workspace_id: Option<&str>,
    limit: Option<u32>,
) -> Result<Vec<TaskSummary>> {
    let events = store
        .list(EventFilter {
            event_type: Some(EventType::Task),
            workspace_id: workspace_id.map(ToOwned::to_owned),
            limit: Some(u32::MAX),
            ..EventFilter::default()
        })
        .await?;
    let mut tasks = project_tasks(events);
    tasks.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then(left.id.cmp(&right.id))
    });
    if let Some(limit) = limit {
        tasks.truncate(limit as usize);
    }
    Ok(tasks)
}

pub async fn open_tasks(
    store: &impl EventStore,
    workspace_id: &str,
    limit: Option<u32>,
) -> Result<Vec<TaskSummary>> {
    let mut tasks = tasks(store, Some(workspace_id), None).await?;
    tasks.retain(|task| task.status == TaskStatus::Open);
    tasks.truncate(limit.unwrap_or(20) as usize);
    Ok(tasks)
}

pub async fn ensure_task_exists(
    store: &impl EventStore,
    workspace_id: &str,
    task_id: Uuid,
) -> Result<()> {
    if tasks(store, Some(workspace_id), None)
        .await?
        .iter()
        .any(|task| task.id == task_id)
    {
        Ok(())
    } else {
        Err(ShuttleError::Store(format!("unknown task id: {task_id}")))
    }
}

pub async fn claimed_tasks(
    store: &impl EventStore,
    workspace_id: &str,
    limit: Option<u32>,
) -> Result<Vec<TaskSummary>> {
    let mut tasks = tasks(store, Some(workspace_id), None).await?;
    tasks.retain(|task| task.status == TaskStatus::Claimed);
    tasks.truncate(limit.unwrap_or(20) as usize);
    Ok(tasks)
}

pub async fn handoffs(
    store: &impl EventStore,
    workspace_id: Option<&str>,
    limit: Option<u32>,
) -> Result<Vec<HandoffSummary>> {
    let events = store
        .list(EventFilter {
            event_type: Some(EventType::Handoff),
            workspace_id: workspace_id.map(ToOwned::to_owned),
            limit: Some(u32::MAX),
            ..EventFilter::default()
        })
        .await?;
    let mut handoffs = project_handoffs(events);
    handoffs.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then(left.id.cmp(&right.id))
    });
    if let Some(limit) = limit {
        handoffs.truncate(limit as usize);
    }
    Ok(handoffs)
}

pub async fn pending_handoffs(
    store: &impl EventStore,
    workspace_id: &str,
    limit: Option<u32>,
) -> Result<Vec<HandoffSummary>> {
    let mut handoffs = handoffs(store, Some(workspace_id), None).await?;
    handoffs.retain(|handoff| handoff.status == HandoffStatus::Pending);
    handoffs.truncate(limit.unwrap_or(20) as usize);
    Ok(handoffs)
}

pub async fn completed_handoffs(
    store: &impl EventStore,
    workspace_id: &str,
    limit: Option<u32>,
) -> Result<Vec<HandoffSummary>> {
    let mut handoffs = handoffs(store, Some(workspace_id), None).await?;
    handoffs.retain(|handoff| handoff.status == HandoffStatus::Completed);
    handoffs.truncate(limit.unwrap_or(20) as usize);
    Ok(handoffs)
}

pub async fn ensure_handoff_exists(
    store: &impl EventStore,
    workspace_id: &str,
    handoff_id: Uuid,
) -> Result<()> {
    if handoffs(store, Some(workspace_id), None)
        .await?
        .iter()
        .any(|handoff| handoff.id == handoff_id)
    {
        Ok(())
    } else {
        Err(ShuttleError::Store(format!(
            "unknown handoff id: {handoff_id}"
        )))
    }
}

fn project_tasks(events: Vec<Event>) -> Vec<TaskSummary> {
    let mut events = events;
    events.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then(left.id.cmp(&right.id))
    });
    let mut tasks: HashMap<Uuid, TaskSummary> = HashMap::new();
    for event in events {
        let action = action(&event);
        let task_id = referenced_id(&event, "task_id", "task_ref").unwrap_or(event.id);
        match action.as_deref() {
            Some("claimed") => {
                if let Some(task) = tasks.get_mut(&task_id) {
                    task.status = TaskStatus::Claimed;
                    task.claimed_by = Some(
                        string_metadata(&event.metadata_json, "claimed_by")
                            .unwrap_or_else(|| event.agent.clone()),
                    );
                    task.updated_at = event.created_at;
                    task.source_event_ids.push(event.id);
                }
            }
            Some("updated") => {
                if let Some(task) = tasks.get_mut(&task_id) {
                    task.content = event.content.clone();
                    task.updated_at = event.created_at;
                    task.source_event_ids.push(event.id);
                }
            }
            Some("completed") => {
                if let Some(task) = tasks.get_mut(&task_id) {
                    task.status = TaskStatus::Completed;
                    task.updated_at = event.created_at;
                    task.source_event_ids.push(event.id);
                }
            }
            _ if event.tags.iter().any(|tag| tag == TAG_OPEN) => {
                tasks.entry(task_id).or_insert_with(|| TaskSummary {
                    id: task_id,
                    status: TaskStatus::Open,
                    content: event.content.clone(),
                    created_by: event.agent.clone(),
                    claimed_by: None,
                    created_at: event.created_at,
                    updated_at: event.created_at,
                    source_event_ids: vec![event.id],
                });
            }
            _ if event.tags.iter().any(|tag| tag == TAG_CLAIMED) => {
                if let Some(task) = tasks.get_mut(&task_id) {
                    task.status = TaskStatus::Claimed;
                    task.claimed_by = Some(event.agent.clone());
                    task.updated_at = event.created_at;
                    task.source_event_ids.push(event.id);
                }
            }
            _ => {}
        }
    }
    tasks.into_values().collect()
}

fn project_handoffs(events: Vec<Event>) -> Vec<HandoffSummary> {
    let mut events = events;
    events.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then(left.id.cmp(&right.id))
    });
    let mut handoffs: HashMap<Uuid, HandoffSummary> = HashMap::new();
    for event in events {
        let action = action(&event);
        let handoff_id = referenced_id(&event, "handoff_id", "handoff_ref").unwrap_or(event.id);
        match action.as_deref() {
            Some("accepted") => {
                if let Some(handoff) = handoffs.get_mut(&handoff_id) {
                    handoff.status = HandoffStatus::Accepted;
                    handoff.accepted_by = Some(
                        string_metadata(&event.metadata_json, "accepted_by")
                            .unwrap_or_else(|| event.agent.clone()),
                    );
                    handoff.updated_at = event.created_at;
                    handoff.source_event_ids.push(event.id);
                }
            }
            Some("completed") => {
                if let Some(handoff) = handoffs.get_mut(&handoff_id) {
                    handoff.status = HandoffStatus::Completed;
                    handoff.updated_at = event.created_at;
                    handoff.source_event_ids.push(event.id);
                }
            }
            _ => {
                let to_agent = string_metadata(&event.metadata_json, "to");
                if let Some(to_agent) = to_agent {
                    handoffs
                        .entry(handoff_id)
                        .or_insert_with(|| HandoffSummary {
                            id: handoff_id,
                            status: HandoffStatus::Pending,
                            content: event.content.clone(),
                            from_agent: string_metadata(&event.metadata_json, "from")
                                .unwrap_or_else(|| event.agent.clone()),
                            to_agent,
                            accepted_by: None,
                            created_at: event.created_at,
                            updated_at: event.created_at,
                            source_event_ids: vec![event.id],
                        });
                }
            }
        }
    }
    handoffs.into_values().collect()
}

fn action(event: &Event) -> Option<String> {
    string_metadata(&event.metadata_json, "action")
}

fn string_metadata(metadata: &Value, key: &str) -> Option<String> {
    metadata
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn referenced_id(event: &Event, metadata_key: &str, tag_prefix: &str) -> Option<Uuid> {
    event
        .metadata_json
        .get(metadata_key)
        .and_then(Value::as_str)
        .and_then(|value| Uuid::parse_str(value).ok())
        .or_else(|| {
            event
                .tags
                .iter()
                .filter_map(|tag| tag.strip_prefix(&format!("{tag_prefix}:")))
                .find_map(|id| Uuid::parse_str(id).ok())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::EventStore;
    use crate::store::SqliteEventStore;

    #[test]
    fn task_create_and_claim_use_event_tags() {
        let task = new_task(
            "workspace".into(),
            "codex".into(),
            "session".into(),
            "ship mvp".into(),
        );
        assert_eq!(task.event_type, EventType::Task);
        assert_eq!(task.tags, vec![TAG_OPEN]);

        let claim = new_claim(
            "workspace".into(),
            "codex".into(),
            "session".into(),
            task.id,
        );
        assert!(claim.tags.contains(&TAG_CLAIMED.to_owned()));
        assert!(claim.tags.contains(&"claimed_by:codex".to_owned()));
        assert!(claim.tags.contains(&format!("task_ref:{}", task.id)));
    }

    #[test]
    fn open_tasks_excludes_claimed_tasks() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteEventStore::open(dir.path().join("shuttle.db")).unwrap();
        let first = new_task(
            "workspace".into(),
            "codex".into(),
            "session".into(),
            "ship first".into(),
        );
        let second = new_task(
            "workspace".into(),
            "codex".into(),
            "session".into(),
            "ship second".into(),
        );
        let claim = new_claim(
            "workspace".into(),
            "claude".into(),
            "session".into(),
            first.id,
        );

        futures_executor::block_on(store.append(first)).unwrap();
        futures_executor::block_on(store.append(second)).unwrap();
        futures_executor::block_on(store.append(claim)).unwrap();

        let tasks = futures_executor::block_on(open_tasks(&store, "workspace", None)).unwrap();

        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].content, "ship second");
    }

    #[test]
    fn open_tasks_considers_claims_beyond_default_projection_window() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteEventStore::open(dir.path().join("shuttle.db")).unwrap();
        let claimed_task = new_task(
            "workspace".into(),
            "codex".into(),
            "session".into(),
            "claimed".into(),
        );
        let open_task = new_task(
            "workspace".into(),
            "codex".into(),
            "session".into(),
            "still open".into(),
        );
        let old_claim = new_claim(
            "workspace".into(),
            "claude".into(),
            "session".into(),
            claimed_task.id,
        );
        futures_executor::block_on(store.append(claimed_task)).unwrap();
        futures_executor::block_on(store.append(open_task)).unwrap();
        futures_executor::block_on(store.append(old_claim)).unwrap();

        for _ in 0..500 {
            let task = new_task(
                "workspace".into(),
                "codex".into(),
                "session".into(),
                "noise".into(),
            );
            let claim = new_claim(
                "workspace".into(),
                "claude".into(),
                "session".into(),
                task.id,
            );
            futures_executor::block_on(store.append(task)).unwrap();
            futures_executor::block_on(store.append(claim)).unwrap();
        }

        let tasks = futures_executor::block_on(open_tasks(&store, "workspace", None)).unwrap();

        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].content, "still open");
    }

    #[test]
    fn task_projection_tracks_update_claim_and_completion() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteEventStore::open(dir.path().join("shuttle.db")).unwrap();
        let task = new_task(
            "workspace".into(),
            "codex".into(),
            "session".into(),
            "first description".into(),
        );
        let update = new_task_update(
            "workspace".into(),
            "codex".into(),
            "session".into(),
            task.id,
            "latest description".into(),
        );
        let claim = new_claim(
            "workspace".into(),
            "claude".into(),
            "session".into(),
            task.id,
        );
        let done = new_task_done(
            "workspace".into(),
            "claude".into(),
            "session".into(),
            task.id,
        );

        futures_executor::block_on(store.append(task)).unwrap();
        futures_executor::block_on(store.append(update)).unwrap();
        futures_executor::block_on(store.append(claim)).unwrap();
        futures_executor::block_on(store.append(done)).unwrap();

        let tasks = futures_executor::block_on(tasks(&store, Some("workspace"), None)).unwrap();
        let open = futures_executor::block_on(open_tasks(&store, "workspace", None)).unwrap();

        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].status, TaskStatus::Completed);
        assert_eq!(tasks[0].content, "latest description");
        assert_eq!(tasks[0].claimed_by.as_deref(), Some("claude"));
        assert!(open.is_empty());
    }

    #[test]
    fn handoff_projection_tracks_accept_and_completion() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteEventStore::open(dir.path().join("shuttle.db")).unwrap();
        let handoff = new_handoff(
            "workspace".into(),
            "codex".into(),
            "session".into(),
            "claude".into(),
            "continue this branch".into(),
        );
        let accept = new_handoff_accept(
            "workspace".into(),
            "claude".into(),
            "session".into(),
            handoff.id,
        );
        let done = new_handoff_done(
            "workspace".into(),
            "claude".into(),
            "session".into(),
            handoff.id,
        );

        futures_executor::block_on(store.append(handoff)).unwrap();
        let pending =
            futures_executor::block_on(pending_handoffs(&store, "workspace", None)).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].to_agent, "claude");

        futures_executor::block_on(store.append(accept)).unwrap();
        futures_executor::block_on(store.append(done)).unwrap();

        let handoffs =
            futures_executor::block_on(handoffs(&store, Some("workspace"), None)).unwrap();
        let completed =
            futures_executor::block_on(completed_handoffs(&store, "workspace", None)).unwrap();

        assert_eq!(handoffs.len(), 1);
        assert_eq!(handoffs[0].status, HandoffStatus::Completed);
        assert_eq!(handoffs[0].accepted_by.as_deref(), Some("claude"));
        assert_eq!(completed.len(), 1);
    }
}
