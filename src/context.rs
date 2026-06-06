use std::path::{Path, PathBuf};
use std::process::Command;

use crate::core::{Event, EventFilter, EventStore, EventType, Result, ShuttleError};
use crate::task::{HandoffStatus, HandoffSummary, TaskStatus, TaskSummary};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Context {
    pub repo: String,
    pub branch: String,
    pub commit: String,
    pub git_remote: Option<String>,
    pub dirty: bool,
    pub dirty_files: Vec<String>,
    pub open_tasks: Vec<TaskSummary>,
    pub claimed_tasks: Vec<TaskSummary>,
    pub recent_decisions: Vec<Event>,
    pub related_memories: Vec<Event>,
    pub recent_messages: Vec<Event>,
    pub pending_handoffs: Vec<HandoffSummary>,
    pub recent_completed_handoffs: Vec<HandoffSummary>,
    pub inbox: Vec<Event>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoStatus {
    pub repo_path: String,
    pub git_remote: Option<String>,
    pub branch: String,
    pub commit: String,
    pub dirty: bool,
    pub dirty_files: Vec<String>,
}

pub fn repo_status(path: impl AsRef<Path>) -> Result<RepoStatus> {
    let path = path.as_ref();
    let repo_path = git(path, ["rev-parse", "--show-toplevel"])?;
    let repo_path_buf = PathBuf::from(repo_path.trim());
    let branch = git(&repo_path_buf, ["rev-parse", "--abbrev-ref", "HEAD"])?;
    let commit = git(&repo_path_buf, ["rev-parse", "HEAD"])?;
    let remote = git(&repo_path_buf, ["config", "--get", "remote.origin.url"]).ok();
    let status = git(&repo_path_buf, ["status", "--porcelain"])?;
    let dirty_files = parse_dirty_files(&status);

    Ok(RepoStatus {
        repo_path: repo_path.trim().to_owned(),
        git_remote: remote
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty()),
        branch: branch.trim().to_owned(),
        commit: commit.trim().to_owned(),
        dirty: !dirty_files.is_empty(),
        dirty_files,
    })
}

pub async fn assemble_context(
    store: &impl EventStore,
    cwd: impl AsRef<Path>,
    workspace_id: &str,
    agent: &str,
) -> Result<Context> {
    let status = repo_status(cwd)?;
    let task_summaries = crate::task::tasks(store, Some(workspace_id), None).await?;
    let open_tasks = task_summaries
        .iter()
        .filter(|task| task.status == TaskStatus::Open)
        .take(20)
        .cloned()
        .collect();
    let claimed_tasks = task_summaries
        .iter()
        .filter(|task| task.status == TaskStatus::Claimed)
        .take(20)
        .cloned()
        .collect();
    let recent_decisions = store
        .list(EventFilter {
            event_type: Some(EventType::Decision),
            workspace_id: Some(workspace_id.to_owned()),
            limit: Some(20),
            ..EventFilter::default()
        })
        .await?;
    let related_memories = store
        .list(EventFilter {
            event_type: Some(EventType::Memory),
            workspace_id: Some(workspace_id.to_owned()),
            limit: Some(20),
            ..EventFilter::default()
        })
        .await?;
    let recent_messages = store
        .list(EventFilter {
            event_type: Some(EventType::Message),
            workspace_id: Some(workspace_id.to_owned()),
            limit: Some(20),
            ..EventFilter::default()
        })
        .await?;
    let handoff_summaries = crate::task::handoffs(store, Some(workspace_id), None).await?;
    let pending_handoffs = handoff_summaries
        .iter()
        .filter(|handoff| handoff.status == HandoffStatus::Pending)
        .take(20)
        .cloned()
        .collect();
    let recent_completed_handoffs = handoff_summaries
        .iter()
        .filter(|handoff| handoff.status == HandoffStatus::Completed)
        .take(20)
        .cloned()
        .collect();
    let inbox = inbox_events(store, workspace_id, agent).await?;

    Ok(Context {
        repo: status.repo_path,
        branch: status.branch,
        commit: status.commit,
        git_remote: status.git_remote,
        dirty: status.dirty,
        dirty_files: status.dirty_files,
        open_tasks,
        claimed_tasks,
        recent_decisions,
        related_memories,
        recent_messages,
        pending_handoffs,
        recent_completed_handoffs,
        inbox,
    })
}

