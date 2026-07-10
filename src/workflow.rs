use crate::core::{Event, EventFilter, EventStore, EventType, NewEvent, Result, ShuttleError};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use uuid::Uuid;

pub const MANIFEST_FILE: &str = "shuttle.workflows.toml";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowManifest {
    pub version: u32,
    #[serde(default, rename = "workflow")]
    pub workflows: Vec<WorkflowDefinition>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowDefinition {
    pub id: String,
    pub title: String,
    pub spec: PathBuf,
    #[serde(default, rename = "step")]
    pub steps: Vec<WorkflowStep>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowStep {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub kind: StepKind,
    #[serde(default)]
    pub approval_required: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepKind {
    #[default]
    ReadOnly,
    Idempotent,
    NonIdempotent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Active,
    NeedsReconcile,
    Failed,
    Completed,
    Aborted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    Claimed,
    Completed,
    Failed,
    NeedsReconcile,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepSummary {
    pub id: String,
    pub title: String,
    pub kind: StepKind,
    pub approval_required: bool,
    pub status: StepStatus,
    pub claimed_by: Option<String>,
    pub output: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowRun {
    pub id: Uuid,
    pub workflow_id: String,
    pub title: String,
    pub spec: PathBuf,
    pub status: RunStatus,
    pub input: Value,
    pub steps: Vec<StepSummary>,
    pub current_step: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub source_event_ids: Vec<Uuid>,
}

pub fn load_manifest(repo: impl AsRef<Path>) -> Result<WorkflowManifest> {
    let repo = repo.as_ref();
    let path = repo.join(MANIFEST_FILE);
    let content = fs::read_to_string(&path)
        .map_err(|err| ShuttleError::Store(format!("failed to read {}: {err}", path.display())))?;
    let manifest: WorkflowManifest = toml::from_str(&content)
        .map_err(|err| ShuttleError::Serialization(format!("invalid {}: {err}", path.display())))?;
    validate_manifest(repo, &manifest)?;
    Ok(manifest)
}

pub fn load_manifest_if_present(repo: impl AsRef<Path>) -> Result<Option<WorkflowManifest>> {
    let repo = repo.as_ref();
    if !repo.join(MANIFEST_FILE).is_file() {
        return Ok(None);
    }
    load_manifest(repo).map(Some)
}

pub fn validate_manifest(repo: &Path, manifest: &WorkflowManifest) -> Result<()> {
    if manifest.version != 1 {
        return invalid(format!(
            "unsupported workflow manifest version {}",
            manifest.version
        ));
    }
    let mut workflow_ids = HashSet::new();
    for workflow in &manifest.workflows {
        validate_id("workflow", &workflow.id)?;
        if !workflow_ids.insert(&workflow.id) {
            return invalid(format!("duplicate workflow id: {}", workflow.id));
        }
        validate_repo_path(repo, &workflow.spec)?;
        let mut step_ids = HashSet::new();
        if workflow.steps.is_empty() {
            return invalid(format!("workflow {} has no steps", workflow.id));
        }
        for step in &workflow.steps {
            validate_id("step", &step.id)?;
            if !step_ids.insert(&step.id) {
                return invalid(format!("duplicate step id in {}: {}", workflow.id, step.id));
            }
        }
    }
    Ok(())
}

fn validate_repo_path(repo: &Path, path: &Path) -> Result<()> {
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return invalid(format!(
            "spec path must stay inside the repository: {}",
            path.display()
        ));
    }
    if !repo.join(path).is_file() {
        return invalid(format!("workflow spec does not exist: {}", path.display()));
    }
    Ok(())
}

fn validate_id(kind: &str, id: &str) -> Result<()> {
    if id.is_empty()
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return invalid(format!("invalid {kind} id: {id}"));
    }
    Ok(())
}

fn invalid<T>(message: String) -> Result<T> {
    Err(ShuttleError::Store(message))
}

pub fn definition<'a>(manifest: &'a WorkflowManifest, id: &str) -> Result<&'a WorkflowDefinition> {
    manifest
        .workflows
        .iter()
        .find(|workflow| workflow.id == id)
        .ok_or_else(|| ShuttleError::Store(format!("unknown workflow: {id}")))
}

pub fn start_event(
    workspace_id: String,
    agent: String,
    session_id: String,
    workflow: &WorkflowDefinition,
    input: Value,
) -> Event {
    let run_id = Uuid::new_v4();
    workflow_event(
        workspace_id,
        agent,
        session_id,
        run_id,
        "started",
        format!("started workflow {}", workflow.id),
        json!({
            "workflow_id": workflow.id,
            "title": workflow.title,
            "spec": workflow.spec,
            "input": input,
            "steps": workflow.steps,
        }),
    )
}

pub fn action_event(
    workspace_id: String,
    agent: String,
    session_id: String,
    run_id: Uuid,
    action: &str,
    step_id: Option<&str>,
    value: Option<Value>,
) -> Event {
    let mut details = json!({});
    if let Some(step_id) = step_id {
        details["step_id"] = json!(step_id);
    }
    if let Some(value) = value {
        details["value"] = value;
    }
    workflow_event(
        workspace_id,
        agent,
        session_id,
        run_id,
        action,
        format!("workflow {run_id}: {action}"),
        details,
    )
}

fn workflow_event(
    workspace_id: String,
    agent: String,
    session_id: String,
    run_id: Uuid,
    action: &str,
    content: String,
    details: Value,
) -> Event {
    let mut metadata = json!({ "action": action, "run_id": run_id });
    if let (Some(target), Some(source)) = (metadata.as_object_mut(), details.as_object()) {
        target.extend(source.clone());
    }
    Event::new(NewEvent {
        event_type: EventType::Workflow,
        workspace_id,
        repo_id: None,
        repo_path: None,
        git_remote: None,
        bit_repo_id: None,
        branch: None,
        commit: None,
        repo_dirty: None,
        agent,
        session_id,
        title: Some("workflow".to_owned()),
        content,
        tags: vec![
            format!("workflow_run:{run_id}"),
            format!("workflow:{action}"),
        ],
        metadata_json: metadata,
    })
}

pub async fn runs(store: &impl EventStore, workspace_id: &str) -> Result<Vec<WorkflowRun>> {
    let events = store
        .list(EventFilter {
            event_type: Some(EventType::Workflow),
            workspace_id: Some(workspace_id.to_owned()),
            limit: Some(u32::MAX),
            ..EventFilter::default()
        })
        .await?;
    project_runs(events)
}

pub async fn run(store: &impl EventStore, workspace_id: &str, id: Uuid) -> Result<WorkflowRun> {
    runs(store, workspace_id)
        .await?
        .into_iter()
        .find(|run| run.id == id)
        .ok_or_else(|| ShuttleError::Store(format!("workflow run not found: {id}")))
}

pub async fn active_runs(store: &impl EventStore, workspace_id: &str) -> Result<Vec<WorkflowRun>> {
    Ok(runs(store, workspace_id)
        .await?
        .into_iter()
        .filter(|run| {
            matches!(
                run.status,
                RunStatus::Active | RunStatus::NeedsReconcile | RunStatus::Failed
            )
        })
        .collect())
}

fn project_runs(mut events: Vec<Event>) -> Result<Vec<WorkflowRun>> {
    events.sort_by_key(|event| (event.created_at, event.id));
    let mut runs = HashMap::<Uuid, WorkflowRun>::new();
    for event in events {
        let run_id = metadata_uuid(&event, "run_id")?;
        let action = metadata_string(&event, "action")?;
        if action == "started" {
            let steps: Vec<WorkflowStep> =
                serde_json::from_value(event.metadata_json["steps"].clone())
                    .map_err(|err| ShuttleError::Serialization(err.to_string()))?;
            runs.insert(
                run_id,
                WorkflowRun {
                    id: run_id,
                    workflow_id: metadata_string(&event, "workflow_id")?.to_owned(),
                    title: metadata_string(&event, "title")?.to_owned(),
                    spec: PathBuf::from(metadata_string(&event, "spec")?),
                    status: RunStatus::Active,
                    input: event.metadata_json["input"].clone(),
                    steps: steps
                        .into_iter()
                        .map(|step| StepSummary {
                            id: step.id,
                            title: step.title,
                            kind: step.kind,
                            approval_required: step.approval_required,
                            status: StepStatus::Pending,
                            claimed_by: None,
                            output: None,
                        })
                        .collect(),
                    current_step: None,
                    created_at: event.created_at,
                    updated_at: event.created_at,
                    source_event_ids: vec![event.id],
                },
            );
            continue;
        }
        let run = runs.get_mut(&run_id).ok_or_else(|| {
            ShuttleError::Store(format!("workflow action before start: {run_id}"))
        })?;
        apply_action(run, &event, action)?;
    }
    let mut runs = runs.into_values().collect::<Vec<_>>();
    runs.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then(left.id.cmp(&right.id))
    });
    Ok(runs)
}

