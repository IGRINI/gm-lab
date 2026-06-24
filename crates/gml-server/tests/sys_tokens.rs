//! The dev-key system-prompt token override: a disk-cache hit (keyed by SHA of
//! the system prompt) must set the accurate baseline that `sys_est()` returns,
//! synchronously and WITHOUT any network call — the no-network success path of
//! `sys_tokens::ensure`.

use sha2::{Digest, Sha256};

#[test]
fn sys_token_cache_hit_sets_accurate_override() {
    let tmp = tempfile::tempdir().unwrap();
    // Route both the key dir (→ cache sibling) and a non-empty dev key so
    // `ensure` proceeds to the cache lookup.
    std::env::set_var("GM_OPENAI_KEY_PATH", tmp.path().join("openai-key.json"));
    std::env::set_var("OPENAI_API_KEY", "sk-test-dev-key");

    // Pre-seed the cache for the CURRENT system prompt + counting model.
    let mut hasher = Sha256::new();
    hasher.update(gml_prompts::GM_SYSTEM.as_bytes());
    let sha = format!("{:x}", hasher.finalize());
    std::fs::write(
        tmp.path().join("sys-token-cache.json"),
        serde_json::json!({ "sha": sha, "model": "gpt-4o-mini", "tokens": 4242 }).to_string(),
    )
    .unwrap();

    gml_orchestrator::compact::clear_sys_tokens_override();
    gml_server::sys_tokens::reset();
    // Cache hit → override applied synchronously (returns before any spawn/POST).
    gml_server::sys_tokens::ensure(&reqwest::Client::new());

    assert_eq!(
        gml_orchestrator::compact::sys_est(),
        4242,
        "cache hit must set the accurate system-prompt baseline"
    );

    // Don't leak the override / env into other test binaries' expectations.
    gml_orchestrator::compact::clear_sys_tokens_override();
    std::env::remove_var("OPENAI_API_KEY");
    std::env::remove_var("GM_OPENAI_KEY_PATH");
}
