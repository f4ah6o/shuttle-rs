use std::path::PathBuf;
use std::process::Command;

use crate::core::{Event, EventStore, Result, ShuttleError};
use crate::store::SqliteEventStore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

#[derive(Clone)]
pub struct McpRuntime {
    pub store: SqliteEventStore,
    pub cwd: PathBuf,
    pub workspace_id: String,
    pub agent: String,
    pub session_id: String,
}

#[derive(Debug, Deserialize)]
pub struct Request {
    pub jsonrpc: Option<String>,
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Serialize)]
struct Tool {
    name: &'static str,
    description: &'static str,
    #[serde(rename = "inputSchema")]
    input_schema: Value,
    #[serde(rename = "outputSchema")]
    output_schema: Value,
}

pub async fn handle_request(runtime: &McpRuntime, request: Request) -> Value {
    let id = request.id.clone().unwrap_or(Value::Null);
    if request.jsonrpc.as_deref() != Some("2.0") {
        return error(id, -32600, "invalid jsonrpc version");
    }

    match request.method.as_str() {
        "initialize" => ok(
            id,
            json!({
                "protocolVersion": "2025-11-25",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "shuttle-rs", "version": env!("CARGO_PKG_VERSION") }
            }),
        ),
        "notifications/initialized" => json!({"jsonrpc": "2.0"}),
        "tools/list" => ok(id, json!({ "tools": tools() })),
        "tools/call" => {
            let tool_name = request
                .params
                .get("name")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            match call_tool(runtime, request.params).await {
                Ok(value) => ok(
                    id,
                    json!({
                        "content": [{ "type": "text", "text": value.to_string() }],
                        "structuredContent": structured_content(tool_name.as_deref(), &value),
                    }),
                ),
                Err(err) => error(id, -32603, &err.to_string()),
            }
        }
        _ => error(id, -32601, "method not found"),
    }
}