fn apply_action(run: &mut WorkflowRun, event: &Event, action: &str) -> Result<()> {
    run.updated_at = event.created_at;
    run.source_event_ids.push(event.id);
    match action {
        "claimed" | "taken_over" | "completed_step" | "failed_step" | "reconciled" => {
            let step_id = metadata_string(event, "step_id")?;
            let step = run
                .steps
                .iter_mut()
                .find(|step| step.id == step_id)
                .ok_or_else(|| ShuttleError::Store(format!("unknown workflow step: {step_id}")))?;
            match action {
                "claimed" | "taken_over" => {
                    if action == "taken_over"
                        && step.status == StepStatus::Claimed
                        && step.kind == StepKind::NonIdempotent
                    {
                        step.status = StepStatus::NeedsReconcile;
                        run.status = RunStatus::NeedsReconcile;
                    } else {
                        step.status = StepStatus::Claimed;
                    }
                    step.claimed_by = Some(event.agent.clone());
                    run.current_step = Some(step_id.to_owned());
                }
                "completed_step" => {
                    step.status = StepStatus::Completed;
                    step.output = event.metadata_json.get("value").cloned();
                    run.current_step = None;
                    run.status = RunStatus::Active;
                }
                "failed_step" => {
                    step.status = StepStatus::Failed;
                    step.output = event.metadata_json.get("value").cloned();
                    run.status = RunStatus::Failed;
                }
                "reconciled" => {
                    step.status = StepStatus::Completed;
                    step.output = event.metadata_json.get("value").cloned();
                    run.current_step = None;
                    run.status = RunStatus::Active;
                }
                _ => unreachable!(),
            }
        }
        "completed" => run.status = RunStatus::Completed,
        "aborted" => run.status = RunStatus::Aborted,
        other => {
            return Err(ShuttleError::Store(format!(
                "unknown workflow action: {other}"
            )))
        }
    }
    Ok(())
}

