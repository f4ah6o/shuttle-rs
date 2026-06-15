//! Local adapter registry management.

use serde_json::json;

use super::embedding::embed_text;
use super::AdapterRecord;
use crate::core::Result;
use crate::store::SqliteEventStore;

/// Input for registering (or replacing) an adapter.
#[derive(Debug, Clone, Default)]
pub struct RegisterInput {
    /// Registry id; defaults to the adapter name when omitted.
    pub id: Option<String>,
    pub name: String,
    pub base_model: String,
    pub path: String,
    pub tags: Vec<String>,
    pub description: Option<String>,
    /// Externally produced embedding. When omitted, one is computed from the
    /// adapter's descriptive text so it lives in the same space as projects.
    pub embedding: Option<Vec<f32>>,
}

/// Register an adapter, computing its embedding from descriptive text when one
/// is not supplied, and persist it to the local registry.
pub fn register_adapter(store: &SqliteEventStore, input: RegisterInput) -> Result<AdapterRecord> {
    let id = input.id.clone().unwrap_or_else(|| input.name.clone());
    let embedding = input
        .embedding
        .clone()
        .unwrap_or_else(|| embed_text(&descriptive_text(&input)));
    let metadata = json!({
        "tags": input.tags,
        "description": input.description,
    });
    let record = AdapterRecord {
        id,
        name: input.name,
        base_model: input.base_model,
        path: input.path,
        embedding,
        metadata,
    };
    store.upsert_adapter(&record)?;
    Ok(record)
}

fn descriptive_text(input: &RegisterInput) -> String {
    let mut parts = vec![input.name.clone(), input.base_model.clone()];
    parts.extend(input.tags.iter().cloned());
    if let Some(description) = &input.description {
        parts.push(description.clone());
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> SqliteEventStore {
        let dir = tempfile::tempdir().unwrap().keep();
        SqliteEventStore::open(dir.join("shuttle.db")).unwrap()
    }

    #[test]
    fn register_computes_embedding_and_round_trips() {
        let store = store();
        let record = register_adapter(
            &store,
            RegisterInput {
                name: "rust-cli".to_owned(),
                base_model: "Qwen/Qwen2.5-Coder-7B-Instruct".to_owned(),
                path: "/adapters/rust-cli".to_owned(),
                tags: vec!["rust".to_owned(), "cli".to_owned()],
                ..RegisterInput::default()
            },
        )
        .unwrap();
        assert_eq!(record.id, "rust-cli");
        assert_eq!(record.embedding.len(), super::super::embedding::EMBED_DIM);

        let listed = store.list_adapters().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0], record);
    }

    #[test]
    fn register_is_idempotent_on_id() {
        let store = store();
        for path in ["/a", "/b"] {
            register_adapter(
                &store,
                RegisterInput {
                    name: "mcp-server".to_owned(),
                    base_model: "base".to_owned(),
                    path: path.to_owned(),
                    ..RegisterInput::default()
                },
            )
            .unwrap();
        }
        let listed = store.list_adapters().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].path, "/b");
    }
}
