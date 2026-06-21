//! Error type for gml-rag.

use thiserror::Error;

/// Errors surfaced by the RAG subsystem.
#[derive(Debug, Error)]
pub enum RagError {
    /// SQLite-level failure in the embedding cache.
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// HTTP failure talking to the embeddings endpoint.
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),

    /// base64 decode failure for a stored/served embedding.
    #[error("base64 decode error: {0}")]
    Base64(#[from] base64::DecodeError),

    /// JSON (de)serialization failure.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// An embedding value was neither a base64 string nor a float array.
    /// Mirrors Python `ValueError("embedding must be a base64 string or float array")`.
    #[error("embedding must be a base64 string or float array")]
    BadEmbedding,

    /// Generic value error for unexpected shapes.
    #[error("{0}")]
    Value(String),
}

/// Convenience result alias.
pub type Result<T> = std::result::Result<T, RagError>;