async fn call_tool(runtime: &McpRuntime, params: Value) -> Result<Value> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| ShuttleError::Store("missing tool name".to_owned()))?;
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    match name {
        "shuttle_memory_search" | "recall" => {
            let query = string_arg(&args, "query")?;
            let events = crate::memory::recall(&runtime.store, &query).await?;
            serde_json::to_value(events).map_err(|err| ShuttleError::Serialization(err.to_string()))
        }
        "shuttle_memory_store" | "remember" => {
            let content = string_arg(&args, "content")?;
            let event = with_repo_metadata(
                crate::memory::new_memory(
                    runtime.workspace_id.clone(),
                    runtime.agent.clone(),
                    runtime.session_id.clone(),
                    content,
                ),
                runtime,
            );
            let event = runtime.store.append(event).await?;
            serde_json::to_value(event).map_err(|err| ShuttleError::Serialization(err.to_string()))
        }
        "shuttle_message_inbox" | "inbox" => {
            let agent = args
                .get("agent")
                .and_then(Value::as_str)
                .unwrap_or(&runtime.agent);
            let events = crate::message::inbox(&runtime.store, agent).await?;
            serde_json::to_value(events).map_err(|err| ShuttleError::Serialization(err.to_string()))
        }
        "shuttle_message_history" | "history" => {
            let events = crate::message::history(&runtime.store).await?;
            serde_json::to_value(events).map_err(|err| ShuttleError::Serialization(err.to_string()))
        }
        "shuttle_message_send" | "send" => {
            let to_agent = string_arg(&args, "agent")?;
            let content = string_arg(&args, "content")?;
            let event = with_repo_metadata(
                crate::message::new_message(
                    runtime.workspace_id.clone(),
                    runtime.agent.clone(),
                    runtime.session_id.clone(),
                    to_agent,
                    content,
                ),
                runtime,
            );
            let event = runtime.store.append(event).await?;
            serde_json::to_value(event).map_err(|err| ShuttleError::Serialization(err.to_string()))
        }
        "shuttle_task_list" | "tasks" => {
            let tasks =
                crate::task::tasks(&runtime.store, Some(&runtime.workspace_id), None).await?;
            serde_json::to_value(tasks).map_err(|err| ShuttleError::Serialization(err.to_string()))
        }
        "shuttle_task_create" => {
            let content = string_arg(&args, "content")?;
            let event = with_repo_metadata(
                crate::task::new_task(
                    runtime.workspace_id.clone(),
                    runtime.agent.clone(),
                    runtime.session_id.clone(),
                    content,
                ),
                runtime,
            );
            let event = runtime.store.append(event).await?;
            serde_json::to_value(event).map_err(|err| ShuttleError::Serialization(err.to_string()))
        }
        "shuttle_task_claim" => {
            let id = Uuid::parse_str(&string_arg(&args, "id")?)
                .map_err(|err| ShuttleError::Store(err.to_string()))?;
            crate::task::ensure_task_exists(&runtime.store, &runtime.workspace_id, id).await?;
            let event = with_repo_metadata(
                crate::task::new_claim(
                    runtime.workspace_id.clone(),
                    runtime.agent.clone(),
                    runtime.session_id.clone(),
                    id,
                ),
                runtime,
            );
            let event = runtime.store.append(event).await?;
            serde_json::to_value(event).map_err(|err| ShuttleError::Serialization(err.to_string()))
        }
        "shuttle_task_update" => {
            let id = Uuid::parse_str(&string_arg(&args, "id")?)
                .map_err(|err| ShuttleError::Store(err.to_string()))?;
            let content = string_arg(&args, "content")?;
            crate::task::ensure_task_exists(&runtime.store, &runtime.workspace_id, id).await?;
            let event = with_repo_metadata(
                crate::task::new_task_update(
                    runtime.workspace_id.clone(),
                    runtime.agent.clone(),
                    runtime.session_id.clone(),
                    id,
                    content,
                ),
                runtime,
            );
            let event = runtime.store.append(event).await?;
            serde_json::to_value(event).map_err(|err| ShuttleError::Serialization(err.to_string()))
        }
        "shuttle_task_done" => {
            let id = Uuid::parse_str(&string_arg(&args, "id")?)
                .map_err(|err| ShuttleError::Store(err.to_string()))?;
            crate::task::ensure_task_exists(&runtime.store, &runtime.workspace_id, id).await?;
            let event = with_repo_metadata(
                crate::task::new_task_done(
                    runtime.workspace_id.clone(),
                    runtime.agent.clone(),
                    runtime.session_id.clone(),
                    id,
                ),
                runtime,
            );
            let event = runtime.store.append(event).await?;
            serde_json::to_value(event).map_err(|err| ShuttleError::Serialization(err.to_string()))
        }
        "shuttle_handoff_request" => {
            let to_agent = string_arg(&args, "agent")?;
            let content = string_arg(&args, "content")?;
            let event = with_repo_metadata(
                crate::task::new_handoff(
                    runtime.workspace_id.clone(),
                    runtime.agent.clone(),
                    runtime.session_id.clone(),
                    to_agent,
                    content,
                ),
                runtime,
            );
            let event = runtime.store.append(event).await?;
            serde_json::to_value(event).map_err(|err| ShuttleError::Serialization(err.to_string()))
        }
        "shuttle_handoff_list" => {
            let handoffs =
                crate::task::handoffs(&runtime.store, Some(&runtime.workspace_id), None).await?;
            serde_json::to_value(handoffs)
                .map_err(|err| ShuttleError::Serialization(err.to_string()))
        }
        "shuttle_handoff_accept" => {
            let id = Uuid::parse_str(&string_arg(&args, "id")?)
                .map_err(|err| ShuttleError::Store(err.to_string()))?;
            crate::task::ensure_handoff_exists(&runtime.store, &runtime.workspace_id, id).await?;
            let event = with_repo_metadata(
                crate::task::new_handoff_accept(
                    runtime.workspace_id.clone(),
                    runtime.agent.clone(),
                    runtime.session_id.clone(),
                    id,
                ),
                runtime,
            );
            let event = runtime.store.append(event).await?;
            serde_json::to_value(event).map_err(|err| ShuttleError::Serialization(err.to_string()))
        }
        "shuttle_handoff_done" => {
            let id = Uuid::parse_str(&string_arg(&args, "id")?)
                .map_err(|err| ShuttleError::Store(err.to_string()))?;
            crate::task::ensure_handoff_exists(&runtime.store, &runtime.workspace_id, id).await?;
            let event = with_repo_metadata(
                crate::task::new_handoff_done(
                    runtime.workspace_id.clone(),
                    runtime.agent.clone(),
                    runtime.session_id.clone(),
                    id,
                ),
                runtime,
            );
            let event = runtime.store.append(event).await?;
            serde_json::to_value(event).map_err(|err| ShuttleError::Serialization(err.to_string()))
        }
        "shuttle_collab_start" => {
            let content = string_arg(&args, "content")?;
            let agents = string_list_arg(&args, "agents")
                .unwrap_or_else(|| vec!["codex".to_owned(), "claude".to_owned()]);
            let start = crate::collab::start_events(
                runtime.workspace_id.clone(),
                runtime.agent.clone(),
                runtime.session_id.clone(),
                content,
                agents,
            );
            let task = runtime
                .store
                .append(with_repo_metadata(start.task, runtime))
                .await?;
            let mut messages = Vec::new();
            for message in start.messages {
                messages.push(
                    runtime
                        .store
                        .append(with_repo_metadata(message, runtime))
                        .await?,
                );
            }
            serde_json::to_value(crate::collab::CollabStart { task, messages })
                .map_err(|err| ShuttleError::Serialization(err.to_string()))
        }
        "shuttle_collab_status" => {
            let status = crate::collab::status(&runtime.store, &runtime.workspace_id).await?;
            serde_json::to_value(status).map_err(|err| ShuttleError::Serialization(err.to_string()))
        }
        "shuttle_collab_nudge" => {
            let to_agent = string_arg(&args, "agent")?;
            let content = string_arg(&args, "content")?;
            let nudge = crate::collab::nudge_event(
                runtime.workspace_id.clone(),
                runtime.agent.clone(),
                runtime.session_id.clone(),
                to_agent,
                content,
            );
            let message = runtime
                .store
                .append(with_repo_metadata(nudge.message, runtime))
                .await?;
            serde_json::to_value(crate::collab::CollabNudge { message })
                .map_err(|err| ShuttleError::Serialization(err.to_string()))
        }
        "shuttle_collab_pass" => {
            let to_agent = string_arg(&args, "agent")?;
            let task_id = Uuid::parse_str(&string_arg(&args, "task_id")?)
                .map_err(|err| ShuttleError::Store(err.to_string()))?;
            let note = string_arg(&args, "note")?;
            crate::task::ensure_task_exists(&runtime.store, &runtime.workspace_id, task_id).await?;
            let pass = crate::collab::pass_events(
                runtime.workspace_id.clone(),
                runtime.agent.clone(),
                runtime.session_id.clone(),
                to_agent,
                task_id,
                note,
            );
            let task_update = runtime
                .store
                .append(with_repo_metadata(pass.task_update, runtime))
                .await?;
            let handoff = runtime
                .store
                .append(with_repo_metadata(pass.handoff, runtime))
                .await?;
            serde_json::to_value(crate::collab::CollabPass {
                task_update,
                handoff,
            })
            .map_err(|err| ShuttleError::Serialization(err.to_string()))
        }
        "shuttle_repo_context" | "context" => {
            let context = crate::context::assemble_context(
                &runtime.store,
                &runtime.cwd,
                &runtime.workspace_id,
                &runtime.agent,
            )
            .await?;
            serde_json::to_value(context)
                .map_err(|err| ShuttleError::Serialization(err.to_string()))
        }
        "shuttle_repo_status" => {
            let status = crate::context::repo_status(&runtime.cwd)?;
            serde_json::to_value(status).map_err(|err| ShuttleError::Serialization(err.to_string()))
        }
        "shuttle_repo_changed_files" => {
            let files = git(&runtime.cwd, ["diff", "--name-only"])?;
            Ok(json!({
                "files": files.lines().filter(|line| !line.trim().is_empty()).collect::<Vec<_>>()
            }))
        }
        "shuttle_repo_diff" => {
            let max_bytes = args
                .get("max_bytes")
                .and_then(Value::as_u64)
                .unwrap_or(60_000)
                .min(200_000) as usize;
            let path = args.get("path").and_then(Value::as_str);
            let diff = if let Some(path) = path {
                git_vec(&runtime.cwd, vec!["diff", "--", path])?
            } else {
                git(&runtime.cwd, ["diff"])?
            };
            let truncated = diff.len() > max_bytes;
            let diff = if truncated {
                diff.chars().take(max_bytes).collect::<String>()
            } else {
                diff
            };
            Ok(json!({ "diff": diff, "truncated": truncated }))
        }
        _ => Err(ShuttleError::Store(format!("unknown tool: {name}"))),
    }
}

