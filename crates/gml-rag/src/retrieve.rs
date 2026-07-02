//! Per-world engine registry, cache-path routing, `retrieve_world_fact*`,
//! `delete_world_cache`, and the world-scoped `purge_embeddings_for_texts`.
//!
//! The engine was a process-global singleton pinned to ONE cache path (the
//! first config's `rag_cache_path`). Phase A (RAG_PER_WORLD_TZ §2.2) replaces
//! it with a process REGISTRY keyed by the resolved cache path: the global path
//! is the sentinel for `world.world_ref == None` (built-in stories); every
//! world with a `world_ref` routes to `<GM_RAG_WORLDS_DIR>/<id>.sqlite3`. The
//! "first config wins" semantics of the old singleton are preserved, now
//! per-key — one [`RagEngine`] (one `init_db`) per cache file per process, so
//! nothing is rebuilt on the hot path. Two sessions of the same world share one
//! file; WAL + `busy_timeout=10000` + content-addressed `INSERT OR REPLACE`
//! (see [`crate::cache`]) make concurrent writers safe.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use gml_config::Config;
use serde_json::{json, Map, Value};

use crate::cache::EmbeddingCache;
use crate::client::{Embedder, LocalEmbeddingClient};
use crate::doc::{py_strip, RagDocument};
use crate::engine::{RagEngine, GOOD_STATUS};
use crate::error::Result;
use crate::vector::sha_text;

/// Process registry of per-cache-path engines. Key = the resolved cache path
/// (the global `rag_cache_path` is the sentinel for `world_ref == None`); value
/// = the shared engine built from the FIRST config that touched that path.
type EngineRegistry = HashMap<PathBuf, Arc<RagEngine<LocalEmbeddingClient>>>;

static ENGINES: OnceLock<Mutex<EngineRegistry>> = OnceLock::new();

fn registry() -> &'static Mutex<EngineRegistry> {
    ENGINES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Compute the per-world cache path `<rag_worlds_dir>/<sanitized>.sqlite3`.
///
/// The file stem is `world_id` sanitized to `[A-Za-z0-9_-]`. A non-empty id
/// that CHANGES under sanitization (so distinct ids can't alias to the same
/// stem) falls back to a deterministic `w-<sha256(world_id)[..32]>` stem —
/// content-addressed, so it is stable across runs and collision-resistant (a
/// 128-bit prefix of SHA-256 over the raw id makes an accidental clash
/// negligible, though not provably impossible). This keeps arbitrary/unicode
/// ids from escaping the directory while remaining reproducible.
///
/// Callers should route through [`resolve_cache_path`], which sends empty /
/// whitespace ids to the global sentinel BEFORE reaching here (an empty id is
/// not a distinct world). This fn still has a fallback for the empty stem so it
/// is safe if called directly.
pub fn world_cache_path(config: &Config, world_id: &str) -> PathBuf {
    let stem = sanitize_world_id(world_id);
    Path::new(&config.rag_worlds_dir).join(format!("{stem}.sqlite3"))
}

/// Map a world id to a filesystem-safe stem (see [`world_cache_path`]).
fn sanitize_world_id(world_id: &str) -> String {
    let safe = world_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if safe && !world_id.is_empty() {
        return world_id.to_string();
    }
    // Deterministic fallback: `w-` + 32 hex chars (128 bits) of sha256(id).
    let full = sha_text(world_id); // 64-hex sha256 (same hash as the cache key)
    format!("w-{}", &full[..32])
}

/// Resolve the cache path for an optional world id: a non-empty `Some(id)` ->
/// per-world file, `None` (or an empty/whitespace `Some`) -> the global
/// `rag_cache_path`. Single source of truth for the routing decision so every
/// entry point agrees. Folding the emptiness rule in here (rather than at each
/// call site) guarantees writes and purge land on the SAME file for every id
/// shape: a blank id is not a distinct world, it is the global sentinel — same
/// as `None` (RAG_PER_WORLD_TZ §2.2).
pub fn resolve_cache_path(config: &Config, world_id: Option<&str>) -> PathBuf {
    match world_id {
        Some(id) if !id.trim().is_empty() => world_cache_path(config, id),
        _ => PathBuf::from(&config.rag_cache_path),
    }
}

