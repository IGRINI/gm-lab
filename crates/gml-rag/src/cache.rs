//! `EmbeddingCache` — rusqlite-backed, compatible with the Python embeddings DB.
//!
//! Table `embeddings(model, text_hash, text, dims, vector_b64, created_at REAL,
//! PRIMARY KEY(model, text_hash))`; `PRAGMA journal_mode=WAL`,
//! `synchronous=NORMAL`, `busy_timeout=10000`. One short-lived connection per op
//! (matches Python).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;

use crate::error::Result;
use crate::vector::{decode_embedding_b64, encode_vector, sha_text};

/// SQLite-backed content-addressed embedding cache.
pub struct EmbeddingCache {
    path: PathBuf,
}

impl EmbeddingCache {
    /// Open (and create if needed) the cache at `path`. Mirrors Python
    /// `EmbeddingCache.__init__` which calls `_init_db()`.
    pub fn new(path: impl AsRef<Path>) -> Result<Self> {
        let cache = EmbeddingCache {
            path: path.as_ref().to_path_buf(),
        };
        cache.init_db()?;
        Ok(cache)
    }

    /// String form of the resolved path (mirrors Python `str(Path(path))`).
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn connect(&self) -> Result<Connection> {
        let con = Connection::open(&self.path)?;
        con.execute_batch(
            "PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL; PRAGMA busy_timeout = 10000;",
        )?;
        Ok(con)
    }

    fn init_db(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                let _ = std::fs::create_dir_all(parent);
            }
        }
        let con = self.connect()?;
        con.execute(
            "CREATE TABLE IF NOT EXISTS embeddings (
                    model TEXT NOT NULL,
                    text_hash TEXT NOT NULL,
                    text TEXT NOT NULL,
                    dims INTEGER NOT NULL,
                    vector_b64 TEXT NOT NULL,
                    created_at REAL NOT NULL,
                    PRIMARY KEY (model, text_hash)
                )",
            [],
        )?;
        Ok(())
    }

    /// Port of `get_many(model, texts)`: returns `{text_hash -> normalized vec}`
    /// for the cached entries of this model.
    pub fn get_many(&self, model: &str, texts: &[String]) -> Result<HashMap<String, Vec<f64>>> {
        let hashes: Vec<String> = texts.iter().map(|t| sha_text(t)).collect();
        if hashes.is_empty() {
            return Ok(HashMap::new());
        }
        let placeholders = vec!["?"; hashes.len()].join(",");
        let sql = format!(
            "SELECT text_hash, vector_b64 FROM embeddings WHERE model = ? AND text_hash IN ({placeholders})"
        );
        let con = self.connect()?;
        let mut stmt = con.prepare(&sql)?;
        // params: model, *hashes
        let mut params: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(hashes.len() + 1);
        params.push(&model);
        for h in &hashes {
            params.push(h);
        }
        let mut found: HashMap<String, Vec<f64>> = HashMap::new();
        let rows = stmt.query_map(params.as_slice(), |row| {
            let text_hash: String = row.get(0)?;
            let vector_b64: String = row.get(1)?;
            Ok((text_hash, vector_b64))
        })?;
        for row in rows {
            let (text_hash, vector_b64) = row?;
            found.insert(text_hash, decode_embedding_b64(&vector_b64)?);
        }
        Ok(found)
    }

    /// Port of `put_many(model, rows)` — INSERT OR REPLACE with
    /// `created_at = unix time` (one timestamp for the whole batch, like Python).
    pub fn put_many(&self, model: &str, rows: &[(String, Vec<f64>)]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let now = unix_time();
        let con = self.connect()?;
        let mut stmt = con.prepare(
            "INSERT OR REPLACE INTO embeddings
                    (model, text_hash, text, dims, vector_b64, created_at)
                VALUES (?, ?, ?, ?, ?, ?)",
        )?;
        for (text, vec) in rows {
            stmt.execute(rusqlite::params![
                model,
                sha_text(text),
                text,
                vec.len() as i64,
                encode_vector(vec),
                now,
            ])?;
        }
        Ok(())
    }

    /// Port of `delete_by_text_hashes(text_hashes)`: drop cached vectors for
    /// these content hashes across every model, in chunks of 400. Returns rows
    /// removed.
    pub fn delete_by_text_hashes(&self, text_hashes: &[String]) -> Result<i64> {
        let hashes: Vec<&String> = text_hashes.iter().filter(|h| !h.is_empty()).collect();
        if hashes.is_empty() {
            return Ok(0);
        }
        let con = self.connect()?;
        let mut total: i64 = 0;
        for chunk in hashes.chunks(400) {
            let placeholders = vec!["?"; chunk.len()].join(",");
            let sql = format!("DELETE FROM embeddings WHERE text_hash IN ({placeholders})");
            let params: Vec<&dyn rusqlite::ToSql> =
                chunk.iter().map(|h| *h as &dyn rusqlite::ToSql).collect();
            let removed = con.execute(&sql, params.as_slice())?;
            total += removed as i64;
        }
        Ok(total)
    }
}

/// `time.time()` — unix seconds as f64.
fn unix_time() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}
