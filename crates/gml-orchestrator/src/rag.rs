//! RAG retrieval adapter bridging `gml-world`'s `RagRetriever` trait to
//! `gml-rag`'s per-world cache surface (handling the cross-crate `RagDocument`
//! seam). Each entry point first resolves the world's cache path via
//! [`gml_rag::resolve_cache_path`] and routes fact/memory retrieval through the
//! path-scoped `gml_rag::retrieve_world_fact_at` / `with_engine_at` seam, so a
//! world's texts never cross into another world's cache.
//!
//! RAG degrades gracefully: when disabled, when the embedding client cannot be
//! built, or when retrieval errors, the retriever is absent / returns `None`
//! and `world.fact` falls back to its keyword path.

use std::path::PathBuf;

use serde_json::{json, Value};

use gml_config::Config;
use gml_world::{MemoryAccess, MemoryUnit, RagRetriever, RetrievedFact, World};

/// Resolve the RAG cache path for a world: a world launched from a package
/// (`world_ref == Some`) routes to its per-world file
/// (`<GM_RAG_WORLDS_DIR>/<id>.sqlite3`); a `None` world (built-in / procedural
/// story) stays on the global cache. This is the orchestrator's single routing
/// decision, applied at EVERY entry point so fact + memory caches never cross
/// worlds (RAG_PER_WORLD_TZ §2.2).
fn world_cache_path(world: &World, config: &Config) -> PathBuf {
    let world_id = world.world_ref.as_ref().map(|r| r.id.as_str());
    gml_rag::resolve_cache_path(config, world_id)
}

/// A retriever bound to a pre-computed document set + config + resolved cache
/// path.
///
/// `world.fact(query, actor, retriever)` borrows `world` immutably, but building
/// the actor-scoped documents needs `&mut world` (`ensure_npc_whereabouts`).
/// We therefore snapshot the documents up-front (here, while we still hold a
/// `&mut World`) and hand the retriever the owned snapshot. The per-world cache
/// path is snapshotted the same way, so the retriever writes only that world's
/// cache file.
pub struct DocRetriever {
    documents: Vec<gml_rag::RagDocument>,
    config: Config,
    cache_path: PathBuf,
}

/// Memory retrieval output plus operational diagnostics.
///
/// `rows=None` means the caller should keep the existing deterministic lexical
/// fallback. `status` is still returned to the tool caller so RAG failures are
/// observable instead of silently changing ranking behaviour.
#[derive(Clone, Debug)]
pub struct MemoryRetrievalReport {
    pub rows: Option<Vec<Value>>,
    pub status: Value,
}

/// World-fact RAG output plus operational diagnostics for the tool payload.
#[derive(Clone, Debug)]
pub struct WorldFactRetrievalReport {
    pub fact: Option<RetrievedFact>,
    pub status: Value,
}

/// One existing-roster NPC scored against a generation brief by the reranker.
/// Carries only non-sensitive identity fields (no secret/knowledge/mechanics) so
/// it is safe to surface in a `duplicate_candidates` tool result.
#[derive(Clone, Debug)]
pub struct NpcDedupCandidate {
    pub npc_id: String,
    pub internal_name: String,
    pub player_label: String,
    pub role: String,
    pub score: f64,
}

/// Semantic NPC-dedup gate output plus operational diagnostics.
///
/// `candidates` are best-first by rerank score. The gate-firing decision lives in
/// `status["duplicate"]` (true iff the top raw-cosine score >= the configured
/// threshold); it is ABSENT on the disabled / no_candidates / degraded statuses,
/// so those all degrade to "gate skipped, generation proceeds".
#[derive(Clone, Debug)]
pub struct NpcDedupReport {
    pub candidates: Vec<NpcDedupCandidate>,
    pub status: Value,
}

