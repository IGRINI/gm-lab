//! gml-rag — hybrid retrieval (dense cosine + BM25 + RRF reranker).
//!
//! Faithful port of `gm-lab/rag.py` (PORT_PLAN.md §4.6). Public surface:
//!
//! - [`RagDocument`] / [`RagHit`] value types ([`RagDocument::contextual_text`]).
//! - [`tokenize::tokens`] + [`tokenize::STOPWORDS`] (the tokenizer).
//! - vector helpers ([`vector::normalize`], [`vector::decode_embedding_b64`],
//!   [`vector::encode_vector`], [`vector::sha_text`]).
//! - [`EmbeddingCache`] (rusqlite, Python-DB compatible).
//! - the [`Embedder`] trait + [`LocalEmbeddingClient`] / [`HashEmbeddingClient`].
//! - [`RagEngine`] with [`RagEngine::search`].
//! - the per-world cache surface (RAG_PER_WORLD_TZ §2.2): retrieval is routed by
//!   resolved cache path through [`retrieve_world_fact_at`] +
//!   [`retrieve_world_fact_with`] and the path-keyed engine registry
//!   [`with_engine_at`]; [`world_cache_path`] / [`resolve_cache_path`] map a
//!   world id to its file (global path is the `None`/blank sentinel);
//!   [`purge_embeddings_for_texts`] is world-scoped and [`delete_world_cache`]
//!   GCs a world's file + sqlite sidecars. [`retrieve_world_fact`] /
//!   [`with_default_engine`] are retained singleton-era shims over the global
//!   path.
//!
//! Secret isolation lives upstream in `gml-world`; this crate trusts its input.

pub mod cache;
pub mod client;
pub mod doc;
pub mod engine;
pub mod error;
pub mod retrieve;
pub mod tokenize;
pub mod vector;

pub use cache::EmbeddingCache;
pub use client::{
    rerank_documents, rerank_scored, Embedder, HashEmbeddingClient, LocalEmbeddingClient,
};
pub use doc::{RagDocument, RagHit};
pub use engine::{bm25_scores, query_instruction, rank_map, RagEngine, GOOD_STATUS};
pub use error::{RagError, Result};
pub use retrieve::{
    delete_world_cache, purge_embeddings_for_texts, resolve_cache_path, retrieve_world_fact,
    retrieve_world_fact_at, retrieve_world_fact_with, set_default_engine, with_default_engine,
    with_engine_at, world_cache_path,
};
pub use tokenize::{tokens, STOPWORDS};
pub use vector::{
    decode_embedding_b64, decode_embedding_value, encode_vector, normalize, sha_text,
};
