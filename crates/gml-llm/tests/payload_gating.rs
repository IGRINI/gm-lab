//! Payload field-gating + key-order tests for `OpenAICompatClient`.
//!
//! These exercise the public `gml_llm::build_payload` free function (the port of
//! Python `OpenAICompatClient._payload`) without a live server, asserting the
//! exact request-body key set/order and the conditional gating that prompt-cache
//! prefix byte-identity depends on.

use std::sync::atomic::{AtomicUsize, Ordering};

use gml_config::{Config, RuntimeSettings};
use gml_llm::build_payload;
use serde_json::Value;

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Build a Config + RuntimeSettings with a fresh temp settings file. Settings
/// start from defaults: max_output_tokens=0, tool_choice=auto,
/// parallel_tool_calls=true, gm/npc reasoning effort=low, compact=none.
fn fixture(cfg: Config) -> (Config, RuntimeSettings) {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!("gml_llm_payload_test_{n}.json"));
    let _ = std::fs::remove_file(&path);
    for role in ["GM", "NPC", "COMPACT"] {
        std::env::remove_var(format!("GM_{role}_REASONING_EFFORT"));
        std::env::remove_var(format!("GM_{role}_REASONING_SUMMARY"));
    }
    std::env::remove_var("GM_CODEX_REASONING_EFFORT");
    std::env::remove_var("GM_CODEX_REASONING_SUMMARY");
    std::env::remove_var("GM_TOOL_CHOICE");
    std::env::remove_var("GM_PARALLEL_TOOL_CALLS");
    let rs = RuntimeSettings::new(&cfg, path);
    rs.get(); // materialize defaults onto disk
    (cfg, rs)
}

fn base_cfg() -> Config {
    let mut c = Config::from_env();
    c.model = "test-model".to_string();
    c.prompt_cache_key = String::new();
    c.prompt_cache_retention = String::new();
    c.use_llama_template_kwargs = false;
    c.llama_cache_reuse = 0;
    c
}

fn messages() -> Value {
    serde_json::json!([{"role": "user", "content": "hi"}])
}

#[test]
fn payload_base_keys_and_order_no_think() {
    let (cfg, rs) = fixture(base_cfg());
    // think=None -> no reasoning/sampling block at all.
    let p = build_payload(&cfg, &rs, "test-model", &messages(), None, None, None, false, "gm");
    let s = serde_json::to_string(&p).unwrap();
    assert_eq!(
        s,
        r#"{"model":"test-model","messages":[{"role":"user","content":"hi"}],"stream":false}"#
    );
}

#[test]
fn payload_cache_keys_absent_when_empty() {
    let (cfg, rs) = fixture(base_cfg());
    let p = build_payload(&cfg, &rs, "m", &messages(), None, None, None, false, "gm");
    let obj = p.as_object().unwrap();
    assert!(!obj.contains_key("prompt_cache_key"));
    assert!(!obj.contains_key("prompt_cache_retention"));
}

#[test]
fn payload_cache_keys_present_when_set() {
    let mut cfg = base_cfg();
    cfg.prompt_cache_key = "thread-xyz".to_string();
    cfg.prompt_cache_retention = "24h".to_string();
    let (cfg, rs) = fixture(cfg);
    let p = build_payload(&cfg, &rs, "m", &messages(), None, None, None, false, "gm");
    let obj = p.as_object().unwrap();
    assert_eq!(obj.get("prompt_cache_key"), Some(&Value::from("thread-xyz")));
    assert_eq!(obj.get("prompt_cache_retention"), Some(&Value::from("24h")));
    let keys: Vec<&String> = obj.keys().collect();
    assert_eq!(
        keys,
        vec!["model", "messages", "stream", "prompt_cache_key", "prompt_cache_retention"]
    );
}

#[test]
fn payload_max_tokens_gating() {
    let (cfg, rs) = fixture(base_cfg());
    let p = build_payload(&cfg, &rs, "m", &messages(), None, None, None, false, "gm");
    assert!(!p.as_object().unwrap().contains_key("max_tokens"));

    let mut m = serde_json::Map::new();
    m.insert("max_output_tokens".into(), Value::from(512));
    rs.update(Some(&m));
    let p2 = build_payload(&cfg, &rs, "m", &messages(), None, None, None, false, "gm");
    assert_eq!(p2.as_object().unwrap().get("max_tokens"), Some(&Value::from(512)));
}