fn with_repo_metadata(mut event: Event, runtime: &McpRuntime) -> Event {
    if let Ok(status) = crate::context::repo_status(&runtime.cwd) {
        let repo_id = crate::context::repo_id(&status);
        event.repo_id = Some(repo_id.clone());
        event.repo_path = Some(status.repo_path.clone());
        event.git_remote = status.git_remote.clone();
        event.branch = Some(status.branch.clone());
        event.commit = Some(status.commit.clone());
        event.repo_dirty = Some(status.dirty);
        if let Some(metadata) = event.metadata_json.as_object_mut() {
            metadata.insert("repo_id".to_owned(), json!(repo_id));
            metadata.insert("repo_path".to_owned(), json!(status.repo_path));
            metadata.insert("git_remote".to_owned(), json!(status.git_remote));
            metadata.insert("branch".to_owned(), json!(status.branch));
            metadata.insert("commit".to_owned(), json!(status.commit));
            metadata.insert("repo_dirty".to_owned(), json!(status.dirty));
            metadata.insert("dirty_files".to_owned(), json!(status.dirty_files));
        }
    }
    event
}

fn string_arg(args: &Value, name: &str) -> Result<String> {
    args.get(name)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| ShuttleError::Store(format!("missing string argument: {name}")))
}

fn string_list_arg(args: &Value, name: &str) -> Option<Vec<String>> {
    let value = args.get(name)?;
    if let Some(items) = value.as_array() {
        return Some(
            items
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect(),
        );
    }
    value.as_str().map(|items| {
        items
            .split(',')
            .map(|item| item.trim().to_owned())
            .collect()
    })
}

