//! doc-to-lora generation bridge.
//!
//! Unlike the rest of the adapter router — which only *routes* to adapters that
//! already exist — this module drives an external [doc-to-lora] generator to
//! produce a fresh LoRA adapter from the current project's context, then registers
//! it into the local registry so the router can select, merge, and export it.
//!
//! Shuttle still never runs model inference itself. It assembles a context
//! document from repository metadata and the local event log, hands it to a
//! configurable external runner (a doc-to-lora CLI or service), and consumes the
//! manifest the runner reports back.
//!
//! [doc-to-lora]: https://github.com/SakanaAI/doc-to-lora

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use super::embedding::{classify_project_type, embed_text, tokenize};
use super::registry::{register_adapter, RegisterInput};
use super::AdapterRecord;
use crate::context::{repo_id, repo_status};
use crate::core::{EventFilter, EventStore, EventType, Result, ShuttleError};
use crate::store::SqliteEventStore;

/// Default runner program when neither `--runner` nor the environment override is set.
pub const DEFAULT_RUNNER: &str = "doc2lora";

/// Environment variable holding the doc-to-lora runner program path.
pub const RUNNER_ENV: &str = "SHUTTLE_DOC2LORA_RUNNER";

/// File name of the context document written into the output directory.
pub const DOCUMENT_FILE: &str = "context.md";

/// Event-log sections, in document order, that carry durable project knowledge.
const DOC_SECTIONS: [(EventType, &str); 8] = [
    (EventType::Decision, "Decisions"),
    (EventType::Fact, "Facts"),
    (EventType::Pattern, "Patterns"),
    (EventType::Observation, "Observations"),
    (EventType::Bug, "Bugs"),
    (EventType::Task, "Tasks"),
    (EventType::Handoff, "Handoffs"),
    (EventType::Memory, "Memories"),
];

/// User-supplied inputs for a doc-to-lora generation run.
#[derive(Debug, Clone)]
pub struct Doc2LoraInput {
    /// Name (and default registry id) for the generated adapter.
    pub name: String,
    /// Base model the adapter targets.
    pub base_model: String,
    /// Directory the generator writes the adapter (and context document) into.
    pub out_dir: PathBuf,
    /// Runner program override; falls back to `SHUTTLE_DOC2LORA_RUNNER`, then `doc2lora`.
    pub runner: Option<String>,
    /// Tags recorded on the registered adapter.
    pub tags: Vec<String>,
    /// Optional focus query that biases and annotates the context document.
    pub focus: Option<String>,
}

/// A context document assembled from repository metadata and the event log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextDocument {
    pub repo: String,
    pub branch: String,
    pub commit: String,
    pub project_type: String,
    pub text: String,
}

/// Request handed to a [`Generator`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GenerationRequest {
    pub name: String,
    pub base_model: String,
    pub document_path: PathBuf,
    pub output_dir: PathBuf,
}

/// Manifest a generator reports after producing adapter weights.
///
/// `base_model` and `name` are optional so a runner can echo back the requested
/// values or override them; missing fields fall back to the request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GenerationResult {
    /// Filesystem path to the produced adapter.
    pub path: String,
    #[serde(default)]
    pub base_model: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

/// Anything that can turn a [`GenerationRequest`] into adapter weights.
pub trait Generator {
    fn generate(&self, request: &GenerationRequest) -> Result<GenerationResult>;
}

/// A [`Generator`] backed by an external doc-to-lora command.
///
/// Invokes `<program> generate --base-model <m> --document <doc> --output <dir>
/// --name <name>` and parses a [`GenerationResult`] from the command's stdout.
#[derive(Debug, Clone)]
pub struct CommandGenerator {
    pub program: String,
}

impl CommandGenerator {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
        }
    }

    /// Resolve the runner program from input, then environment, then the default.
    pub fn from_input(input: &Doc2LoraInput) -> Self {
        let program = input
            .runner
            .clone()
            .or_else(|| std::env::var(RUNNER_ENV).ok())
            .unwrap_or_else(|| DEFAULT_RUNNER.to_owned());
        Self::new(program)
    }
}

