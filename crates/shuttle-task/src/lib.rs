use serde_json::json;
use shuttle_core::{Event, EventFilter, EventStore, EventType, NewEvent, Result};
use std::collections::HashSet;
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

pub async fn open_tasks(
    store: &impl EventStore,
    workspace_id: &str,
    limit: Option<u32>,
) -> Result<Vec<Event>> {
    let limit = limit.unwrap_or(20);
    let claims = store
        .list(EventFilter {
            event_type: Some(EventType::Task),
            workspace_id: Some(workspace_id.to_owned()),
            tag: Some(TAG_CLAIMED.to_owned()),
            limit: Some(u32::MAX),
            ..EventFilter::default()
        })
        .await?;
    let claimed_task_ids = claims
        .iter()
        .flat_map(|event| event.tags.iter())
        .filter_map(|tag| tag.strip_prefix("task_ref:"))
        .filter_map(|id| Uuid::parse_str(id).ok())
        .collect::<HashSet<_>>();

    let mut tasks = store
        .list(EventFilter {
            event_type: Some(EventType::Task),
            workspace_id: Some(workspace_id.to_owned()),
            tag: Some(TAG_OPEN.to_owned()),
            limit: Some(u32::MAX),
            ..EventFilter::default()
        })
        .await?;
    tasks.retain(|event| !claimed_task_ids.contains(&event.id));
    tasks.truncate(limit as usize);
    Ok(tasks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use shuttle_core::EventStore;
    use shuttle_store::SqliteEventStore;

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
}