fn tools() -> Vec<Tool> {
    vec![
        tool(
            "remember",
            "Store a local Shuttle memory",
            event_output_schema(),
        ),
        tool(
            "recall",
            "Search local Shuttle memories",
            events_output_schema(),
        ),
        tool("inbox", "Read an agent inbox", events_output_schema()),
        tool("send", "Send a message to an agent", event_output_schema()),
        tool("history", "Read message history", events_output_schema()),
        tool(
            "context",
            "Read assembled repo context",
            context_output_schema(),
        ),
        tool("tasks", "List Shuttle task state", tasks_output_schema()),
        tool(
            "shuttle_memory_search",
            "Search local Shuttle memories",
            events_output_schema(),
        ),
        tool(
            "shuttle_memory_store",
            "Store a local Shuttle memory",
            event_output_schema(),
        ),
        tool(
            "shuttle_message_inbox",
            "Read an agent inbox",
            events_output_schema(),
        ),
        tool(
            "shuttle_message_history",
            "Read message history",
            events_output_schema(),
        ),
        tool(
            "shuttle_message_send",
            "Send a message to an agent",
            event_output_schema(),
        ),
        tool(
            "shuttle_task_list",
            "List Shuttle task state",
            tasks_output_schema(),
        ),
        tool(
            "shuttle_task_create",
            "Create a Shuttle task",
            event_output_schema(),
        ),
        tool(
            "shuttle_task_claim",
            "Claim a Shuttle task",
            event_output_schema(),
        ),
        tool(
            "shuttle_task_update",
            "Update a Shuttle task",
            event_output_schema(),
        ),
        tool(
            "shuttle_task_done",
            "Complete a Shuttle task",
            event_output_schema(),
        ),
        tool(
            "shuttle_handoff_request",
            "Request a Shuttle handoff",
            event_output_schema(),
        ),
        tool(
            "shuttle_handoff_list",
            "List Shuttle handoff state",
            handoffs_output_schema(),
        ),
        tool(
            "shuttle_handoff_accept",
            "Accept a Shuttle handoff",
            event_output_schema(),
        ),
        tool(
            "shuttle_handoff_done",
            "Complete a Shuttle handoff",
            event_output_schema(),
        ),
        tool(
            "shuttle_collab_start",
            "Start a shared Shuttle collaboration task and notify peer agents",
            collab_output_schema(),
        ),
        tool(
            "shuttle_collab_status",
            "Read shared Shuttle collaboration status",
            collab_output_schema(),
        ),
        tool(
            "shuttle_collab_nudge",
            "Send a collaboration message to another agent",
            collab_output_schema(),
        ),
        tool(
            "shuttle_collab_pass",
            "Update a task and request a handoff to another agent",
            collab_output_schema(),
        ),
        tool(
            "shuttle_repo_context",
            "Read assembled repo context",
            context_output_schema(),
        ),
        tool(
            "shuttle_repo_status",
            "Read Git repo status",
            repo_status_output_schema(),
        ),
        tool(
            "shuttle_repo_changed_files",
            "List changed files in the current Git repo",
            changed_files_output_schema(),
        ),
        tool(
            "shuttle_repo_diff",
            "Read the current Git diff, optionally for one path",
            diff_output_schema(),
        ),
    ]
}

