use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::process::Command;

use futures_executor::block_on;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use shuttle_core::{EventStore, Result, ShuttleError};
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
            Ok(request) => handle_request(&runtime, request),
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

pub fn handle_request(runtime: &McpRuntime, request: Request) -> Value {
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
        "tools/call" => match call_tool(runtime, request.params) {
            Ok(value) => ok(
                id,
                json!({ "content": [{ "type": "text", "text": value.to_string() }] }),
            ),
            Err(err) => error(id, -32603, &err.to_string()),
        },
        _ => error(id, -32601, "method not found"),
    }
}

fn call_tool(runtime: &McpRuntime, params: Value) -> Result<Value> {
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
            let events = block_on(shuttle_memory::recall(&runtime.store, &query))?;
            serde_json::to_value(events).map_err(|err| ShuttleError::Serialization(err.to_string()))
        }
        "shuttle.memory.store" => {
            let content = string_arg(&args, "content")?;
            let event = shuttle_memory::new_memory(
                runtime.workspace_id.clone(),
                runtime.agent.clone(),
                runtime.session_id.clone(),
                content,
            );
            let event = block_on(runtime.store.append(event))?;
            serde_json::to_value(event).map_err(|err| ShuttleError::Serialization(err.to_string()))
        }
        "shuttle.message.inbox" => {
            let agent = args
                .get("agent")
                .and_then(Value::as_str)
                .unwrap_or(&runtime.agent);
            let events = block_on(shuttle_message::inbox(&runtime.store, agent))?;
            serde_json::to_value(events).map_err(|err| ShuttleError::Serialization(err.to_string()))
        }
        "shuttle.message.send" => {
            let to_agent = string_arg(&args, "agent")?;
            let content = string_arg(&args, "content")?;
            let event = shuttle_message::new_message(
                runtime.workspace_id.clone(),
                runtime.agent.clone(),
                runtime.session_id.clone(),
                to_agent,
                content,
            );
            let event = block_on(runtime.store.append(event))?;
            serde_json::to_value(event).map_err(|err| ShuttleError::Serialization(err.to_string()))
        }
        "shuttle.task.list" => {
            let events = block_on(shuttle_task::list(&runtime.store))?;
            serde_json::to_value(events).map_err(|err| ShuttleError::Serialization(err.to_string()))
        }
        "shuttle.task.create" => {
            let content = string_arg(&args, "content")?;
            let event = shuttle_task::new_task(
                runtime.workspace_id.clone(),
                runtime.agent.clone(),
                runtime.session_id.clone(),
                content,
            );
            let event = block_on(runtime.store.append(event))?;
            serde_json::to_value(event).map_err(|err| ShuttleError::Serialization(err.to_string()))
        }
        "shuttle.task.claim" => {
            let id = Uuid::parse_str(&string_arg(&args, "id")?)
                .map_err(|err| ShuttleError::Store(err.to_string()))?;
            let event = shuttle_task::new_claim(
                runtime.workspace_id.clone(),
                runtime.agent.clone(),
                runtime.session_id.clone(),
                id,
            );
            let event = block_on(runtime.store.append(event))?;
            serde_json::to_value(event).map_err(|err| ShuttleError::Serialization(err.to_string()))
        }
        "shuttle.repo.context" => {
            let context = block_on(shuttle_context::assemble_context(
                &runtime.store,
                &runtime.cwd,
                &runtime.workspace_id,
                &runtime.agent,
            ))?;
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
