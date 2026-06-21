//! `RagEngine` — hybrid dense + BM25 + RRF retrieval. Port of `rag.py`.

use std::collections::HashMap;

use gml_config::Config;

use crate::client::Embedder;
use crate::doc::{py_strip, RagDocument, RagHit};
use crate::error::Result;
use crate::tokenize::tokens;

/// Statuses that count as established/"good" for ranking and fact labeling.
/// Port of `_GOOD_STATUS`.
pub const GOOD_STATUS: [&str; 3] = ["known", "current", "present"];

/// Port of `_query_instruction(query)`.
pub fn query_instruction(query: &str) -> String {
    format!(
        "Instruct: Given a game master's query, retrieve relevant public world facts, \
current scene facts, known NPC whereabouts, evidence, and unconfirmed witness \
statements for a tabletop RPG. Do not retrieve hidden canon or private secrets.\n\
Query: {}",
        py_strip(query)
    )
}

/// Port of `_rank_map(scores)`.
///
/// Builds `(idx, score)` for `score > 0`, sorts by score descending (stable —
/// ties keep ascending index order, matching Python `sorted`), and maps
/// `idx -> rank (1-based)`.
pub fn rank_map(scores: &[f64]) -> HashMap<usize, usize> {
    let mut ranked: Vec<(usize, f64)> = scores
        .iter()
        .enumerate()
        .filter(|(_, &score)| score > 0.0)
        .map(|(idx, &score)| (idx, score))
        .collect();
    // Stable sort by score descending. `sort_by` is stable in Rust std.
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let mut out: HashMap<usize, usize> = HashMap::new();
    for (rank, (idx, _score)) in ranked.into_iter().enumerate() {
        out.insert(idx, rank + 1);
    }
    out
}

/// Port of `_bm25_scores(query, documents)` with `k1=1.5, b=0.75`.
pub fn bm25_scores(query: &str, documents: &[RagDocument]) -> Vec<f64> {
    let query_terms = tokens(query);
    if query_terms.is_empty() {
        return vec![0.0; documents.len()];
    }
    let doc_terms: Vec<Vec<String>> = documents
        .iter()
        .map(|doc| tokens(&doc.contextual_text()))
        .collect();
    let total_terms: usize = doc_terms.iter().map(|t| t.len()).sum();
    let avgdl = total_terms as f64 / std::cmp::max(1, doc_terms.len()) as f64;

    let mut dfs: HashMap<String, i64> = HashMap::new();
    for terms in &doc_terms {
        let unique: std::collections::HashSet<&String> = terms.iter().collect();
        for term in unique {
            *dfs.entry(term.clone()).or_insert(0) += 1;
        }
    }
    let n_docs = documents.len() as f64;
    let k1 = 1.5_f64;
    let b = 0.75_f64;

    let mut scores: Vec<f64> = Vec::with_capacity(doc_terms.len());
    for terms in &doc_terms {
        let mut counts: HashMap<String, i64> = HashMap::new();
        for term in terms {
            *counts.entry(term.clone()).or_insert(0) += 1;
        }
        let dl = if terms.is_empty() { 1.0 } else { terms.len() as f64 };
        let mut score = 0.0_f64;
        for term in &query_terms {
            let tf = *counts.get(term).unwrap_or(&0);
            if tf == 0 {
                continue;
            }
            let df = *dfs.get(term).unwrap_or(&0) as f64;
            let idf = (1.0 + (n_docs - df + 0.5) / (df + 0.5)).ln();
            let denom = tf as f64 + k1 * (1.0 - b + b * dl / avgdl.max(1.0));
            score += idf * (tf as f64 * (k1 + 1.0)) / denom;
        }
        scores.push(score);
    }
    scores
}

/// Hybrid retrieval engine over an [`Embedder`].
pub struct RagEngine<E: Embedder> {
    pub embedder: E,
}

impl<E: Embedder> RagEngine<E> {
    pub fn new(embedder: E) -> Self {
        RagEngine { embedder }
    }

