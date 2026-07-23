use crate::core::{Event, EventFilter, EventStore, Result};
use crate::message;
use crate::task::{self, HandoffStatus, HandoffSummary, TaskStatus, TaskSummary};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollabStart {
    pub task: Event,
    pub messages: Vec<Event>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollabNudge {
    pub message: Event,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollabPass {
    pub task_update: Event,
    pub handoff: Event,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollabStatus {
    pub tasks: Vec<TaskSummary>,
    pub pending_handoffs: Vec<HandoffSummary>,
    pub recent_messages: Vec<Event>,
}

pub fn start_events(
    workspace_id: String,
    from_agent: String,
    session_id: String,
    content: String,
    agents: Vec<String>,
) -> CollabStart {
    let agents = normalized_agents(agents);
    let mut task = task::new_task(
        workspace_id.clone(),
        from_agent.clone(),
        session_id.clone(),
        content.clone(),
    );
    if let Some(metadata) = task.metadata_json.as_object_mut() {
        metadata.insert("collab".to_owned(), json!(true));
        metadata.insert("agents".to_owned(), json!(agents));
    }

    let messages = agents
        .iter()
        .filter(|agent| *agent != &from_agent)
        .map(|agent| {
            message::new_message(
                workspace_id.clone(),
                from_agent.clone(),
                session_id.clone(),
                agent.clone(),
                format!("Collab task started: {content}"),
            )
        })
        .collect();

    CollabStart { task, messages }
}

pub fn nudge_event(
    workspace_id: String,
    from_agent: String,
    session_id: String,
    to_agent: String,
    content: String,
) -> CollabNudge {
    CollabNudge {
        message: message::new_message(workspace_id, from_agent, session_id, to_agent, content),
    }
}

pub fn pass_events(
    workspace_id: String,
    from_agent: String,
    session_id: String,
    to_agent: String,
    task_id: Uuid,
    note: String,
) -> CollabPass {
    let mut handoff = task::new_handoff(
        workspace_id.clone(),
        from_agent.clone(),
        session_id.clone(),
        to_agent,
        note.clone(),
    );
    if let Some(metadata) = handoff.metadata_json.as_object_mut() {
        metadata.insert("collab".to_owned(), json!(true));
        metadata.insert("task_id".to_owned(), json!(task_id));
    }

    CollabPass {
        task_update: task::new_task_update(workspace_id, from_agent, session_id, task_id, note),
        handoff,
    }
}

pub async fn status(store: &impl EventStore, workspace_id: &str) -> Result<CollabStatus> {
    let mut tasks = task::tasks(store, Some(workspace_id), None).await?;
    tasks.retain(|task| task.status != TaskStatus::Completed);
    tasks.truncate(20);

    let mut pending_handoffs = task::handoffs(store, Some(workspace_id), None).await?;
    pending_handoffs.retain(|handoff| handoff.status == HandoffStatus::Pending);
    pending_handoffs.truncate(20);

    let mut recent_messages = store
        .list(EventFilter {
            event_type: Some(crate::core::EventType::Message),
            workspace_id: Some(workspace_id.to_owned()),
            limit: Some(20),
            ..EventFilter::default()
        })
        .await?;
    recent_messages.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then(left.id.cmp(&right.id))
    });
    recent_messages.truncate(20);

    Ok(CollabStatus {
        tasks,
        pending_handoffs,
        recent_messages,
    })
}

fn normalized_agents(agents: Vec<String>) -> Vec<String> {
    let mut normalized = Vec::new();
    for agent in agents {
        let agent = agent.trim();
        if !agent.is_empty() && !normalized.iter().any(|existing| existing == agent) {
            normalized.push(agent.to_owned());
        }
    }
    if normalized.is_empty() {
        normalized.push("codex".to_owned());
        normalized.push("claude".to_owned());
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::SqliteEventStore;

    #[test]
    fn start_events_create_task_and_peer_messages() {
        let start = start_events(
            "workspace".into(),
            "codex".into(),
            "session".into(),
            "ship feature".into(),
            vec!["codex".into(), "claude".into()],
        );

        assert_eq!(start.task.content, "ship feature");
        assert_eq!(start.task.metadata_json["collab"], true);
        assert_eq!(start.task.metadata_json["agents"][0], "codex");
        assert_eq!(start.messages.len(), 1);
        assert_eq!(start.messages[0].metadata_json["to"], "claude");
    }

    #[test]
    fn pass_events_link_update_to_task_and_create_handoff() {
        let task_id = Uuid::new_v4();
        let pass = pass_events(
            "workspace".into(),
            "codex".into(),
            "session".into(),
            "claude".into(),
            task_id,
            "please continue".into(),
        );

        assert_eq!(
            pass.task_update.metadata_json["task_id"],
            task_id.to_string()
        );
        assert_eq!(pass.handoff.metadata_json["to"], "claude");
    }

    #[test]
    fn status_reports_active_tasks_handoffs_and_messages() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteEventStore::open(dir.path().join("shuttle.db")).unwrap();
        let start = start_events(
            "workspace".into(),
            "codex".into(),
            "session".into(),
            "ship feature".into(),
            vec!["codex".into(), "claude".into()],
        );
        let task_id = start.task.id;
        futures_executor::block_on(store.append(start.task)).unwrap();
        for message in start.messages {
            futures_executor::block_on(store.append(message)).unwrap();
        }
        let pass = pass_events(
            "workspace".into(),
            "codex".into(),
            "session".into(),
            "claude".into(),
            task_id,
            "continue this".into(),
        );
        futures_executor::block_on(store.append(pass.task_update)).unwrap();
        futures_executor::block_on(store.append(pass.handoff)).unwrap();

        let status = futures_executor::block_on(status(&store, "workspace")).unwrap();
        assert_eq!(status.tasks.len(), 1);
        assert_eq!(status.pending_handoffs.len(), 1);
        assert_eq!(status.recent_messages.len(), 1);
    }
}