fn tool(name: &'static str, description: &'static str, output_schema: Value) -> Tool {
    Tool {
        name,
        description,
        input_schema: json!({ "type": "object", "additionalProperties": true }),
        output_schema,
    }
}

fn structured_content(tool_name: Option<&str>, value: &Value) -> Value {
    match tool_name {
        Some(
            "remember"
            | "send"
            | "shuttle_memory_store"
            | "shuttle_message_send"
            | "shuttle_task_create"
            | "shuttle_task_claim"
            | "shuttle_task_update"
            | "shuttle_task_done"
            | "shuttle_handoff_request"
            | "shuttle_handoff_accept"
            | "shuttle_handoff_done",
        ) => json!({ "event": value }),
        Some(
            "shuttle_collab_start"
            | "shuttle_collab_status"
            | "shuttle_collab_nudge"
            | "shuttle_collab_pass",
        ) => json!({ "collab": value }),
        Some(
            "recall"
            | "inbox"
            | "history"
            | "shuttle_memory_search"
            | "shuttle_message_inbox"
            | "shuttle_message_history",
        ) => {
            json!({ "events": value })
        }
        Some("tasks" | "shuttle_task_list") => json!({ "tasks": value }),
        Some("shuttle_handoff_list") => json!({ "handoffs": value }),
        _ => value.clone(),
    }
}

fn event_output_schema() -> Value {
    object_schema(json!({ "event": event_schema() }), vec!["event"])
}

fn events_output_schema() -> Value {
    object_schema(
        json!({ "events": array_schema(event_schema()) }),
        vec!["events"],
    )
}

fn tasks_output_schema() -> Value {
    object_schema(
        json!({ "tasks": array_schema(task_schema()) }),
        vec!["tasks"],
    )
}

fn handoffs_output_schema() -> Value {
    object_schema(
        json!({ "handoffs": array_schema(handoff_schema()) }),
        vec!["handoffs"],
    )
}

fn collab_output_schema() -> Value {
    json!({ "type": "object", "additionalProperties": true })
}

fn context_output_schema() -> Value {
    object_schema(
        json!({
            "repo": string_schema("Repository path"),
            "branch": string_schema("Git branch"),
            "commit": string_schema("Git commit"),
            "git_remote": nullable_string_schema("Git remote URL"),
            "dirty": boolean_schema("Whether the repository has changes"),
            "dirty_files": array_schema(string_schema("Changed file path")),
            "open_tasks": array_schema(task_schema()),
            "claimed_tasks": array_schema(task_schema()),
            "recent_decisions": array_schema(event_schema()),
            "related_memories": array_schema(event_schema()),
            "recent_messages": array_schema(event_schema()),
            "pending_handoffs": array_schema(handoff_schema()),
            "recent_completed_handoffs": array_schema(handoff_schema()),
            "inbox": array_schema(event_schema()),
        }),
        vec![
            "repo",
            "branch",
            "commit",
            "dirty",
            "dirty_files",
            "open_tasks",
            "claimed_tasks",
            "recent_decisions",
            "related_memories",
            "recent_messages",
            "pending_handoffs",
            "recent_completed_handoffs",
            "inbox",
        ],
    )
}

