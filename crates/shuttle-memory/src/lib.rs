use serde_json::json;
use shuttle_core::{Event, EventFilter, EventStore, EventType, NewEvent, Result};

pub fn new_memory(
    workspace_id: String,
    agent: String,
    session_id: String,
    content: String,
) -> Event {
    Event::new(NewEvent {
        event_type: EventType::Memory,
        workspace_id,
        repo_id: None,
        repo_path: None,
        git_remote: None,
        bit_repo_id: None,
        branch: None,
        commit: None,
        agent,
        session_id,
        title: None,
        content,
        tags: Vec::new(),
        metadata_json: json!({}),
    })
}

pub async fn memories(store: &impl EventStore) -> Result<Vec<Event>> {
    store
        .list(EventFilter {
            event_type: Some(EventType::Memory),
            ..EventFilter::default()
        })
        .await
}

pub async fn recall(store: &impl EventStore, query: &str) -> Result<Vec<Event>> {
    store
        .list(EventFilter {
            event_type: Some(EventType::Memory),
            query: Some(query.to_owned()),
            ..EventFilter::default()
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_is_an_event() {
        let event = new_memory(
            "workspace".into(),
            "codex".into(),
            "session".into(),
            "SQLite chosen".into(),
        );
        assert_eq!(event.event_type, EventType::Memory);
        assert_eq!(event.content, "SQLite chosen");
    }
}
