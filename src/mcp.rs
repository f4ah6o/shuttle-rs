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
        "tools/call" => match call_tool(runtime, request.params).await {
            Ok(value) => ok(
                id,
                json!({ "content": [{ "type": "text", "text": value.to_string() }] }),
            ),
            Err(err) => error(id, -32603, &err.to_string()),
        },
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
        "shuttle.memory.search" | "recall" => {
            let query = string_arg(&args, "query")?;
            let events = crate::memory::recall(&runtime.store, &query).await?;
            serde_json::to_value(events).map_err(|err| ShuttleError::Serialization(err.to_string()))
        }
        "shuttle.memory.store" | "remember" => {
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
        "shuttle.message.inbox" | "inbox" => {
            let agent = args
                .get("agent")
                .and_then(Value::as_str)
                .unwrap_or(&runtime.agent);
            let events = crate::message::inbox(&runtime.store, agent).await?;
            serde_json::to_value(events).map_err(|err| ShuttleError::Serialization(err.to_string()))
        }
        "shuttle.message.history" | "history" => {
            let events = crate::message::history(&runtime.store).await?;
            serde_json::to_value(events).map_err(|err| ShuttleError::Serialization(err.to_string()))
        }
        "shuttle.message.send" | "send" => {
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
        "shuttle.task.list" | "tasks" => {
            let tasks =
                crate::task::tasks(&runtime.store, Some(&runtime.workspace_id), None).await?;
            serde_json::to_value(tasks).map_err(|err| ShuttleError::Serialization(err.to_string()))
        }
        "shuttle.task.create" => {
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
        "shuttle.task.claim" => {
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
        "shuttle.task.update" => {
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
        "shuttle.task.done" => {
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
        "shuttle.handoff.request" => {
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
        "shuttle.handoff.list" => {
            let handoffs =
                crate::task::handoffs(&runtime.store, Some(&runtime.workspace_id), None).await?;
            serde_json::to_value(handoffs)
                .map_err(|err| ShuttleError::Serialization(err.to_string()))
        }
        "shuttle.handoff.accept" => {
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
        "shuttle.handoff.done" => {
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
        "shuttle.repo.context" | "context" => {
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
        "shuttle.repo.status" => {
            let status = crate::context::repo_status(&runtime.cwd)?;
            serde_json::to_value(status).map_err(|err| ShuttleError::Serialization(err.to_string()))
        }
        "shuttle.repo.changed_files" => {
            let files = git(&runtime.cwd, ["diff", "--name-only"])?;
            Ok(json!({
                "files": files.lines().filter(|line| !line.trim().is_empty()).collect::<Vec<_>>()
            }))
        }
        "shuttle.repo.diff" => {
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

fn tools() -> Vec<Tool> {
    vec![
        tool("remember", "Store a local Shuttle memory"),
        tool("recall", "Search local Shuttle memories"),
        tool("inbox", "Read an agent inbox"),
        tool("send", "Send a message to an agent"),
        tool("history", "Read message history"),
        tool("context", "Read assembled repo context"),
        tool("tasks", "List Shuttle task state"),
        tool("shuttle.memory.search", "Search local Shuttle memories"),
        tool("shuttle.memory.store", "Store a local Shuttle memory"),
        tool("shuttle.message.inbox", "Read an agent inbox"),
        tool("shuttle.message.history", "Read message history"),
        tool("shuttle.message.send", "Send a message to an agent"),
        tool("shuttle.task.list", "List Shuttle task state"),
        tool("shuttle.task.create", "Create a Shuttle task"),
        tool("shuttle.task.claim", "Claim a Shuttle task"),
        tool("shuttle.task.update", "Update a Shuttle task"),
        tool("shuttle.task.done", "Complete a Shuttle task"),
        tool("shuttle.handoff.request", "Request a Shuttle handoff"),
        tool("shuttle.handoff.list", "List Shuttle handoff state"),
        tool("shuttle.handoff.accept", "Accept a Shuttle handoff"),
        tool("shuttle.handoff.done", "Complete a Shuttle handoff"),
        tool("shuttle.repo.context", "Read assembled repo context"),
        tool("shuttle.repo.status", "Read Git repo status"),
        tool(
            "shuttle.repo.changed_files",
            "List changed files in the current Git repo",
        ),
        tool(
            "shuttle.repo.diff",
            "Read the current Git diff, optionally for one path",
        ),
    ]
}

fn tool(name: &'static str, description: &'static str) -> Tool {
    Tool {
        name,
        description,
        input_schema: json!({ "type": "object", "additionalProperties": true }),
    }
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
                "name": "shuttle.memory.store",
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
        let names = tools()
            .into_iter()
            .map(|tool| tool.name)
            .collect::<Vec<_>>();

        assert!(names.contains(&"shuttle.message.history"));
        assert!(names.contains(&"shuttle.task.update"));
        assert!(names.contains(&"shuttle.task.done"));
        assert!(names.contains(&"shuttle.handoff.request"));
        assert!(names.contains(&"shuttle.handoff.list"));
        assert!(names.contains(&"shuttle.handoff.accept"));
        assert!(names.contains(&"shuttle.handoff.done"));
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
                "shuttle.task.create",
                json!({ "content": "ship task tools" }),
            ),
        ));
        let task_id = response_text_json(&task_response)["id"]
            .as_str()
            .unwrap()
            .to_owned();
        futures_executor::block_on(handle_request(
            &runtime,
            tool_request("shuttle.task.claim", json!({ "id": task_id })),
        ));
        futures_executor::block_on(handle_request(
            &runtime,
            tool_request(
                "shuttle.task.update",
                json!({ "id": task_id, "content": "updated task tools" }),
            ),
        ));
        futures_executor::block_on(handle_request(
            &runtime,
            tool_request("shuttle.task.done", json!({ "id": task_id })),
        ));
        let task_list = futures_executor::block_on(handle_request(
            &runtime,
            tool_request("shuttle.task.list", json!({})),
        ));
        let task_json = response_text_json(&task_list);
        assert_eq!(task_json[0]["status"], "completed");
        assert_eq!(task_json[0]["content"], "updated task tools");

        futures_executor::block_on(handle_request(
            &runtime,
            tool_request(
                "shuttle.message.send",
                json!({ "agent": "claude", "content": "review this" }),
            ),
        ));
        let history = futures_executor::block_on(handle_request(
            &runtime,
            tool_request("shuttle.message.history", json!({})),
        ));
        let history_json = response_text_json(&history);
        assert_eq!(history_json[0]["content"], "review this");

        let handoff_response = futures_executor::block_on(handle_request(
            &runtime,
            tool_request(
                "shuttle.handoff.request",
                json!({ "agent": "claude", "content": "continue this" }),
            ),
        ));
        let handoff_id = response_text_json(&handoff_response)["id"]
            .as_str()
            .unwrap()
            .to_owned();
        futures_executor::block_on(handle_request(
            &runtime,
            tool_request("shuttle.handoff.accept", json!({ "id": handoff_id })),
        ));
        let handoff_list = futures_executor::block_on(handle_request(
            &runtime,
            tool_request("shuttle.handoff.list", json!({})),
        ));
        let handoff_json = response_text_json(&handoff_list);
        assert_eq!(handoff_json[0]["status"], "accepted");
        assert_eq!(handoff_json[0]["to_agent"], "claude");
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
