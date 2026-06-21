//! SSE event accumulators for the Codex Responses stream.
//!
//! Faithful port of `_StreamAccumulator` and `_ToolCallAccumulator` from
//! `codex_client.py`. The accumulator consumes parsed SSE event objects, emits
//! `(channel, delta)` pairs through a sink, and produces the final
//! `(thinking, content, calls, usage)` once `response.completed` arrives.

use serde_json::{Map, Value};

use crate::responses::{extract_output_text, extract_tool_calls, loads};

/// `(channel, delta)` pair emitted while handling a stream event — the Rust
/// stand-in for the Python generator `yield`.
pub type Delta = (&'static str, String);

/// `channel` constants matching the orchestrator's expected values.
pub mod channel {
    /// Reasoning/thinking delta.
    pub const THINKING: &str = "thinking";
    /// Player-facing content delta.
    pub const CONTENT: &str = "content";
}

/// The final accumulated stream result (`_StreamResult` minus `elapsed_ms`,
/// which the client computes).
#[derive(Debug, Clone, PartialEq)]
pub struct StreamResult {
    /// `"".join(thinking_parts)`.
    pub thinking: String,
    /// `"".join(content_parts)`.
    pub content: String,
    /// Final tool calls (`[{id, name, arguments(dict)}]`).
    pub calls: Vec<Value>,
    /// `response.completed` usage object, if any.
    pub usage: Option<Value>,
}

/// `_StreamAccumulator`.
pub struct StreamAccumulator {
    thinking_parts: Vec<String>,
    content_parts: Vec<String>,
    tool_calls: ToolCallAccumulator,
    completed_tool_calls: Vec<Value>,
    usage: Option<Value>,
    /// `self.done` — set true once `response.completed` is seen.
    pub done: bool,
}

impl Default for StreamAccumulator {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamAccumulator {
    /// New empty accumulator.
    pub fn new() -> Self {
        StreamAccumulator {
            thinking_parts: Vec::new(),
            content_parts: Vec::new(),
            tool_calls: ToolCallAccumulator::new(),
            completed_tool_calls: Vec::new(),
            usage: None,
            done: false,
        }
    }

    /// `handle(event)` — process one SSE event, returning the `(channel, delta)`
    /// pairs it yields (in order). May set `self.done` or raise on a failure
    /// event.
    pub fn handle(&mut self, event: &Value) -> Result<Vec<Delta>, String> {
        let kind = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let mut out: Vec<Delta> = Vec::new();
        match kind {
            "response.output_text.delta" => {
                let delta = str_delta(event);
                if !delta.is_empty() {
                    self.content_parts.push(delta.clone());
                    out.push((channel::CONTENT, delta));
                }
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                let delta = str_delta(event);
                if !delta.is_empty() {
                    self.thinking_parts.push(delta.clone());
                    out.push((channel::THINKING, delta));
                }
            }
            "response.output_item.added" => {
                self.tool_calls.merge_item(event.get("item"), output_index(event));
            }
            "response.function_call_arguments.delta" => {
                let item_id = event
                    .get("item_id")
                    .and_then(|v| v.as_str())
                    .or_else(|| event.get("call_id").and_then(|v| v.as_str()));
                let delta = event.get("delta").and_then(|v| v.as_str()).unwrap_or("");
                self.tool_calls
                    .merge_arguments_delta(output_index(event), item_id, delta);
            }
            "response.function_call_arguments.done" => {
                self.tool_calls.merge_done(event);
            }
            "response.output_item.done" => {
                self.tool_calls.merge_item(event.get("item"), output_index(event));
            }
            "response.completed" => {
                let response = event
                    .get("response")
                    .filter(|v| v.is_object())
                    .cloned()
                    .unwrap_or(Value::Object(Map::new()));
                self.usage = response
                    .get("usage")
                    .filter(|v| v.is_object())
                    .cloned();
                if self.content_parts.is_empty() {
                    let text = extract_output_text(&response);
                    if !text.is_empty() {
                        self.content_parts.push(text);
                    }
                }
                self.completed_tool_calls = extract_tool_calls(&response);
                self.done = true;
            }
            "response.failed" | "response.incomplete" | "error" => {
                return Err(event_error_message(event));
            }
            _ => {}
        }
        Ok(out)
    }

    /// `finish(elapsed_ms)` — produce the final result (the client attaches the
    /// elapsed time separately).
    pub fn finish(self) -> StreamResult {
        let mut calls = self.tool_calls.finish();
        if calls.is_empty() {
            calls = self.completed_tool_calls;
        }
        StreamResult {
            thinking: self.thinking_parts.concat(),
            content: self.content_parts.concat(),
            calls,
            usage: self.usage,
        }
    }
}

/// `_ToolCallAccumulator`.
struct ToolCallAccumulator {
    calls: Vec<Map<String, Value>>,
}

impl ToolCallAccumulator {
    fn new() -> Self {
        ToolCallAccumulator { calls: Vec::new() }
    }

    /// `merge_item(item, output_index)`.
    fn merge_item(&mut self, item: Option<&Value>, output_index: i64) {
        let item = match item {
            Some(v @ Value::Object(_)) => v,
            _ => return,
        };
        if item.get("type").and_then(|v| v.as_str()) != Some("function_call") {
            return;
        }
        let id = item.get("id").and_then(|v| v.as_str()).map(|s| s.to_string());
        let idx = self.find_or_create(output_index, id.as_deref());
        let call = &mut self.calls[idx];
        if let Some(item_id) = item.get("id").and_then(|v| v.as_str()) {
            if !item_id.is_empty() || item.get("id").map(|v| !v.is_null()).unwrap_or(false) {
                call.insert("item_id".into(), Value::String(item_id.to_string()));
            }
        } else if let Some(v) = item.get("id") {
            // non-string truthy id -> str(item["id"])
            if is_truthy(v) {
                call.insert("item_id".into(), Value::String(py_str(v)));
            }
        }
        if let Some(v) = item.get("call_id") {
            if is_truthy(v) {
                call.insert("id".into(), Value::String(py_str(v)));
            }
        }
        if let Some(v) = item.get("name") {
            if is_truthy(v) {
                call.insert("name".into(), Value::String(py_str(v)));
            }
        }
        if let Some(Value::String(args)) = item.get("arguments") {
            call.insert("arguments_raw".into(), Value::String(args.clone()));
        }
    }

    /// `merge_arguments_delta(output_index, item_id, delta)`.
    fn merge_arguments_delta(&mut self, output_index: i64, item_id: Option<&str>, delta: &str) {
        let idx = self.find_or_create(output_index, item_id);
        let call = &mut self.calls[idx];
        let prev = call
            .get("arguments_raw")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        call.insert(
            "arguments_raw".into(),
            Value::String(format!("{prev}{delta}")),
        );
    }

    /// `merge_done(event)`.
    fn merge_done(&mut self, event: &Value) {
        let item_id = event
            .get("item_id")
            .and_then(|v| v.as_str())
            .or_else(|| event.get("call_id").and_then(|v| v.as_str()));
        let idx = self.find_or_create(output_index(event), item_id);
        {
            let call = &mut self.calls[idx];
            if let Some(v) = event.get("call_id") {
                if is_truthy(v) {
                    call.insert("id".into(), Value::String(py_str(v)));
                }
            }
            if let Some(v) = event.get("name") {
                if is_truthy(v) {
                    call.insert("name".into(), Value::String(py_str(v)));
                }
            }
            if let Some(Value::String(args)) = event.get("arguments") {
                call.insert("arguments_raw".into(), Value::String(args.clone()));
            }
        }
        if let Some(item @ Value::Object(_)) = event.get("item") {
            self.merge_item(Some(item), output_index(event));
        }
    }

    /// `finish()` — sort by output_index, drop nameless calls, parse arguments.
    fn finish(self) -> Vec<Value> {
        let mut sorted = self.calls;
        sorted.sort_by_key(|c| c.get("output_index").and_then(|v| v.as_i64()).unwrap_or(0));
        let mut out = Vec::new();
        for call in sorted {
            let name = call
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if name.is_empty() {
                continue;
            }
            let raw_args = {
                let s = call.get("arguments_raw").and_then(|v| v.as_str()).unwrap_or("");
                if s.is_empty() {
                    "{}".to_string()
                } else {
                    s.to_string()
                }
            };
            // id = str(call.get("id") or call.get("item_id") or f"responses_call_{output_index}")
            let id = {
                let id = nonempty(call.get("id"));
                let item_id = nonempty(call.get("item_id"));
                if let Some(id) = id {
                    id
                } else if let Some(item_id) = item_id {
                    item_id
                } else {
                    let oi = call.get("output_index").and_then(|v| v.as_i64()).unwrap_or(0);
                    format!("responses_call_{oi}")
                }
            };
            let mut entry = Map::new();
            entry.insert("id".into(), Value::String(id));
            entry.insert("name".into(), Value::String(name));
            entry.insert("arguments".into(), Value::Object(loads(&raw_args)));
            out.push(Value::Object(entry));
        }
        out
    }

    /// `_find_or_create(output_index, item_id)` — return the index of the
    /// matching call entry, creating one if needed.
    fn find_or_create(&mut self, output_index: i64, item_id: Option<&str>) -> usize {
        // item_id = str(item_id or "") or None
        let item_id = item_id.filter(|s| !s.is_empty());
        if let Some(item_id) = item_id {
            for (i, call) in self.calls.iter().enumerate() {
                let matches_item = call.get("item_id").and_then(|v| v.as_str()) == Some(item_id);
                let matches_id = call.get("id").and_then(|v| v.as_str()) == Some(item_id);
                if matches_item || matches_id {
                    return i;
                }
            }
        }
        for (i, call) in self.calls.iter().enumerate() {
            if call.get("output_index").and_then(|v| v.as_i64()) == Some(output_index) {
                return i;
            }
        }
        // call = {output_index, item_id, id: None, name: "", arguments_raw: ""}
        let mut call = Map::new();
        call.insert("output_index".into(), Value::from(output_index));
        call.insert(
            "item_id".into(),
            item_id.map(|s| Value::String(s.to_string())).unwrap_or(Value::Null),
        );
        call.insert("id".into(), Value::Null);
        call.insert("name".into(), Value::String(String::new()));
        call.insert("arguments_raw".into(), Value::String(String::new()));
        self.calls.push(call);
        self.calls.len() - 1
    }
}

/// `_output_index(event)` — `int(event.get("output_index") or event.get("index") or 0)`.
fn output_index(event: &Value) -> i64 {
    let pick = event
        .get("output_index")
        .filter(|v| is_truthy(v))
        .or_else(|| event.get("index").filter(|v| is_truthy(v)));
    match pick {
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)).unwrap_or(0),
        Some(Value::String(s)) => s.trim().parse::<i64>().unwrap_or(0),
        _ => 0,
    }
}

