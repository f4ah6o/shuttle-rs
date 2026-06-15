//! Deterministic, dependency-free project embeddings.
//!
//! The project embedding is a feature-hashed bag-of-words over repository metadata
//! and selected Shuttle event-log signals. It is fully deterministic (no network,
//! no model) so adapter selection is reproducible and testable. Callers that have
//! an externally produced embedding can bypass this and supply a vector directly.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::context::{repo_id, repo_status};
use crate::core::{EventFilter, EventStore, EventType, Result};

/// Dimensionality of every embedding vector (project and adapter).
pub const EMBED_DIM: usize = 256;

/// Event types that carry useful project signal for routing.
const SIGNAL_TYPES: [EventType; 8] = [
    EventType::Observation,
    EventType::Decision,
    EventType::Fact,
    EventType::Bug,
    EventType::Task,
    EventType::Handoff,
    EventType::Memory,
    EventType::Pattern,
];

/// A computed project embedding plus the repo coordinates it was built from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectEmbedding {
    pub repo: String,
    pub repo_hash: String,
    pub branch: String,
    pub commit: String,
    pub project_type: String,
    pub vector: Vec<f32>,
}

/// Stable hash of a repository identity, used as a cache key.
pub fn repo_hash(repo_identity: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(repo_identity.as_bytes());
    hex(&hasher.finalize())
}

/// Tokenize free text: lowercase, split on non-alphanumeric, drop short tokens.
pub fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|token| token.len() >= 2)
        .map(|token| token.to_lowercase())
        .collect()
}

/// Embed already-tokenized text via feature hashing, then L2-normalize.
pub fn embed_tokens(tokens: &[String]) -> Vec<f32> {
    let mut vector = vec![0.0f32; EMBED_DIM];
    for token in tokens {
        let index = (token_hash(token) % EMBED_DIM as u64) as usize;
        vector[index] += 1.0;
    }
    l2_normalize(&mut vector);
    vector
}

/// Embed free text (convenience over [`tokenize`] + [`embed_tokens`]).
pub fn embed_text(text: &str) -> Vec<f32> {
    embed_tokens(&tokenize(text))
}

/// Build a project embedding from repo metadata and the local event log.
pub async fn build_project_embedding(
    store: &impl EventStore,
    cwd: &Path,
    workspace_id: &str,
) -> Result<ProjectEmbedding> {
    let status = repo_status(cwd)?;
    let identity = repo_id(&status);

    let mut tokens: Vec<String> = Vec::new();
    // Repository structure: tracked file paths (components + extensions).
    for path in git_tracked_files(&status.repo_path) {
        tokens.extend(tokenize(&path));
    }
    // Git metadata.
    tokens.extend(tokenize(&status.branch));
    tokens.extend(tokenize(&identity));

    // Event-log signals.
    for event_type in SIGNAL_TYPES {
        let events = store
            .list(EventFilter {
                event_type: Some(event_type),
                workspace_id: Some(workspace_id.to_owned()),
                limit: Some(50),
                ..EventFilter::default()
            })
            .await?;
        for event in events {
            if let Some(title) = &event.title {
                tokens.extend(tokenize(title));
            }
            tokens.extend(tokenize(&event.content));
            for tag in &event.tags {
                tokens.extend(tokenize(tag));
            }
        }
    }

    let project_type = classify_project_type(&tokens);
    let vector = embed_tokens(&tokens);

    Ok(ProjectEmbedding {
        repo: status.repo_path,
        repo_hash: repo_hash(&identity),
        branch: status.branch,
        commit: status.commit,
        project_type,
        vector,
    })
}

/// Best-effort hyphenated project label, e.g. `rust-mcp-service`.
pub fn classify_project_type(tokens: &[String]) -> String {
    let set: BTreeSet<&str> = tokens.iter().map(String::as_str).collect();
    let present = |candidates: &[&str]| candidates.iter().any(|c| set.contains(c));

    // Language detection (first match wins, fixed priority).
    let language = [
        (&["rs", "cargo", "rust"][..], "rust"),
        (&["mbt", "moonbit"][..], "moonbit"),
        (&["ts", "tsx"][..], "typescript"),
        (&["js", "jsx", "node"][..], "javascript"),
        (&["py", "python"][..], "python"),
        (&["go", "golang"][..], "go"),
    ]
    .into_iter()
    .find_map(|(markers, label)| present(markers).then_some(label));

    // Role detection (fixed priority order, deduplicated).
    let roles: Vec<&str> = [
        (&["mcp"][..], "mcp"),
        (&["cli"][..], "cli"),
        (&["axum", "http", "api", "server", "service"][..], "service"),
        (&["gateway"][..], "gateway"),
        (&["cloudflare", "worker", "workers"][..], "worker"),
        (&["sqlite"][..], "sqlite"),
    ]
    .into_iter()
    .filter_map(|(markers, label)| present(markers).then_some(label))
    .collect();

    let mut parts: Vec<&str> = Vec::new();
    if let Some(language) = language {
        parts.push(language);
    }
    parts.extend(roles);
    if parts.is_empty() {
        "unknown".to_owned()
    } else {
        parts.join("-")
    }
}

fn git_tracked_files(repo_path: &str) -> Vec<String> {
    let output = Command::new("git")
        .args(["ls-files"])
        .current_dir(Path::new(repo_path))
        .output();
    match output {
        Ok(output) if output.status.success() => String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|line| line.to_owned())
            .collect(),
        _ => Vec::new(),
    }
}

fn token_hash(token: &str) -> u64 {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(bytes)
}

fn l2_normalize(vector: &mut [f32]) {
    let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > 0.0 {
        for value in vector.iter_mut() {
            *value /= norm;
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedding_is_deterministic_and_normalized() {
        let a = embed_text("rust cli sqlite event log");
        let b = embed_text("rust cli sqlite event log");
        assert_eq!(a, b);
        let norm = a.iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "norm was {norm}");
    }

    #[test]
    fn empty_text_embeds_to_zero_vector() {
        let v = embed_text("");
        assert_eq!(v.len(), EMBED_DIM);
        assert!(v.iter().all(|value| *value == 0.0));
    }

    #[test]
    fn classify_detects_rust_mcp_service() {
        let tokens = tokenize("main rs cargo toml mcp server axum handler");
        assert_eq!(classify_project_type(&tokens), "rust-mcp-service");
    }

    #[test]
    fn classify_falls_back_to_unknown() {
        let tokens = tokenize("readme license notes");
        assert_eq!(classify_project_type(&tokens), "unknown");
    }
}
