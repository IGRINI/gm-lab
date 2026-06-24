//! Response cleaning + stats normalization helpers ported from `llm_client.py`.

use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{Map, Value};

/// Chat-template service tokens that occasionally leak into text.
/// Python: `_TOKEN_RE = re.compile(r"</?\|?[a-zA-Z_]+\|?>")`.
static TOKEN_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"</?\|?[a-zA-Z_]+\|?>").expect("valid token regex"));

/// Leading channel marker.
/// Python: `_LEAD_CHANNEL_RE = re.compile(r"^\s*(thought|analysis|final|commentary)\b[\s:]*", re.I)`.
static LEAD_CHANNEL_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)^\s*(thought|analysis|final|commentary)\b[\s:]*")
        .expect("valid lead-channel regex")
});

/// `_clean(text)` — strip leaked chat-template tokens and a leading channel
/// marker, then `strip()`.
///
/// Python:
/// ```python
/// def _clean(text: str) -> str:
///     text = _TOKEN_RE.sub("", text)
///     text = _LEAD_CHANNEL_RE.sub("", text)
///     return text.strip()
/// ```
pub fn clean(text: &str) -> String {
    let t = TOKEN_RE.replace_all(text, "");
    let t = LEAD_CHANNEL_RE.replace(&t, "");
    python_strip(&t).to_string()
}

/// `_think(s)` — strip `<think>` / `</think>` fragments, then `strip()`.
///
/// Python: `re.sub(r"</?think>", "", s or "").strip()`.
pub fn think(s: Option<&str>) -> String {
    static THINK_RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"</?think>").expect("valid think regex"));
    let s = s.unwrap_or("");
    python_strip(&THINK_RE.replace_all(s, "")).to_string()
}

/// `_assistant_msg(content, raw_tool_calls)` — build the assistant message to
/// store in history (OpenAI format). `reasoning_content` is deliberately NOT
/// stored — Qwen does not need past thoughts re-fed in multi-turn.
///
/// Python:
/// ```python
/// def _assistant_msg(content, raw_tool_calls) -> dict:
///     msg = {"role": "assistant", "content": content or ""}
///     if raw_tool_calls:
///         msg["tool_calls"] = raw_tool_calls
///     return msg
/// ```
///
/// Key order is `role`, `content`, then optionally `tool_calls` — preserved via
/// the `preserve_order` serde feature. `raw_tool_calls` is the *raw* OpenAI
/// `tool_calls` array (not the parsed [`gml_types::ParsedCall`] form).
pub fn assistant_msg(content: &str, raw_tool_calls: Option<&Value>) -> Value {
    let mut msg = Map::new();
    msg.insert("role".to_string(), Value::String("assistant".to_string()));
    msg.insert("content".to_string(), Value::String(content.to_string()));
    if let Some(tc) = raw_tool_calls {
        // `if raw_tool_calls:` — only attach when truthy (non-empty array).
        let truthy = match tc {
            Value::Array(a) => !a.is_empty(),
            Value::Null => false,
            _ => true,
        };
        if truthy {
            msg.insert("tool_calls".to_string(), tc.clone());
        }
    }
    Value::Object(msg)
}