/// Ensure and run a closure with the engine bound to `cache_path`, building a
/// [`LocalEmbeddingClient`] over that path on first use (first config wins for
/// that key). This is the per-world generalization of the old
/// `with_default_engine`.
pub fn with_engine_at<T>(
    config: &Config,
    cache_path: impl AsRef<Path>,
    f: impl FnOnce(&RagEngine<LocalEmbeddingClient>) -> Result<T>,
) -> Result<T> {
    let cache_path = cache_path.as_ref().to_path_buf();
    // Fetch-or-build under the registry lock, then release it before running
    // `f`: the closure does I/O (HTTP embed) and must not hold the global lock,
    // and distinct engines are independent.
    let engine = {
        let mut guard = registry().lock().expect("engine registry mutex poisoned");
        if let Some(existing) = guard.get(&cache_path) {
            existing.clone()
        } else {
            let client = LocalEmbeddingClient::from_config_at(config, &cache_path)?;
            let engine = Arc::new(RagEngine::new(client));
            guard.insert(cache_path.clone(), engine.clone());
            engine
        }
    };
    f(&engine)
}

/// Ensure and run a closure with the GLOBAL-path engine (the `world_ref == None`
/// sentinel). Retained name/signature for compat; now a thin
/// [`with_engine_at`] against `config.rag_cache_path`.
pub fn with_default_engine<T>(
    config: &Config,
    f: impl FnOnce(&RagEngine<LocalEmbeddingClient>) -> Result<T>,
) -> Result<T> {
    with_engine_at(config, PathBuf::from(&config.rag_cache_path), f)
}

/// Port of `set_default_engine` — nothing in-repo calls it. Repurposed to a
/// registry-clear: drop every cached engine so the next `with_*` call rebuilds
/// from the current config (the closest sound behavior for a per-key registry).
/// The argument is ignored; kept only so the historical public signature still
/// compiles.
pub fn set_default_engine(_engine: Option<RagEngine<LocalEmbeddingClient>>) {
    let mut guard = registry().lock().expect("engine registry mutex poisoned");
    guard.clear();
}

/// Best-effort GC of a world's per-world cache file plus its sqlite sidecars
/// (`-wal`, `-shm`, `-journal`). NEVER errors — matches the existing purge-hook
/// culture (GC failures must not fail a delete/import). Returns whether the
/// MAIN `.sqlite3` file existed before removal (diagnostic only).
///
/// Also drops any registered engine for that path so a subsequent import under
/// the SAME id never serves the previous world's in-process engine/cache.
pub fn delete_world_cache(config: &Config, world_id: &str) -> bool {
    let main = world_cache_path(config, world_id);
    let existed = main.is_file();
    // Evict the in-process engine for this path first (best-effort).
    if let Ok(mut guard) = registry().lock() {
        guard.remove(&main);
    }
    let _ = std::fs::remove_file(&main);
    for suffix in ["-wal", "-shm", "-journal"] {
        let sidecar = sidecar_path(&main, suffix);
        let _ = std::fs::remove_file(&sidecar);
    }
    existed
}

/// Append a sqlite sidecar suffix to a db path: `<path><suffix>` (e.g.
/// `foo.sqlite3` + `-wal` -> `foo.sqlite3-wal`), matching how sqlite names WAL
/// and shared-memory files.
fn sidecar_path(main: &Path, suffix: &str) -> PathBuf {
    let mut s = main.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
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

/// Registry-routed variant of [`retrieve_world_fact_with`]: runs against the
/// engine bound to `cache_path` (per-world file, or the global path for
/// `world_ref == None`). This is the fact-path routing seam.
pub fn retrieve_world_fact_at(
    cache_path: impl AsRef<Path>,
    query: &str,
    documents: &[RagDocument],
    config: &Config,
) -> Result<Option<Value>> {
    if !config.rag_enabled {
        return Ok(None);
    }
    with_engine_at(config, cache_path, |engine| {
        retrieve_world_fact_with(engine, query, documents, config)
    })
}

/// Global-path variant of [`retrieve_world_fact_with`] (compat: the historical
/// `retrieve_world_fact`, now routed through the registry's global-path slot).
/// Callers that know the world should prefer [`retrieve_world_fact_at`].
pub fn retrieve_world_fact(
    query: &str,
    documents: &[RagDocument],
    config: &Config,
) -> Result<Option<Value>> {
    retrieve_world_fact_at(&config.rag_cache_path, query, documents, config)
}

/// Port of `purge_embeddings_for_texts(texts)`, now WORLD-SCOPED. Best-effort;
/// never raises (returns 0 on any error). Matches `EmbeddingCache.embed()`
/// normalization (caches by `_sha(text.strip())`).
///
/// `world_id`: `Some` -> purge only that world's per-world file; `None` ->
/// purge the global cache. This keeps a chat-delete from sweeping every world's
/// cache (RAG_PER_WORLD_TZ §2.4): a session's texts only ever live in its own
/// world file (or the global file for built-in stories).
pub fn purge_embeddings_for_texts(texts: &[String], config: &Config, world_id: Option<&str>) -> i64 {
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
    let path = resolve_cache_path(config, world_id);
    match EmbeddingCache::new(&path) {
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