fn metadata_string<'a>(event: &'a Event, key: &str) -> Result<&'a str> {
    event.metadata_json[key].as_str().ok_or_else(|| {
        ShuttleError::Serialization(format!("workflow event {} has invalid {key}", event.id))
    })
}

fn metadata_uuid(event: &Event, key: &str) -> Result<Uuid> {
    Uuid::parse_str(metadata_string(event, key)?)
        .map_err(|err| ShuttleError::Serialization(err.to_string()))
}

pub fn validate_claim(run: &WorkflowRun, step_id: &str, takeover: bool) -> Result<()> {
    if !matches!(
        run.status,
        RunStatus::Active | RunStatus::Failed | RunStatus::NeedsReconcile
    ) {
        return invalid(format!("workflow run {} is not active", run.id));
    }
    let expected = run
        .steps
        .iter()
        .find(|step| step.status != StepStatus::Completed)
        .ok_or_else(|| ShuttleError::Store("all workflow steps are completed".to_owned()))?;
    if expected.id != step_id {
        return invalid(format!(
            "next workflow step is {}, not {step_id}",
            expected.id
        ));
    }
    match expected.status {
        StepStatus::Pending | StepStatus::Failed => Ok(()),
        StepStatus::Claimed if takeover => Ok(()),
        StepStatus::Claimed => invalid(format!("workflow step {step_id} is already claimed")),
        StepStatus::NeedsReconcile => {
            invalid(format!("workflow step {step_id} needs reconciliation"))
        }
        StepStatus::Completed => unreachable!(),
    }
}

pub fn validate_complete(
    run: &WorkflowRun,
    step_id: &str,
    agent: &str,
    approval: Option<&str>,
) -> Result<()> {
    let step = validate_step_claimed(run, step_id)?;
    if step.claimed_by.as_deref() != Some(agent) {
        return invalid(format!(
            "workflow step {step_id} is claimed by {}; use takeover before completing it",
            step.claimed_by.as_deref().unwrap_or("unknown")
        ));
    }
    if step.approval_required && approval.is_none_or(|approval| approval.trim().is_empty()) {
        return invalid(format!(
            "workflow step {step_id} requires approval evidence"
        ));
    }
    Ok(())
}

pub fn validate_step_claimed<'a>(run: &'a WorkflowRun, step_id: &str) -> Result<&'a StepSummary> {
    let step = run
        .steps
        .iter()
        .find(|step| step.id == step_id)
        .ok_or_else(|| ShuttleError::Store(format!("unknown workflow step: {step_id}")))?;
    if step.status != StepStatus::Claimed {
        return invalid(format!("workflow step {step_id} is not claimed"));
    }
    Ok(step)
}

