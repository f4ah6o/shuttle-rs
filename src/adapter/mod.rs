//! Project-aware adapter router.
//!
//! Shuttle acts as a lightweight project intelligence layer: it builds a project
//! embedding from repository context and the local event log, selects relevant
//! coding adapters (such as LoRA/PEFT adapters) from a local registry by cosine
//! similarity, produces a deterministic merge plan, and exports a runtime manifest
//! that external inference engines can consume.
//!
//! Routing is intentionally inference-free: Shuttle scores and exports adapters
//! without running a model. The [`doc2lora`] bridge is the one place Shuttle drives
//! adapter *generation*, and it does so by delegating to an external doc-to-lora
//! runner — Shuttle assembles the context document and registers the result, but
//! still never runs model inference itself.

pub mod doc2lora;
pub mod embedding;
pub mod export;
pub mod merge;
pub mod registry;
pub mod select;

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::Result;
use crate::store::SqliteEventStore;

pub use doc2lora::{
    build_context_document, run_doc2lora, CommandGenerator, ContextDocument, Doc2LoraInput,
    Doc2LoraOutcome, GenerationRequest, GenerationResult, Generator,
};
pub use embedding::{build_project_embedding, classify_project_type, ProjectEmbedding};
pub use export::{export_manifest, ExportEntry, ExportFormat, ExportManifest};
pub use merge::{merge_plan, MergeEntry, MergePlan};
pub use registry::{register_adapter, RegisterInput};
pub use select::{cosine_similarity, select_adapters, ScoredAdapter, SelectResult};

/// A coding adapter known to the local registry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdapterRecord {
    pub id: String,
    pub name: String,
    pub base_model: String,
    pub path: String,
    /// Embedding vector in the same space as the project embedding.
    pub embedding: Vec<f32>,
    /// Free-form metadata (tags, description, runtime hints, ...).
    pub metadata: Value,
}

/// A cached project embedding plus the selection it produced, keyed by
/// repo/branch/commit so repeated commands are deterministic and cheap.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectCacheEntry {
    pub repo_hash: String,
    pub branch: String,
    pub commit: String,
    pub project_embedding: Vec<f32>,
    pub selected_adapters: Vec<ScoredAdapter>,
    pub created_at: DateTime<Utc>,
}

/// A full adapter selection for a project: the embedding, the registry it was
/// scored against, and the ranked result.
#[derive(Debug, Clone)]
pub struct Selection {
    pub embedding: ProjectEmbedding,
    pub adapters: Vec<AdapterRecord>,
    pub result: SelectResult,
}

/// Build the project embedding and persist it to the cache (no selection yet).
pub async fn index_project(
    store: &SqliteEventStore,
    cwd: &Path,
    workspace_id: &str,
) -> Result<ProjectEmbedding> {
    let embedding = build_project_embedding(store, cwd, workspace_id).await?;
    store.put_project_cache(&cache_entry(&embedding, &[]))?;
    Ok(embedding)
}

/// Build the project embedding, score it against the registry, cache the
/// selection, and return everything callers need for merge/export.
pub async fn select_for_project(
    store: &SqliteEventStore,
    cwd: &Path,
    workspace_id: &str,
) -> Result<Selection> {
    let embedding = build_project_embedding(store, cwd, workspace_id).await?;
    let adapters = store.list_adapters()?;
    let scored = select_adapters(&embedding.vector, &adapters);
    store.put_project_cache(&cache_entry(&embedding, &scored))?;
    let result = SelectResult {
        project_type: embedding.project_type.clone(),
        adapters: scored,
    };
    Ok(Selection {
        embedding,
        adapters,
        result,
    })
}

fn cache_entry(embedding: &ProjectEmbedding, selected: &[ScoredAdapter]) -> ProjectCacheEntry {
    ProjectCacheEntry {
        repo_hash: embedding.repo_hash.clone(),
        branch: embedding.branch.clone(),
        commit: embedding.commit.clone(),
        project_embedding: embedding.vector.clone(),
        selected_adapters: selected.to_vec(),
        created_at: Utc::now(),
    }
}