/// Soft semantic dedup for `generate_npc` (NPC_GEN_DESIGN §5). Reranks the
/// existing roster against the GM's qualitative brief and reports whether the top
/// match is close enough to warrant a "are you sure?" prompt.
///
/// `Config::from_env()` is read per call (env-lock test convention). Candidate
/// docs are `"имя; роль; persona; goals"` per `world.npcs` entry, capped at
/// `npc_dedup_candidates`. NEVER blocks the turn: disabled / no candidates / any
/// rerank error all return an empty-or-partial report with NO `duplicate` flag so
/// the caller proceeds with generation.
pub fn npc_dedup_report(world: &World, brief: &str) -> NpcDedupReport {
    let config = Config::from_env();
    if !config.npc_dedup_enabled {
        return NpcDedupReport {
            candidates: Vec::new(),
            status: json!({
                "enabled": false,
                "degraded": false,
                "reason": "disabled",
            }),
        };
    }
    // Candidate rows (id, internal name, player label, role) + the doc text sent
    // to the reranker. `world.npcs` is a BTreeMap → deterministic order; cap the
    // number of docs at the configured budget.
    let cap = config.npc_dedup_candidates.max(0) as usize;
    let rows: Vec<(NpcDedupCandidate, String)> = world
        .npcs
        .values()
        .take(cap)
        .map(|npc| {
            let doc = format!("{}; {}; {}; {}", npc.name, npc.role, npc.persona, npc.goals);
            (
                NpcDedupCandidate {
                    npc_id: npc.npc_id.clone(),
                    internal_name: npc.name.clone(),
                    player_label: npc.public_label.clone(),
                    role: npc.role.clone(),
                    score: 0.0,
                },
                doc,
            )
        })
        .collect();
    if rows.is_empty() {
        return NpcDedupReport {
            candidates: Vec::new(),
            status: json!({
                "enabled": true,
                "degraded": false,
                "reason": "no_candidates",
                "candidates": 0,
            }),
        };
    }
    let documents: Vec<String> = rows.iter().map(|(_, doc)| doc.clone()).collect();
    match gml_rag::rerank_scored(
        &config.rag_rerank_url,
        brief,
        &documents,
        documents.len(),
        config.rag_timeout_seconds,
    ) {
        Ok(scored) => {
            // Reorder candidates best-first, attaching the raw cosine score. The
            // reranker returns indices into `documents`; ignore out-of-range ids.
            let mut candidates: Vec<NpcDedupCandidate> = scored
                .into_iter()
                .filter_map(|(idx, score)| {
                    rows.get(idx).map(|(candidate, _)| {
                        let mut candidate = candidate.clone();
                        candidate.score = score;
                        candidate
                    })
                })
                .collect();
            // Defensive: if the sidecar dropped some rows, keep only what it
            // returned but ensure best-first ordering by score.
            candidates.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            let top_score = candidates.first().map(|c| c.score).unwrap_or(f64::MIN);
            let duplicate = top_score >= config.npc_dedup_threshold;
            NpcDedupReport {
                status: json!({
                    "enabled": true,
                    "degraded": false,
                    "reason": "checked",
                    "candidates": candidates.len(),
                    "threshold": config.npc_dedup_threshold,
                    "top_score": top_score,
                    "duplicate": duplicate,
                }),
                candidates,
            }
        }
        Err(error) => NpcDedupReport {
            candidates: Vec::new(),
            status: json!({
                "enabled": true,
                "degraded": true,
                "reason": "rerank_error",
                "documents": documents.len(),
                "error": compact_error(&error.to_string()),
            }),
        },
    }
}

impl DocRetriever {
    /// Coerce a `&DocRetriever` to a `&dyn RagRetriever` for `world.fact`.
    pub fn as_dyn(&self) -> &dyn RagRetriever {
        self
    }
}

impl RagRetriever for DocRetriever {
    fn retrieve_world_fact(&self, query: &str, _actor_id: &str) -> Option<RetrievedFact> {
        gml_rag::retrieve_world_fact_at(&self.cache_path, query, &self.documents, &self.config)
            .ok()
            .flatten()
            .map(retrieved_fact_from_payload)
    }
}

