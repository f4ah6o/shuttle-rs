use serde::{Deserialize, Serialize};
use shuttle_core::Event;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Context {
    pub repo: String,
    pub branch: String,
    pub commit: String,
    pub open_tasks: Vec<Event>,
    pub recent_decisions: Vec<Event>,
    pub related_memories: Vec<Event>,
    pub inbox: Vec<Event>,
}
