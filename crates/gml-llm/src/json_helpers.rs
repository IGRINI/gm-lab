//! JSON extraction / parsing helpers ported from `llm_client.py`.
//!
//! These are byte-fidelity-critical: `extract_json_string` / `json_unescape`
//! are used to pull a string field out of a *growing* (streamed) JSON buffer,
//! and the limited escape set of `json_unescape` is a deliberate, documented
//! limitation of the Python source (it does NOT handle `\uXXXX`). We replicate
//! it exactly, including the sequential `str.replace` ordering, so that the
//! streamed output matches the Python implementation byte-for-byte.

use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{Map, Value};

use gml_types::ParsedCall;

/// `_loads(text)` — extract a JSON object from a string, tolerating fences /
/// surrounding garbage.
///
/// Python:
/// ```python
/// def _loads(text: str) -> dict:
///     text = (text or "").strip()
///     if not text:
///         return {}
///     try:
///         return json.loads(text)
///     except Exception:
///         m = re.search(r"\{.*\}", text, re.DOTALL)
///         if m:
///             try:
///                 return json.loads(m.group(0))
///             except Exception:
///                 return {}
///     return {}
/// ```
/// Returns an empty object `{}` on any failure. Note Python's `json.loads`
/// could return a non-dict (e.g. a list) on the happy path; the function is
/// type-annotated `-> dict` but does not enforce it. We mirror Python's actual
/// runtime behaviour: whatever `json.loads` produced is returned. Callers in
/// the original code always feed object-shaped payloads, but to preserve the
/// observable behaviour we return the parsed [`Value`] and let the caller treat
/// non-object results as they would in Python (truthiness etc.).
pub fn loads_value(text: &str) -> Value {
    let trimmed = python_strip(text);
    if trimmed.is_empty() {
        return Value::Object(Map::new());
    }
    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
        return v;
    }
    static BRACE_RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"(?s)\{.*\}").expect("valid brace regex"));
    if let Some(m) = BRACE_RE.find(trimmed) {
        if let Ok(v) = serde_json::from_str::<Value>(m.as_str()) {
            return v;
        }
        return Value::Object(Map::new());
    }
    Value::Object(Map::new())
}

/// `_loads(text)` returning a JSON object map. Mirrors the dataflow at the call
/// sites (`chat_json` etc.) which immediately use the result as a dict and
/// treat a missing/empty parse as falsy `{}`. A non-object parse result becomes
/// an empty map (Python would have returned the list/scalar, but every caller
/// in `llm_client.py` then does `if out:` / `.get(...)`, and the contract tests
/// only exercise object payloads — we keep the object-typed convenience here and
/// expose [`loads_value`] for the rare non-object case).
pub fn loads_map(text: &str) -> Map<String, Value> {
    match loads_value(text) {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

/// Python `str.strip()` — trims leading/trailing whitespace per Python's
/// `str.isspace` set (which includes ASCII whitespace and some unicode). For the
/// inputs here (model output), ASCII + common unicode spaces suffice; we use
/// Rust's `char::is_whitespace`, which matches Python's set for the practical
/// cases (and is what other ported crates rely on).
fn python_strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace())
}