pub fn repo_id(status: &RepoStatus) -> String {
    status
        .git_remote
        .clone()
        .unwrap_or_else(|| status.repo_path.clone())
}

fn parse_dirty_files(status: &str) -> Vec<String> {
    status
        .lines()
        .filter_map(|line| {
            let path = line.get(3..)?.trim();
            if path.is_empty() {
                None
            } else if let Some((_, destination)) = path.split_once(" -> ") {
                Some(destination.to_owned())
            } else {
                Some(path.to_owned())
            }
        })
        .collect()
}

async fn inbox_events(
    store: &impl EventStore,
    workspace_id: &str,
    agent: &str,
) -> Result<Vec<Event>> {
    let mut events = store
        .list(EventFilter {
            event_type: Some(EventType::Message),
            workspace_id: Some(workspace_id.to_owned()),
            recipient: Some(agent.to_owned()),
            limit: Some(20),
            ..EventFilter::default()
        })
        .await?;
    events.extend(
        store
            .list(EventFilter {
                event_type: Some(EventType::Handoff),
                workspace_id: Some(workspace_id.to_owned()),
                recipient: Some(agent.to_owned()),
                limit: Some(20),
                ..EventFilter::default()
            })
            .await?,
    );
    events.sort_by(|left, right| right.created_at.cmp(&left.created_at));
    events.truncate(20);
    Ok(events)
}

fn git<const N: usize>(cwd: &Path, args: [&str; N]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|err| ShuttleError::Store(format!("failed to run git: {err}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ShuttleError::Store(format!(
            "git command failed: {}",
            stderr.trim()
        )));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::EventStore;
    use crate::store::SqliteEventStore;
    use std::fs;

    #[test]
    fn repo_status_reports_dirty_files() {
        let dir = tempfile::tempdir().unwrap();
        Command::new("git")
            .arg("init")
            .current_dir(dir.path())
            .output()
            .unwrap();
        fs::write(dir.path().join("README.md"), "repo").unwrap();
        Command::new("git")
            .args(["add", "README.md"])
            .current_dir(dir.path())
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
            .current_dir(dir.path())
            .output()
            .unwrap();
        fs::write(dir.path().join("note.txt"), "dirty").unwrap();

        let status = repo_status(dir.path()).unwrap();

        assert!(status.dirty);
        assert_eq!(status.dirty_files, vec!["note.txt"]);
    }

    #[test]
    fn repo_id_prefers_remote_over_path() {
        let status = RepoStatus {
            repo_path: "/tmp/repo".into(),
            git_remote: Some("https://example.test/repo.git".into()),
            branch: "main".into(),
            commit: "abc".into(),
            dirty: false,
            dirty_files: Vec::new(),
        };

        assert_eq!(repo_id(&status), "https://example.test/repo.git");
    }

    #[test]
    fn dirty_file_parser_normalizes_rename_destinations() {
        let files = parse_dirty_files("R  old-name.txt -> new-name.txt\nC  old.rs -> copy.rs\n");

        assert_eq!(files, vec!["new-name.txt", "copy.rs"]);
    }

    #[test]
    fn context_excludes_claimed_tasks_from_open_tasks() {
        let repo = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        init_git_repo(repo.path());
        let store = SqliteEventStore::open(data.path().join("shuttle.db")).unwrap();
        let task = crate::task::new_task(
            "workspace".into(),
            "codex".into(),
            "session".into(),
            "ship mvp".into(),
        );
        let claim = crate::task::new_claim(
            "workspace".into(),
            "claude".into(),
            "session".into(),
            task.id,
        );
        futures_executor::block_on(store.append(task)).unwrap();
        futures_executor::block_on(store.append(claim)).unwrap();

        let context =
            futures_executor::block_on(assemble_context(&store, repo.path(), "workspace", "codex"))
                .unwrap();

        assert!(context.open_tasks.is_empty());
    }

    fn init_git_repo(path: &Path) {
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