pub fn validate_run_complete(run: &WorkflowRun) -> Result<()> {
    if run
        .steps
        .iter()
        .any(|step| step.status != StepStatus::Completed)
    {
        return invalid("workflow still has incomplete steps".to_owned());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::SqliteEventStore;

    fn workflow() -> WorkflowDefinition {
        WorkflowDefinition {
            id: "daily-triage".into(),
            title: "Daily triage".into(),
            spec: "docs/spec.md".into(),
            steps: vec![
                WorkflowStep {
                    id: "read".into(),
                    title: "Read".into(),
                    kind: StepKind::ReadOnly,
                    approval_required: false,
                },
                WorkflowStep {
                    id: "write".into(),
                    title: "Write".into(),
                    kind: StepKind::NonIdempotent,
                    approval_required: true,
                },
            ],
        }
    }

    #[test]
    fn manifest_rejects_paths_outside_repository() {
        let repo = tempfile::tempdir().unwrap();
        let manifest = WorkflowManifest {
            version: 1,
            workflows: vec![WorkflowDefinition {
                id: "unsafe".into(),
                title: "Unsafe".into(),
                spec: "../secret.md".into(),
                steps: vec![WorkflowStep {
                    id: "read".into(),
                    title: "Read".into(),
                    kind: StepKind::ReadOnly,
                    approval_required: false,
                }],
            }],
        };

        assert!(validate_manifest(repo.path(), &manifest).is_err());
    }

    #[test]
    fn manifest_loads_repository_spec_and_steps() {
        let repo = tempfile::tempdir().unwrap();
        fs::create_dir(repo.path().join("docs")).unwrap();
        fs::write(repo.path().join("docs/spec.md"), "workflow").unwrap();
        fs::write(
            repo.path().join(MANIFEST_FILE),
            r#"
version = 1
[[workflow]]
id = "daily"
title = "Daily"
spec = "docs/spec.md"
[[workflow.step]]
id = "read"
title = "Read"
kind = "read_only"
"#,
        )
        .unwrap();

        let manifest = load_manifest(repo.path()).unwrap();
        assert_eq!(manifest.workflows[0].steps[0].id, "read");
    }

    #[test]
    fn projects_run_across_agents() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteEventStore::open(dir.path().join("shuttle.db")).unwrap();
        let start = start_event(
            "ws".into(),
            "claude".into(),
            "s1".into(),
            &workflow(),
            json!({}),
        );
        let run_id = metadata_uuid(&start, "run_id").unwrap();
        futures_executor::block_on(store.append(start)).unwrap();
        let claim = action_event(
            "ws".into(),
            "claude".into(),
            "s1".into(),
            run_id,
            "claimed",
            Some("read"),
            None,
        );
        futures_executor::block_on(store.append(claim)).unwrap();
        let done = action_event(
            "ws".into(),
            "codex".into(),
            "s2".into(),
            run_id,
            "completed_step",
            Some("read"),
            Some(json!({"ok": true})),
        );
        futures_executor::block_on(store.append(done)).unwrap();

        let run = futures_executor::block_on(run(&store, "ws", run_id)).unwrap();
        assert_eq!(run.steps[0].status, StepStatus::Completed);
        assert_eq!(run.steps[1].status, StepStatus::Pending);
    }

    #[test]
    fn taking_over_non_idempotent_step_requires_reconciliation() {
        let mut run = WorkflowRun {
            id: Uuid::new_v4(),
            workflow_id: "x".into(),
            title: "x".into(),
            spec: "x".into(),
            status: RunStatus::Active,
            input: json!({}),
            current_step: Some("write".into()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            source_event_ids: vec![],
            steps: vec![StepSummary {
                id: "write".into(),
                title: "Write".into(),
                kind: StepKind::NonIdempotent,
                approval_required: true,
                status: StepStatus::Claimed,
                claimed_by: Some("claude".into()),
                output: None,
            }],
        };
        let event = action_event(
            "ws".into(),
            "codex".into(),
            "s2".into(),
            run.id,
            "taken_over",
            Some("write"),
            None,
        );
        apply_action(&mut run, &event, "taken_over").unwrap();
        assert_eq!(run.status, RunStatus::NeedsReconcile);
        assert_eq!(run.steps[0].status, StepStatus::NeedsReconcile);
    }

    #[test]
    fn approval_required_step_rejects_empty_evidence() {
        let run = WorkflowRun {
            id: Uuid::new_v4(),
            workflow_id: "x".into(),
            title: "x".into(),
            spec: "x".into(),
            status: RunStatus::Active,
            input: json!({}),
            current_step: Some("approve".into()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            source_event_ids: vec![],
            steps: vec![StepSummary {
                id: "approve".into(),
                title: "Approve".into(),
                kind: StepKind::ReadOnly,
                approval_required: true,
                status: StepStatus::Claimed,
                claimed_by: Some("codex".into()),
                output: None,
            }],
        };

        assert!(validate_complete(&run, "approve", "codex", None).is_err());
        assert!(
            validate_complete(&run, "approve", "claude", Some("user approved payload")).is_err()
        );
        assert!(validate_complete(&run, "approve", "codex", Some("user approved payload")).is_ok());
    }
}
