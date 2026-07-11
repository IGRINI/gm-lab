//! Embedders: the `Embedder` trait, `LocalEmbeddingClient`, `HashEmbeddingClient`.

use std::collections::HashMap;

use blake2::digest::consts::U8;
use blake2::{Blake2b, Digest};
use gml_config::Config;
use serde_json::{json, Value};

use crate::cache::EmbeddingCache;
use crate::doc::py_strip;
use crate::error::{RagError, Result};
use crate::tokenize::tokens;
use crate::vector::{decode_embedding_value, normalize, sha_text};

/// Abstraction over an embedding backend.
///
/// Port of the implicit Python protocol: `embed(texts) -> list[list[float]]`.
pub trait Embedder {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f64>>>;

    /// Embed a QUERY for asymmetric retrieval. The DEFAULT applies the
    /// client-side instruction template ([`crate::engine::query_instruction`])
    /// and embeds it like any text — correct for dumb embedders (and what the
    /// golden tests pin). [`LocalEmbeddingClient`] overrides this to send the bare
    /// query with `input_type:"query"` so the SIDECAR applies the Qwen3 template;
    /// documents are always embedded bare via [`Embedder::embed`].
    fn embed_query(&self, query: &str) -> Result<Vec<f64>> {
        let text = crate::engine::query_instruction(query);
        self.embed(std::slice::from_ref(&text))?
            .into_iter()
            .next()
            .ok_or(RagError::BadEmbedding)
    }
}

/// HTTP embedding client with a cache-through, batching to `RAG_BATCH_SIZE`.
///
/// Faithful port of `LocalEmbeddingClient`.
pub struct LocalEmbeddingClient {
    pub url: String,
    pub model: String,
    pub encoding_format: String,
    pub batch_size: usize,
    pub timeout: f64,
    pub cache: EmbeddingCache,
}

impl LocalEmbeddingClient {
    /// Build from a [`Config`] against the GLOBAL cache path (`rag_cache_path`).
    /// Mirrors Python `LocalEmbeddingClient.__init__`; kept for the
    /// `world_ref == None` (built-in stories) path. Delegates to
    /// [`LocalEmbeddingClient::from_config_at`].
    pub fn from_config(config: &Config) -> Result<Self> {
        Self::from_config_at(config, &config.rag_cache_path)
    }

    /// Build from a [`Config`] but with an EXPLICIT cache path — the per-world
    /// routing seam. All client knobs come from `config`; only the cache file
    /// differs. `cache_path` is the per-world file (`world_cache_path`) or the
    /// global path (`rag_cache_path`) for the `None` sentinel.
    pub fn from_config_at(config: &Config, cache_path: impl AsRef<std::path::Path>) -> Result<Self> {
        let batch_size = std::cmp::max(1, config.rag_batch_size) as usize;
        Ok(LocalEmbeddingClient {
            url: config.rag_embeddings_url.clone(),
            // Fold the embedder quant into the cache key so switching bf16<->nf4
            // (or the model) never serves stale/incompatible cached vectors. The
            // sidecar ignores the payload `model` field, so this is cache-only.
            model: format!("{}@{}", config.rag_embeddings_model, config.embedder_quant),
            encoding_format: config.rag_encoding_format.clone(),
            batch_size,
            timeout: config.rag_timeout_seconds,
            cache: EmbeddingCache::new(cache_path)?,
        })
    }
}

fn post_json(url: &str, payload: Value, timeout: f64) -> Result<Value> {
    let url = url.to_string();
    std::thread::spawn(move || -> Result<Value> {
        let http = reqwest::blocking::Client::new();
        let response = http
            .post(url)
            .timeout(std::time::Duration::from_secs_f64(timeout))
            .json(&payload)
            .send()?
            .error_for_status()?;
        Ok(response.json()?)
    })
    .join()
    .map_err(|_| RagError::Value("RAG HTTP worker panicked".to_string()))?
}

/// Blocking POST to the unified sidecar's `/rerank` (jina-reranker-v3).
///
/// Returns the reranked indices into `documents`, best-first, as the sidecar
/// ordered them (scores are raw cosine — only the ORDER is used). On ANY
/// transport / HTTP / parse error returns `Err` so the caller can surface
/// degraded retrieval or choose its own fallback. Mirrors the per-call
/// blocking-client style of [`LocalEmbeddingClient::embed`].
pub fn rerank_documents(
    url: &str,
    query: &str,
    documents: &[String],
    top_n: usize,
    timeout: f64,
) -> Result<Vec<usize>> {
    let payload = json!({ "query": query, "documents": documents, "top_n": top_n });
    let body = post_json(url, payload, timeout)?;
    let results = body
        .get("results")
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default();
    let mut order: Vec<usize> = Vec::with_capacity(results.len());
    for item in &results {
        if let Some(idx) = item.get("index").and_then(|v| v.as_u64()) {
            order.push(idx as usize);
        }
    }
    Ok(order)
}

