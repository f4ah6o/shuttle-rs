use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::process::Command;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use shuttle_core::{Event, EventStore, Result, ShuttleError};
use shuttle_store::SqliteEventStore;
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

pub fn serve_stdio(runtime: McpRuntime) -> Result<()> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines() {
        let line = line.map_err(|err| ShuttleError::Store(err.to_string()))?;
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Request>(&line) {
            Ok(request) => futures_executor::block_on(handle_request(&runtime, request)),
            Err(err) => json!({
                "jsonrpc": "2.0",
                "id": null,
                "error": { "code": -32700, "message": err.to_string() }
            }),
        };
        writeln!(stdout, "{response}").map_err(|err| ShuttleError::Store(err.to_string()))?;
        stdout
            .flush()
            .map_err(|err| ShuttleError::Store(err.to_string()))?;
    }

    Ok(())
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
        "shuttle.memory.search" => {
            let query = string_arg(&args, "query")?;
            let events = shuttle_memory::recall(&runtime.store, &query).await?;
            serde_json::to_value(events).map_err(|err| ShuttleError::Serialization(err.to_string()))
        }
        "shuttle.memory.store" => {
            let content = string_arg(&args, "content")?;
            let event = with_repo_metadata(
                shuttle_memory::new_memory(
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
        "shuttle.message.inbox" => {
            let agent = args
                .get("agent")
                .and_then(Value::as_str)
                .unwrap_or(&runtime.agent);
            let events = shuttle_message::inbox(&runtime.store, agent).await?;
            serde_json::to_value(events).map_err(|err| ShuttleError::Serialization(err.to_string()))
        }
        "shuttle.message.send" => {
            let to_agent = string_arg(&args, "agent")?;
            let content = string_arg(&args, "content")?;
            let event = with_repo_metadata(
                shuttle_message::new_message(
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
        "shuttle.task.list" => {
            let events = shuttle_task::list(&runtime.store).await?;
            serde_json::to_value(events).map_err(|err| ShuttleError::Serialization(err.to_string()))
        }
        "shuttle.task.create" => {
            let content = string_arg(&args, "content")?;
            let event = with_repo_metadata(
                shuttle_task::new_task(
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
            let event = with_repo_metadata(
                shuttle_task::new_claim(
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
        "shuttle.repo.context" => {
            let context = shuttle_context::assemble_context(
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
            let status = shuttle_context::repo_status(&runtime.cwd)?;
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
    if let Ok(status) = shuttle_context::repo_status(&runtime.cwd) {
        let repo_id = shuttle_context::repo_id(&status);
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
        tool("shuttle.memory.search", "Search local Shuttle memories"),
        tool("shuttle.memory.store", "Store a local Shuttle memory"),
        tool("shuttle.message.inbox", "Read an agent inbox"),
        tool("shuttle.message.send", "Send a message to an agent"),
        tool("shuttle.task.list", "List Shuttle task events"),
        tool("shuttle.task.create", "Create a Shuttle task"),
        tool("shuttle.task.claim", "Claim a Shuttle task"),
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
    use shuttle_core::{EventFilter, EventStore, EventType};
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