fn repo_status_output_schema() -> Value {
    object_schema(
        json!({
            "repo_path": string_schema("Repository path"),
            "git_remote": nullable_string_schema("Git remote URL"),
            "branch": string_schema("Git branch"),
            "commit": string_schema("Git commit"),
            "dirty": boolean_schema("Whether the repository has changes"),
            "dirty_files": array_schema(string_schema("Changed file path")),
        }),
        vec!["repo_path", "branch", "commit", "dirty", "dirty_files"],
    )
}

fn changed_files_output_schema() -> Value {
    object_schema(
        json!({ "files": array_schema(string_schema("Changed file path")) }),
        vec!["files"],
    )
}

fn diff_output_schema() -> Value {
    object_schema(
        json!({
            "diff": string_schema("Git diff text"),
            "truncated": boolean_schema("Whether the diff was truncated"),
        }),
        vec!["diff", "truncated"],
    )
}

fn event_schema() -> Value {
    object_schema(
        json!({
            "id": string_schema("Event UUID"),
            "event_type": string_schema("Event type"),
            "workspace_id": string_schema("Workspace identifier"),
            "agent": string_schema("Agent identifier"),
            "session_id": string_schema("Session identifier"),
            "content": string_schema("Event content"),
            "tags": array_schema(string_schema("Event tag")),
            "metadata_json": json!({ "type": "object", "additionalProperties": true }),
            "created_at": string_schema("RFC3339 creation timestamp"),
        }),
        vec![
            "id",
            "event_type",
            "workspace_id",
            "agent",
            "session_id",
            "content",
            "tags",
            "metadata_json",
            "created_at",
        ],
    )
}

fn task_schema() -> Value {
    object_schema(
        json!({
            "id": string_schema("Task UUID"),
            "status": enum_schema("Task status", &["open", "claimed", "completed"]),
            "content": string_schema("Task content"),
            "created_by": string_schema("Creating agent"),
            "claimed_by": nullable_string_schema("Claiming agent"),
            "created_at": string_schema("RFC3339 creation timestamp"),
            "updated_at": string_schema("RFC3339 update timestamp"),
            "source_event_ids": array_schema(string_schema("Source event UUID")),
        }),
        vec![
            "id",
            "status",
            "content",
            "created_by",
            "created_at",
            "updated_at",
            "source_event_ids",
        ],
    )
}

fn handoff_schema() -> Value {
    object_schema(
        json!({
            "id": string_schema("Handoff UUID"),
            "status": enum_schema("Handoff status", &["pending", "accepted", "completed"]),
            "content": string_schema("Handoff content"),
            "from_agent": string_schema("Requesting agent"),
            "to_agent": string_schema("Receiving agent"),
            "accepted_by": nullable_string_schema("Accepting agent"),
            "created_at": string_schema("RFC3339 creation timestamp"),
            "updated_at": string_schema("RFC3339 update timestamp"),
            "source_event_ids": array_schema(string_schema("Source event UUID")),
        }),
        vec![
            "id",
            "status",
            "content",
            "from_agent",
            "to_agent",
            "created_at",
            "updated_at",
            "source_event_ids",
        ],
    )
}

fn object_schema(properties: Value, required: Vec<&str>) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": true,
    })
}

fn array_schema(items: Value) -> Value {
    json!({ "type": "array", "items": items })
}

fn string_schema(description: &str) -> Value {
    json!({ "type": "string", "description": description })
}

fn nullable_string_schema(description: &str) -> Value {
    json!({ "type": ["string", "null"], "description": description })
}

fn boolean_schema(description: &str) -> Value {
    json!({ "type": "boolean", "description": description })
}

fn enum_schema(description: &str, values: &[&str]) -> Value {
    json!({ "type": "string", "description": description, "enum": values })
}

