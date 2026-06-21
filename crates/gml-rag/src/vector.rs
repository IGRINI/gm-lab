//! Vector helpers — `_normalize`, `_decode_embedding`, `_encode_vector`.
//!
//! Python stores vectors as little-endian f32 (`array('f')`) but computes the L2
//! norm in f64 (native Python floats). We mirror that exactly: decode f32 bytes,
//! widen each to f64, then normalize in f64.

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::Value;

use crate::error::{RagError, Result};

/// EXACT port of `_normalize(vec)` (L2). Returns the input unchanged when the
/// norm is zero/falsy.
pub fn normalize(vec: Vec<f64>) -> Vec<f64> {
    let norm = vec.iter().map(|v| v * v).sum::<f64>().sqrt();
    if norm == 0.0 {
        return vec;
    }
    vec.into_iter().map(|v| v / norm).collect()
}

/// Decode a base64 little-endian f32 vector and L2-normalize it.
///
/// Port of `_decode_embedding(value)` for the string (base64) case.
pub fn decode_embedding_b64(value: &str) -> Result<Vec<f64>> {
    let raw = STANDARD.decode(value.as_bytes())?;
    let mut floats: Vec<f64> = Vec::with_capacity(raw.len() / 4);
    // array('f').frombytes truncates to whole 4-byte units; chunks_exact does the same.
    for chunk in raw.chunks_exact(4) {
        let bytes: [u8; 4] = [chunk[0], chunk[1], chunk[2], chunk[3]];
        floats.push(f32::from_le_bytes(bytes) as f64);
    }
    Ok(normalize(floats))
}

/// Port of `_decode_embedding(value)` operating on a JSON value: a list of
/// numbers OR a base64 string. Anything else is an error.
pub fn decode_embedding_value(value: &Value) -> Result<Vec<f64>> {
    match value {
        Value::Array(items) => {
            let mut out: Vec<f64> = Vec::with_capacity(items.len());
            for item in items {
                let f = item
                    .as_f64()
                    .ok_or_else(|| RagError::Value("embedding element is not a float".into()))?;
                out.push(f);
            }
            Ok(normalize(out))
        }
        Value::String(s) => decode_embedding_b64(s),
        _ => Err(RagError::BadEmbedding),
    }
}

/// EXACT port of `_encode_vector(vec)`: f64 -> f32 little-endian bytes -> base64.
pub fn encode_vector(vec: &[f64]) -> String {
    let mut bytes: Vec<u8> = Vec::with_capacity(vec.len() * 4);
    for &v in vec {
        bytes.extend_from_slice(&(v as f32).to_le_bytes());
    }
    STANDARD.encode(bytes)
}

/// `sha256(text)` hex digest — the embedding cache key. Port of `_sha`.
pub fn sha_text(text: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    let digest = hasher.finalize();
    let mut s = String::with_capacity(64);
    for b in digest {
        s.push_str(&format!("{:02x}", b));
    }
    s
}
