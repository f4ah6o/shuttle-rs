//! Runtime manifest export.
//!
//! Phase 1 emits a single generic JSON manifest (base model + weighted adapter
//! paths) that downstream tooling can adapt. Runtime-specific presets (PEFT, vLLM,
//! MLX, ...) are Phase 2 and intentionally not implemented yet.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use super::merge::MergePlan;
use super::AdapterRecord;

/// A single adapter entry in a runtime manifest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExportEntry {
    pub name: String,
    pub path: String,
    pub weight: f32,
}

/// A runtime manifest for external inference engines.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExportManifest {
    pub base_model: String,
    pub adapters: Vec<ExportEntry>,
}

/// Requested export format. Only [`ExportFormat::Json`] is implemented in Phase 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Json,
    Peft,
    Vllm,
    Mlx,
}

impl ExportFormat {
    /// Whether this format is supported in the current phase.
    pub fn is_supported(self) -> bool {
        matches!(self, ExportFormat::Json)
    }
}

impl fmt::Display for ExportFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            ExportFormat::Json => "json",
            ExportFormat::Peft => "peft",
            ExportFormat::Vllm => "vllm",
            ExportFormat::Mlx => "mlx",
        };
        f.write_str(label)
    }
}

impl FromStr for ExportFormat {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_lowercase().as_str() {
            "json" => Ok(ExportFormat::Json),
            "peft" => Ok(ExportFormat::Peft),
            "vllm" => Ok(ExportFormat::Vllm),
            "mlx" => Ok(ExportFormat::Mlx),
            other => Err(format!("unknown adapter export format: {other}")),
        }
    }
}

/// Build a runtime manifest by joining merge weights with registered adapter paths.
pub fn export_manifest(plan: &MergePlan, adapters: &[AdapterRecord]) -> ExportManifest {
    let entries = plan
        .adapters
        .iter()
        .map(|entry| ExportEntry {
            name: entry.name.clone(),
            path: path_for(&entry.name, adapters),
            weight: entry.weight,
        })
        .collect();
    ExportManifest {
        base_model: plan.base_model.clone(),
        adapters: entries,
    }
}

fn path_for(name: &str, adapters: &[AdapterRecord]) -> String {
    adapters
        .iter()
        .find(|adapter| adapter.name == name)
        .map(|adapter| adapter.path.clone())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::super::merge::MergeEntry;
    use super::*;
    use serde_json::json;

    #[test]
    fn manifest_joins_paths_by_name() {
        let plan = MergePlan {
            base_model: "base".to_owned(),
            adapters: vec![MergeEntry {
                name: "rust-cli".to_owned(),
                weight: 1.0,
            }],
        };
        let adapters = vec![AdapterRecord {
            id: "rust-cli".to_owned(),
            name: "rust-cli".to_owned(),
            base_model: "base".to_owned(),
            path: "/adapters/rust-cli".to_owned(),
            embedding: vec![],
            metadata: json!({}),
        }];
        let manifest = export_manifest(&plan, &adapters);
        assert_eq!(manifest.adapters[0].path, "/adapters/rust-cli");
    }

    #[test]
    fn format_parsing_and_support() {
        assert_eq!("json".parse::<ExportFormat>().unwrap(), ExportFormat::Json);
        assert!(ExportFormat::Json.is_supported());
        assert!(!ExportFormat::Peft.is_supported());
        assert!("bogus".parse::<ExportFormat>().is_err());
    }
}
