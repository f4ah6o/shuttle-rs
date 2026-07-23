use std::collections::HashSet;

use serde_json::json;
use shuttle_core::{Event, EventFilter, EventStore, EventType, NewEvent, Result};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RecallResult {
    pub event: Event,
    pub score: i64,
    pub reasons: Vec<String>,
}

pub fn new_memory(
    workspace_id: String,
    agent: String,
    session_id: String,
    content: String,
) -> Event {
    new_typed_memory(EventType::Memory, workspace_id, agent, session_id, content)
}

pub fn new_typed_memory(
    event_type: EventType,
    workspace_id: String,
    agent: String,
    session_id: String,
    content: String,
) -> Event {
    Event::new(NewEvent {
        event_type,
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
        title: title_for(event_type),
        content,
        tags: Vec::new(),
        metadata_json: json!({ "kind": event_type.as_str() }),
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
    recall_by_type(store, query, Some(EventType::Memory), None).await
}

pub async fn recall_by_type(
    store: &impl EventStore,
    query: &str,
    event_type: Option<EventType>,
    workspace_id: Option<&str>,
) -> Result<Vec<Event>> {
    if let Some(event_type) = event_type {
        return recall_candidates_for_type(store, query, event_type, workspace_id).await;
    }

    let mut events = Vec::new();
    for event_type in memory_event_types() {
        events.extend(recall_candidates_for_type(store, query, event_type, workspace_id).await?);
    }
    dedup_events(&mut events);
    events.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then(left.id.cmp(&right.id))
    });
    events.truncate(50);
    Ok(events)
}

async fn recall_candidates_for_type(
    store: &impl EventStore,
    query: &str,
    event_type: EventType,
    workspace_id: Option<&str>,
) -> Result<Vec<Event>> {
    let mut events = store
        .list(EventFilter {
            event_type: Some(event_type),
            workspace_id: workspace_id.map(ToOwned::to_owned),
            query: Some(query.to_owned()),
            limit: Some(50),
            ..EventFilter::default()
        })
        .await?;

    let mut tokens = query
        .split_whitespace()
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    tokens.sort_unstable();
    tokens.dedup();
    for token in tokens.into_iter().take(8) {
        events.extend(
            store
                .list(EventFilter {
                    event_type: Some(event_type),
                    workspace_id: workspace_id.map(ToOwned::to_owned),
                    query: Some(token.to_owned()),
                    limit: Some(50),
                    ..EventFilter::default()
                })
                .await?,
        );
    }

    dedup_events(&mut events);
    events.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then(left.id.cmp(&right.id))
    });
    events.truncate(50);
    Ok(events)
}

fn dedup_events(events: &mut Vec<Event>) {
    let mut seen = HashSet::new();
    events.retain(|event| seen.insert(event.id));
}

pub async fn ranked_recall(
    store: &impl EventStore,
    query: &str,
    event_type: Option<EventType>,
    workspace_id: Option<&str>,
    repo_id: Option<&str>,
    branch: Option<&str>,
) -> Result<Vec<RecallResult>> {
    let events = recall_by_type(store, query, event_type, workspace_id).await?;
    let mut results = events
        .into_iter()
        .map(|event| score_event(event, query, repo_id, branch))
        .collect::<Vec<_>>();
    results.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then(right.event.created_at.cmp(&left.event.created_at))
            .then(left.event.id.cmp(&right.event.id))
    });
    Ok(results)
}

pub fn memory_event_types() -> Vec<EventType> {
    vec![
        EventType::Memory,
        EventType::Decision,
        EventType::Observation,
        EventType::Pattern,
        EventType::Fact,
        EventType::Bug,
        EventType::Handoff,
    ]
}

fn title_for(event_type: EventType) -> Option<String> {
    match event_type {
        EventType::Memory => None,
        EventType::Decision => Some("decision".to_owned()),
        EventType::Observation => Some("observation".to_owned()),
        EventType::Pattern => Some("pattern".to_owned()),
        EventType::Fact => Some("fact".to_owned()),
        EventType::Bug => Some("bug".to_owned()),
        EventType::Handoff => Some("handoff".to_owned()),
        _ => Some(event_type.as_str().to_owned()),
    }
}

fn score_event(
    event: Event,
    query: &str,
    repo_id: Option<&str>,
    branch: Option<&str>,
) -> RecallResult {
    let query = query.to_lowercase();
    let tokens = query
        .split_whitespace()
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    let searchable = format!(
        "{}\n{}\n{}\n{}",
        event.title.as_deref().unwrap_or_default(),
        event.content,
        event.tags.join(" "),
        event.metadata_json
    )
    .to_lowercase();
    let mut score = 0;
    let mut reasons = Vec::new();

    let exact_match = !query.is_empty() && searchable.contains(&query);
    if exact_match {
        score += 50;
        reasons.push("exact text match".to_owned());
    }
    if !exact_match {
        let token_matches = tokens
            .iter()
            .filter(|token| searchable.contains(**token))
            .count();
        if token_matches > 0 {
            score += (token_matches as i64) * 10;
            reasons.push(format!("{token_matches} token match(es)"));
        }
    }
    if matches!(event.event_type, EventType::Decision) {
        score += 8;
        reasons.push("decision event".to_owned());
    } else if event.event_type != EventType::Memory {
        score += 4;
        reasons.push(format!("typed {} event", event.event_type.as_str()));
    }
    if let (Some(current), Some(event_repo)) = (repo_id, event.repo_id.as_deref()) {
        if current == event_repo {
            score += 12;
            reasons.push("same repo".to_owned());
        }
    }
    if let (Some(current), Some(event_branch)) = (branch, event.branch.as_deref()) {
        if current == event_branch {
            score += 6;
            reasons.push("same branch".to_owned());
        }
    }

    RecallResult {
        event,
        score,
        reasons,
    }
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

    #[test]
    fn typed_memory_uses_event_type_and_kind_metadata() {
        let event = new_typed_memory(
            EventType::Decision,
            "workspace".into(),
            "codex".into(),
            "session".into(),
            "SQLite chosen".into(),
        );

        assert_eq!(event.event_type, EventType::Decision);
        assert_eq!(event.title.as_deref(), Some("decision"));
        assert_eq!(event.metadata_json["kind"], "decision");
    }

    #[test]
    fn ranking_prefers_same_repo_and_decisions() {
        let mut decision = new_typed_memory(
            EventType::Decision,
            "workspace".into(),
            "codex".into(),
            "session".into(),
            "SQLite storage decision".into(),
        );
        decision.repo_id = Some("repo".into());
        decision.branch = Some("main".into());

        let result = score_event(decision, "SQLite", Some("repo"), Some("main"));

        assert!(result.score >= 76);
        assert!(result.reasons.contains(&"decision event".to_owned()));
        assert!(result.reasons.contains(&"same repo".to_owned()));
        assert!(result.reasons.contains(&"same branch".to_owned()));
    }
}
