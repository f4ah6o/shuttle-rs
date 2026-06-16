use std::collections::HashSet;
use std::env;
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command as ProcessCommand};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand, ValueEnum};
use futures_executor::block_on;
use serde::{Deserialize, Serialize};
use serde_json::json;
use shuttle_rs::core::{Event, EventFilter, EventStore, EventType};
use shuttle_rs::store::SqliteEventStore;
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
    #[command(name = "version")]
    ShowVersion,
    Init,
    Send {
        #[arg(long = "from")]
        from_agent: Option<String>,
        agent: String,
        content: String,
    },
    Inbox {
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        watch: bool,
        #[arg(long, default_value_t = 2)]
        interval: u64,
    },
    History,
    Identity {
        #[command(subcommand)]
        command: IdentityCommand,
    },
    Remember {
        content: Option<String>,
        #[arg(long)]
        from_message: Option<Uuid>,
    },
    Recall {
        query: String,
        #[arg(long = "type")]
        kind: Option<MemoryKindArg>,
    },
    Memories,
    Decide {
        content: Option<String>,
        #[arg(long)]
        from_message: Option<Uuid>,
    },
    Observe {
        content: Option<String>,
        #[arg(long)]
        from_message: Option<Uuid>,
    },
    Pattern {
        content: Option<String>,
        #[arg(long)]
        from_message: Option<Uuid>,
    },
    Fact {
        content: Option<String>,
        #[arg(long)]
        from_message: Option<Uuid>,
    },
    Bug {
        content: Option<String>,
        #[arg(long)]
        from_message: Option<Uuid>,
    },
    Task {
        #[command(subcommand)]
        command: TaskCommand,
    },
    Handoff {
        #[command(subcommand)]
        command: HandoffCommand,
    },
    Mesh {
        #[command(subcommand)]
        command: MeshCommand,
    },
    Context {
        #[arg(long)]
        repo: bool,
        #[arg(long)]
        branch: bool,
    },
    App {
        #[command(subcommand)]
        command: AppCommand,
    },
    Skill {
        #[command(subcommand)]
        command: SkillCommand,
    },
    /// Project-aware coding adapter routing (LoRA/PEFT selection and export).
    Adapter {
        #[command(subcommand)]
        command: AdapterCommand,
    },
}

#[derive(Debug, Subcommand)]
enum TaskCommand {
    List,
    Create {
        content: Option<String>,
        #[arg(long)]
        from_message: Option<Uuid>,
    },
    Claim {
        id: Uuid,
    },
    Update {
        id: Uuid,
        content: String,
    },
    Done {
        id: Uuid,
    },
}

#[derive(Debug, Subcommand)]
enum HandoffCommand {
    Request {
        agent: String,
        content: Option<String>,
        #[arg(long)]
        from_message: Option<Uuid>,
    },
    List,
    Accept {
        id: Uuid,
    },
    Done {
        id: Uuid,
    },
}

#[derive(Debug, Subcommand)]
enum MeshCommand {
    Export { path: PathBuf },
    Import { path: PathBuf },
    Sync { peer_database: PathBuf },
}

#[derive(Debug, Subcommand)]
enum IdentityCommand {
    Current,
    Set { agent: String },
}

