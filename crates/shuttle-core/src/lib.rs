use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    Message,
    Memory,
    Decision,
    Task,
    Handoff,
    Observation,
    Artifact,
}

impl EventType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::Memory => "memory",
            Self::Decision => "decision",
            Self::Task => "task",
            Self::Handoff => "handoff",
            Self::Observation => "observation",
            Self::Artifact => "artifact",
        }
    }
}

impl TryFrom<&str> for EventType {
    type Error = ShuttleError;

    fn try_from(value: &str) -> Result<Self> {
        match value {
            "message" => Ok(Self::Message),
            "memory" => Ok(Self::Memory),
            "decision" => Ok(Self::Decision),
            "task" => Ok(Self::Task),
            "handoff" => Ok(Self::Handoff),
            "observation" => Ok(Self::Observation),
            "artifact" => Ok(Self::Artifact),
            other => Err(ShuttleError::InvalidEventType(other.to_owned())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub id: Uuid,
    pub event_type: EventType,
    pub workspace_id: String,
    pub repo_id: Option<String>,
    pub repo_path: Option<String>,
    pub git_remote: Option<String>,
    pub bit_repo_id: Option<String>,
    pub branch: Option<String>,
    pub commit: Option<String>,
    pub agent: String,
    pub session_id: String,
    pub title: Option<String>,
    pub content: String,
    pub tags: Vec<String>,
    pub metadata_json: Value,
    pub created_at: DateTime<Utc>,
}

impl Event {
    pub fn new(input: NewEvent) -> Self {
        Self {
            id: Uuid::new_v4(),
            event_type: input.event_type,
            workspace_id: input.workspace_id,
            repo_id: input.repo_id,
            repo_path: input.repo_path,
            git_remote: input.git_remote,
            bit_repo_id: input.bit_repo_id,
            branch: input.branch,
            commit: input.commit,
            agent: input.agent,
            session_id: input.session_id,
            title: input.title,
            content: input.content,
            tags: input.tags,
            metadata_json: input.metadata_json,
            created_at: Utc::now(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewEvent {
    pub event_type: EventType,
    pub workspace_id: String,
    pub repo_id: Option<String>,
    pub repo_path: Option<String>,
    pub git_remote: Option<String>,
    pub bit_repo_id: Option<String>,
    pub branch: Option<String>,
    pub commit: Option<String>,
    pub agent: String,
    pub session_id: String,
    pub title: Option<String>,
    pub content: String,
    pub tags: Vec<String>,
    pub metadata_json: Value,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct EventFilter {
    pub event_type: Option<EventType>,
    pub workspace_id: Option<String>,
    pub agent: Option<String>,
    pub recipient: Option<String>,
    pub tag: Option<String>,
    pub query: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Error)]
pub enum ShuttleError {
    #[error("invalid event type: {0}")]
    InvalidEventType(String),
    #[error("store error: {0}")]
    Store(String),
    #[error("serialization error: {0}")]
    Serialization(String),
}

pub type Result<T> = std::result::Result<T, ShuttleError>;

#[async_trait]
pub trait EventStore: Send + Sync {
    async fn append(&self, event: Event) -> Result<Event>;
    async fn list(&self, filter: EventFilter) -> Result<Vec<Event>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_type_round_trips_through_string_values() {
        assert_eq!(EventType::try_from("memory").unwrap(), EventType::Memory);
        assert_eq!(EventType::Message.as_str(), "message");
        assert!(EventType::try_from("unknown").is_err());
    }
}
