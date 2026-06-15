//! Deterministic adapter weighting and merge planning.

use serde::{Deserialize, Serialize};

use super::select::{round2, ScoredAdapter};
use super::AdapterRecord;

/// A single adapter entry in a merge plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MergeEntry {
    pub name: String,
    pub weight: f32,
}

/// A merge plan: the base model plus weighted adapters that sum to 1.0.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MergePlan {
    pub base_model: String,
    pub adapters: Vec<MergeEntry>,
}

/// Build a deterministic merge plan from a ranked selection.
///
/// Keeps the top `top_k` adapters whose score is at least `min_score` (and above
/// zero), then normalizes their scores into weights summing to exactly 1.0. The
/// base model is taken from the highest-ranked retained adapter. Weights are
/// rounded to two decimals with any rounding residual folded into the top entry.
pub fn merge_plan(
    selected: &[ScoredAdapter],
    adapters: &[AdapterRecord],
    top_k: usize,
    min_score: f32,
) -> MergePlan {
    let retained: Vec<&ScoredAdapter> = selected
        .iter()
        .filter(|adapter| adapter.score > 0.0 && adapter.score >= min_score)
        .take(top_k)
        .collect();

    let base_model = retained
        .first()
        .and_then(|top| base_model_for(&top.name, adapters))
        .unwrap_or_default();

    let total: f32 = retained.iter().map(|adapter| adapter.score).sum();
    let mut entries: Vec<MergeEntry> = Vec::with_capacity(retained.len());
    if total > 0.0 {
        for adapter in &retained {
            entries.push(MergeEntry {
                name: adapter.name.clone(),
                weight: round2(adapter.score / total),
            });
        }
        // Fold the rounding residual into the top (first) entry so weights sum to 1.0.
        let rounded_total: f32 = entries.iter().map(|entry| entry.weight).sum();
        if let Some(first) = entries.first_mut() {
            first.weight = round2(first.weight + (1.0 - rounded_total));
        }
    }

    MergePlan {
        base_model,
        adapters: entries,
    }
}

fn base_model_for(name: &str, adapters: &[AdapterRecord]) -> Option<String> {
    adapters
        .iter()
        .find(|adapter| adapter.name == name)
        .map(|adapter| adapter.base_model.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn adapter(name: &str) -> AdapterRecord {
        AdapterRecord {
            id: name.to_owned(),
            name: name.to_owned(),
            base_model: "Qwen/Qwen2.5-Coder-7B-Instruct".to_owned(),
            path: format!("/adapters/{name}"),
            embedding: vec![],
            metadata: json!({}),
        }
    }

    fn scored(name: &str, score: f32) -> ScoredAdapter {
        ScoredAdapter {
            name: name.to_owned(),
            score,
        }
    }

    #[test]
    fn weights_sum_to_one_and_respect_top_k() {
        let selected = vec![
            scored("a", 0.9),
            scored("b", 0.6),
            scored("c", 0.3),
            scored("d", 0.1),
        ];
        let adapters = vec![adapter("a"), adapter("b"), adapter("c"), adapter("d")];
        let plan = merge_plan(&selected, &adapters, 3, 0.0);
        assert_eq!(plan.adapters.len(), 3);
        assert_eq!(plan.base_model, "Qwen/Qwen2.5-Coder-7B-Instruct");
        let sum: f32 = plan.adapters.iter().map(|entry| entry.weight).sum();
        assert!((sum - 1.0).abs() < 1e-6, "weights summed to {sum}");
    }

    #[test]
    fn min_score_filters_low_matches() {
        let selected = vec![scored("a", 0.8), scored("b", 0.2)];
        let adapters = vec![adapter("a"), adapter("b")];
        let plan = merge_plan(&selected, &adapters, 5, 0.5);
        assert_eq!(plan.adapters.len(), 1);
        assert_eq!(plan.adapters[0].name, "a");
        assert!((plan.adapters[0].weight - 1.0).abs() < 1e-6);
    }

    #[test]
    fn empty_selection_yields_empty_plan() {
        let plan = merge_plan(&[], &[], 3, 0.0);
        assert!(plan.adapters.is_empty());
        assert!(plan.base_model.is_empty());
    }
}