/// Build an actor-scoped retriever, or `None` when RAG is disabled. The
/// document snapshot is taken now (needs `&mut World`); the resulting retriever
/// can be passed to the immutable `world.fact(...)`.
///
/// Errors building the embedding client / docs degrade to `None`.
pub fn build_retriever(world: &mut World, actor_id: &str, _query: &str) -> Option<DocRetriever> {
    let config = Config::from_env();
    if !config.rag_enabled {
        return None;
    }
    let cache_path = world_cache_path(world, &config);
    let world_docs = world.retrieval_documents(actor_id);
    let documents: Vec<gml_rag::RagDocument> = world_docs.into_iter().map(convert_doc).collect();
    Some(DocRetriever {
        documents,
        config,
        cache_path,
    })
}

/// Try RAG for `get_world_fact` and return a status object even when callers
/// fall back to the deterministic world matcher.
pub fn retrieve_world_fact_report(
    world: &mut World,
    actor_id: &str,
    query: &str,
) -> WorldFactRetrievalReport {
    let config = Config::from_env();
    if !config.rag_enabled {
        return WorldFactRetrievalReport {
            fact: None,
            status: json!({
                "enabled": false,
                "backend": "lexical",
                "degraded": false,
                "reason": "disabled",
            }),
        };
    }
    let cache_path = world_cache_path(world, &config);
    let documents: Vec<gml_rag::RagDocument> = world
        .retrieval_documents(actor_id)
        .into_iter()
        .map(convert_doc)
        .collect();
    if documents.is_empty() {
        return WorldFactRetrievalReport {
            fact: None,
            status: json!({
                "enabled": true,
                "backend": "lexical_fallback",
                "degraded": false,
                "reason": "no_scoped_documents",
                "documents": 0,
            }),
        };
    }
    match gml_rag::retrieve_world_fact_at(&cache_path, query, &documents, &config) {
        Ok(Some(payload)) => WorldFactRetrievalReport {
            fact: Some(retrieved_fact_from_payload(payload)),
            status: json!({
                "enabled": true,
                "backend": "rag",
                "degraded": false,
                "documents": documents.len(),
            }),
        },
        Ok(None) => WorldFactRetrievalReport {
            fact: None,
            status: json!({
                "enabled": true,
                "backend": "lexical_fallback",
                "degraded": false,
                "reason": "rag_no_hits",
                "documents": documents.len(),
            }),
        },
        Err(error) => WorldFactRetrievalReport {
            fact: None,
            status: json!({
                "enabled": true,
                "backend": "lexical_fallback",
                "degraded": true,
                "reason": "rag_error",
                "documents": documents.len(),
                "error": compact_error(&error.to_string()),
            }),
        },
    }
}

/// Rerank scoped memory after the world layer has already applied access gates.
///
/// Returns `None` when RAG is disabled or unavailable so callers can fall back to
/// the deterministic lexical memory lookup. Returned rows keep the existing
/// memory tool shape; RAG only changes ordering/selection.
pub fn retrieve_memory_rows(
    world: &World,
    access: &MemoryAccess,
    query: &str,
    limit: usize,
    include_cold: bool,
    include_details: bool,
) -> Option<Vec<Value>> {
    retrieve_memory_rows_report(world, access, query, limit, include_cold, include_details).rows
}

/// Rerank scoped memory and report whether RAG was used or degraded.
pub fn retrieve_memory_rows_report(
    world: &World,
    access: &MemoryAccess,
    query: &str,
    limit: usize,
    include_cold: bool,
    include_details: bool,
) -> MemoryRetrievalReport {
    let config = Config::from_env();
    if !config.rag_enabled {
        return MemoryRetrievalReport {
            rows: None,
            status: json!({
                "enabled": false,
                "backend": "lexical",
                "degraded": false,
                "reason": "disabled",
            }),
        };
    }
    let cache_path = world_cache_path(world, &config);
    let documents: Vec<gml_rag::RagDocument> = world
        .memory_documents_for_access(access, include_cold, include_details)
        .into_iter()
        .map(convert_doc)
        .collect();
    retrieve_memory_rows_with_documents(
        world,
        query,
        limit,
        include_details,
        &config,
        &cache_path,
        &documents,
    )
}

