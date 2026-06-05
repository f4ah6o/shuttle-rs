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
        branch: None,
        commit: None,
        agent: from_agent,
        session_id,
        title: None,
        content,
        tags: vec![recipient_tag(&to_agent)],
    })
}

pub async fn inbox(store: &impl EventStore, agent: &str) -> Result<Vec<Event>> {
    store
        .list(EventFilter {
            event_type: Some(EventType::Message),
            tag: Some(recipient_tag(agent)),
            ..EventFilter::default()
        })
        .await
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
    fn message_uses_recipient_tag() {
        let event = new_message(
            "workspace".into(),
            "codex".into(),
            "session".into(),
            "claude".into(),
            "review this".into(),
        );
        assert_eq!(event.event_type, EventType::Message);
        assert_eq!(event.tags, vec!["to:claude"]);
    }
}
