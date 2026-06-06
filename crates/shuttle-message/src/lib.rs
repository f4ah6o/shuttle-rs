use serde_json::json;
use shuttle_core::{Event, EventFilter, EventStore, EventType, NewEvent, Result};

pub fn recipient_tag(agent: &str) -> String {
    format!("to:{agent}")
}

pub fn new_message(
    workspace_id: String,
    from_agent: String,
    session_id: String,
    to_agent: String,
    content: String,
) -> Event {
    Event::new(NewEvent {
        event_type: EventType::Message,
        workspace_id,
        repo_id: None,
        repo_path: None,
        git_remote: None,
        bit_repo_id: None,
        branch: None,
        commit: None,
        agent: from_agent,
        session_id,
        title: None,
        content,
        tags: Vec::new(),
        metadata_json: json!({ "to": to_agent }),
    })
}

pub async fn inbox(store: &impl EventStore, agent: &str) -> Result<Vec<Event>> {
    let mut events = store
        .list(EventFilter {
            event_type: Some(EventType::Message),
            recipient: Some(agent.to_owned()),
            ..EventFilter::default()
        })
        .await?;
    events.extend(
        store
            .list(EventFilter {
                event_type: Some(EventType::Handoff),
                recipient: Some(agent.to_owned()),
                ..EventFilter::default()
            })
            .await?,
    );
    events.sort_by(|left, right| right.created_at.cmp(&left.created_at));
    Ok(events)
}

pub async fn history(store: &impl EventStore) -> Result<Vec<Event>> {
    store
        .list(EventFilter {
            event_type: Some(EventType::Message),
            ..EventFilter::default()
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_uses_recipient_metadata() {
        let event = new_message(
            "workspace".into(),
            "codex".into(),
            "session".into(),
            "claude".into(),
            "review this".into(),
        );
        assert_eq!(event.event_type, EventType::Message);
        assert!(event.tags.is_empty());
        assert_eq!(event.metadata_json["to"], "claude");
    }

    #[test]
    fn recipient_tag_is_shared_by_messages_and_handoffs() {
        assert_eq!(recipient_tag("codex"), "to:codex");
    }
}