/// `_stats(usage, timings)` — normalize llama.cpp stats into the `_meta` shape
/// (durations in nanoseconds).
///
/// Python:
/// ```python
/// def _stats(usage, timings) -> dict:
///     s = {"load_duration": 0, "cached_tokens": 0}
///     if usage:
///         s["prompt_eval_count"] = usage.get("prompt_tokens", 0)
///         s["eval_count"] = usage.get("completion_tokens", 0)
///         prompt_details = usage.get("prompt_tokens_details") or usage.get("input_tokens_details") or {}
///         s["cached_tokens"] = int(prompt_details.get("cached_tokens", 0) or 0)
///     if timings:
///         pm, em = timings.get("prompt_ms", 0) or 0, timings.get("predicted_ms", 0) or 0
///         s["prompt_eval_duration"] = int(pm * 1e6)
///         s["eval_duration"] = int(em * 1e6)
///         s["total_duration"] = int((pm + em) * 1e6)
///     return s
/// ```
///
/// Insertion order matches Python exactly: `load_duration`, `cached_tokens`,
/// then (if usage) `prompt_eval_count`, `eval_count`, then `cached_tokens` is
/// *overwritten* in place (Python dict assignment keeps original position), then
/// (if timings) `prompt_eval_duration`, `eval_duration`, `total_duration`.
pub fn stats(usage: Option<&Value>, timings: Option<&Value>) -> Map<String, Value> {
    let mut s = Map::new();
    s.insert("load_duration".to_string(), Value::from(0));
    s.insert("cached_tokens".to_string(), Value::from(0));

    if let Some(usage) = truthy(usage) {
        let prompt_tokens = get_number_or(usage, "prompt_tokens", 0);
        let completion_tokens = get_number_or(usage, "completion_tokens", 0);
        s.insert("prompt_eval_count".to_string(), Value::from(prompt_tokens));
        s.insert("eval_count".to_string(), Value::from(completion_tokens));

        // prompt_tokens_details or input_tokens_details or {}
        let details = usage
            .get("prompt_tokens_details")
            .filter(|v| is_truthy(v))
            .or_else(|| usage.get("input_tokens_details").filter(|v| is_truthy(v)));
        let cached = match details {
            Some(d) => get_number_or(d, "cached_tokens", 0),
            None => 0,
        };
        // Overwrite cached_tokens in place (Python dict re-assignment).
        s.insert("cached_tokens".to_string(), Value::from(cached));
    }

    if let Some(timings) = truthy(timings) {
        let pm = get_float_or(timings, "prompt_ms", 0.0);
        let em = get_float_or(timings, "predicted_ms", 0.0);
        s.insert(
            "prompt_eval_duration".to_string(),
            Value::from(py_int(pm * 1e6)),
        );
        s.insert("eval_duration".to_string(), Value::from(py_int(em * 1e6)));
        s.insert(
            "total_duration".to_string(),
            Value::from(py_int((pm + em) * 1e6)),
        );
    }

    s
}

/// `_mock_stats()` — canned stats for the mock backend.
///
/// Python:
/// ```python
/// def _mock_stats():
///     return {"prompt_eval_count": 760, "eval_count": 120, "prompt_eval_duration": 80_000_000,
///             "eval_duration": 640_000_000, "total_duration": 730_000_000, "load_duration": 0}
/// ```
pub fn mock_stats() -> Map<String, Value> {
    let mut s = Map::new();
    s.insert("prompt_eval_count".to_string(), Value::from(760));
    s.insert("eval_count".to_string(), Value::from(120));
    s.insert("prompt_eval_duration".to_string(), Value::from(80_000_000));
    s.insert("eval_duration".to_string(), Value::from(640_000_000));
    s.insert("total_duration".to_string(), Value::from(730_000_000));
    s.insert("load_duration".to_string(), Value::from(0));
    s
}

/// `_proper_nouns_line(proper_nouns=None)` — build the proper-nouns guidance
/// line for `summarize`. (Identical logic to `gml_prompts::gm_compact_proper_nouns_line`,
/// re-implemented here to keep `llm_client.summarize` self-contained and exact.)
pub fn proper_nouns_line(proper_nouns: &[String]) -> String {
    gml_prompts::gm_compact_proper_nouns_line(proper_nouns.iter())
}

// --- small coercion helpers --------------------------------------------------

/// Python truthiness of an Optional JSON value used as `if usage:` / `if timings:`.
fn truthy(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| is_truthy(x))
}

fn is_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i != 0
            } else if let Some(u) = n.as_u64() {
                u != 0
            } else {
                n.as_f64().map(|f| f != 0.0).unwrap_or(false)
            }
        }
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// `usage.get(key, 0)` returning an integer; non-numeric -> 0.
fn get_number_or(obj: &Value, key: &str, default: i64) -> i64 {
    obj.get(key)
        .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)))
        .unwrap_or(default)
}

/// `timings.get(key, 0) or 0` as a float.
fn get_float_or(obj: &Value, key: &str, default: f64) -> f64 {
    match obj.get(key) {
        Some(v) if is_truthy(v) => v.as_f64().unwrap_or(default),
        _ => default,
    }
}

/// Python `int(x)` for the duration computations — truncates toward zero.
fn py_int(x: f64) -> i64 {
    x.trunc() as i64
}