/// Like [`rerank_documents`] but ALSO returns each result's raw cosine score.
///
/// Same `/rerank` payload and blocking transport as [`rerank_documents`], but
/// extracts `results[i].relevance_score` alongside `results[i].index`, yielding
/// `(index, score)` pairs best-first. Scores are the sidecar's raw cosine in
/// `[-1, 1]` (no sigmoid) — callers that need an actual similarity value (e.g. a
/// threshold gate) use this instead of the order-only [`rerank_documents`],
/// whose contract the retrieval engine pins. On ANY transport / HTTP / parse
/// error returns `Err` so the caller can degrade.
pub fn rerank_scored(
    url: &str,
    query: &str,
    documents: &[String],
    top_n: usize,
    timeout: f64,
) -> Result<Vec<(usize, f64)>> {
    let payload = json!({ "query": query, "documents": documents, "top_n": top_n });
    let body = post_json(url, payload, timeout)?;
    let results = body
        .get("results")
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default();
    let mut scored: Vec<(usize, f64)> = Vec::with_capacity(results.len());
    for item in &results {
        if let (Some(idx), Some(score)) = (
            item.get("index").and_then(|v| v.as_u64()),
            item.get("relevance_score").and_then(|v| v.as_f64()),
        ) {
            scored.push((idx as usize, score));
        }
    }
    Ok(scored)
}

impl Embedder for LocalEmbeddingClient {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f64>>> {
        let normalized_texts: Vec<String> = texts.iter().map(|t| py_strip(t).to_string()).collect();
        let cached = self.cache.get_many(&self.model, &normalized_texts)?;
        let mut out: HashMap<String, Vec<f64>> = HashMap::new();
        let mut missing: Vec<String> = Vec::new();
        for text in &normalized_texts {
            let text_hash = sha_text(text);
            if let Some(vec) = cached.get(&text_hash) {
                out.insert(text_hash, vec.clone());
            } else {
                missing.push(text.clone());
            }
        }

        let mut start = 0;
        while start < missing.len() {
            let end = std::cmp::min(start + self.batch_size, missing.len());
            let batch = &missing[start..end];
            // Key order: model, input, encoding_format (matches Python dict).
            let payload = json!({
                "model": self.model,
                "input": batch,
                "encoding_format": self.encoding_format,
            });
            let body = post_json(&self.url, payload, self.timeout)?;
            let data = body
                .get("data")
                .and_then(|d| d.as_array())
                .cloned()
                .unwrap_or_default();

            // vectors_by_index: int(item.get("index", idx)) -> decoded vec
            let mut vectors_by_index: HashMap<i64, Vec<f64>> = HashMap::new();
            for (idx, item) in data.iter().enumerate() {
                let index = item
                    .get("index")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(idx as i64);
                let embedding = item.get("embedding").ok_or(RagError::BadEmbedding)?;
                vectors_by_index.insert(index, decode_embedding_value(embedding)?);
            }

            let mut rows: Vec<(String, Vec<f64>)> = Vec::with_capacity(batch.len());
            for (idx, text) in batch.iter().enumerate() {
                let vec = vectors_by_index
                    .get(&(idx as i64))
                    .cloned()
                    .ok_or_else(|| RagError::Value(format!("missing embedding for index {idx}")))?;
                out.insert(sha_text(text), vec.clone());
                rows.push((text.clone(), vec));
            }
            self.cache.put_many(&self.model, &rows)?;
            start = end;
        }

        let mut result: Vec<Vec<f64>> = Vec::with_capacity(normalized_texts.len());
        for text in &normalized_texts {
            let vec = out
                .get(&sha_text(text))
                .cloned()
                .ok_or_else(|| RagError::Value("embedding missing after embed".into()))?;
            result.push(vec);
        }
        Ok(result)
    }

    fn embed_query(&self, query: &str) -> Result<Vec<f64>> {
        // Bare query + input_type:"query" + the domain task -> the SIDECAR builds
        // the `Instruct: {task}\nQuery:{q}` template (documents are embedded bare
        // via `embed`). Queries are transient, so no cache round-trip here.
        let q = py_strip(query).to_string();
        let payload = json!({
            "model": self.model,
            "input": [q],
            "encoding_format": self.encoding_format,
            "input_type": "query",
            "task": crate::engine::QUERY_TASK,
        });
        let body = post_json(&self.url, payload, self.timeout)?;
        let item = body
            .get("data")
            .and_then(|d| d.as_array())
            .and_then(|a| a.first())
            .ok_or(RagError::BadEmbedding)?;
        let embedding = item.get("embedding").ok_or(RagError::BadEmbedding)?;
        decode_embedding_value(embedding)
    }
}

/// Deterministic tiny embedder for tests and offline fallback.
///
/// Faithful port of `HashEmbeddingClient`. Per token: blake2b digest_size=8;
/// idx = u32 little-endian of digest[..4] % dims; sign = + when digest[4] even
/// else -; accumulate, then L2-normalize.
pub struct HashEmbeddingClient {
    pub dims: usize,
}

impl HashEmbeddingClient {
    /// Python default `dims=128`.
    pub fn new(dims: usize) -> Self {
        HashEmbeddingClient { dims }
    }
}

impl Default for HashEmbeddingClient {
    fn default() -> Self {
        HashEmbeddingClient { dims: 128 }
    }
}

impl Embedder for HashEmbeddingClient {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f64>>> {
        let mut vectors: Vec<Vec<f64>> = Vec::with_capacity(texts.len());
        for text in texts {
            let mut vec = vec![0.0_f64; self.dims];
            for token in tokens(text) {
                // blake2b with 8-byte (64-bit) output.
                let mut hasher = Blake2b::<U8>::new();
                hasher.update(token.as_bytes());
                let digest = hasher.finalize();
                let idx = (u32::from_le_bytes([digest[0], digest[1], digest[2], digest[3]])
                    as usize)
                    % self.dims;
                let sign = if digest[4] % 2 == 0 { 1.0 } else { -1.0 };
                vec[idx] += sign;
            }
            vectors.push(normalize(vec));
        }
        Ok(vectors)
    }
}
