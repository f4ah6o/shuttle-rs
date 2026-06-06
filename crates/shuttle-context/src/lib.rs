use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use shuttle_core::{Event, EventFilter, EventStore, EventType, Result, ShuttleError};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Context {
    pub repo: String,
    pub branch: String,
    pub commit: String,
    pub git_remote: Option<String>,
    pub dirty: bool,
    pub dirty_files: Vec<String>,
    pub open_tasks: Vec<Event>,
    pub recent_decisions: Vec<Event>,
    pub related_memories: Vec<Event>,
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
    let open_tasks = store
        .list(EventFilter {
            event_type: Some(EventType::Task),
            workspace_id: Some(workspace_id.to_owned()),
            tag: Some("task:open".to_owned()),
            limit: Some(20),
            ..EventFilter::default()
        })
        .await?;
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
    let inbox = inbox_events(store, workspace_id, agent).await?;

    Ok(Context {
        repo: status.repo_path,
        branch: status.branch,
        commit: status.commit,
        git_remote: status.git_remote,
        dirty: status.dirty,
        dirty_files: status.dirty_files,
        open_tasks,
        recent_decisions,
        related_memories,
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

#[cfg(test)]
mod tests {
    use super::*;
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
