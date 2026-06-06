use std::env;
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand, ValueEnum};
use futures_executor::block_on;
use serde::{Deserialize, Serialize};
use serde_json::json;
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
        #[arg(long = "type")]
        kind: Option<MemoryKindArg>,
    },
    Memories,
    Decide {
        content: String,
    },
    Observe {
        content: String,
    },
    Pattern {
        content: String,
    },
    Fact {
        content: String,
    },
    Bug {
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
    Context {
        #[arg(long)]
        repo: bool,
        #[arg(long)]
        branch: bool,
    },
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
        Command::Recall { query, kind } => {
            let store = open_store(&env)?;
            let status = shuttle_context::repo_status(&env.cwd).ok();
            let repo_id = status.as_ref().map(shuttle_context::repo_id);
            let branch = status.as_ref().map(|status| status.branch.as_str());
            let results = block_on(shuttle_memory::ranked_recall(
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
            let events = block_on(shuttle_memory::memories(&store))?;
            output_events(cli.json, &events, "memories")?;
        }
        Command::Decide { content } => {
            let store = open_store(&env)?;
            let event = append_typed_memory(&store, &env, EventType::Decision, content)?;
            output(cli.json, &event, || format!("decided: {}", event.content))?;
        }
        Command::Observe { content } => {
            let store = open_store(&env)?;
            let event = append_typed_memory(&store, &env, EventType::Observation, content)?;
            output(cli.json, &event, || format!("observed: {}", event.content))?;
        }
        Command::Pattern { content } => {
            let store = open_store(&env)?;
            let event = append_typed_memory(&store, &env, EventType::Pattern, content)?;
            output(cli.json, &event, || {
                format!("recorded pattern: {}", event.content)
            })?;
        }
        Command::Fact { content } => {
            let store = open_store(&env)?;
            let event = append_typed_memory(&store, &env, EventType::Fact, content)?;
            output(cli.json, &event, || {
                format!("recorded fact: {}", event.content)
            })?;
        }
        Command::Bug { content } => {
            let store = open_store(&env)?;
            let event = append_typed_memory(&store, &env, EventType::Bug, content)?;
            output(cli.json, &event, || {
                format!("recorded bug: {}", event.content)
            })?;
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
                    repo_dirty: None,
                    agent: env.agent.clone(),
                    session_id: env.session_id.clone(),
                    title: Some("handoff".to_owned()),
                    content,
                    tags: Vec::new(),
                    metadata_json: json!({ "to": agent }),
                }),
                &env,
            );
            let event = block_on(store.append(event))?;
            output(cli.json, &event, || {
                format!("handed off to {agent}: {}", event.content)
            })?;
        }
        Command::Context { repo, branch } => {
            if repo && branch {
                anyhow::bail!("--repo and --branch cannot be used together");
            }
            let store = open_store(&env)?;
            let context = block_on(shuttle_context::assemble_context(
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
                        session_id: env.session_id,
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
        let root = repo_root(&cwd).unwrap_or_else(|| cwd.clone());
        let shuttle_dir = root.join(".shuttle");
        let database_path = shuttle_dir.join("shuttle.db");
        let workspace_id = load_or_create_workspace_id(&shuttle_dir, &root)?;
        let agent = env::var("SHUTTLE_AGENT").unwrap_or_else(|_| "unknown".to_owned());
        let session_id = load_or_create_session_id(&shuttle_dir)?;

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

fn append_typed_memory(
    store: &SqliteEventStore,
    env: &RuntimeEnv,
    event_type: EventType,
    content: String,
) -> Result<Event> {
    let event = with_repo_metadata(
        shuttle_memory::new_typed_memory(
            event_type,
            env.workspace_id.clone(),
            env.agent.clone(),
            env.session_id.clone(),
            content,
        ),
        env,
    );
    Ok(block_on(store.append(event))?)
}

fn with_repo_metadata(mut event: Event, env: &RuntimeEnv) -> Event {
    if let Ok(status) = shuttle_context::repo_status(&env.cwd) {
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

fn format_context(context: &shuttle_context::Context) -> String {
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
    push_event_section(&mut output, "Open Tasks", &context.open_tasks);
    push_event_section(&mut output, "Recent Decisions", &context.recent_decisions);
    push_event_section(&mut output, "Related Memories", &context.related_memories);
    push_event_section(&mut output, "Recent Messages", &context.recent_messages);
    push_event_section(&mut output, "Pending Handoffs", &context.pending_handoffs);
    push_event_section(&mut output, "Inbox", &context.inbox);
    output.trim_end().to_owned()
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

fn output_recall(json: bool, results: &[shuttle_memory::RecallResult]) -> Result<()> {
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
            shuttle_memory::new_memory(
                env.workspace_id.clone(),
                env.agent.clone(),
                env.session_id.clone(),
                "repo memory".into(),
            ),
            &env,
        );
        let message = with_repo_metadata(
            shuttle_message::new_message(
                env.workspace_id.clone(),
                env.agent.clone(),
                env.session_id.clone(),
                "reviewer".into(),
                "repo message".into(),
            ),
            &env,
        );
        let decision =
            append_typed_memory(&store, &env, EventType::Decision, "repo decision".into()).unwrap();
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
            shuttle_memory::new_memory(
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
        )
        .unwrap();
        block_on(store.append(memory)).unwrap();

        let status = shuttle_context::repo_status(repo.path()).unwrap();
        let repo_id = shuttle_context::repo_id(&status);
        let results = block_on(shuttle_memory::ranked_recall(
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
        let status = shuttle_context::repo_status(repo.path()).unwrap();
        let repo_id = shuttle_context::repo_id(&status);
        let mut generic = shuttle_memory::new_memory(
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
        )
        .unwrap();
        block_on(store.append(generic)).unwrap();

        let results = block_on(shuttle_memory::ranked_recall(
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
