//! Similarity scoring and ranking of registered adapters.

use serde::{Deserialize, Serialize};

use super::AdapterRecord;

/// A single ranked adapter and its similarity score.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoredAdapter {
    pub name: String,
    pub score: f32,
}

/// The result of selecting adapters for a project.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SelectResult {
    pub project_type: String,
    pub adapters: Vec<ScoredAdapter>,
}

/// Cosine similarity of two equal-length vectors.
///
/// Vectors produced by [`super::embedding`] are L2-normalized, so this reduces to
/// a dot product; we divide by the norms anyway to stay correct for any input.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm_a = a.iter().map(|v| v * v).sum::<f32>().sqrt();
    let norm_b = b.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        0.0
    } else {
        dot / (norm_a * norm_b)
    }
}

/// Score every adapter against the project embedding and rank by similarity.
///
/// Scores are rounded to two decimals. Ties break deterministically by name so
/// repeated runs are stable.
pub fn select_adapters(
    project_embedding: &[f32],
    adapters: &[AdapterRecord],
) -> Vec<ScoredAdapter> {
    let mut scored: Vec<ScoredAdapter> = adapters
        .iter()
        .map(|adapter| ScoredAdapter {
            name: adapter.name.clone(),
            score: round2(cosine_similarity(project_embedding, &adapter.embedding)),
        })
        .collect();
    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.name.cmp(&b.name))
    });
    scored
}

pub(crate) fn round2(value: f32) -> f32 {
    (value * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn adapter(name: &str, embedding: Vec<f32>) -> AdapterRecord {
        AdapterRecord {
            id: name.to_owned(),
            name: name.to_owned(),
            base_model: "base".to_owned(),
            path: format!("/adapters/{name}"),
            embedding,
            metadata: json!({}),
        }
    }

    #[test]
    fn cosine_of_identical_and_orthogonal_vectors() {
        assert!((cosine_similarity(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!(cosine_similarity(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
    }

    #[test]
    fn ranks_by_similarity_then_name() {
        let project = vec![1.0, 0.0];
        let adapters = vec![
            adapter("far", vec![0.0, 1.0]),
            adapter("near-b", vec![1.0, 0.0]),
            adapter("near-a", vec![1.0, 0.0]),
        ];
        let ranked = select_adapters(&project, &adapters);
        assert_eq!(ranked[0].name, "near-a");
        assert_eq!(ranked[1].name, "near-b");
        assert_eq!(ranked[2].name, "far");
    }
}
