//! RAG retrieval adapter bridging `gml-world`'s `RagRetriever` trait to
//! `gml-rag::retrieve_world_fact` (handling the cross-crate `RagDocument` seam).
//!
//! RAG degrades gracefully: when disabled, when the embedding client cannot be
//! built, or when retrieval errors, the retriever is absent / returns `None`
//! and `world.fact` falls back to its keyword path.

use serde_json::{json, Value};

use gml_config::Config;
use gml_world::{MemoryAccess, MemoryUnit, RagRetriever, RetrievedFact, World};

/// A retriever bound to a pre-computed document set + config.
///
/// `world.fact(query, actor, retriever)` borrows `world` immutably, but building
/// the actor-scoped documents needs `&mut world` (`ensure_npc_whereabouts`).
/// We therefore snapshot the documents up-front (here, while we still hold a
/// `&mut World`) and hand the retriever the owned snapshot.
pub struct DocRetriever {
    documents: Vec<gml_rag::RagDocument>,
    config: Config,
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

impl DocRetriever {
    /// Coerce a `&DocRetriever` to a `&dyn RagRetriever` for `world.fact`.
    pub fn as_dyn(&self) -> &dyn RagRetriever {
        self
    }
}

impl RagRetriever for DocRetriever {
    fn retrieve_world_fact(&self, query: &str, _actor_id: &str) -> Option<RetrievedFact> {
        gml_rag::retrieve_world_fact(query, &self.documents, &self.config)
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
    let world_docs = world.retrieval_documents(actor_id);
    let documents: Vec<gml_rag::RagDocument> = world_docs.into_iter().map(convert_doc).collect();
    Some(DocRetriever { documents, config })
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
    match gml_rag::retrieve_world_fact(query, &documents, &config) {
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
    let documents: Vec<gml_rag::RagDocument> = world
        .memory_documents_for_access(access, include_cold, include_details)
        .into_iter()
        .map(convert_doc)
        .collect();
    retrieve_memory_rows_with_documents(world, query, limit, include_details, &config, &documents)
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

fn retrieve_memory_rows_with_documents(
    world: &World,
    query: &str,
    limit: usize,
    include_details: bool,
    config: &Config,
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
    let hits = match gml_rag::with_default_engine(config, |engine| {
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
    use gml_world::{MemoryAccess, MemoryUnit};

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
}
