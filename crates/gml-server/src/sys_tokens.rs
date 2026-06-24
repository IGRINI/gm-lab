//! Accurate system-prompt token baseline via OpenAI's input-token counter.
//!
//! The starting context size shown in the UI is dominated by the constant GM
//! system prompt. By default it is a `chars / CHARS_PER_TOKEN` estimate. When a
//! dev OpenAI key is configured, we replace that estimate with the REAL token
//! count from `/v1/responses/input_tokens`, cached on disk by SHA-256 of the
//! prompt so OpenAI is called at most once per prompt revision. After the first
//! model turn the live token totals already come from the response `_meta`; this
//! only sharpens the pre-turn baseline. Display-only, dev-key-gated, best-effort.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::openai_key;

/// Tokenizer model for the count. OpenAI's input-token endpoint only supports
/// OpenAI models, so this is a proxy tokenizer (same default as the dev
/// `/debug/tokenize` tool). Part of the cache key so a change invalidates it.
const COUNT_MODEL: &str = "gpt-4o-mini";

/// One warmup attempt per process unless [`reset`] is called (key add/remove).
static ATTEMPTED: AtomicBool = AtomicBool::new(false);

fn cache_path() -> PathBuf {
    openai_key::sibling_path("sys-token-cache.json")
}

fn sys_sha() -> String {
    let mut hasher = Sha256::new();
    hasher.update(gml_prompts::GM_SYSTEM.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Cached count for the current prompt SHA + model, or `None` on miss/mismatch.
fn cache_get(sha: &str) -> Option<i64> {
    let raw = std::fs::read_to_string(cache_path()).ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    let same_sha = v.get("sha").and_then(Value::as_str) == Some(sha);
    let same_model = v.get("model").and_then(Value::as_str) == Some(COUNT_MODEL);
    if same_sha && same_model {
        v.get("tokens").and_then(Value::as_i64)
    } else {
        None
    }
}

fn cache_put(sha: &str, tokens: i64) {
    let path = cache_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let body = json!({ "sha": sha, "model": COUNT_MODEL, "tokens": tokens }).to_string();
    let _ = std::fs::write(path, body);
}

/// Reset the one-shot guard so the next [`ensure`] re-evaluates. Call when the
/// dev key is added or removed.
pub fn reset() {
    ATTEMPTED.store(false, Ordering::Relaxed);
}

/// Best-effort: ensure the accurate system-prompt token override is set when a
/// dev key is present. Runs the network call at most once per process (until
/// [`reset`]), and never blocks the caller — the lookup/POST happens on a
/// spawned task. A cache hit applies synchronously. No dev key → no-op (the
/// chars/token estimate stands).
pub fn ensure(http: &reqwest::Client) {
    if ATTEMPTED.swap(true, Ordering::Relaxed) {
        return;
    }
    let key = openai_key::load_key();
    if key.is_empty() {
        return;
    }
    let sha = sys_sha();
    if let Some(tokens) = cache_get(&sha) {
        gml_orchestrator::compact::set_sys_tokens_override(tokens);
        return;
    }
    let http = http.clone();
    tokio::spawn(async move {
        match count_input_tokens(&http, &key, gml_prompts::GM_SYSTEM).await {
            Some(tokens) => {
                gml_orchestrator::compact::set_sys_tokens_override(tokens);
                cache_put(&sha, tokens);
            }
            None => {
                // Transient failure — allow a later attempt.
                ATTEMPTED.store(false, Ordering::Relaxed);
            }
        }
    });
}

async fn count_input_tokens(http: &reqwest::Client, key: &str, text: &str) -> Option<i64> {
    let resp = http
        .post("https://api.openai.com/v1/responses/input_tokens")
        .header("Authorization", format!("Bearer {key}"))
        .header("Content-Type", "application/json")
        .json(&json!({ "model": COUNT_MODEL, "input": text }))
        .timeout(std::time::Duration::from_secs(60))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let raw = resp.text().await.ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    v.get("input_tokens").and_then(Value::as_i64)
}
