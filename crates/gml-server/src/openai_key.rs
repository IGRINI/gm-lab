//! Local storage for an optional OpenAI Platform API key (dev token counter).
//!
//! Faithful port of `openai_key.py`. The raw key never leaves the server — only a
//! masked hint and a saved/not-saved flag are exposed to the UI. An
//! `OPENAI_API_KEY` env var wins so it can't be clobbered by the UI. The key is
//! NOT required for the app itself; it's used by the dev `/debug/tokenize` tool.

use std::path::PathBuf;

use serde_json::{Map, Value};

/// `key_path()` — `GM_OPENAI_KEY_PATH` → `%APPDATA%/gm-lab/openai-key.json` →
/// `~/.config/gm-lab/openai-key.json` (matching the Python resolution order).
fn key_path() -> PathBuf {
    if let Ok(p) = std::env::var("GM_OPENAI_KEY_PATH") {
        let t = p.trim();
        if !t.is_empty() {
            return PathBuf::from(t);
        }
    }
    if let Ok(appdata) = std::env::var("APPDATA") {
        let t = appdata.trim();
        if !t.is_empty() {
            return PathBuf::from(t).join("gm-lab").join("openai-key.json");
        }
    }
    let home = std::env::var("HOME")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| std::env::var("USERPROFILE").ok())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".config").join("gm-lab").join("openai-key.json")
}

/// `load_key()` — env var wins, else the saved file (empty on any failure).
pub fn load_key() -> String {
    if let Ok(env) = std::env::var("OPENAI_API_KEY") {
        let t = env.trim();
        if !t.is_empty() {
            return t.to_string();
        }
    }
    let path = key_path();
    if !path.exists() {
        return String::new();
    }
    match std::fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str::<Value>(&s)
            .ok()
            .and_then(|v| {
                v.get("openai_api_key")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .map(|s| s.trim().to_string())
            .unwrap_or_default(),
        Err(_) => String::new(),
    }
}

/// `save_key(key)`.
pub fn save_key(key: &str) {
    let key = key.trim();
    let path = key_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let body = serde_json::json!({ "openai_api_key": key }).to_string();
    let _ = std::fs::write(&path, body);
}

/// `delete_key()` — best-effort (ignores "not found").
pub fn delete_key() {
    let _ = std::fs::remove_file(key_path());
}

/// `masked()` — `head6…tail4`, or bullets for short keys, "" when unset.
pub fn masked() -> String {
    let key = load_key();
    if key.is_empty() {
        return String::new();
    }
    let chars: Vec<char> = key.chars().collect();
    if chars.len() <= 10 {
        return "\u{2022}".repeat(chars.len());
    }
    let head: String = chars[..6].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}\u{2026}{tail}")
}

/// `status()` — `{saved, hint}` (merged into the `{ok: true, ...}` responses).
pub fn status() -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("saved".into(), Value::Bool(!load_key().is_empty()));
    m.insert("hint".into(), Value::String(masked()));
    m
}