/// `extract_json_string(buf, field)` — from a GROWING JSON buffer, extract the
/// value of a string field. Returns `(raw_or_none, complete)`.
///
/// Python:
/// ```python
/// def extract_json_string(buf: str, field: str):
///     key = '"' + field + '"'
///     i = buf.find(key)
///     if i < 0:
///         return None, False
///     j = buf.find(":", i + len(key))
///     if j < 0:
///         return None, False
///     k = buf.find('"', j + 1)
///     if k < 0:
///         return None, False
///     out, p, esc = [], k + 1, False
///     while p < len(buf):
///         c = buf[p]
///         if esc:
///             out.append(c); esc = False
///         elif c == "\\":
///             out.append(c); esc = True
///         elif c == '"':
///             return "".join(out), True
///         else:
///             out.append(c)
///         p += 1
///     return "".join(out), False
/// ```
///
/// The raw substring keeps escape sequences verbatim (a literal `\n` stays as
/// the two chars backslash-n); decode with [`json_unescape`]. The boolean is
/// `true` only once the closing quote has arrived.
///
/// Fidelity: Python iterates over *code points* (`buf[p]` indexes characters),
/// and `find` returns character indices. We replicate this by working over a
/// `Vec<char>`, so multi-byte (Cyrillic) content behaves identically.
pub fn extract_json_string(buf: &str, field: &str) -> (Option<String>, bool) {
    let chars: Vec<char> = buf.chars().collect();
    let key: Vec<char> = {
        let mut k = Vec::with_capacity(field.chars().count() + 2);
        k.push('"');
        k.extend(field.chars());
        k.push('"');
        k
    };

    let i = match find_subslice(&chars, &key, 0) {
        Some(idx) => idx,
        None => return (None, false),
    };
    let j = match find_char(&chars, ':', i + key.len()) {
        Some(idx) => idx,
        None => return (None, false),
    };
    let k = match find_char(&chars, '"', j + 1) {
        Some(idx) => idx,
        None => return (None, false),
    };

    let mut out = String::new();
    let mut p = k + 1;
    let mut esc = false;
    while p < chars.len() {
        let c = chars[p];
        if esc {
            out.push(c);
            esc = false;
        } else if c == '\\' {
            out.push(c);
            esc = true;
        } else if c == '"' {
            return (Some(out), true);
        } else {
            out.push(c);
        }
        p += 1;
    }
    (Some(out), false)
}

/// `json_unescape(s)` — decode the LIMITED escape set the Python helper handles.
///
/// Python:
/// ```python
/// def json_unescape(s: str) -> str:
///     return (s.replace('\\"', '"').replace("\\n", "\n").replace("\\t", "\t")
///              .replace("\\/", "/").replace("\\\\", "\\"))
/// ```
///
/// DEVIATION (documented, faithful to source): `\uXXXX` is NOT handled — this is
/// a latent limitation of the Python implementation (PORT_PLAN §9 risk #8). We
/// replicate the exact *sequential* `str.replace` order, which can interact in
/// surprising ways (e.g. `\\n` -> first `\\"`/`\\n` passes are applied left to
/// right). Reproducing the order guarantees byte-identical output.
pub fn json_unescape(s: &str) -> String {
    // Each `.replace` is a full pass over the (progressively rewritten) string,
    // applied in this exact order. `str::replace` in Rust is non-overlapping
    // left-to-right, matching Python `str.replace`.
    s.replace("\\\"", "\"")
        .replace("\\n", "\n")
        .replace("\\t", "\t")
        .replace("\\/", "/")
        .replace("\\\\", "\\")
}

/// `_parse_tool_calls(raw)` — OpenAI `tool_calls` -> `[{name, arguments(dict), id}]`.
///
/// Python:
/// ```python
/// def _parse_tool_calls(raw) -> list:
///     calls = []
///     for tc in raw or []:
///         fn = (tc.get("function") if isinstance(tc, dict) else None) or {}
///         args = fn.get("arguments")
///         if isinstance(args, str):
///             args = _loads(args)
///         calls.append({"name": fn.get("name"), "arguments": args or {}, "id": tc.get("id", "")})
///     return calls
/// ```
///
/// The wire form sends `arguments` as a JSON *string*; it is parsed via
/// [`loads_map`]. `name` may be `null` in Python (`fn.get("name")`); we coerce a
/// missing name to an empty string for the [`ParsedCall`] shape (the orchestrator
/// only inspects `name` as a string). `arguments` defaults to `{}`, `id` to `""`.
pub fn parse_tool_calls(raw: Option<&Value>) -> Vec<ParsedCall> {
    let mut calls = Vec::new();
    let items: &[Value] = match raw {
        Some(Value::Array(a)) => a.as_slice(),
        _ => &[],
    };
    for tc in items {
        let func = tc.get("function").and_then(|f| f.as_object());
        let name = func
            .and_then(|f| f.get("name"))
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_string();
        // args = fn.get("arguments"); if isinstance(args, str): args = _loads(args)
        let arguments: Map<String, Value> = match func.and_then(|f| f.get("arguments")) {
            Some(Value::String(s)) => loads_map(s),
            Some(Value::Object(m)) => m.clone(),
            // `args or {}` — anything falsy/absent -> {}
            _ => Map::new(),
        };
        let id = tc
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        calls.push(ParsedCall::new(name, arguments, id));
    }
    calls
}