/// Convert a `gml_world::RagDocument` to a `gml_rag::RagDocument`
/// (the cross-crate seam — identical field set, distinct types).
fn convert_doc(d: gml_world::RagDocument) -> gml_rag::RagDocument {
    gml_rag::RagDocument {
        doc_id: d.doc_id,
        kind: d.kind,
        text: d.text,
        status: d.status,
        source: d.source,
        visibility: d.visibility,
        tags: d.tags,
        metadata: d.metadata,
    }
}

fn retrieved_fact_from_payload(payload: Value) -> RetrievedFact {
    RetrievedFact {
        status: payload
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        text: payload
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        sources: match payload.get("sources") {
            Some(Value::Array(a)) => a.clone(),
            _ => Vec::new(),
        },
    }
}

#[allow(clippy::too_many_arguments)]
fn retrieve_memory_rows_with_documents(
    world: &World,
    query: &str,
    limit: usize,
    include_details: bool,
    config: &Config,
    cache_path: &std::path::Path,
    documents: &[gml_rag::RagDocument],
) -> MemoryRetrievalReport {
    if documents.is_empty() {
        return MemoryRetrievalReport {
            rows: Some(Vec::new()),
            status: json!({
                "enabled": true,
                "backend": "rag",
                "degraded": false,
                "reason": "no_scoped_documents",
                "documents": 0,
                "hits": 0,
            }),
        };
    }
    // Memory texts are this world's session texts; route them to the world's
    // per-world cache (closes the `with_default_engine` cross-world hole).
    let hits = match gml_rag::with_engine_at(config, cache_path, |engine| {
        engine.search(query, documents, Some(limit), config)
    }) {
        Ok(hits) => hits,
        Err(error) => {
            return MemoryRetrievalReport {
                rows: None,
                status: json!({
                    "enabled": true,
                    "backend": "lexical_fallback",
                    "degraded": true,
                    "reason": "rag_error",
                    "documents": documents.len(),
                    "error": compact_error(&error.to_string()),
                }),
            };
        }
    };
    let mut rows = Vec::new();
    for hit in hits {
        let Some(memory_id) = hit
            .document
            .metadata
            .get("memory_id")
            .and_then(Value::as_str)
        else {
            continue;
        };
        let Some(unit) = world.world_canon.memory.get(memory_id) else {
            continue;
        };
        if !memory_hit_allowed_for_query(unit, query) {
            continue;
        }
        rows.push(unit.to_row(include_details));
    }
    if rows.is_empty() {
        MemoryRetrievalReport {
            rows: None,
            status: json!({
                "enabled": true,
                "backend": "lexical_fallback",
                "degraded": false,
                "reason": "rag_no_mapped_hits",
                "documents": documents.len(),
                "hits": 0,
            }),
        }
    } else {
        MemoryRetrievalReport {
            status: json!({
                "enabled": true,
                "backend": "rag",
                "degraded": false,
                "documents": documents.len(),
                "hits": rows.len(),
            }),
            rows: Some(rows),
        }
    }
}

fn compact_error(error: &str) -> String {
    const MAX: usize = 220;
    let mut out = error.split_whitespace().collect::<Vec<_>>().join(" ");
    if out.chars().count() > MAX {
        out = out.chars().take(MAX.saturating_sub(3)).collect();
        out.push_str("...");
    }
    out
}

fn memory_hit_allowed_for_query(unit: &MemoryUnit, query: &str) -> bool {
    !is_known_name_memory(unit) || query_requests_known_name(query)
}

