use std::env;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use futures_executor::block_on;
use serde::Serialize;
use shuttle_core::{Event, EventStore};
use shuttle_store::SqliteEventStore;
use uuid::Uuid;

#[derive(Debug, Parser)]
#[command(name = "stl", about = "Local-first middleware for agent collaboration")]
struct Cli {
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Init,
    Send { agent: String, content: String },
    Inbox,
    History,
    Remember { content: String },
    Recall { query: String },
    Memories,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let env = RuntimeEnv::load()?;

    match cli.command {
        Command::Init => {
            fs::create_dir_all(&env.shuttle_dir)
                .with_context(|| format!("failed to create {}", env.shuttle_dir.display()))?;
            SqliteEventStore::open(&env.database_path)
                .with_context(|| format!("failed to initialize {}", env.database_path.display()))?;
            output(
                cli.json,
                &InitOutput {
                    database: env.database_path.display().to_string(),
                },
                || format!("initialized {}", env.database_path.display()),
            )?;
        }
        Command::Send { agent, content } => {
            let store = open_store(&env)?;
            let event = shuttle_message::new_message(
                env.workspace_id,
                env.agent,
                env.session_id,
                agent.clone(),
                content,
            );
            let event = block_on(store.append(event))?;
            output(cli.json, &event, || {
                format!("sent message to {agent}: {}", event.content)
            })?;
        }
        Command::Inbox => {
            let store = open_store(&env)?;
            let events = block_on(shuttle_message::inbox(&store, &env.agent))?;
            output_events(cli.json, &events, "inbox")?;
        }
        Command::History => {
            let store = open_store(&env)?;
            let events = block_on(shuttle_message::history(&store))?;
            output_events(cli.json, &events, "message history")?;
        }
        Command::Remember { content } => {
            let store = open_store(&env)?;
            let event =
                shuttle_memory::new_memory(env.workspace_id, env.agent, env.session_id, content);
            let event = block_on(store.append(event))?;
            output(cli.json, &event, || {
                format!("remembered: {}", event.content)
            })?;
        }
        Command::Recall { query } => {
            let store = open_store(&env)?;
            let events = block_on(shuttle_memory::recall(&store, &query))?;
            output_events(cli.json, &events, "recall")?;
        }
        Command::Memories => {
            let store = open_store(&env)?;
            let events = block_on(shuttle_memory::memories(&store))?;
            output_events(cli.json, &events, "memories")?;
        }
    }

    Ok(())
}

#[derive(Debug)]
struct RuntimeEnv {
    shuttle_dir: PathBuf,
    database_path: PathBuf,
    workspace_id: String,
    agent: String,
    session_id: String,
}

impl RuntimeEnv {
    fn load() -> Result<Self> {
        let cwd = env::current_dir().context("failed to read current directory")?;
        let shuttle_dir = cwd.join(".shuttle");
        let database_path = shuttle_dir.join("shuttle.db");
        let workspace_id = cwd
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("workspace")
            .to_owned();
        let agent = env::var("SHUTTLE_AGENT").unwrap_or_else(|_| "unknown".to_owned());
        let session_id =
            env::var("SHUTTLE_SESSION_ID").unwrap_or_else(|_| Uuid::new_v4().to_string());

        Ok(Self {
            shuttle_dir,
            database_path,
            workspace_id,
            agent,
            session_id,
        })
    }
}

#[derive(Debug, Serialize)]
struct InitOutput {
    database: String,
}

fn open_store(env: &RuntimeEnv) -> Result<SqliteEventStore> {
    fs::create_dir_all(&env.shuttle_dir)
        .with_context(|| format!("failed to create {}", env.shuttle_dir.display()))?;
    SqliteEventStore::open(&env.database_path)
        .with_context(|| format!("failed to open {}", env.database_path.display()))
}

fn output<T, F>(json: bool, value: &T, text: F) -> Result<()>
where
    T: Serialize,
    F: FnOnce() -> String,
{
    if json {
        println!("{}", serde_json::to_string_pretty(value)?);
    } else {
        println!("{}", text());
    }
    Ok(())
}

fn output_events(json: bool, events: &[Event], label: &str) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(events)?);
        return Ok(());
    }

    if events.is_empty() {
        println!("no {label}");
        return Ok(());
    }

    for event in events {
        let title = event.title.as_deref().unwrap_or(event.event_type.as_str());
        println!(
            "- [{}] {}: {}",
            event.created_at.format("%Y-%m-%d %H:%M:%S UTC"),
            title,
            event.content
        );
    }

    Ok(())
}