fn ok(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error(id: Value, code: i32, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn git<const N: usize>(cwd: &PathBuf, args: [&str; N]) -> Result<String> {
    git_vec(cwd, args.to_vec())
}

fn git_vec(cwd: &PathBuf, args: Vec<&str>) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|err| ShuttleError::Store(format!("failed to run git: {err}")))?;

    if !output.status.success() {
        return Err(ShuttleError::Store(format!(
            "git command failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{EventFilter, EventStore, EventType};
    use std::fs;

    #[test]
    fn memory_store_tool_adds_repo_metadata() {
        let repo = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        init_git_repo(repo.path());
        fs::write(repo.path().join("dirty.txt"), "dirty").unwrap();
        let store = SqliteEventStore::open(data.path().join("shuttle.db")).unwrap();
        let runtime = McpRuntime {
            store: store.clone(),
            cwd: repo.path().to_path_buf(),
            workspace_id: "workspace".into(),
            agent: "codex".into(),
            session_id: "session".into(),
        };
        let request = Request {
            jsonrpc: Some("2.0".into()),
            id: Some(json!(1)),
            method: "tools/call".into(),
            params: json!({
                "name": "shuttle_memory_store",
                "arguments": { "content": "repo-aware memory" }
            }),
        };

        let response = futures_executor::block_on(handle_request(&runtime, request));
        assert!(response.get("error").is_none());
        let events = futures_executor::block_on(store.list(EventFilter {
            event_type: Some(EventType::Memory),
            ..EventFilter::default()
        }))
        .unwrap();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].repo_dirty, Some(true));
        assert_eq!(events[0].metadata_json["repo_dirty"], true);
        assert_eq!(events[0].metadata_json["dirty_files"], json!(["dirty.txt"]));
        assert!(events[0].repo_id.is_some());
        assert!(events[0].repo_path.is_some());
        assert!(events[0].branch.is_some());
        assert!(events[0].commit.is_some());
    }

    #[test]
    fn tools_list_includes_phase_two_tools() {
        let tools = tools();
        let names = tools.iter().map(|tool| tool.name).collect::<Vec<_>>();

        assert!(names.contains(&"shuttle_message_history"));
        assert!(names.contains(&"shuttle_task_update"));
        assert!(names.contains(&"shuttle_task_done"));
        assert!(names.contains(&"shuttle_handoff_request"));
        assert!(names.contains(&"shuttle_handoff_list"));
        assert!(names.contains(&"shuttle_handoff_accept"));
        assert!(names.contains(&"shuttle_handoff_done"));
        assert!(names.contains(&"shuttle_collab_start"));
        assert!(names.contains(&"shuttle_collab_status"));
        assert!(names.contains(&"shuttle_collab_nudge"));
        assert!(names.contains(&"shuttle_collab_pass"));
        assert!(tools
            .iter()
            .all(|tool| tool.output_schema["type"] == "object"));
    }

    #[test]
    fn task_and_handoff_tools_round_trip() {
        let repo = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        init_git_repo(repo.path());
        let store = SqliteEventStore::open(data.path().join("shuttle.db")).unwrap();
        let runtime = McpRuntime {
            store,
            cwd: repo.path().to_path_buf(),
            workspace_id: "workspace".into(),
            agent: "codex".into(),
            session_id: "session".into(),
        };

        let task_response = futures_executor::block_on(handle_request(
            &runtime,
            tool_request(
                "shuttle_task_create",
                json!({ "content": "ship task tools" }),
            ),
        ));
        let task_id = response_text_json(&task_response)["id"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(
            task_response["result"]["structuredContent"]["event"]["id"],
            task_id
        );
        futures_executor::block_on(handle_request(
            &runtime,
            tool_request("shuttle_task_claim", json!({ "id": task_id })),
        ));
        futures_executor::block_on(handle_request(
            &runtime,
            tool_request(
                "shuttle_task_update",
                json!({ "id": task_id, "content": "updated task tools" }),
            ),
        ));
        futures_executor::block_on(handle_request(
            &runtime,
            tool_request("shuttle_task_done", json!({ "id": task_id })),
        ));
        let task_list = futures_executor::block_on(handle_request(
            &runtime,
            tool_request("shuttle_task_list", json!({})),
        ));
        let task_json = response_text_json(&task_list);
        assert_eq!(task_json[0]["status"], "completed");
        assert_eq!(task_json[0]["content"], "updated task tools");
        assert_eq!(
            task_list["result"]["structuredContent"]["tasks"][0]["status"],
            "completed"
        );

        futures_executor::block_on(handle_request(
            &runtime,
            tool_request(
                "shuttle_message_send",
                json!({ "agent": "claude", "content": "review this" }),
            ),
        ));
        let history = futures_executor::block_on(handle_request(
            &runtime,
            tool_request("shuttle_message_history", json!({})),
        ));
        let history_json = response_text_json(&history);
        assert_eq!(history_json[0]["content"], "review this");

        let handoff_response = futures_executor::block_on(handle_request(
            &runtime,
            tool_request(
                "shuttle_handoff_request",
                json!({ "agent": "claude", "content": "continue this" }),
            ),
        ));
        let handoff_id = response_text_json(&handoff_response)["id"]
            .as_str()
            .unwrap()
            .to_owned();
        futures_executor::block_on(handle_request(
            &runtime,
            tool_request("shuttle_handoff_accept", json!({ "id": handoff_id })),
        ));
        let handoff_list = futures_executor::block_on(handle_request(
            &runtime,
            tool_request("shuttle_handoff_list", json!({})),
        ));
        let handoff_json = response_text_json(&handoff_list);
        assert_eq!(handoff_json[0]["status"], "accepted");
        assert_eq!(handoff_json[0]["to_agent"], "claude");
    }

    #[test]
    fn collab_tools_round_trip() {
        let repo = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        init_git_repo(repo.path());
        let store = SqliteEventStore::open(data.path().join("shuttle.db")).unwrap();
        let runtime = McpRuntime {
            store,
            cwd: repo.path().to_path_buf(),
            workspace_id: "workspace".into(),
            agent: "codex".into(),
            session_id: "session".into(),
        };

        let start = futures_executor::block_on(handle_request(
            &runtime,
            tool_request(
                "shuttle_collab_start",
                json!({ "content": "ship together", "agents": ["codex", "claude"] }),
            ),
        ));
        let start_json = response_text_json(&start);
        let task_id = start_json["task"]["id"].as_str().unwrap().to_owned();
        assert_eq!(
            start["result"]["structuredContent"]["collab"]["messages"][0]["metadata_json"]["to"],
            "claude"
        );

        futures_executor::block_on(handle_request(
            &runtime,
            tool_request(
                "shuttle_collab_nudge",
                json!({ "agent": "claude", "content": "please look" }),
            ),
        ));
        let pass = futures_executor::block_on(handle_request(
            &runtime,
            tool_request(
                "shuttle_collab_pass",
                json!({ "agent": "claude", "task_id": task_id, "note": "please continue" }),
            ),
        ));
        assert_eq!(
            pass["result"]["structuredContent"]["collab"]["handoff"]["metadata_json"]["to"],
            "claude"
        );

        let status = futures_executor::block_on(handle_request(
            &runtime,
            tool_request("shuttle_collab_status", json!({})),
        ));
        let status_json = response_text_json(&status);
        assert_eq!(status_json["tasks"].as_array().unwrap().len(), 1);
        assert_eq!(
            status["result"]["structuredContent"]["collab"]["pending_handoffs"][0]["status"],
            "pending"
        );
    }

    fn tool_request(name: &str, arguments: Value) -> Request {
        Request {
            jsonrpc: Some("2.0".into()),
            id: Some(json!(1)),
            method: "tools/call".into(),
            params: json!({ "name": name, "arguments": arguments }),
        }
    }

    fn response_text_json(response: &Value) -> Value {
        let text = response["result"]["content"][0]["text"].as_str().unwrap();
        serde_json::from_str(text).unwrap()
    }

    fn init_git_repo(path: &std::path::Path) {
        Command::new("git")
            .arg("init")
            .current_dir(path)
            .output()
            .unwrap();
        fs::write(path.join("README.md"), "repo").unwrap();
        Command::new("git")
            .args(["add", "README.md"])
            .current_dir(path)
            .output()
            .unwrap();
        Command::new("git")
            .args([
                "-c",
                "user.name=Shuttle Test",
                "-c",
                "user.email=shuttle@example.test",
                "commit",
                "-m",
                "initial",
            ])
            .current_dir(path)
            .output()
            .unwrap();
    }
}
