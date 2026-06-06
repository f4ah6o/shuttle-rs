use serde_json::json;
use shuttle_core::{Event, EventFilter, EventStore, EventType, NewEvent, Result};
use uuid::Uuid;

pub const TAG_OPEN: &str = "task:open";
pub const TAG_CLAIMED: &str = "task:claimed";

pub fn claim_tag(agent: &str) -> String {
    format!("claimed_by:{agent}")
}

pub fn task_ref_tag(id: Uuid) -> String {
    format!("task_ref:{id}")
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
        metadata_json: json!({}),
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
        metadata_json: json!({}),
    })
}

pub async fn list(store: &impl EventStore) -> Result<Vec<Event>> {
    store
        .list(EventFilter {
            event_type: Some(EventType::Task),
            ..EventFilter::default()
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
