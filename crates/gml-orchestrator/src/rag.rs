//! RAG retrieval adapter bridging `gml-world`'s `RagRetriever` trait to
//! `gml-rag::retrieve_world_fact` (handling the cross-crate `RagDocument` seam).
//!
//! RAG degrades gracefully: when disabled, when the embedding client cannot be
//! built, or when retrieval errors, the retriever is absent / returns `None`
//! and `world.fact` falls back to its keyword path.

use serde_json::Value;

use gml_config::Config;
use gml_world::{RagRetriever, RetrievedFact, World};

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

impl DocRetriever {
    /// Coerce a `&DocRetriever` to a `&dyn RagRetriever` for `world.fact`.
    pub fn as_dyn(&self) -> &dyn RagRetriever {
        self
    }
}

impl RagRetriever for DocRetriever {
    fn retrieve_world_fact(&self, query: &str, _actor_id: &str) -> Option<RetrievedFact> {
        // gml_rag::retrieve_world_fact returns Result<Option<Value>>; swallow errors.
        let result = gml_rag::retrieve_world_fact(query, &self.documents, &self.config);
        let payload = match result {
            Ok(Some(v)) => v,
            _ => return None,
        };
        Some(RetrievedFact {
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
        })
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
