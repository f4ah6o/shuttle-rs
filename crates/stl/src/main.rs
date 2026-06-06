use std::env;
use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use futures_executor::block_on;
use serde::Serialize;
use shuttle_core::{Event, EventStore, EventType, NewEvent};
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
    Send {
        agent: String,
        content: String,
    },
    Inbox,
    History,
    Remember {
        content: String,
    },
    Recall {
        query: String,
    },
    Memories,
    Decide {
        content: String,
    },
    Task {
        #[command(subcommand)]
        command: TaskCommand,
    },
    Handoff {
        agent: String,
        content: String,
    },
    Context,
    Mcp {
        #[command(subcommand)]
        command: McpCommand,
    },
    App {
        #[command(subcommand)]
        command: AppCommand,
    },
}

#[derive(Debug, Subcommand)]
enum TaskCommand {
    List,
    Create { content: String },
    Claim { id: Uuid },
}

#[derive(Debug, Subcommand)]
enum McpCommand {
    Serve,
}

#[derive(Debug, Subcommand)]
enum AppCommand {
    Serve {
        #[arg(long, default_value = "127.0.0.1:8787")]
        addr: SocketAddr,
    },
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
            let event = with_repo_metadata(
                shuttle_message::new_message(
                    env.workspace_id.clone(),
                    env.agent.clone(),
                    env.session_id.clone(),
                    agent.clone(),
                    content,
                ),
                &env,
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
            let event = with_repo_metadata(
                shuttle_memory::new_memory(
                    env.workspace_id.clone(),
                    env.agent.clone(),
                    env.session_id.clone(),
                    content,
                ),
                &env,
            );
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
        Command::Decide { content } => {
            let store = open_store(&env)?;
            let event = with_repo_metadata(
                Event::new(NewEvent {
                    event_type: EventType::Decision,
                    workspace_id: env.workspace_id.clone(),
                    repo_id: None,
                    repo_path: None,
                    git_remote: None,
                    bit_repo_id: None,
                    branch: None,
                    commit: None,
                    agent: env.agent.clone(),
                    session_id: env.session_id.clone(),
                    title: Some("decision".to_owned()),
                    content,
                    tags: Vec::new(),
                }),
                &env,
            );
            let event = block_on(store.append(event))?;
            output(cli.json, &event, || format!("decided: {}", event.content))?;
        }
        Command::Task { command } => {
            let store = open_store(&env)?;
            match command {
                TaskCommand::List => {
                    let events = block_on(shuttle_task::list(&store))?;
                    output_events(cli.json, &events, "tasks")?;
                }
                TaskCommand::Create { content } => {
                    let event = with_repo_metadata(
                        shuttle_task::new_task(
                            env.workspace_id.clone(),
                            env.agent.clone(),
                            env.session_id.clone(),
                            content,
                        ),
                        &env,
                    );
                    let event = block_on(store.append(event))?;
                    output(cli.json, &event, || format!("created task {}", event.id))?;
                }
                TaskCommand::Claim { id } => {
                    let event = with_repo_metadata(
                        shuttle_task::new_claim(
                            env.workspace_id.clone(),
                            env.agent.clone(),
                            env.session_id.clone(),
                            id,
                        ),
                        &env,
                    );
                    let event = block_on(store.append(event))?;
                    output(cli.json, &event, || format!("claimed task {id}"))?;
                }
            }
        }
        Command::Handoff { agent, content } => {
            let store = open_store(&env)?;
            let event = with_repo_metadata(
                Event::new(NewEvent {
                    event_type: EventType::Handoff,
                    workspace_id: env.workspace_id.clone(),
                    repo_id: None,
                    repo_path: None,
                    git_remote: None,
                    bit_repo_id: None,
                    branch: None,
                    commit: None,
                    agent: env.agent.clone(),
                    session_id: env.session_id.clone(),
                    title: Some("handoff".to_owned()),
                    content,
                    tags: vec![shuttle_message::recipient_tag(&agent)],
                }),
                &env,
            );
            let event = block_on(store.append(event))?;
            output(cli.json, &event, || {
                format!("handed off to {agent}: {}", event.content)
            })?;
        }
        Command::Context => {
            let store = open_store(&env)?;
            let context = block_on(shuttle_context::assemble_context(
                &store,
                &env.cwd,
                &env.workspace_id,
                &env.agent,
            ))?;
            output(cli.json, &context, || {
                format!(
                    "{} {} {} dirty={}",
                    context.repo, context.branch, context.commit, context.dirty
                )
            })?;
        }
        Command::Mcp { command } => match command {
            McpCommand::Serve => {
                let store = open_store(&env)?;
                shuttle_mcp::serve_stdio(shuttle_mcp::McpRuntime {
                    store,
                    cwd: env.cwd,
                    workspace_id: env.workspace_id,
                    agent: env.agent,
                    session_id: env.session_id,
                })?;
            }
        },
        Command::App { command } => match command {
            AppCommand::Serve { addr } => {
                let store = open_store(&env)?;
                println!("serving shuttle app at http://{addr}");
                let runtime = tokio::runtime::Runtime::new()?;
                runtime.block_on(shuttle_app::serve(
                    shuttle_app::AppRuntime {
                        store,
                        cwd: env.cwd,
                        workspace_id: env.workspace_id,
                        agent: env.agent,
                    },
                    addr,
                ))?;
            }
        },
    }

    Ok(())
}

#[derive(Debug)]
struct RuntimeEnv {
    cwd: PathBuf,
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
            cwd,
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

fn with_repo_metadata(mut event: Event, env: &RuntimeEnv) -> Event {
    if let Ok(status) = shuttle_context::repo_status(&env.cwd) {
        event.repo_path = Some(status.repo_path);
        event.git_remote = status.git_remote;
        event.branch = Some(status.branch);
        event.commit = Some(status.commit);
    }
    event
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
