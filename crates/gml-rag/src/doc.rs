//! `RagDocument` and `RagHit` — faithful port of `rag.py` dataclasses.

use serde_json::{Map, Value};

/// A retrievable world-memory document.
///
/// Port of Python `@dataclass(frozen=True) RagDocument`. Field defaults match
/// the Python defaults: `status="known"`, `source=""`, `visibility="player"`,
/// empty `tags`, empty `metadata`.
#[derive(Debug, Clone, PartialEq)]
pub struct RagDocument {
    pub doc_id: String,
    pub kind: String,
    pub text: String,
    pub status: String,
    pub source: String,
    pub visibility: String,
    pub tags: Vec<String>,
    pub metadata: Map<String, Value>,
}

impl RagDocument {
    /// Construct with Python defaults for the optional fields.
    pub fn new(
        doc_id: impl Into<String>,
        kind: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        RagDocument {
            doc_id: doc_id.into(),
            kind: kind.into(),
            text: text.into(),
            status: "known".to_string(),
            source: String::new(),
            visibility: "player".to_string(),
            tags: Vec::new(),
            metadata: Map::new(),
        }
    }

    /// EXACT port of `RagDocument.contextual_text()`.
    ///
    /// ```text
    /// RPG world memory block.
    /// Kind: {kind}.
    /// Status: {status}.
    /// [Source: {source}.]        # only when source is non-empty
    /// [Tags: a, b.]              # only when tags is non-empty
    /// Text: {text.strip()}
    /// ```
    pub fn contextual_text(&self) -> String {
        let mut meta: Vec<String> = vec![
            "RPG world memory block.".to_string(),
            format!("Kind: {}.", self.kind),
            format!("Status: {}.", self.status),
        ];
        if !self.source.is_empty() {
            meta.push(format!("Source: {}.", self.source));
        }
        if !self.tags.is_empty() {
            meta.push(format!("Tags: {}.", self.tags.join(", ")));
        }
        format!("{}\nText: {}", meta.join("\n"), py_strip(&self.text))
    }
}

/// A scored retrieval hit. Port of Python `@dataclass(frozen=True) RagHit`.
#[derive(Debug, Clone)]
pub struct RagHit {
    pub document: RagDocument,
    pub score: f64,
    pub dense_score: f64,
    pub keyword_score: f64,
}

/// Python `str.strip()`: strip leading/trailing Unicode whitespace.
pub(crate) fn py_strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace())
}