// --- char-index search helpers (mirror Python str.find over code points) -----

fn find_char(haystack: &[char], needle: char, from: usize) -> Option<usize> {
    if from > haystack.len() {
        return None;
    }
    haystack[from..]
        .iter()
        .position(|&c| c == needle)
        .map(|p| p + from)
}

fn find_subslice(haystack: &[char], needle: &[char], from: usize) -> Option<usize> {
    if needle.is_empty() {
        return Some(from.min(haystack.len()));
    }
    if from >= haystack.len() || needle.len() > haystack.len() - from {
        // still allow scanning when needle could fit
    }
    let mut i = from;
    while i + needle.len() <= haystack.len() {
        if haystack[i..i + needle.len()] == *needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extract_json_string_complete() {
        let buf = r#"{"speech":"привет","action":"кивает"}"#;
        let (raw, complete) = extract_json_string(buf, "speech");
        assert_eq!(raw.as_deref(), Some("привет"));
        assert!(complete);
    }

    #[test]
    fn extract_json_string_partial_then_complete() {
        // Growing buffer: speech value not yet closed.
        let partial = r#"{"reasoning":"ok","speech":"я выйду"#;
        let (raw, complete) = extract_json_string(partial, "speech");
        assert_eq!(raw.as_deref(), Some("я выйду"));
        assert!(!complete, "no closing quote yet");

        let grown = r#"{"reasoning":"ok","speech":"я выйду через дверь"}"#;
        let (raw2, complete2) = extract_json_string(grown, "speech");
        assert_eq!(raw2.as_deref(), Some("я выйду через дверь"));
        assert!(complete2);
    }

    #[test]
    fn extract_json_string_field_not_present() {
        let buf = r#"{"reasoning":"thinking"#;
        let (raw, complete) = extract_json_string(buf, "speech");
        assert_eq!(raw, None);
        assert!(!complete);
    }

    #[test]
    fn extract_json_string_no_colon_yet() {
        // "speech" appeared but the colon hasn't streamed yet.
        let buf = r#"{"reasoning":"x","speech"#;
        let (raw, complete) = extract_json_string(buf, "speech");
        assert_eq!(raw, None);
        assert!(!complete);
    }

    #[test]
    fn extract_json_string_no_opening_quote_yet() {
        let buf = r#"{"speech": "#; // colon present, no opening quote
        let (raw, complete) = extract_json_string(buf, "speech");
        assert_eq!(raw, None);
        assert!(!complete);
    }

    #[test]
    fn extract_json_string_keeps_escapes_raw() {
        // The raw substring preserves the escape sequences verbatim.
        let buf = r#"{"speech":"line1\nline2 \"q\""}"#;
        let (raw, complete) = extract_json_string(buf, "speech");
        assert_eq!(raw.as_deref(), Some(r#"line1\nline2 \"q\""#));
        assert!(complete);
        // The closing quote that ends the value is the one NOT preceded by `\`.
        let decoded = json_unescape(&raw.unwrap());
        assert_eq!(decoded, "line1\nline2 \"q\"");
    }

    #[test]
    fn extract_json_string_escaped_quote_does_not_terminate() {
        // An escaped quote mid-value must not end extraction prematurely.
        let buf = r#"{"speech":"he said \"hi\" loudly"}"#;
        let (raw, complete) = extract_json_string(buf, "speech");
        assert_eq!(raw.as_deref(), Some(r#"he said \"hi\" loudly"#));
        assert!(complete);
    }

    #[test]
    fn extract_json_string_trailing_backslash_partial() {
        // Buffer ends right after a backslash (esc=true) -> incomplete, backslash kept.
        let buf = r#"{"speech":"x\"#;
        let (raw, complete) = extract_json_string(buf, "speech");
        assert_eq!(raw.as_deref(), Some("x\\"));
        assert!(!complete);
    }

    #[test]
    fn json_unescape_limited_set() {
        assert_eq!(json_unescape(r#"a\"b"#), "a\"b");
        assert_eq!(json_unescape(r"a\nb"), "a\nb");
        assert_eq!(json_unescape(r"a\tb"), "a\tb");
        assert_eq!(json_unescape(r"a\/b"), "a/b");
        assert_eq!(json_unescape(r"a\\b"), "a\\b");
    }

    #[test]
    fn json_unescape_does_not_handle_unicode_escape() {
        // DEVIATION mirror: \uXXXX is passed through unchanged (Python limitation).
        assert_eq!(json_unescape(r"И"), r"И");
    }

    #[test]
    fn json_unescape_sequential_order_matches_python() {
        // `\\n`: first pass `\\"`->`"` no-op; `\\n`->`\n`? No: input is backslash,
        // backslash, n. Python replaces "\\n" (backslash+n) first occurrence:
        // the SECOND backslash + n -> newline, leaving a leading backslash.
        // Then "\\\\" (two backslashes) -> one backslash finds nothing new.
        // Result: "\" + "\n".
        let input = "\\\\n"; // backslash backslash n
        assert_eq!(json_unescape(input), "\\\n");
    }

    #[test]
    fn loads_plain_object() {
        let m = loads_map(r#"{"a":1,"b":"x"}"#);
        assert_eq!(m.get("a"), Some(&json!(1)));
        assert_eq!(m.get("b"), Some(&json!("x")));
    }

    #[test]
    fn loads_with_surrounding_garbage() {
        let m = loads_map("```json\n{\"k\": 5}\n```");
        assert_eq!(m.get("k"), Some(&json!(5)));
    }

    #[test]
    fn loads_empty_and_unparseable() {
        assert!(loads_map("").is_empty());
        assert!(loads_map("   ").is_empty());
        assert!(loads_map("not json at all").is_empty());
    }

    #[test]
    fn parse_tool_calls_string_arguments() {
        let raw = json!([
            {"id": "call_1", "type": "function",
             "function": {"name": "ask_npc", "arguments": "{\"npc_id\":\"iva\"}"}}
        ]);
        let calls = parse_tool_calls(Some(&raw));
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "ask_npc");
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].arguments.get("npc_id"), Some(&json!("iva")));
    }

    #[test]
    fn parse_tool_calls_object_arguments() {
        // MockClient hands arguments already as a dict.
        let raw = json!([
            {"id": "mock0", "type": "function",
             "function": {"name": "roll_dice", "arguments": {"notation": "1d20"}}}
        ]);
        let calls = parse_tool_calls(Some(&raw));
        assert_eq!(calls[0].arguments.get("notation"), Some(&json!("1d20")));
    }

    #[test]
    fn parse_tool_calls_missing_fields_defaults() {
        let raw = json!([{ "function": {} }]);
        let calls = parse_tool_calls(Some(&raw));
        assert_eq!(calls[0].name, "");
        assert!(calls[0].arguments.is_empty());
        assert_eq!(calls[0].id, "");
    }

    #[test]
    fn parse_tool_calls_none_and_empty() {
        assert!(parse_tool_calls(None).is_empty());
        assert!(parse_tool_calls(Some(&json!([]))).is_empty());
        assert!(parse_tool_calls(Some(&json!("notalist"))).is_empty());
    }
}