fn python_strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn clean_strips_tokens_and_channel() {
        assert_eq!(clean("<|im_start|>hello<|im_end|>"), "hello");
        assert_eq!(clean("  final: the door opens  "), "the door opens");
        assert_eq!(clean("analysis the player acts"), "the player acts");
        // `_clean` strips chat-template-style tokens via _TOKEN_RE; <think>/</think>
        // match that regex too, so they are removed (leaving the inner text). This
        // is distinct from `_think`, which only strips <think> tags.
        assert_eq!(clean("<think>x</think>y"), "xy");
        assert_eq!(clean("plain"), "plain");
    }

    #[test]
    fn clean_leading_channel_case_insensitive() {
        assert_eq!(clean("FINAL: done"), "done");
        assert_eq!(clean("Commentary:  note"), "note");
    }

    #[test]
    fn think_strips_think_tags() {
        assert_eq!(think(Some("<think>reasoning</think>")), "reasoning");
        assert_eq!(think(Some("  partial</think>  ")), "partial");
        assert_eq!(think(None), "");
        assert_eq!(think(Some("")), "");
    }

    #[test]
    fn assistant_msg_no_tool_calls() {
        let m = assistant_msg("hi", None);
        assert_eq!(
            serde_json::to_string(&m).unwrap(),
            r#"{"role":"assistant","content":"hi"}"#
        );
    }

    #[test]
    fn assistant_msg_empty_content() {
        // content or "" -> we pass "" directly when content is empty.
        let m = assistant_msg("", None);
        assert_eq!(
            serde_json::to_string(&m).unwrap(),
            r#"{"role":"assistant","content":""}"#
        );
    }

    #[test]
    fn assistant_msg_with_tool_calls_order() {
        let tc = json!([{"id":"c1","type":"function","function":{"name":"x","arguments":"{}"}}]);
        let m = assistant_msg("", Some(&tc));
        let s = serde_json::to_string(&m).unwrap();
        // role, content, tool_calls in that order
        assert!(s.starts_with(r#"{"role":"assistant","content":"","tool_calls":"#));
    }

    #[test]
    fn assistant_msg_empty_tool_calls_omitted() {
        let tc = json!([]);
        let m = assistant_msg("text", Some(&tc));
        assert_eq!(
            serde_json::to_string(&m).unwrap(),
            r#"{"role":"assistant","content":"text"}"#
        );
    }

    #[test]
    fn stats_no_usage_no_timings() {
        let s = stats(None, None);
        assert_eq!(s.get("load_duration"), Some(&json!(0)));
        assert_eq!(s.get("cached_tokens"), Some(&json!(0)));
        assert_eq!(s.len(), 2);
        // key order: load_duration, cached_tokens
        let out = serde_json::to_string(&Value::Object(s)).unwrap();
        assert_eq!(out, r#"{"load_duration":0,"cached_tokens":0}"#);
    }

    #[test]
    fn stats_with_usage_and_timings() {
        let usage = json!({
            "prompt_tokens": 100,
            "completion_tokens": 20,
            "prompt_tokens_details": {"cached_tokens": 64}
        });
        let timings = json!({"prompt_ms": 50.0, "predicted_ms": 200.0});
        let s = stats(Some(&usage), Some(&timings));
        assert_eq!(s.get("prompt_eval_count"), Some(&json!(100)));
        assert_eq!(s.get("eval_count"), Some(&json!(20)));
        assert_eq!(s.get("cached_tokens"), Some(&json!(64)));
        assert_eq!(s.get("prompt_eval_duration"), Some(&json!(50_000_000)));
        assert_eq!(s.get("eval_duration"), Some(&json!(200_000_000)));
        assert_eq!(s.get("total_duration"), Some(&json!(250_000_000)));
        // Exact key order: load_duration, cached_tokens, prompt_eval_count,
        // eval_count, prompt_eval_duration, eval_duration, total_duration.
        let out = serde_json::to_string(&Value::Object(s)).unwrap();
        assert_eq!(
            out,
            r#"{"load_duration":0,"cached_tokens":64,"prompt_eval_count":100,"eval_count":20,"prompt_eval_duration":50000000,"eval_duration":200000000,"total_duration":250000000}"#
        );
    }

    #[test]
    fn stats_input_tokens_details_fallback() {
        let usage = json!({
            "prompt_tokens": 5,
            "completion_tokens": 1,
            "input_tokens_details": {"cached_tokens": 3}
        });
        let s = stats(Some(&usage), None);
        assert_eq!(s.get("cached_tokens"), Some(&json!(3)));
    }

    #[test]
    fn mock_stats_exact() {
        let s = mock_stats();
        let out = serde_json::to_string(&Value::Object(s)).unwrap();
        assert_eq!(
            out,
            r#"{"prompt_eval_count":760,"eval_count":120,"prompt_eval_duration":80000000,"eval_duration":640000000,"total_duration":730000000,"load_duration":0}"#
        );
    }

    #[test]
    fn proper_nouns_line_empty_and_named() {
        assert_eq!(
            proper_nouns_line(&[]),
            "Keep proper nouns exactly as written in the transcript; never transliterate them."
        );
        assert_eq!(
            proper_nouns_line(&[
                "Борин".to_string(),
                "  ".to_string(),
                "Нордхольм".to_string()
            ]),
            "Keep these proper nouns exactly as written if they appear; never translate or \
             transliterate them: Борин, Нордхольм."
        );
    }
}
