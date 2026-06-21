//! `retrieve_world_fact`, default-engine accessor, `purge_embeddings_for_texts`.

use std::sync::{Mutex, OnceLock};

use gml_config::Config;
use serde_json::{json, Map, Value};

use crate::cache::EmbeddingCache;
use crate::client::{Embedder, LocalEmbeddingClient};
use crate::doc::{py_strip, RagDocument};
use crate::engine::{RagEngine, GOOD_STATUS};
use crate::error::Result;
use crate::vector::sha_text;

/// Process-wide default engine, lazily built from [`Config`] (port of
/// `_DEFAULT_ENGINE` + `default_engine()` / `set_default_engine`).
static DEFAULT_ENGINE: OnceLock<Mutex<Option<RagEngine<LocalEmbeddingClient>>>> = OnceLock::new();

fn default_slot() -> &'static Mutex<Option<RagEngine<LocalEmbeddingClient>>> {
    DEFAULT_ENGINE.get_or_init(|| Mutex::new(None))
}

/// Ensure and run a closure with the default engine, building a
/// [`LocalEmbeddingClient`] from `config` on first use. Mirrors `default_engine()`.
pub fn with_default_engine<T>(
    config: &Config,
    f: impl FnOnce(&RagEngine<LocalEmbeddingClient>) -> Result<T>,
) -> Result<T> {
    let slot = default_slot();
    let mut guard = slot.lock().expect("default engine mutex poisoned");
    if guard.is_none() {
        let client = LocalEmbeddingClient::from_config(config)?;
        *guard = Some(RagEngine::new(client));
    }
    f(guard.as_ref().expect("engine present"))
}

/// Port of `set_default_engine(engine)` — install or clear the default engine.
pub fn set_default_engine(engine: Option<RagEngine<LocalEmbeddingClient>>) {
    let slot = default_slot();
    let mut guard = slot.lock().expect("default engine mutex poisoned");
    *guard = engine;
}

/// Faithful port of `retrieve_world_fact(query, documents)`, generalized over
/// the engine so callers (and the golden tests) can supply any [`Embedder`].
///
/// Returns `None` when RAG is disabled or there are no hits. The returned JSON
/// object has keys `status`, `text`, `sources` (in that order); each source has
/// `n, doc_id, kind, status, source, score, dense, keyword, metadata` (in that
/// order) with scores `round(_, 5)`.
pub fn retrieve_world_fact_with<E: Embedder>(
    engine: &RagEngine<E>,
    query: &str,
    documents: &[RagDocument],
    config: &Config,
) -> Result<Option<Value>> {
    if !config.rag_enabled {
        return Ok(None);
    }
    let hits = engine.search(query, documents, Some(config.rag_top_k as usize), config)?;
    if hits.is_empty() {
        return Ok(None);
    }

    let select_k = config.rag_fact_select_k as usize;
    let selected: Vec<_> = hits.into_iter().take(select_k).collect();
    if selected.is_empty() {
        return Ok(None);
    }

    let all_good = selected
        .iter()
        .all(|hit| GOOD_STATUS.contains(&hit.document.status.as_str()));
    let status = if all_good { "known" } else { "unknown" };

    let mut lines: Vec<String> = Vec::with_capacity(selected.len());
    let mut sources: Vec<Value> = Vec::with_capacity(selected.len());
    for (i, hit) in selected.iter().enumerate() {
        let n = i + 1;
        let doc = &hit.document;
        let label = if GOOD_STATUS.contains(&doc.status.as_str()) {
            "known"
        } else {
            "unconfirmed"
        };
        lines.push(format!("[{n}] {label}: {}", doc.text));

        let mut src = Map::new();
        src.insert("n".to_string(), json!(n));
        src.insert("doc_id".to_string(), json!(doc.doc_id));
        src.insert("kind".to_string(), json!(doc.kind));
        src.insert("status".to_string(), json!(doc.status));
        src.insert("source".to_string(), json!(doc.source));
        src.insert("score".to_string(), round_json(hit.score, 5));
        src.insert("dense".to_string(), round_json(hit.dense_score, 5));
        src.insert("keyword".to_string(), round_json(hit.keyword_score, 5));
        src.insert("metadata".to_string(), Value::Object(doc.metadata.clone()));
        sources.push(Value::Object(src));
    }

    let mut out = Map::new();
    out.insert("status".to_string(), json!(status));
    out.insert("text".to_string(), json!(lines.join(" ")));
    out.insert("sources".to_string(), Value::Array(sources));
    Ok(Some(Value::Object(out)))
}

/// Default-engine variant of [`retrieve_world_fact_with`] (port of the
/// module-level `retrieve_world_fact`, which uses `default_engine()`).
pub fn retrieve_world_fact(
    query: &str,
    documents: &[RagDocument],
    config: &Config,
) -> Result<Option<Value>> {
    if !config.rag_enabled {
        return Ok(None);
    }
    with_default_engine(config, |engine| {
        retrieve_world_fact_with(engine, query, documents, config)
    })
}

/// Port of `purge_embeddings_for_texts(texts)`. Best-effort; never raises
/// (returns 0 on any error). Matches `EmbeddingCache.embed()` normalization
/// (caches by `_sha(text.strip())`).
pub fn purge_embeddings_for_texts(texts: &[String], config: &Config) -> i64 {
    if !config.rag_enabled {
        return 0;
    }
    let clean: Vec<String> = texts
        .iter()
        .map(|t| py_strip(t).to_string())
        .filter(|t| !t.is_empty())
        .collect();
    if clean.is_empty() {
        return 0;
    }
    let hashes: Vec<String> = clean.iter().map(|t| sha_text(t)).collect();
    match EmbeddingCache::new(&config.rag_cache_path) {
        Ok(cache) => cache.delete_by_text_hashes(&hashes).unwrap_or(0),
        Err(_) => 0,
    }
}

/// Python `round(x, ndigits)` — round-half-to-even ("banker's rounding") on the
/// scaled value, returned as a JSON number. Matches `round(hit.score, 5)`.
fn round_json(x: f64, ndigits: i32) -> Value {
    let r = py_round(x, ndigits);
    json!(r)
}

/// Port of CPython `round(float, ndigits)` (round-half-to-even). CPython uses
/// `_Py_dg_dtoa`-based correct rounding; for our value range (scores well under
/// 10) scaling + `round_ties_even` reproduces it.
pub(crate) fn py_round(x: f64, ndigits: i32) -> f64 {
    if !x.is_finite() {
        return x;
    }
    let pow = 10f64.powi(ndigits);
    let scaled = x * pow;
    // round half to even
    let rounded = round_ties_even(scaled);
    rounded / pow
}

/// Round to nearest integer, ties to even.
fn round_ties_even(v: f64) -> f64 {
    let floor = v.floor();
    let diff = v - floor;
    if diff > 0.5 {
        floor + 1.0
    } else if diff < 0.5 {
        floor
    } else {
        // exactly halfway: round to even
        if (floor as i64) % 2 == 0 {
            floor
        } else {
            floor + 1.0
        }
    }
}