impl Generator for CommandGenerator {
    fn generate(&self, request: &GenerationRequest) -> Result<GenerationResult> {
        let output = Command::new(&self.program)
            .arg("generate")
            .arg("--base-model")
            .arg(&request.base_model)
            .arg("--document")
            .arg(&request.document_path)
            .arg("--output")
            .arg(&request.output_dir)
            .arg("--name")
            .arg(&request.name)
            .output()
            .map_err(|err| {
                ShuttleError::Store(format!(
                    "failed to run doc-to-lora runner '{}': {err}",
                    self.program
                ))
            })?;
        if !output.status.success() {
            return Err(ShuttleError::Store(format!(
                "doc-to-lora runner '{}' failed: {}",
                self.program,
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        serde_json::from_str(stdout.trim()).map_err(|err| {
            ShuttleError::Serialization(format!(
                "doc-to-lora runner '{}' did not return a valid manifest: {err}",
                self.program
            ))
        })
    }
}

/// Everything a doc-to-lora run produced, for reporting back to the caller.
#[derive(Debug, Clone, Serialize)]
pub struct Doc2LoraOutcome {
    pub document: ContextDocument,
    pub request: GenerationRequest,
    pub result: GenerationResult,
    pub record: AdapterRecord,
}

/// Build a context document from repository metadata and durable event-log knowledge.
pub async fn build_context_document(
    store: &impl EventStore,
    cwd: &Path,
    workspace_id: &str,
    focus: Option<&str>,
) -> Result<ContextDocument> {
    let status = repo_status(cwd)?;
    let identity = repo_id(&status);

    let mut text = String::new();
    let mut tokens: Vec<String> = Vec::new();

    let _ = writeln!(text, "# Project Context: {}", status.repo_path);
    let _ = writeln!(text, "Branch: {}", status.branch);
    let _ = writeln!(text, "Commit: {}", status.commit);
    if let Some(focus) = focus {
        let _ = writeln!(text, "Focus: {focus}");
        tokens.extend(tokenize(focus));
    }
    tokens.extend(tokenize(&status.branch));
    tokens.extend(tokenize(&identity));

    for (event_type, heading) in DOC_SECTIONS {
        let events = store
            .list(EventFilter {
                event_type: Some(event_type),
                workspace_id: Some(workspace_id.to_owned()),
                limit: Some(50),
                ..EventFilter::default()
            })
            .await?;
        if events.is_empty() {
            continue;
        }
        let _ = writeln!(text, "\n## {heading}");
        for event in events {
            match &event.title {
                Some(title) if !title.is_empty() => {
                    let _ = writeln!(text, "- {title}: {}", event.content);
                    tokens.extend(tokenize(title));
                }
                _ => {
                    let _ = writeln!(text, "- {}", event.content);
                }
            }
            tokens.extend(tokenize(&event.content));
            for tag in &event.tags {
                tokens.extend(tokenize(tag));
            }
        }
    }

    Ok(ContextDocument {
        repo: status.repo_path,
        branch: status.branch,
        commit: status.commit,
        project_type: classify_project_type(&tokens),
        text,
    })
}

/// Drive a doc-to-lora generator end to end: assemble the context document, write
/// it to the output directory, invoke the generator, and register the produced
/// adapter into the local registry so the router can select it immediately.
pub async fn run_doc2lora<G: Generator>(
    store: &SqliteEventStore,
    generator: &G,
    cwd: &Path,
    workspace_id: &str,
    input: &Doc2LoraInput,
) -> Result<Doc2LoraOutcome> {
    let document = build_context_document(store, cwd, workspace_id, input.focus.as_deref()).await?;

    std::fs::create_dir_all(&input.out_dir).map_err(|err| {
        ShuttleError::Store(format!(
            "failed to create output directory {}: {err}",
            input.out_dir.display()
        ))
    })?;
    let document_path = input.out_dir.join(DOCUMENT_FILE);
    std::fs::write(&document_path, &document.text).map_err(|err| {
        ShuttleError::Store(format!(
            "failed to write context document {}: {err}",
            document_path.display()
        ))
    })?;

    let request = GenerationRequest {
        name: input.name.clone(),
        base_model: input.base_model.clone(),
        document_path,
        output_dir: input.out_dir.clone(),
    };
    let result = generator.generate(&request)?;

    let base_model = result
        .base_model
        .clone()
        .unwrap_or_else(|| input.base_model.clone());
    let name = result.name.clone().unwrap_or_else(|| input.name.clone());

    // Embed the source document so the generated adapter lives in the same space
    // as the project embedding and is selectable by the router right away.
    let record = register_adapter(
        store,
        RegisterInput {
            id: None,
            name,
            base_model,
            path: result.path.clone(),
            tags: input.tags.clone(),
            description: Some(format!(
                "Generated by doc-to-lora from {} project context",
                document.project_type
            )),
            embedding: Some(embed_text(&document.text)),
        },
    )?;

    Ok(Doc2LoraOutcome {
        document,
        request,
        result,
        record,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;

    use super::*;
    use crate::core::{EventStore, EventType};
    use crate::memory::new_typed_memory;

    fn init_git_repo(path: &Path) {
        Command::new("git")
            .arg("init")
            .current_dir(path)
            .output()
            .unwrap();
        fs::write(path.join("Cargo.toml"), "[package]\nname=\"demo\"").unwrap();
        Command::new("git")
            .args(["add", "Cargo.toml"])
            .current_dir(path)
            .output()
            .unwrap();
        Command::new("git")
            .args([
                "-c",
                "user.name=Shuttle Test",
                "-c",
                "user.email=shuttle@example.test",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-m",
                "initial",
            ])
            .current_dir(path)
            .output()
            .unwrap();
    }

    fn store() -> SqliteEventStore {
        let dir = tempfile::tempdir().unwrap().keep();
        SqliteEventStore::open(dir.join("shuttle.db")).unwrap()
    }

    fn seed(store: &SqliteEventStore, event_type: EventType, content: &str) {
        let event = new_typed_memory(
            event_type,
            "workspace".into(),
            "codex".into(),
            "session".into(),
            content.into(),
        );
        futures_executor::block_on(store.append(event)).unwrap();
    }

    /// Generator that records the request and emits a manifest, mimicking a runner.
    struct FakeGenerator {
        path: String,
    }

    impl Generator for FakeGenerator {
        fn generate(&self, request: &GenerationRequest) -> Result<GenerationResult> {
            // A real runner reads the document; assert it was written for us.
            assert!(request.document_path.exists(), "document should be written");
            Ok(GenerationResult {
                path: self.path.clone(),
                base_model: None,
                name: None,
            })
        }
    }

    #[test]
    fn document_includes_repo_metadata_and_event_sections() {
        let repo = tempfile::tempdir().unwrap();
        init_git_repo(repo.path());
        let store = store();
        seed(&store, EventType::Decision, "use sqlite as the event store");
        seed(&store, EventType::Fact, "the event log is append-only");

        let document = futures_executor::block_on(build_context_document(
            &store,
            repo.path(),
            "workspace",
            Some("routing"),
        ))
        .unwrap();

        assert!(document.text.contains("# Project Context:"));
        assert!(document.text.contains("Focus: routing"));
        assert!(document.text.contains("## Decisions"));
        assert!(document.text.contains("use sqlite as the event store"));
        assert!(document.text.contains("## Facts"));
        // Empty sections are omitted.
        assert!(!document.text.contains("## Bugs"));
    }

    #[test]
    fn run_registers_generated_adapter() {
        let repo = tempfile::tempdir().unwrap();
        init_git_repo(repo.path());
        let out = tempfile::tempdir().unwrap();
        let store = store();
        seed(&store, EventType::Decision, "rust cli sqlite mcp service");

        let generator = FakeGenerator {
            path: out.path().join("adapter").display().to_string(),
        };
        let input = Doc2LoraInput {
            name: "project-lora".into(),
            base_model: "Qwen/Qwen2.5-Coder-7B-Instruct".into(),
            out_dir: out.path().to_path_buf(),
            runner: None,
            tags: vec!["generated".into()],
            focus: None,
        };

        let outcome = futures_executor::block_on(run_doc2lora(
            &store,
            &generator,
            repo.path(),
            "workspace",
            &input,
        ))
        .unwrap();

        // Context document was written for the runner.
        assert!(out.path().join(DOCUMENT_FILE).exists());
        // Result falls back to requested base model / name.
        assert_eq!(outcome.record.name, "project-lora");
        assert_eq!(outcome.record.base_model, "Qwen/Qwen2.5-Coder-7B-Instruct");
        assert_eq!(outcome.record.path, outcome.result.path);
        assert_eq!(
            outcome.record.embedding.len(),
            super::super::embedding::EMBED_DIM
        );

        // The adapter is now in the registry and routable.
        let listed = store.list_adapters().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "project-lora");
    }

    #[test]
    fn command_generator_resolves_program_precedence() {
        let input = Doc2LoraInput {
            name: "n".into(),
            base_model: "m".into(),
            out_dir: PathBuf::from("/tmp/x"),
            runner: Some("/usr/bin/custom-runner".into()),
            tags: vec![],
            focus: None,
        };
        assert_eq!(
            CommandGenerator::from_input(&input).program,
            "/usr/bin/custom-runner"
        );
    }
}