/// `_event_error_message(event)`.
pub fn event_error_message(event: &Value) -> String {
    let mut candidates: Vec<Option<&str>> = Vec::new();
    candidates.push(event.get("message").and_then(|v| v.as_str()));
    candidates.push(
        event
            .get("error")
            .and_then(|v| v.as_object())
            .and_then(|o| o.get("message"))
            .and_then(|v| v.as_str()),
    );
    candidates.push(
        event
            .get("response")
            .and_then(|v| v.as_object())
            .and_then(|o| o.get("error"))
            .and_then(|e| e.as_object())
            .and_then(|eo| eo.get("message"))
            .and_then(|v| v.as_str()),
    );
    for message in candidates.into_iter().flatten() {
        let trimmed = message.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    "Codex stream failed".to_string()
}

// --- helpers ----------------------------------------------------------------

fn str_delta(event: &Value) -> String {
    match event.get("delta") {
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    }
}

fn is_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(b) => if *b { "True" } else { "False" }.to_string(),
        Value::Null => "None".to_string(),
        Value::Number(n) => n.to_string(),
        _ => v.to_string(),
    }
}

fn nonempty(v: Option<&Value>) -> Option<String> {
    match v {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run(events: &[Value]) -> StreamResult {
        let mut acc = StreamAccumulator::new();
        for ev in events {
            let _ = acc.handle(ev).unwrap();
            if acc.done {
                break;
            }
        }
        acc.finish()
    }

    #[test]
    fn content_and_thinking_deltas() {
        let mut acc = StreamAccumulator::new();
        let yielded = acc.handle(&json!({"type": "response.output_text.delta", "delta": "Hi"})).unwrap();
        assert_eq!(yielded, vec![(channel::CONTENT, "Hi".to_string())]);
        let yielded2 = acc.handle(&json!({"type": "response.reasoning_text.delta", "delta": "think"})).unwrap();
        assert_eq!(yielded2, vec![(channel::THINKING, "think".to_string())]);
        let r = acc.finish();
        assert_eq!(r.content, "Hi");
        assert_eq!(r.thinking, "think");
    }

    #[test]
    fn streamed_tool_call_accumulates_arguments() {
        let events = vec![
            json!({"type": "response.output_item.added", "output_index": 0,
                   "item": {"type": "function_call", "id": "item1", "call_id": "c1", "name": "roll"}}),
            json!({"type": "response.function_call_arguments.delta", "output_index": 0,
                   "item_id": "item1", "delta": "{\"n\":"}),
            json!({"type": "response.function_call_arguments.delta", "output_index": 0,
                   "item_id": "item1", "delta": "2}"}),
            json!({"type": "response.output_item.done", "output_index": 0,
                   "item": {"type": "function_call", "id": "item1", "call_id": "c1", "name": "roll",
                            "arguments": "{\"n\":2}"}}),
            json!({"type": "response.completed", "response": {"usage": {"input_tokens": 10, "output_tokens": 3}}}),
        ];
        let r = run(&events);
        assert_eq!(r.calls.len(), 1);
        assert_eq!(r.calls[0].get("id").unwrap(), "c1");
        assert_eq!(r.calls[0].get("name").unwrap(), "roll");
        assert_eq!(r.calls[0].get("arguments").unwrap(), &json!({"n": 2}));
        assert_eq!(r.usage.unwrap().get("input_tokens").unwrap(), &json!(10));
    }

    #[test]
    fn completed_extracts_text_when_no_deltas() {
        let events = vec![json!({
            "type": "response.completed",
            "response": {
                "output": [{"content": [{"type": "output_text", "text": "final text"}]}],
                "usage": {"input_tokens": 1, "output_tokens": 1}
            }
        })];
        let r = run(&events);
        assert_eq!(r.content, "final text");
    }

    #[test]
    fn completed_tool_calls_used_when_acc_empty() {
        let events = vec![json!({
            "type": "response.completed",
            "response": {
                "output": [{"type": "function_call", "call_id": "x", "name": "f", "arguments": "{}"}]
            }
        })];
        let r = run(&events);
        assert_eq!(r.calls.len(), 1);
        assert_eq!(r.calls[0].get("id").unwrap(), "x");
    }

    #[test]
    fn failed_event_raises_with_message() {
        let mut acc = StreamAccumulator::new();
        let err = acc.handle(&json!({"type": "response.failed", "message": "boom"})).unwrap_err();
        assert_eq!(err, "boom");
    }

    #[test]
    fn error_event_nested_message() {
        let mut acc = StreamAccumulator::new();
        let err = acc
            .handle(&json!({"type": "error", "error": {"message": "nested fail"}}))
            .unwrap_err();
        assert_eq!(err, "nested fail");
    }

    #[test]
    fn event_error_default_message() {
        assert_eq!(event_error_message(&json!({"type": "response.failed"})), "Codex stream failed");
    }
}
