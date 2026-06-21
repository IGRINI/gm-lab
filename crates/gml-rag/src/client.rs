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
    /// Build from a [`Config`], mirroring Python `LocalEmbeddingClient.__init__`.
    pub fn from_config(config: &Config) -> Result<Self> {
        let batch_size = std::cmp::max(1, config.rag_batch_size) as usize;
        Ok(LocalEmbeddingClient {
            url: config.rag_embeddings_url.clone(),
            model: config.rag_embeddings_model.clone(),
            encoding_format: config.rag_encoding_format.clone(),
            batch_size,
            timeout: config.rag_timeout_seconds,
            cache: EmbeddingCache::new(&config.rag_cache_path)?,
        })
    }
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

        let http = reqwest::blocking::Client::new();
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
            let response = http
                .post(&self.url)
                .timeout(std::time::Duration::from_secs_f64(self.timeout))
                .json(&payload)
                .send()?
                .error_for_status()?;
            let body: Value = response.json()?;
            let data = body.get("data").and_then(|d| d.as_array()).cloned().unwrap_or_default();

            // vectors_by_index: int(item.get("index", idx)) -> decoded vec
            let mut vectors_by_index: HashMap<i64, Vec<f64>> = HashMap::new();
            for (idx, item) in data.iter().enumerate() {
                let index = item
                    .get("index")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(idx as i64);
                let embedding = item
                    .get("embedding")
                    .ok_or(RagError::BadEmbedding)?;
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