#[test]
fn payload_tools_block() {
    let (cfg, rs) = fixture(base_cfg());
    let tools = serde_json::json!([{"type": "function", "function": {"name": "roll_dice"}}]);
    let p = build_payload(&cfg, &rs, "m", &messages(), Some(&tools), None, None, false, "gm");
    let obj = p.as_object().unwrap();
    assert!(obj.contains_key("tools"));
    assert_eq!(obj.get("tool_choice"), Some(&Value::from("auto")));
    assert_eq!(obj.get("parallel_tool_calls"), Some(&Value::from(true)));
}

#[test]
fn payload_tools_absent_when_empty() {
    let (cfg, rs) = fixture(base_cfg());
    let empty = serde_json::json!([]);
    let p = build_payload(&cfg, &rs, "m", &messages(), Some(&empty), None, None, false, "gm");
    let obj = p.as_object().unwrap();
    assert!(!obj.contains_key("tools"));
    assert!(!obj.contains_key("tool_choice"));
    assert!(!obj.contains_key("parallel_tool_calls"));
}

#[test]
fn payload_parallel_false_when_tool_choice_none() {
    let (cfg, rs) = fixture(base_cfg());
    let mut m = serde_json::Map::new();
    m.insert("tool_choice".into(), Value::from("none"));
    rs.update(Some(&m));
    let tools = serde_json::json!([{"type": "function", "function": {"name": "x"}}]);
    let p = build_payload(&cfg, &rs, "m", &messages(), Some(&tools), None, None, false, "gm");
    let obj = p.as_object().unwrap();
    assert_eq!(obj.get("tool_choice"), Some(&Value::from("none")));
    assert_eq!(obj.get("parallel_tool_calls"), Some(&Value::from(false)));
}

#[test]
fn payload_non_llama_sampling_subset() {
    // USE_LLAMA_TEMPLATE_KWARGS=false: only temperature/top_p/presence_penalty,
    // NO chat_template_kwargs / top_k / min_p / n_cache_reuse.
    let (cfg, rs) = fixture(base_cfg());
    let p = build_payload(&cfg, &rs, "m", &messages(), None, Some(true), None, false, "gm");
    let obj = p.as_object().unwrap();
    assert!(!obj.contains_key("chat_template_kwargs"));
    assert!(!obj.contains_key("top_k"));
    assert!(!obj.contains_key("min_p"));
    assert!(!obj.contains_key("n_cache_reuse"));
    assert_eq!(obj.get("temperature"), Some(&Value::from(0.6)));
    assert_eq!(obj.get("top_p"), Some(&Value::from(0.95)));
    assert_eq!(obj.get("presence_penalty"), Some(&Value::from(1.5)));
}

#[test]
fn payload_llama_full_sampling_and_cache_reuse_gating() {
    let mut cfg = base_cfg();
    cfg.use_llama_template_kwargs = true;
    cfg.llama_cache_reuse = 0;
    let (cfg, rs) = fixture(cfg);
    let p = build_payload(&cfg, &rs, "m", &messages(), None, Some(true), None, false, "gm");
    let obj = p.as_object().unwrap();
    assert_eq!(
        obj.get("chat_template_kwargs"),
        Some(&serde_json::json!({"enable_thinking": true}))
    );
    assert_eq!(obj.get("temperature"), Some(&Value::from(0.6)));
    assert_eq!(obj.get("top_p"), Some(&Value::from(0.95)));
    assert_eq!(obj.get("top_k"), Some(&Value::from(20)));
    assert_eq!(obj.get("min_p"), Some(&Value::from(0)));
    assert_eq!(obj.get("presence_penalty"), Some(&Value::from(1.5)));
    assert!(!obj.contains_key("n_cache_reuse"));
}