    /// Faithful port of `RagEngine.search`.
    ///
    /// `config` supplies `RAG_TOP_K`, `RAG_RRF_K`, the tiebreaks, the status
    /// boost, and the min dense score. `top_k=None` falls back to
    /// `config.RAG_TOP_K`.
    pub fn search(
        &self,
        query: &str,
        documents: &[RagDocument],
        top_k: Option<usize>,
        config: &Config,
    ) -> Result<Vec<RagHit>> {
        let docs: Vec<RagDocument> = documents
            .iter()
            .filter(|doc| !py_strip(&doc.text).is_empty())
            .cloned()
            .collect();
        if py_strip(query).is_empty() || docs.is_empty() {
            return Ok(Vec::new());
        }
        // Python: top_k = top_k or config.RAG_TOP_K  (0 is falsy too).
        let top_k = match top_k {
            Some(k) if k != 0 => k,
            _ => config.rag_top_k as usize,
        };

        let query_text = query_instruction(query);
        let mut embed_inputs: Vec<String> = Vec::with_capacity(docs.len() + 1);
        embed_inputs.push(query_text);
        for doc in &docs {
            embed_inputs.push(doc.contextual_text());
        }
        let vectors = self.embedder.embed(&embed_inputs)?;
        let query_vec = &vectors[0];
        let doc_vecs = &vectors[1..];

        let dense_scores: Vec<f64> = doc_vecs
            .iter()
            .map(|doc_vec| {
                query_vec
                    .iter()
                    .zip(doc_vec.iter())
                    .map(|(a, b)| a * b)
                    .sum::<f64>()
            })
            .collect();
        let keyword_scores = bm25_scores(query, &docs);
        let max_keyword = keyword_scores.iter().cloned().fold(0.0_f64, f64::max);

        let dense_rank = rank_map(&dense_scores);
        let keyword_rank = rank_map(&keyword_scores);

        let rrf_k = config.rag_rrf_k as f64;
        let mut hits: Vec<RagHit> = Vec::with_capacity(docs.len());
        for (idx, doc) in docs.iter().enumerate() {
            let mut final_score = 0.0_f64;
            if let Some(&rank) = dense_rank.get(&idx) {
                final_score += 1.0 / (rrf_k + rank as f64);
            }
            if let Some(&rank) = keyword_rank.get(&idx) {
                final_score += 1.0 / (rrf_k + rank as f64);
            }
            if max_keyword > 0.0 {
                final_score += config.rag_keyword_tiebreak * (keyword_scores[idx] / max_keyword);
            }
            final_score += config.rag_dense_tiebreak * dense_scores[idx].max(0.0);
            if GOOD_STATUS.contains(&doc.status.as_str()) {
                final_score *= config.rag_status_boost;
            }
            hits.push(RagHit {
                document: doc.clone(),
                score: final_score,
                dense_score: dense_scores[idx],
                keyword_score: keyword_scores[idx],
            });
        }

        // sort by (score, dense, keyword) descending, stable.
        hits.sort_by(|a, b| {
            cmp_desc(a.score, b.score)
                .then_with(|| cmp_desc(a.dense_score, b.dense_score))
                .then_with(|| cmp_desc(a.keyword_score, b.keyword_score))
        });

        let min_dense = config.rag_min_dense_score;
        let filtered: Vec<RagHit> = hits
            .into_iter()
            .filter(|hit| hit.keyword_score > 0.0 || hit.dense_score >= min_dense)
            .collect();

        Ok(filtered.into_iter().take(top_k).collect())
    }
}

/// Descending comparator matching Python `reverse=True` tuple sort. NaN treated
/// as equal (Python would raise, but our scores are never NaN here).
fn cmp_desc(a: f64, b: f64) -> std::cmp::Ordering {
    b.partial_cmp(&a).unwrap_or(std::cmp::Ordering::Equal)
}