fn is_known_name_memory(unit: &MemoryUnit) -> bool {
    let has_known_name = unit
        .metadata
        .get("known_name")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.trim().is_empty());
    has_known_name || unit.topic_tags.iter().any(|tag| tag == "known_name")
}

fn query_requests_known_name(query: &str) -> bool {
    let q = query.trim().to_lowercase();
    [
        "как зовут",
        "имя",
        "имени",
        "звать",
        "зовут",
        "кличка",
        "прозвище",
        "name",
        "called",
    ]
    .iter()
    .any(|needle| q.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gml_rag::{HashEmbeddingClient, RagEngine};
    use gml_world::{MemoryAccess, MemoryUnit, PackageRef};
    use std::sync::Mutex;

    /// Serializes the env-mutating RAG tests in this binary: `Config::from_env`
    /// reads process-global `GM_RAG_*`, so two tests setting them concurrently
    /// would race.
    static RAG_ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Default story seed from a HERMETIC store over a tempdir. There is no
    /// global store; constructing a `StoryStore` over a tempdir materializes the
    /// builtins into the throwaway directory, so these tests never touch the real
    /// user library.
    fn default_story_seed() -> serde_json::Value {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = gml_stories::StoryStore::new(dir.path()).expect("open store");
        store.default_seed()
    }

    #[test]
    fn memory_rag_ranks_only_access_allowed_documents() {
        let mut world = World::from_seed_with_dice_seed(&default_story_seed(), 20260622);
        world.add_memory_unit(MemoryUnit {
            memory_id: "allowed".to_string(),
            owner_scope: "actor:borin".to_string(),
            summary: "ALLOWED_SENTINEL Борин помнит пароль северных ворот.".to_string(),
            ..Default::default()
        });
        world.add_memory_unit(MemoryUnit {
            memory_id: "hidden".to_string(),
            owner_scope: "actor:lysa".to_string(),
            summary: "HIDDEN_SENTINEL Лиза знает другой пароль.".to_string(),
            ..Default::default()
        });

        let mut config = Config::from_env();
        config.rag_enabled = true;
        config.rag_rerank_enabled = false;
        config.rag_top_k = 4;
        config.rag_min_dense_score = -1.0;
        let access = MemoryAccess::scoped(["actor:borin".to_string()].into_iter().collect());
        let documents: Vec<gml_rag::RagDocument> = world
            .memory_documents_for_access(&access, false, false)
            .into_iter()
            .map(convert_doc)
            .collect();
        let engine = RagEngine::new(HashEmbeddingClient::default());
        let hits = engine
            .search("ALLOWED_SENTINEL", &documents, Some(5), &config)
            .expect("hash RAG search");
        let rows: Vec<Value> = hits
            .into_iter()
            .filter_map(|hit| {
                let memory_id = hit
                    .document
                    .metadata
                    .get("memory_id")
                    .and_then(Value::as_str)?;
                let unit = world.world_canon.memory.get(memory_id)?;
                Some(unit.to_row(false))
            })
            .collect();
        let rendered = serde_json::to_string(&rows).unwrap();
        assert!(rendered.contains("ALLOWED_SENTINEL"), "{rendered}");
        assert!(!rendered.contains("HIDDEN_SENTINEL"), "{rendered}");
    }

    #[test]
    fn memory_rag_documents_omit_details_until_explicit_drilldown() {
        let mut world = World::from_seed_with_dice_seed(&default_story_seed(), 20260622);
        world.add_memory_unit(MemoryUnit {
            memory_id: "details_only".to_string(),
            owner_scope: "actor:borin".to_string(),
            summary: "Борин помнит разговор у стойки.".to_string(),
            details: "DETAILS_ONLY_SENTINEL пароль был назван шепотом.".to_string(),
            ..Default::default()
        });

        let mut config = Config::from_env();
        config.rag_enabled = true;
        config.rag_rerank_enabled = false;
        config.rag_top_k = 4;
        config.rag_min_dense_score = -1.0;
        let access = MemoryAccess::scoped(["actor:borin".to_string()].into_iter().collect());
        let documents: Vec<gml_rag::RagDocument> = world
            .memory_documents_for_access(&access, false, false)
            .into_iter()
            .map(convert_doc)
            .collect();
        let rendered = documents
            .iter()
            .map(|doc| doc.contextual_text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!rendered.contains("DETAILS_ONLY_SENTINEL"), "{rendered}");

        let documents_with_details: Vec<gml_rag::RagDocument> = world
            .memory_documents_for_access(&access, false, true)
            .into_iter()
            .map(convert_doc)
            .collect();
        let rendered = documents_with_details
            .iter()
            .map(|doc| doc.contextual_text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("DETAILS_ONLY_SENTINEL"), "{rendered}");
    }

    /// The memory path (`retrieve_memory_rows_report` -> `..._with_documents`)
    /// used to route through the process-global engine (`with_default_engine`)
    /// regardless of world. Phase A closes that hole: a world-bound world's
    /// memory retrieval must hit its PER-WORLD cache file, not the global one.
    ///
    /// We drive the real path with a DEAD embeddings URL so `engine.search`
    /// fails fast (degraded fallback) — but the engine's `EmbeddingCache` is
    /// built (and its file created via `init_db`) BEFORE the failing HTTP call,
    /// so the created file proves which path was routed to.
    #[test]
    fn memory_retrieval_routes_to_the_per_world_cache_file() {
        let _guard = RAG_ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().expect("tempdir");
        let worlds_dir = dir.path().join("rag_worlds");
        let global = dir.path().join("global.sqlite3");
        std::env::set_var("GM_RAG_ENABLED", "1");
        std::env::set_var("GM_RAG_RERANK_ENABLED", "0");
        std::env::set_var("GM_RAG_WORLDS_DIR", &worlds_dir);
        std::env::set_var("GM_RAG_CACHE_PATH", &global);
        // Dead port: embed fails immediately, degrading to the lexical fallback.
        std::env::set_var("GM_RAG_EMBEDDINGS_URL", "http://127.0.0.1:9/v1/embeddings");
        std::env::set_var("GM_RAG_TIMEOUT_SECONDS", "0.2");

        let mut world = World::from_seed_with_dice_seed(&default_story_seed(), 20260622);
        let world_id = "mem-world-alpha";
        world.world_ref = Some(PackageRef {
            id: world_id.to_string(),
            version: 1,
        });
        world.add_memory_unit(MemoryUnit {
            memory_id: "m1".to_string(),
            owner_scope: "actor:borin".to_string(),
            summary: "Борин помнит пароль северных ворот.".to_string(),
            ..Default::default()
        });

        let access = MemoryAccess::scoped(["actor:borin".to_string()].into_iter().collect());
        let report = retrieve_memory_rows_report(&world, &access, "пароль ворот", 4, false, false);
        // Degraded (dead embedder), but that is not what we assert — the routing
        // side effect is the created cache file.
        assert!(report.status.get("enabled").and_then(Value::as_bool) == Some(true));

        let per_world = gml_rag::world_cache_path(&Config::from_env(), world_id);
        assert!(
            per_world.is_file(),
            "memory retrieval must create the per-world cache file at {}",
            per_world.display()
        );
        assert!(
            !global.is_file(),
            "the global cache must be untouched for a world-bound memory retrieval"
        );

        for k in [
            "GM_RAG_ENABLED",
            "GM_RAG_RERANK_ENABLED",
            "GM_RAG_WORLDS_DIR",
            "GM_RAG_CACHE_PATH",
            "GM_RAG_EMBEDDINGS_URL",
            "GM_RAG_TIMEOUT_SECONDS",
        ] {
            std::env::remove_var(k);
        }
    }
}