#[test]
fn payload_n_cache_reuse_present_when_positive() {
    let mut cfg = base_cfg();
    cfg.use_llama_template_kwargs = true;
    cfg.llama_cache_reuse = 256;
    let (cfg, rs) = fixture(cfg);
    let p = build_payload(&cfg, &rs, "m", &messages(), None, Some(true), None, false, "gm");
    assert_eq!(p.as_object().unwrap().get("n_cache_reuse"), Some(&Value::from(256)));
}

#[test]
fn payload_n_cache_reuse_absent_when_not_llama_even_if_positive() {
    let mut cfg = base_cfg();
    cfg.use_llama_template_kwargs = false;
    cfg.llama_cache_reuse = 256;
    let (cfg, rs) = fixture(cfg);
    let p = build_payload(&cfg, &rs, "m", &messages(), None, Some(true), None, false, "gm");
    assert!(!p.as_object().unwrap().contains_key("n_cache_reuse"));
}

#[test]
fn payload_min_p_is_integer_zero_in_wire_bytes() {
    let mut cfg = base_cfg();
    cfg.use_llama_template_kwargs = true;
    let (cfg, rs) = fixture(cfg);
    let p = build_payload(&cfg, &rs, "m", &messages(), None, Some(false), None, false, "gm");
    let s = serde_json::to_string(&p).unwrap();
    assert!(s.contains(r#""min_p":0,"#), "min_p must be int 0 in: {s}");
    assert!(!s.contains("0.0"));
    assert!(s.contains(r#""temperature":0.7"#));
    assert!(s.contains(r#""top_p":0.8"#));
}

#[test]
fn payload_effective_think_disabled_uses_plain_sampling() {
    // compact role effort defaults to "none" -> reasoning_enabled(true,"compact")=false
    // -> SAMPLING_PLAIN, chat_template_kwargs.enable_thinking=false.
    let mut cfg = base_cfg();
    cfg.use_llama_template_kwargs = true;
    let (cfg, rs) = fixture(cfg);
    let p = build_payload(&cfg, &rs, "m", &messages(), None, Some(true), None, false, "compact");
    let obj = p.as_object().unwrap();
    assert_eq!(
        obj.get("chat_template_kwargs"),
        Some(&serde_json::json!({"enable_thinking": false}))
    );
    assert_eq!(obj.get("temperature"), Some(&Value::from(0.7)));
}

#[test]
fn payload_stream_options_when_stream() {
    let (cfg, rs) = fixture(base_cfg());
    let p = build_payload(&cfg, &rs, "m", &messages(), None, None, None, true, "gm");
    let obj = p.as_object().unwrap();
    assert_eq!(obj.get("stream"), Some(&Value::from(true)));
    assert_eq!(
        obj.get("stream_options"),
        Some(&serde_json::json!({"include_usage": true}))
    );
}

#[test]
fn payload_response_format_present() {
    let (cfg, rs) = fixture(base_cfg());
    let rf = serde_json::json!({"type": "json_object"});
    let p = build_payload(&cfg, &rs, "m", &messages(), None, Some(false), Some(&rf), false, "gm");
    let obj = p.as_object().unwrap();
    assert_eq!(obj.get("response_format"), Some(&rf));
}

#[test]
fn payload_full_key_order_with_tools_and_llama_thinking() {
    // Exact key order across all gated sections for a realistic GM streaming call.
    let mut cfg = base_cfg();
    cfg.use_llama_template_kwargs = true;
    cfg.prompt_cache_key = "k".to_string();
    let (cfg, rs) = fixture(cfg);
    let tools = serde_json::json!([{"type": "function", "function": {"name": "x"}}]);
    let p = build_payload(&cfg, &rs, "m", &messages(), Some(&tools), Some(true), None, true, "gm");
    let keys: Vec<&str> = p.as_object().unwrap().keys().map(|s| s.as_str()).collect();
    assert_eq!(
        keys,
        vec![
            "model",
            "messages",
            "stream",
            "prompt_cache_key",
            "tools",
            "tool_choice",
            "parallel_tool_calls",
            "chat_template_kwargs",
            "temperature",
            "top_p",
            "top_k",
            "min_p",
            "presence_penalty",
            "stream_options",
        ]
    );
}