#[derive(Debug, Subcommand)]
enum AdapterCommand {
    /// Register (or replace) an adapter in the local registry.
    Register {
        #[arg(long)]
        name: String,
        #[arg(long = "base-model")]
        base_model: String,
        #[arg(long)]
        path: String,
        #[arg(long)]
        id: Option<String>,
        /// Repeatable tag describing the adapter (e.g. --tag rust --tag cli).
        #[arg(long = "tag")]
        tags: Vec<String>,
        #[arg(long)]
        description: Option<String>,
        /// Externally produced embedding as a JSON array of floats.
        #[arg(long)]
        embedding: Option<String>,
    },
    /// List registered adapters.
    List,
    /// Build (and cache) the project embedding from repo context and event log.
    Index,
    /// Select adapters for the current project by similarity.
    Select,
    /// Produce a deterministic adapter merge plan.
    Merge {
        #[arg(long = "top-k", default_value_t = 3)]
        top_k: usize,
        #[arg(long = "min-score", default_value_t = 0.0)]
        min_score: f32,
    },
    /// Export a runtime manifest for external inference engines.
    Export {
        #[arg(long = "top-k", default_value_t = 3)]
        top_k: usize,
        #[arg(long = "min-score", default_value_t = 0.0)]
        min_score: f32,
        #[arg(long, default_value = "json")]
        format: String,
    },
    /// Generate an adapter from project context via an external doc-to-lora runner.
    Doc2lora {
        /// Name (and default registry id) for the generated adapter.
        #[arg(long)]
        name: String,
        /// Base model the generated adapter targets.
        #[arg(long = "base-model")]
        base_model: String,
        /// Directory the runner writes the adapter (and context document) into.
        #[arg(long = "out-dir")]
        out_dir: PathBuf,
        /// Runner program override (defaults to $SHUTTLE_DOC2LORA_RUNNER, then `doc2lora`).
        #[arg(long)]
        runner: Option<String>,
        /// Repeatable tag recorded on the generated adapter.
        #[arg(long = "tag")]
        tags: Vec<String>,
        /// Optional focus query that biases and annotates the context document.
        #[arg(long)]
        focus: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum AppCommand {
    Serve {
        #[arg(long, default_value = "127.0.0.1:8787")]
        addr: SocketAddr,
        #[arg(long)]
        public_url: Option<String>,
    },
    Tunnel {
        #[arg(long, default_value = "127.0.0.1:8787")]
        addr: SocketAddr,
        #[arg(long)]
        public_url: String,
        #[arg(long, default_value = "CLOUDFLARE_TUNNEL_TOKEN")]
        cloudflare_token_env: String,
        #[arg(long, default_value = "cloudflared")]
        cloudflared: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum SkillCommand {
    Install { target: SkillTarget },
    Print { target: SkillTarget },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum SkillTarget {
    Codex,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum MemoryKindArg {
    Memory,
    Decision,
    Observation,
    Pattern,
    Fact,
    Bug,
    Handoff,
}

impl MemoryKindArg {
    fn event_type(self) -> EventType {
        match self {
            Self::Memory => EventType::Memory,
            Self::Decision => EventType::Decision,
            Self::Observation => EventType::Observation,
            Self::Pattern => EventType::Pattern,
            Self::Fact => EventType::Fact,
            Self::Bug => EventType::Bug,
            Self::Handoff => EventType::Handoff,
        }
    }
}

fn main() -> Result<()> {
    let _telemetry = shuttle_rs::telemetry::init("stl");
    let cli = Cli::parse();
    let command_name = cli.command.name();
    let _command_span =
        tracing::info_span!("stl.command", command = command_name, json = cli.json).entered();
    if matches!(cli.command, Command::ShowVersion) {
        output(
            cli.json,
            &VersionOutput {
                binary: "stl",
                version: env!("CARGO_PKG_VERSION"),
            },
            || env!("CARGO_PKG_VERSION").to_owned(),
        )?;
        return Ok(());
    }

    let env = RuntimeEnv::load()?;

    match cli.command {
        Command::ShowVersion => unreachable!("version exits before runtime environment loading"),
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
        Command::Send {
            from_agent,
            agent,
            content,
        } => {
            let store = open_store(&env)?;
            let sender = from_agent.unwrap_or_else(|| env.agent.clone());
            let event = with_repo_metadata(
                shuttle_rs::message::new_message(
                    env.workspace_id.clone(),
                    sender,
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
        Command::Inbox {
            agent,
            watch,
            interval,
        } => {
            let store = open_store(&env)?;
            let agent = agent.unwrap_or_else(|| env.agent.clone());
            if watch {
                watch_inbox(cli.json, &store, &agent, interval)?;
            } else {
                let events = block_on(shuttle_rs::message::inbox(&store, &agent))?;
                output_events(cli.json, &events, "inbox")?;
            }
        }
        Command::History => {
            let store = open_store(&env)?;
            let events = block_on(shuttle_rs::message::history(&store))?;
            output_events(cli.json, &events, "message history")?;
        }
        Command::Identity { command } => match command {
            IdentityCommand::Current => {
                let identity = current_identity(&env)?;
                output(cli.json, &identity, || {
                    format!("{} ({})", identity.agent, identity.source)
                })?;
            }
            IdentityCommand::Set { agent } => {
                set_persisted_agent(&env.shuttle_dir, &agent)?;
                let identity = IdentityOutput {
                    agent,
                    source: "repo".to_owned(),
                };
                output(cli.json, &identity, || {
                    format!("set repo agent identity to {}", identity.agent)
                })?;
            }
        },
        Command::Remember {
            content,
            from_message,
        } => {
            let store = open_store(&env)?;
            let source = resolve_content(&store, content, from_message)?;
            let event = with_repo_metadata(
                with_source_message_metadata(
                    shuttle_rs::memory::new_memory(
                        env.workspace_id.clone(),
                        env.agent.clone(),
                        env.session_id.clone(),
                        source.content,
                    ),
                    source.message_id,
                ),
                &env,
            );
            let event = block_on(store.append(event))?;
            output(cli.json, &event, || {
                format!("remembered: {}", event.content)
            })?;
        }
        Command::Recall { query, kind } => {
            let store = open_store(&env)?;
            let status = shuttle_rs::context::repo_status(&env.cwd).ok();
            let repo_id = status.as_ref().map(shuttle_rs::context::repo_id);
            let branch = status.as_ref().map(|status| status.branch.as_str());
            let results = block_on(shuttle_rs::memory::ranked_recall(
                &store,
                &query,
                kind.map(MemoryKindArg::event_type),
                Some(&env.workspace_id),
                repo_id.as_deref(),
                branch,
            ))?;
            output_recall(cli.json, &results)?;
        }
        Command::Memories => {
            let store = open_store(&env)?;
            let events = block_on(shuttle_rs::memory::memories(&store))?;
            output_events(cli.json, &events, "memories")?;
        }
        Command::Decide {
            content,
            from_message,
        } => {
            let store = open_store(&env)?;
            let source = resolve_content(&store, content, from_message)?;
            let event = append_typed_memory(
                &store,
                &env,
                EventType::Decision,
                source.content,
                source.message_id,
            )?;
            output(cli.json, &event, || format!("decided: {}", event.content))?;
        }
        Command::Observe {
            content,
            from_message,
        } => {
            let store = open_store(&env)?;
            let source = resolve_content(&store, content, from_message)?;
            let event = append_typed_memory(
                &store,
                &env,
                EventType::Observation,
                source.content,
                source.message_id,
            )?;
            output(cli.json, &event, || format!("observed: {}", event.content))?;
        }
        Command::Pattern {
            content,
            from_message,
        } => {
            let store = open_store(&env)?;
            let source = resolve_content(&store, content, from_message)?;
            let event = append_typed_memory(
                &store,
                &env,
                EventType::Pattern,
                source.content,
                source.message_id,
            )?;
            output(cli.json, &event, || {
                format!("recorded pattern: {}", event.content)
            })?;
        }
        Command::Fact {
            content,
            from_message,
        } => {
            let store = open_store(&env)?;
            let source = resolve_content(&store, content, from_message)?;
            let event = append_typed_memory(
                &store,
                &env,
                EventType::Fact,
                source.content,
                source.message_id,
            )?;
            output(cli.json, &event, || {
                format!("recorded fact: {}", event.content)
            })?;
        }
        Command::Bug {
            content,
            from_message,
        } => {
            let store = open_store(&env)?;
            let source = resolve_content(&store, content, from_message)?;
            let event = append_typed_memory(
                &store,
                &env,
                EventType::Bug,
                source.content,
                source.message_id,
            )?;
            output(cli.json, &event, || {
                format!("recorded bug: {}", event.content)
            })?;
        }
        Command::Task { command } => {
            let store = open_store(&env)?;
            match command {
                TaskCommand::List => {
                    let tasks = block_on(shuttle_rs::task::tasks(
                        &store,
                        Some(&env.workspace_id),
                        None,
                    ))?;
                    output_tasks(cli.json, &tasks)?;
                }
                TaskCommand::Create {
                    content,
                    from_message,
                } => {
                    let source = resolve_content(&store, content, from_message)?;
                    let event = with_repo_metadata(
                        with_source_message_metadata(
                            shuttle_rs::task::new_task(
                                env.workspace_id.clone(),
                                env.agent.clone(),
                                env.session_id.clone(),
                                source.content,
                            ),
                            source.message_id,
                        ),
                        &env,
                    );
                    let event = block_on(store.append(event))?;
                    output(cli.json, &event, || format!("created task {}", event.id))?;
                }
                TaskCommand::Claim { id } => {
                    block_on(shuttle_rs::task::ensure_task_exists(
                        &store,
                        &env.workspace_id,
                        id,
                    ))?;
                    let event = with_repo_metadata(
                        shuttle_rs::task::new_claim(
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
                TaskCommand::Update { id, content } => {
                    block_on(shuttle_rs::task::ensure_task_exists(
                        &store,
                        &env.workspace_id,
                        id,
                    ))?;
                    let event = with_repo_metadata(
                        shuttle_rs::task::new_task_update(
                            env.workspace_id.clone(),
                            env.agent.clone(),
                            env.session_id.clone(),
                            id,
                            content,
                        ),
                        &env,
                    );
                    let event = block_on(store.append(event))?;
                    output(cli.json, &event, || format!("updated task {id}"))?;
                }
                TaskCommand::Done { id } => {
                    block_on(shuttle_rs::task::ensure_task_exists(
                        &store,
                        &env.workspace_id,
                        id,
                    ))?;
                    let event = with_repo_metadata(
                        shuttle_rs::task::new_task_done(
                            env.workspace_id.clone(),
                            env.agent.clone(),
                            env.session_id.clone(),
                            id,
                        ),
                        &env,
                    );
                    let event = block_on(store.append(event))?;
                    output(cli.json, &event, || format!("completed task {id}"))?;
                }
            }
        }
        Command::Handoff { command } => {
            let store = open_store(&env)?;
            match command {
                HandoffCommand::Request {
                    agent,
                    content,
                    from_message,
                } => {
                    let source = resolve_content(&store, content, from_message)?;
                    let event = with_repo_metadata(
                        with_source_message_metadata(
                            shuttle_rs::task::new_handoff(
                                env.workspace_id.clone(),
                                env.agent.clone(),
                                env.session_id.clone(),
                                agent.clone(),
                                source.content,
                            ),
                            source.message_id,
                        ),
                        &env,
                    );
                    let event = block_on(store.append(event))?;
                    output(cli.json, &event, || {
                        format!("requested handoff to {agent}: {}", event.content)
                    })?;
                }
                HandoffCommand::List => {
                    let handoffs = block_on(shuttle_rs::task::handoffs(
                        &store,
                        Some(&env.workspace_id),
                        None,
                    ))?;
                    output_handoffs(cli.json, &handoffs)?;
                }
                HandoffCommand::Accept { id } => {
                    block_on(shuttle_rs::task::ensure_handoff_exists(
                        &store,
                        &env.workspace_id,
                        id,
                    ))?;
                    let event = with_repo_metadata(
                        shuttle_rs::task::new_handoff_accept(
                            env.workspace_id.clone(),
                            env.agent.clone(),
                            env.session_id.clone(),
                            id,
                        ),
                        &env,
                    );
                    let event = block_on(store.append(event))?;
                    output(cli.json, &event, || format!("accepted handoff {id}"))?;
                }
                HandoffCommand::Done { id } => {
                    block_on(shuttle_rs::task::ensure_handoff_exists(
                        &store,
                        &env.workspace_id,
                        id,
                    ))?;
                    let event = with_repo_metadata(
                        shuttle_rs::task::new_handoff_done(
                            env.workspace_id.clone(),
                            env.agent.clone(),
                            env.session_id.clone(),
                            id,
                        ),
                        &env,
                    );
                    let event = block_on(store.append(event))?;
                    output(cli.json, &event, || format!("completed handoff {id}"))?;
                }
            }
        }
        Command::Mesh { command } => {
            let store = open_store(&env)?;
            match command {
                MeshCommand::Export { path } => {
                    let archive = block_on(shuttle_rs::mesh::export_archive(&store))?;
                    shuttle_rs::mesh::write_archive(&path, &archive)?;
                    output(
                        cli.json,
                        &MeshExportOutput {
                            path: path.display().to_string(),
                            event_count: archive.event_count,
                            exported_at: archive.exported_at,
                        },
                        || {
                            format!(
                                "exported {} event(s) to {}",
                                archive.event_count,
                                path.display()
                            )
                        },
                    )?;
                }
                MeshCommand::Import { path } => {
                    let archive = shuttle_rs::mesh::read_archive(&path)?;
                    let report = block_on(shuttle_rs::mesh::import_archive_into_workspace(
                        &store,
                        archive,
                        &env.workspace_id,
                    ))?;
                    output(cli.json, &report, || {
                        format!(
                            "imported {} event(s) from {} ({} duplicate)",
                            report.imported,
                            path.display(),
                            report.skipped_duplicates
                        )
                    })?;
                }
                MeshCommand::Sync { peer_database } => {
                    let peer = open_peer_store(&peer_database)?;
                    let peer_workspace_id = load_peer_workspace_id(&peer_database);
                    let report = block_on(shuttle_rs::mesh::sync_bidirectional_into_workspaces(
                        &store,
                        &env.workspace_id,
                        &peer,
                        peer_workspace_id.as_deref(),
                    ))?;
                    output(cli.json, &report, || {
                        format!(
                            "synced with {}: local imported {}, peer imported {}, {} duplicate",
                            peer_database.display(),
                            report.local_imported,
                            report.peer_imported,
                            report.skipped_duplicates
                        )
                    })?;
                }
            }
        }
        Command::Context { repo, branch } => {
            if repo && branch {
                anyhow::bail!("--repo and --branch cannot be used together");
            }
            let store = open_store(&env)?;
            let context = block_on(shuttle_rs::context::assemble_context(
                &store,
                &env.cwd,
                &env.workspace_id,
                &env.agent,
            ))?;
            output(cli.json, &context, || {
                if repo {
                    context.repo.clone()
                } else if branch {
                    context.branch.clone()
                } else {
                    format_context(&context)
                }
            })?;
        }
        Command::App { command } => match command {
            AppCommand::Serve { addr, public_url } => {
                let store = open_store(&env)?;
                println!("serving shuttle app at http://{addr}");
                let oauth = app_oauth(&env, public_url)?;
                let runtime = tokio::runtime::Runtime::new()?;
                runtime.block_on(shuttle_rs::app::serve(
                    shuttle_rs::app::AppRuntime {
                        store,
                        cwd: env.cwd,
                        workspace_id: env.workspace_id,
                        agent: env.agent,
                        session_id: env.session_id,
                        oauth,
                    },
                    addr,
                ))?;
            }
            AppCommand::Tunnel {
                addr,
                public_url,
                cloudflare_token_env,
                cloudflared,
            } => {
                let store = open_store(&env)?;
                let public_url = shuttle_rs::oauth::OAuthConfig::normalize_public_url(public_url);
                let oauth = app_oauth(&env, Some(public_url.clone()))?;
                let token = env::var(&cloudflare_token_env).with_context(|| {
                    format!("failed to read Cloudflare tunnel token from {cloudflare_token_env}")
                })?;
                if token.trim().is_empty() {
                    anyhow::bail!("Cloudflare tunnel token environment variable is empty");
                }
                let mut tunnel = start_cloudflared(&cloudflared, &token)?;
                println!("serving shuttle app at http://{addr} through {public_url}");
                println!("configure remote MCP clients with {public_url}/mcp");
                let runtime = tokio::runtime::Runtime::new()?;
                let result = runtime.block_on(shuttle_rs::app::serve(
                    shuttle_rs::app::AppRuntime {
                        store,
                        cwd: env.cwd,
                        workspace_id: env.workspace_id,
                        agent: env.agent,
                        session_id: env.session_id,
                        oauth,
                    },
                    addr,
                ));
                stop_child(&mut tunnel);
                result?;
            }
        },
        Command::Skill { command } => match command {
            SkillCommand::Install { target } => {
                let install = install_skill(target)?;
                output(cli.json, &install, || {
                    format!("installed {} skill at {}", target.as_str(), install.path)
                })?;
            }
            SkillCommand::Print { target } => {
                let skill = skill_content(target);
                if cli.json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&SkillPrintOutput {
                            target: target.as_str().to_owned(),
                            content: skill,
                        })?
                    );
                } else {
                    print!("{skill}");
                }
            }
        },
        Command::Adapter { command } => {
            let store = open_store(&env)?;
            match command {
                AdapterCommand::Register {
                    name,
                    base_model,
                    path,
                    id,
                    tags,
                    description,
                    embedding,
                } => {
                    let embedding = match embedding {
                        Some(raw) => Some(
                            serde_json::from_str::<Vec<f32>>(&raw)
                                .context("--embedding must be a JSON array of numbers")?,
                        ),
                        None => None,
                    };
                    let record = shuttle_rs::adapter::register_adapter(
                        &store,
                        shuttle_rs::adapter::RegisterInput {
                            id,
                            name,
                            base_model,
                            path,
                            tags,
                            description,
                            embedding,
                        },
                    )?;
                    let summary = AdapterSummary::from(&record);
                    output(cli.json, &summary, || {
                        format!("registered adapter '{}' ({})", summary.name, summary.id)
                    })?;
                }
                AdapterCommand::List => {
                    let adapters = store.list_adapters()?;
                    let summaries: Vec<AdapterSummary> =
                        adapters.iter().map(AdapterSummary::from).collect();
                    output(cli.json, &summaries, || {
                        if summaries.is_empty() {
                            "no adapters registered".to_owned()
                        } else {
                            summaries
                                .iter()
                                .map(|adapter| {
                                    format!(
                                        "{} -> {} ({})",
                                        adapter.name, adapter.path, adapter.base_model
                                    )
                                })
                                .collect::<Vec<_>>()
                                .join("\n")
                        }
                    })?;
                }
                AdapterCommand::Index => {
                    let embedding = block_on(shuttle_rs::adapter::index_project(
                        &store,
                        &env.cwd,
                        &env.workspace_id,
                    ))?;
                    let summary = AdapterIndexOutput::from(&embedding);
                    output(cli.json, &summary, || {
                        format!(
                            "indexed {} ({}) on {} as project type '{}'",
                            summary.repo, &summary.commit, summary.branch, summary.project_type
                        )
                    })?;
                }
                AdapterCommand::Select => {
                    let selection = block_on(shuttle_rs::adapter::select_for_project(
                        &store,
                        &env.cwd,
                        &env.workspace_id,
                    ))?;
                    output(cli.json, &selection.result, || {
                        format!(
                            "project type '{}', {} adapter(s) scored",
                            selection.result.project_type,
                            selection.result.adapters.len()
                        )
                    })?;
                }
                AdapterCommand::Merge { top_k, min_score } => {
                    let selection = block_on(shuttle_rs::adapter::select_for_project(
                        &store,
                        &env.cwd,
                        &env.workspace_id,
                    ))?;
                    let plan = shuttle_rs::adapter::merge_plan(
                        &selection.result.adapters,
                        &selection.adapters,
                        top_k,
                        min_score,
                    );
                    output(cli.json, &plan, || {
                        format!("merge plan with {} adapter(s)", plan.adapters.len())
                    })?;
                }
                AdapterCommand::Export {
                    top_k,
                    min_score,
                    format,
                } => {
                    let format = format
                        .parse::<shuttle_rs::adapter::ExportFormat>()
                        .map_err(|err| anyhow::anyhow!(err))?;
                    if !format.is_supported() {
                        anyhow::bail!(
                            "export format '{format}' is not supported yet (Phase 1 supports 'json')"
                        );
                    }
                    let selection = block_on(shuttle_rs::adapter::select_for_project(
                        &store,
                        &env.cwd,
                        &env.workspace_id,
                    ))?;
                    let plan = shuttle_rs::adapter::merge_plan(
                        &selection.result.adapters,
                        &selection.adapters,
                        top_k,
                        min_score,
                    );
                    let manifest = shuttle_rs::adapter::export_manifest(&plan, &selection.adapters);
                    output(cli.json, &manifest, || {
                        format!(
                            "exported manifest with {} adapter(s)",
                            manifest.adapters.len()
                        )
                    })?;
                }
                AdapterCommand::Doc2lora {
                    name,
                    base_model,
                    out_dir,
                    runner,
                    tags,
                    focus,
                } => {
                    let input = shuttle_rs::adapter::Doc2LoraInput {
                        name,
                        base_model,
                        out_dir,
                        runner,
                        tags,
                        focus,
                    };
                    let generator = shuttle_rs::adapter::CommandGenerator::from_input(&input);
                    let outcome = block_on(shuttle_rs::adapter::run_doc2lora(
                        &store,
                        &generator,
                        &env.cwd,
                        &env.workspace_id,
                        &input,
                    ))?;
                    output(cli.json, &outcome, || {
                        format!(
                            "generated and registered adapter '{}' -> {} (base {})",
                            outcome.record.name, outcome.record.path, outcome.record.base_model
                        )
                    })?;
                }
            }
        }
    }

    Ok(())
}

impl Command {
    fn name(&self) -> &'static str {
        match self {
            Self::ShowVersion => "version",
            Self::Init => "init",
            Self::Send { .. } => "send",
            Self::Inbox { .. } => "inbox",
            Self::History => "history",
            Self::Identity { .. } => "identity",
            Self::Remember { .. } => "remember",
            Self::Recall { .. } => "recall",
            Self::Memories => "memories",
            Self::Decide { .. } => "decide",
            Self::Observe { .. } => "observe",
            Self::Pattern { .. } => "pattern",
            Self::Fact { .. } => "fact",
            Self::Bug { .. } => "bug",
            Self::Task { .. } => "task",
            Self::Handoff { .. } => "handoff",
            Self::Mesh { .. } => "mesh",
            Self::Context { .. } => "context",
            Self::App { .. } => "app",
            Self::Skill { .. } => "skill",
            Self::Adapter { .. } => "adapter",
        }
    }
}

#[derive(Debug, Serialize)]
struct AdapterSummary {
    id: String,
    name: String,
    base_model: String,
    path: String,
    tags: Vec<String>,
}

impl From<&shuttle_rs::adapter::AdapterRecord> for AdapterSummary {
    fn from(record: &shuttle_rs::adapter::AdapterRecord) -> Self {
        let tags = record
            .metadata
            .get("tags")
            .and_then(|value| serde_json::from_value::<Vec<String>>(value.clone()).ok())
            .unwrap_or_default();
        Self {
            id: record.id.clone(),
            name: record.name.clone(),
            base_model: record.base_model.clone(),
            path: record.path.clone(),
            tags,
        }
    }
}

#[derive(Debug, Serialize)]
struct AdapterIndexOutput {
    repo: String,
    repo_hash: String,
    branch: String,
    commit: String,
    project_type: String,
    dim: usize,
}

impl From<&shuttle_rs::adapter::ProjectEmbedding> for AdapterIndexOutput {
    fn from(embedding: &shuttle_rs::adapter::ProjectEmbedding) -> Self {
        Self {
            repo: embedding.repo.clone(),
            repo_hash: embedding.repo_hash.clone(),
            branch: embedding.branch.clone(),
            commit: embedding.commit.clone(),
            project_type: embedding.project_type.clone(),
            dim: embedding.vector.len(),
        }
    }
}

#[derive(Debug)]
struct RuntimeEnv {
    cwd: PathBuf,
    shuttle_dir: PathBuf,
    database_path: PathBuf,
    workspace_id: String,
    agent: String,
    agent_source: String,
    session_id: String,
}

#[derive(Debug, Serialize)]
struct VersionOutput {
    binary: &'static str,
    version: &'static str,
}

impl RuntimeEnv {
    fn load() -> Result<Self> {
        let cwd = env::current_dir().context("failed to read current directory")?;
        let root = repo_root(&cwd).unwrap_or_else(|| cwd.clone());
        let shuttle_dir = root.join(".shuttle");
        let database_path = shuttle_dir.join("shuttle.db");
        let workspace_id = load_or_create_workspace_id(&shuttle_dir, &root)?;
        let (agent, agent_source) = load_agent(&shuttle_dir);
        let session_id = load_or_create_session_id(&shuttle_dir)?;

        Ok(Self {
            cwd,
            shuttle_dir,
            database_path,
            workspace_id,
            agent,
            agent_source,
            session_id,
        })
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct WorkspaceFile {
    workspace_id: String,
    repo_path: String,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
struct InitOutput {
    database: String,
}

#[derive(Debug, Serialize)]
struct MeshExportOutput {
    path: String,
    event_count: usize,
    exported_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
struct SkillInstallOutput {
    target: String,
    path: String,
}

#[derive(Debug, Serialize)]
struct SkillPrintOutput {
    target: String,
    content: &'static str,
}

#[derive(Debug, Serialize)]
struct IdentityOutput {
    agent: String,
    source: String,
}

struct ResolvedContent {
    content: String,
    message_id: Option<Uuid>,
}

fn repo_root(cwd: &Path) -> Option<PathBuf> {
    let output = ProcessCommand::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let root = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if root.is_empty() {
        None
    } else {
        Some(PathBuf::from(root))
    }
}

fn load_or_create_workspace_id(shuttle_dir: &Path, root: &Path) -> Result<String> {
    let path = shuttle_dir.join("workspace.json");
    if let Ok(contents) = fs::read_to_string(&path) {
        let workspace: WorkspaceFile = serde_json::from_str(&contents)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        return Ok(workspace.workspace_id);
    }

    fs::create_dir_all(shuttle_dir)
        .with_context(|| format!("failed to create {}", shuttle_dir.display()))?;
    let workspace = WorkspaceFile {
        workspace_id: Uuid::new_v4().to_string(),
        repo_path: root.display().to_string(),
        created_at: Utc::now(),
    };
    fs::write(&path, serde_json::to_string_pretty(&workspace)?)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(workspace.workspace_id)
}

fn load_agent(shuttle_dir: &Path) -> (String, String) {
    if let Ok(agent) = env::var("SHUTTLE_AGENT") {
        let agent = agent.trim();
        if !agent.is_empty() {
            return (agent.to_owned(), "SHUTTLE_AGENT".to_owned());
        }
    }

    let path = shuttle_dir.join("agent");
    if let Ok(contents) = fs::read_to_string(path) {
        let agent = contents.trim();
        if !agent.is_empty() {
            return (agent.to_owned(), "repo".to_owned());
        }
    }

    ("unknown".to_owned(), "default".to_owned())
}

fn set_persisted_agent(shuttle_dir: &Path, agent: &str) -> Result<()> {
    let agent = agent.trim();
    if agent.is_empty() {
        anyhow::bail!("agent identity cannot be empty");
    }
    fs::create_dir_all(shuttle_dir)
        .with_context(|| format!("failed to create {}", shuttle_dir.display()))?;
    fs::write(shuttle_dir.join("agent"), format!("{agent}\n"))
        .with_context(|| format!("failed to write {}", shuttle_dir.join("agent").display()))
}

fn current_identity(env: &RuntimeEnv) -> Result<IdentityOutput> {
    Ok(IdentityOutput {
        agent: env.agent.clone(),
        source: env.agent_source.clone(),
    })
}

fn load_or_create_session_id(shuttle_dir: &Path) -> Result<String> {
    if let Ok(session_id) = env::var("SHUTTLE_SESSION_ID") {
        return Ok(session_id);
    }

    let path = shuttle_dir.join("session");
    if let Ok(contents) = fs::read_to_string(&path) {
        let session_id = contents.trim();
        if !session_id.is_empty() {
            return Ok(session_id.to_owned());
        }
    }

    fs::create_dir_all(shuttle_dir)
        .with_context(|| format!("failed to create {}", shuttle_dir.display()))?;
    let session_id = Uuid::new_v4().to_string();
    fs::write(&path, format!("{session_id}\n"))
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(session_id)
}

fn open_store(env: &RuntimeEnv) -> Result<SqliteEventStore> {
    fs::create_dir_all(&env.shuttle_dir)
        .with_context(|| format!("failed to create {}", env.shuttle_dir.display()))?;
    SqliteEventStore::open(&env.database_path)
        .with_context(|| format!("failed to open {}", env.database_path.display()))
}

fn resolve_content(
    store: &SqliteEventStore,
    content: Option<String>,
    from_message: Option<Uuid>,
) -> Result<ResolvedContent> {
    match (content, from_message) {
        (Some(_), Some(_)) => anyhow::bail!("provide content or --from-message, not both"),
        (Some(content), None) => Ok(ResolvedContent {
            content,
            message_id: None,
        }),
        (None, Some(message_id)) => {
            let message = load_message(store, message_id)?;
            Ok(ResolvedContent {
                content: message.content,
                message_id: Some(message_id),
            })
        }
        (None, None) => anyhow::bail!("missing content or --from-message"),
    }
}

fn load_message(store: &SqliteEventStore, id: Uuid) -> Result<Event> {
    let mut events = block_on(store.list(EventFilter {
        id: Some(id),
        event_type: Some(EventType::Message),
        ..EventFilter::default()
    }))?;
    events
        .pop()
        .ok_or_else(|| anyhow::anyhow!("unknown message id: {id}"))
}

fn with_source_message_metadata(mut event: Event, message_id: Option<Uuid>) -> Event {
    if let Some(message_id) = message_id {
        if let Some(metadata) = event.metadata_json.as_object_mut() {
            metadata.insert("source_message_id".to_owned(), json!(message_id));
        }
    }
    event
}

fn watch_inbox(json: bool, store: &SqliteEventStore, agent: &str, interval: u64) -> Result<()> {
    let interval = Duration::from_secs(interval.max(1));
    let mut seen = HashSet::new();

    loop {
        let events = block_on(shuttle_rs::message::inbox(store, agent))?;
        for event in events.iter().rev() {
            if seen.insert(event.id) {
                output_event_line(json, event)?;
            }
        }
        thread::sleep(interval);
    }
}

fn app_oauth(
    env: &RuntimeEnv,
    public_url: Option<String>,
) -> Result<Option<shuttle_rs::app::OAuthRuntime>> {
    let Some(public_url) = public_url
        .or_else(|| env::var("SHUTTLE_PUBLIC_URL").ok())
        .filter(|url| !url.trim().is_empty())
    else {
        return Ok(None);
    };
    let public_url = shuttle_rs::oauth::OAuthConfig::normalize_public_url(public_url);
    let admin_token = env::var("SHUTTLE_OAUTH_ADMIN_TOKEN")
        .ok()
        .filter(|token| !token.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!("OAuth public URL mode requires SHUTTLE_OAUTH_ADMIN_TOKEN to be set")
        })?;
    Ok(Some(shuttle_rs::app::OAuthRuntime {
        config: shuttle_rs::oauth::OAuthConfig {
            public_url,
            admin_token: Some(admin_token),
        },
        store: shuttle_rs::oauth::OAuthStore::open(&env.database_path).with_context(|| {
            format!("failed to open OAuth store {}", env.database_path.display())
        })?,
    }))
}

fn start_cloudflared(cloudflared: &Path, token: &str) -> Result<Child> {
    ProcessCommand::new(cloudflared)
        .args(["tunnel", "run", "--token"])
        .arg(token)
        .spawn()
        .with_context(|| format!("failed to start {}", cloudflared.display()))
}

fn stop_child(child: &mut Child) {
    if let Ok(None) = child.try_wait() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn open_peer_store(path: &Path) -> Result<SqliteEventStore> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    SqliteEventStore::open(path).with_context(|| format!("failed to open {}", path.display()))
}

fn load_peer_workspace_id(database_path: &Path) -> Option<String> {
    let workspace_path = database_path.parent()?.join("workspace.json");
    let contents = fs::read_to_string(workspace_path).ok()?;
    serde_json::from_str::<WorkspaceFile>(&contents)
        .ok()
        .map(|workspace| workspace.workspace_id)
}

impl SkillTarget {
    fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
        }
    }
}

fn install_skill(target: SkillTarget) -> Result<SkillInstallOutput> {
    let path = skill_install_path(target)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(&path, skill_content(target))
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(SkillInstallOutput {
        target: target.as_str().to_owned(),
        path: path.display().to_string(),
    })
}

fn skill_install_path(target: SkillTarget) -> Result<PathBuf> {
    match target {
        SkillTarget::Codex => Ok(home_dir()?
            .join(".codex")
            .join("skills")
            .join("shuttle")
            .join("SKILL.md")),
    }
}

fn home_dir() -> Result<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .context("HOME is not set")
}

fn skill_content(target: SkillTarget) -> &'static str {
    match target {
        SkillTarget::Codex => CODEX_SKILL,
    }
}

const CODEX_SKILL: &str = r#"---
name: shuttle
description: Use when working with Shuttle/stl local-first agent memory, tasks, handoffs, messages, mesh sync, MCP app server, or shuttle-gateway multi-project web chat setup.
---

# Shuttle

## Before work

In a Shuttle repo, run:

```bash
stl context
stl inbox
stl recall "current task"
stl task list
```

If the current shell does not set `SHUTTLE_AGENT`, set the repo-local identity:

```bash
stl identity set codex
```

## Local memory and coordination

- Use `stl remember`, `stl observe`, `stl decide`, `stl pattern`, `stl fact`, and `stl bug` for useful durable context.
- Use `stl recall "<query>"` and `stl recall "<query>" --type decision` to retrieve context.
- Use `stl task create/list/claim/update/done` for task coordination.
- Use `stl handoff request/list/accept/done` and `stl send/inbox/history` for agent handoffs and messages.
- Use `stl send` for transient agent-to-agent communication.
- Use `stl handoff` when ownership of work should move to another agent.
- Use `stl task` for trackable work.
- Use typed memory commands for durable project knowledge.
- Do not leave important decisions only in message history.

## Message loop

- At session start, run `stl context`, `stl inbox`, and `stl task list`.
- During work, use `stl send <agent> "<message>"` for transient coordination.
- At session end, run `stl inbox` again and update tasks or handoffs.
- For polling delivery, run `stl inbox --watch` in a separate terminal.
- Promote important message outcomes with `stl decide --from-message <message-id>`, `stl task create --from-message <message-id>`, or `stl handoff request <agent> --from-message <message-id>`.

## MCP

- Single-repo local MCP: `stl app serve --addr 127.0.0.1:8787`.
- Multi-project gateway: `shuttle-gateway serve --config <projects.toml> --addr 127.0.0.1:8788`.
- With OAuth, set `SHUTTLE_OAUTH_ADMIN_TOKEN` via a secret manager or runtime injection; never print it.
- Verify remote MCP with:
  - `curl -i <public-url>/.well-known/oauth-protected-resource/mcp`
  - `curl -i <public-url>/mcp -H 'content-type: application/json' --data '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}'`
- Unauthenticated remote MCP should return 401 with `WWW-Authenticate`.

## Cloudflare named tunnel

- Keep gateway project config and admin token in local user config or a secret manager.
- Run the gateway locally, then expose it with a Cloudflare named tunnel.
- Register MCP clients with `<public-url>/mcp`.
- Check local services with `launchctl list | rg 'shuttle-gateway'` when using LaunchAgents.
- Check tunnel status with `cloudflared tunnel info <tunnel-name>`.
- Do not print local admin token files or token environment values.
"#;

fn append_typed_memory(
    store: &SqliteEventStore,
    env: &RuntimeEnv,
    event_type: EventType,
    content: String,
    source_message_id: Option<Uuid>,
) -> Result<Event> {
    let event = with_repo_metadata(
        with_source_message_metadata(
            shuttle_rs::memory::new_typed_memory(
                event_type,
                env.workspace_id.clone(),
                env.agent.clone(),
                env.session_id.clone(),
                content,
            ),
            source_message_id,
        ),
        env,
    );
    Ok(block_on(store.append(event))?)
}

fn with_repo_metadata(mut event: Event, env: &RuntimeEnv) -> Event {
    if let Ok(status) = shuttle_rs::context::repo_status(&env.cwd) {
        let repo_id = shuttle_rs::context::repo_id(&status);
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

fn format_context(context: &shuttle_rs::context::Context) -> String {
    let mut output = format!(
        "Repository\n- path: {}\n- branch: {}\n- commit: {}\n- dirty: {}\n",
        context.repo, context.branch, context.commit, context.dirty
    );
    if context.dirty_files.is_empty() {
        output.push_str("- dirty files: none\n\n");
    } else {
        output.push_str("- dirty files:\n");
        for file in &context.dirty_files {
            output.push_str(&format!("  - {file}\n"));
        }
        output.push('\n');
    }
    push_task_section(&mut output, "Open Tasks", &context.open_tasks);
    push_task_section(&mut output, "Claimed Tasks", &context.claimed_tasks);
    push_event_section(&mut output, "Recent Decisions", &context.recent_decisions);
    push_event_section(&mut output, "Related Memories", &context.related_memories);
    push_event_section(&mut output, "Recent Messages", &context.recent_messages);
    push_handoff_section(&mut output, "Pending Handoffs", &context.pending_handoffs);
    push_handoff_section(
        &mut output,
        "Recent Completed Handoffs",
        &context.recent_completed_handoffs,
    );
    push_event_section(&mut output, "Inbox", &context.inbox);
    output.trim_end().to_owned()
}

fn push_task_section(output: &mut String, title: &str, tasks: &[shuttle_rs::task::TaskSummary]) {
    output.push_str(title);
    output.push('\n');
    if tasks.is_empty() {
        output.push_str("- none\n\n");
        return;
    }
    for task in tasks {
        let claimed_by = task
            .claimed_by
            .as_deref()
            .map(|agent| format!(" claimed_by={agent}"))
            .unwrap_or_default();
        output.push_str(&format!(
            "- [{}] {}{}: {}\n",
            task.id,
            task.status.as_str(),
            claimed_by,
            task.content
        ));
    }
    output.push('\n');
}

fn push_handoff_section(
    output: &mut String,
    title: &str,
    handoffs: &[shuttle_rs::task::HandoffSummary],
) {
    output.push_str(title);
    output.push('\n');
    if handoffs.is_empty() {
        output.push_str("- none\n\n");
        return;
    }
    for handoff in handoffs {
        output.push_str(&format!(
            "- [{}] {} {} -> {}: {}\n",
            handoff.id,
            handoff.status.as_str(),
            handoff.from_agent,
            handoff.to_agent,
            handoff.content
        ));
    }
    output.push('\n');
}

fn push_event_section(output: &mut String, title: &str, events: &[Event]) {
    output.push_str(title);
    output.push('\n');
    if events.is_empty() {
        output.push_str("- none\n\n");
        return;
    }
    for event in events {
        let title = event.title.as_deref().unwrap_or(event.event_type.as_str());
        output.push_str(&format!("- {}: {}\n", title, event.content));
    }
    output.push('\n');
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

fn output_tasks(json: bool, tasks: &[shuttle_rs::task::TaskSummary]) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(tasks)?);
        return Ok(());
    }
    if tasks.is_empty() {
        println!("no tasks");
        return Ok(());
    }
    for task in tasks {
        let claimed_by = task
            .claimed_by
            .as_deref()
            .map(|agent| format!(" claimed_by={agent}"))
            .unwrap_or_default();
        println!(
            "- [{}] {}{}: {}",
            task.id,
            task.status.as_str(),
            claimed_by,
            task.content
        );
    }
    Ok(())
}

fn output_event_line(json: bool, event: &Event) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string(event)?);
    } else {
        let title = event.title.as_deref().unwrap_or(event.event_type.as_str());
        println!(
            "- [{}] {} from {}: {}",
            event.id, title, event.agent, event.content
        );
    }
    Ok(())
}

fn output_handoffs(json: bool, handoffs: &[shuttle_rs::task::HandoffSummary]) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(handoffs)?);
        return Ok(());
    }
    if handoffs.is_empty() {
        println!("no handoffs");
        return Ok(());
    }
    for handoff in handoffs {
        println!(
            "- [{}] {} {} -> {}: {}",
            handoff.id,
            handoff.status.as_str(),
            handoff.from_agent,
            handoff.to_agent,
            handoff.content
        );
    }
    Ok(())
}

fn output_recall(json: bool, results: &[shuttle_rs::memory::RecallResult]) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(results)?);
        return Ok(());
    }

    if results.is_empty() {
        println!("no recall");
        return Ok(());
    }

    for result in results {
        let title = result
            .event
            .title
            .as_deref()
            .unwrap_or(result.event.event_type.as_str());
        let reasons = if result.reasons.is_empty() {
            "no ranking signals".to_owned()
        } else {
            result.reasons.join(", ")
        };
        println!(
            "- [{}] {}: {} (score {}, {})",
            result.event.created_at.format("%Y-%m-%d %H:%M:%S UTC"),
            title,
            result.event.content,
            result.score,
            reasons
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    #[test]
    fn workspace_id_is_persisted() {
        let dir = tempfile::tempdir().unwrap();
        let shuttle_dir = dir.path().join(".shuttle");

        let first = load_or_create_workspace_id(&shuttle_dir, dir.path()).unwrap();
        let second = load_or_create_workspace_id(&shuttle_dir, dir.path()).unwrap();

        assert_eq!(first, second);
        assert!(shuttle_dir.join("workspace.json").exists());
    }

    #[test]
    fn session_id_is_persisted_without_env_override() {
        let _guard = env_lock();
        let dir = tempfile::tempdir().unwrap();
        let shuttle_dir = dir.path().join(".shuttle");

        env::remove_var("SHUTTLE_SESSION_ID");
        let first = load_or_create_session_id(&shuttle_dir).unwrap();
        let second = load_or_create_session_id(&shuttle_dir).unwrap();

        assert_eq!(first, second);
        assert!(shuttle_dir.join("session").exists());
    }

    #[test]
    fn session_env_overrides_persisted_value() {
        let _guard = env_lock();
        let dir = tempfile::tempdir().unwrap();
        let shuttle_dir = dir.path().join(".shuttle");
        fs::create_dir_all(&shuttle_dir).unwrap();
        fs::write(shuttle_dir.join("session"), "persisted\n").unwrap();

        env::set_var("SHUTTLE_SESSION_ID", "override");
        let session_id = load_or_create_session_id(&shuttle_dir).unwrap();
        env::remove_var("SHUTTLE_SESSION_ID");

        assert_eq!(session_id, "override");
    }

    #[test]
    fn agent_identity_prefers_env_then_repo_then_default() {
        let _guard = env_lock();
        let dir = tempfile::tempdir().unwrap();
        let shuttle_dir = dir.path().join(".shuttle");
        env::remove_var("SHUTTLE_AGENT");

        assert_eq!(
            load_agent(&shuttle_dir),
            ("unknown".to_owned(), "default".to_owned())
        );

        set_persisted_agent(&shuttle_dir, "codex").unwrap();
        assert_eq!(
            load_agent(&shuttle_dir),
            ("codex".to_owned(), "repo".to_owned())
        );

        env::set_var("SHUTTLE_AGENT", "claude");
        assert_eq!(
            load_agent(&shuttle_dir),
            ("claude".to_owned(), "SHUTTLE_AGENT".to_owned())
        );
        env::remove_var("SHUTTLE_AGENT");
    }

    #[test]
    fn resolves_content_from_message_and_tracks_source_metadata() {
        let repo = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        init_git_repo(repo.path());
        let env = test_env(repo.path(), data.path());
        let store = open_store(&env).unwrap();
        let message = block_on(store.append(shuttle_rs::message::new_message(
            env.workspace_id.clone(),
            "claude".into(),
            env.session_id.clone(),
            "codex".into(),
            "promote this".into(),
        )))
        .unwrap();

        let source = resolve_content(&store, None, Some(message.id)).unwrap();
        let decision = append_typed_memory(
            &store,
            &env,
            EventType::Decision,
            source.content,
            source.message_id,
        )
        .unwrap();

        assert_eq!(decision.content, "promote this");
        assert_eq!(
            decision.metadata_json["source_message_id"],
            json!(message.id)
        );
    }

    #[test]
    fn codex_skill_path_uses_home_directory() {
        let _guard = env_lock();
        let dir = tempfile::tempdir().unwrap();
        env::set_var("HOME", dir.path());

        let path = skill_install_path(SkillTarget::Codex).unwrap();

        env::remove_var("HOME");
        assert_eq!(
            path,
            dir.path()
                .join(".codex")
                .join("skills")
                .join("shuttle")
                .join("SKILL.md")
        );
    }

    #[test]
    fn codex_skill_install_writes_skill_file() {
        let _guard = env_lock();
        let dir = tempfile::tempdir().unwrap();
        env::set_var("HOME", dir.path());

        let install = install_skill(SkillTarget::Codex).unwrap();

        env::remove_var("HOME");
        let path = dir
            .path()
            .join(".codex")
            .join("skills")
            .join("shuttle")
            .join("SKILL.md");
        let content = fs::read_to_string(&path).unwrap();
        assert_eq!(install.target, "codex");
        assert_eq!(install.path, path.display().to_string());
        assert!(content.contains("name: shuttle"));
        assert!(content.contains("stl context"));
        assert!(content.contains("SHUTTLE_OAUTH_ADMIN_TOKEN"));
    }

    #[test]
    fn app_oauth_requires_admin_token_for_public_url_mode() {
        let _guard = env_lock();
        let repo = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let env = test_env(repo.path(), data.path());
        fs::create_dir_all(&env.shuttle_dir).unwrap();
        env::remove_var("SHUTTLE_PUBLIC_URL");
        env::remove_var("SHUTTLE_OAUTH_ADMIN_TOKEN");

        let err = match app_oauth(&env, Some("https://shuttle.example.test".to_owned())) {
            Ok(_) => panic!("app_oauth unexpectedly allowed public URL mode without admin token"),
            Err(err) => err,
        };

        assert!(err.to_string().contains("SHUTTLE_OAUTH_ADMIN_TOKEN"));
    }

    #[test]
    fn app_oauth_uses_admin_token_when_public_url_mode_is_enabled() {
        let _guard = env_lock();
        let repo = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let env = test_env(repo.path(), data.path());
        fs::create_dir_all(&env.shuttle_dir).unwrap();
        env::remove_var("SHUTTLE_PUBLIC_URL");
        env::set_var("SHUTTLE_OAUTH_ADMIN_TOKEN", "admin-token");

        let oauth = app_oauth(&env, Some("https://shuttle.example.test".to_owned()))
            .unwrap()
            .unwrap();

        env::remove_var("SHUTTLE_OAUTH_ADMIN_TOKEN");
        assert_eq!(oauth.config.admin_token.as_deref(), Some("admin-token"));
    }

    #[test]
    fn app_oauth_remains_disabled_without_public_url() {
        let _guard = env_lock();
        let repo = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let env = test_env(repo.path(), data.path());
        fs::create_dir_all(&env.shuttle_dir).unwrap();
        env::remove_var("SHUTTLE_PUBLIC_URL");
        env::remove_var("SHUTTLE_OAUTH_ADMIN_TOKEN");

        assert!(app_oauth(&env, None).unwrap().is_none());
    }

    #[test]
    fn repo_root_is_stable_from_nested_directory() {
        let dir = tempfile::tempdir().unwrap();
        ProcessCommand::new("git")
            .arg("init")
            .current_dir(dir.path())
            .output()
            .unwrap();
        let nested = dir.path().join("crates/example");
        fs::create_dir_all(&nested).unwrap();

        assert_eq!(
            repo_root(&nested).unwrap().canonicalize().unwrap(),
            dir.path().canonicalize().unwrap()
        );
    }

    #[test]
    fn repo_metadata_is_added_to_phase_one_events() {
        let repo = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        init_git_repo(repo.path());
        fs::write(repo.path().join("dirty.txt"), "dirty").unwrap();
        let env = test_env(repo.path(), data.path());
        let store = open_store(&env).unwrap();

        let memory = with_repo_metadata(
            shuttle_rs::memory::new_memory(
                env.workspace_id.clone(),
                env.agent.clone(),
                env.session_id.clone(),
                "repo memory".into(),
            ),
            &env,
        );
        let message = with_repo_metadata(
            shuttle_rs::message::new_message(
                env.workspace_id.clone(),
                env.agent.clone(),
                env.session_id.clone(),
                "reviewer".into(),
                "repo message".into(),
            ),
            &env,
        );
        let decision = append_typed_memory(
            &store,
            &env,
            EventType::Decision,
            "repo decision".into(),
            None,
        )
        .unwrap();
        let repo_path = repo.path().canonicalize().unwrap();

        for event in [memory, message, decision] {
            assert!(event.repo_id.is_some());
            assert_eq!(
                PathBuf::from(event.repo_path.as_deref().unwrap())
                    .canonicalize()
                    .unwrap(),
                repo_path
            );
            assert!(event.branch.is_some());
            assert!(event.commit.is_some());
            assert_eq!(event.repo_dirty, Some(true));
            assert_eq!(event.metadata_json["repo_dirty"], true);
            assert_eq!(event.metadata_json["dirty_files"], json!(["dirty.txt"]));
        }
    }

    #[test]
    fn typed_recall_filters_and_preserves_ranked_json_shape() {
        let repo = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        init_git_repo(repo.path());
        let env = test_env(repo.path(), data.path());
        let store = open_store(&env).unwrap();
        let memory = with_repo_metadata(
            shuttle_rs::memory::new_memory(
                env.workspace_id.clone(),
                env.agent.clone(),
                env.session_id.clone(),
                "SQLite storage note".into(),
            ),
            &env,
        );
        let decision = append_typed_memory(
            &store,
            &env,
            EventType::Decision,
            "SQLite storage decision".into(),
            None,
        )
        .unwrap();
        block_on(store.append(memory)).unwrap();

        let status = shuttle_rs::context::repo_status(repo.path()).unwrap();
        let repo_id = shuttle_rs::context::repo_id(&status);
        let results = block_on(shuttle_rs::memory::ranked_recall(
            &store,
            "SQLite storage",
            Some(EventType::Decision),
            Some(&env.workspace_id),
            Some(&repo_id),
            Some(&status.branch),
        ))
        .unwrap();
        let json = serde_json::to_value(&results).unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].event.id, decision.id);
        assert_eq!(json[0]["event"]["event_type"], "decision");
        assert_eq!(json[0]["event"]["metadata_json"]["kind"], "decision");
        assert!(json[0]["score"].as_i64().unwrap() > 0);
        assert!(json[0]["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reason| reason == "decision event"));
    }

    #[test]
    fn ranked_recall_prefers_same_repo_branch_decisions() {
        let repo = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        init_git_repo(repo.path());
        let env = test_env(repo.path(), data.path());
        let store = open_store(&env).unwrap();
        let status = shuttle_rs::context::repo_status(repo.path()).unwrap();
        let repo_id = shuttle_rs::context::repo_id(&status);
        let mut generic = shuttle_rs::memory::new_memory(
            env.workspace_id.clone(),
            env.agent.clone(),
            env.session_id.clone(),
            "SQLite storage decision".into(),
        );
        generic.repo_id = Some("other-repo".into());
        generic.branch = Some("other-branch".into());
        let decision = append_typed_memory(
            &store,
            &env,
            EventType::Decision,
            "SQLite storage decision".into(),
            None,
        )
        .unwrap();
        block_on(store.append(generic)).unwrap();

        let results = block_on(shuttle_rs::memory::ranked_recall(
            &store,
            "SQLite storage decision",
            None,
            Some(&env.workspace_id),
            Some(&repo_id),
            Some(&status.branch),
        ))
        .unwrap();

        assert_eq!(results[0].event.id, decision.id);
        assert!(results[0].reasons.contains(&"decision event".to_owned()));
        assert!(results[0].reasons.contains(&"same repo".to_owned()));
        assert!(results[0].reasons.contains(&"same branch".to_owned()));
    }

    fn test_env(repo: &Path, data: &Path) -> RuntimeEnv {
        RuntimeEnv {
            cwd: repo.to_path_buf(),
            shuttle_dir: data.join(".shuttle"),
            database_path: data.join(".shuttle/shuttle.db"),
            workspace_id: "workspace".into(),
            agent: "codex".into(),
            agent_source: "test".into(),
            session_id: "session".into(),
        }
    }

    fn init_git_repo(path: &Path) {
        ProcessCommand::new("git")
            .arg("init")
            .current_dir(path)
            .output()
            .unwrap();
        fs::write(path.join("README.md"), "repo").unwrap();
        ProcessCommand::new("git")
            .args(["add", "README.md"])
            .current_dir(path)
            .output()
            .unwrap();
        ProcessCommand::new("git")
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
