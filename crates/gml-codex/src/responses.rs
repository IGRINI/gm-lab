//! Codex Responses API request/response transforms and the SSE stream parser.
//!
//! Faithful port of the pure functions and stream accumulators from
//! `gm-lab/codex_client.py`. These are byte-fidelity-critical:
//! [`split_messages_for_responses`] flattens all system messages into ONE
//! `instructions` string with `"\n\n"` (Codex prompt-cache reuse hinges on this
//! being byte-identical — it is reproduced exactly, NOT "fixed"), and the
//! strict-schema transform must match the OpenAI strict-tool shape the Python
//! produced.

use serde_json::{Map, Value};

use gml_llm::loads_value;

// --- small text helpers (codex_client `_clean` / `_think`) ------------------

/// `_clean(text)` — `(text or "").strip()`.
pub fn clean(text: &str) -> String {
    python_strip(text).to_string()
}

/// `_think(text)` — strip `<think>` / `</think>` tags, then `strip()`.
pub fn think(text: &str) -> String {
    use once_cell::sync::Lazy;
    use regex::Regex;
    static THINK_RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"</?think>").expect("valid think regex"));
    python_strip(&THINK_RE.replace_all(text, "")).to_string()
}

/// `_loads(text)` returning an object map (mirrors codex_client `_loads`, which
/// reuses the same brace-fallback logic as llm_client `_loads`).
pub fn loads(text: &str) -> Map<String, Value> {
    match loads_value(text) {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

// --- assistant message + raw tool-call shaping ------------------------------

/// `_assistant_msg(content, raw_tool_calls)`.
pub fn assistant_msg(content: &str, raw_tool_calls: &[Value]) -> Value {
    let mut msg = Map::new();
    msg.insert("role".into(), Value::String("assistant".into()));
    msg.insert("content".into(), Value::String(content.to_string()));
    if !raw_tool_calls.is_empty() {
        msg.insert("tool_calls".into(), Value::Array(raw_tool_calls.to_vec()));
    }
    Value::Object(msg)
}

/// `_raw_tool_calls(calls)` — convert parsed calls (`{id,name,arguments}`) into
/// OpenAI raw `tool_calls` (`{id,type,function:{name,arguments(JSON string)}}`).
pub fn raw_tool_calls(calls: &[Value]) -> Vec<Value> {
    let mut raw = Vec::new();
    for call in calls {
        let name = call
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if name.is_empty() {
            continue;
        }
        // args = call.get("arguments") if isinstance(dict) else {}
        let args = match call.get("arguments") {
            Some(Value::Object(m)) => Value::Object(m.clone()),
            _ => Value::Object(Map::new()),
        };
        let id = call.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let mut function = Map::new();
        function.insert("name".into(), Value::String(name));
        // json.dumps(args, ensure_ascii=False) — compact, raw unicode.
        function.insert(
            "arguments".into(),
            Value::String(serde_json::to_string(&args).unwrap_or_else(|_| "{}".into())),
        );
        let mut entry = Map::new();
        entry.insert("id".into(), Value::String(id));
        entry.insert("type".into(), Value::String("function".into()));
        entry.insert("function".into(), Value::Object(function));
        raw.push(Value::Object(entry));
    }
    raw
}

// --- message splitting ------------------------------------------------------

/// `split_messages_for_responses(messages)` -> `(instructions, input_items)`.
///
/// Joins ALL system message contents with `"\n\n"` into a single `instructions`
/// string and converts the remaining messages (tool / assistant / user) into
/// ordered Responses input items. Reproduced EXACTLY — Codex cache reuse hinges
/// on byte-identical instructions (do NOT "fix" the shape).
pub fn split_messages_for_responses(messages: &Value) -> (String, Vec<Value>) {
    let mut instructions: Vec<String> = Vec::new();
    let mut input_items: Vec<Value> = Vec::new();

    let empty = Vec::new();
    let items: &Vec<Value> = messages.as_array().unwrap_or(&empty);
    for message in items {
        let role = attr_str(message, "role");
        let content = content_text(message.get("content"));
        if role == "system" {
            if !python_strip(&content).is_empty() {
                instructions.push(python_strip(&content).to_string());
            }
            continue;
        }
        if role == "tool" {
            let call_id = attr_str(message, "tool_call_id");
            if !call_id.is_empty() {
                let mut item = Map::new();
                item.insert("type".into(), Value::String("function_call_output".into()));
                item.insert("call_id".into(), Value::String(call_id));
                item.insert("output".into(), Value::String(content));
                input_items.push(Value::Object(item));
            }
            continue;
        }
        if role == "assistant" {
            if !python_strip(&content).is_empty() {
                input_items.push(message_item("assistant", "output_text", &content));
            }
            if let Some(Value::Array(tcs)) = message.get("tool_calls") {
                for tool_call in tcs {
                    if let Some(item) = function_call_item(tool_call) {
                        input_items.push(item);
                    }
                }
            }
            continue;
        }
        if role == "user" && !python_strip(&content).is_empty() {
            input_items.push(message_item("user", "input_text", &content));
        }
    }
    (instructions.join("\n\n"), input_items)
}

/// `_message_item(role, kind, text)`.
fn message_item(role: &str, kind: &str, text: &str) -> Value {
    let mut part = Map::new();
    part.insert("type".into(), Value::String(kind.to_string()));
    part.insert("text".into(), Value::String(text.to_string()));
    let mut item = Map::new();
    item.insert("type".into(), Value::String("message".into()));
    item.insert("role".into(), Value::String(role.to_string()));
    item.insert("content".into(), Value::Array(vec![Value::Object(part)]));
    Value::Object(item)
}

/// `_function_call_item(tool_call)` -> Responses `function_call` item or `None`.
fn function_call_item(tool_call: &Value) -> Option<Value> {
    let fn_obj = tool_call.get("function").and_then(|v| v.as_object());
    // name = str(tool_call.get("name") or fn.get("name") or "").strip()
    let name = {
        let top = tool_call.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let nested = fn_obj
            .and_then(|f| f.get("name"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let chosen = if !top.is_empty() { top } else { nested };
        chosen.trim().to_string()
    };
    if name.is_empty() {
        return None;
    }
    // args = tool_call.get("arguments", fn.get("arguments", {}))
    let args_val = tool_call
        .get("arguments")
        .cloned()
        .or_else(|| fn_obj.and_then(|f| f.get("arguments").cloned()))
        .unwrap_or(Value::Object(Map::new()));
    // if not isinstance(args, str): args = json.dumps(args if dict else {}, ensure_ascii=False)
    let args = match args_val {
        Value::String(s) => s,
        Value::Object(m) => serde_json::to_string(&Value::Object(m)).unwrap_or_else(|_| "{}".into()),
        _ => "{}".to_string(),
    };
    // call_id = str(tool_call.get("id") or tool_call.get("call_id") or f"call_{name}")
    let call_id = {
        let id = tool_call.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let cid = tool_call.get("call_id").and_then(|v| v.as_str()).unwrap_or("");
        if !id.is_empty() {
            id.to_string()
        } else if !cid.is_empty() {
            cid.to_string()
        } else {
            format!("call_{name}")
        }
    };
    let mut item = Map::new();
    item.insert("type".into(), Value::String("function_call".into()));
    item.insert("call_id".into(), Value::String(call_id));
    item.insert("name".into(), Value::String(name));
    item.insert("arguments".into(), Value::String(args));
    Some(Value::Object(item))
}

/// `_content_text(content)` — flatten str / list-of-parts / other to text.
pub fn content_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => {
            let mut parts: Vec<String> = Vec::new();
            for item in items {
                if let Some(obj) = item.as_object() {
                    if let Some(Value::String(t)) = obj.get("text") {
                        parts.push(t.clone());
                        continue;
                    }
                }
                if let Value::String(s) = item {
                    parts.push(s.clone());
                }
            }
            parts.join("\n")
        }
        None | Some(Value::Null) => String::new(),
        Some(other) => value_to_py_str(other),
    }
}

// --- strict schema transform ------------------------------------------------

/// `_nullable_schema(schema)` — make a property schema accept `null`.
pub fn nullable_schema(schema: &Value) -> Value {
    let mut out = schema.clone();
    let obj = match out.as_object_mut() {
        Some(o) => o,
        None => return out,
    };
    let typ = obj.get("type").cloned();
    match typ {
        Some(Value::Array(mut arr)) => {
            // if "null" not in typ: typ + ["null"]
            let has_null = arr.iter().any(|v| v.as_str() == Some("null"));
            if !has_null {
                arr.push(Value::String("null".into()));
            }
            obj.insert("type".into(), Value::Array(arr));
        }
        Some(Value::String(s)) if s != "null" => {
            obj.insert(
                "type".into(),
                Value::Array(vec![Value::String(s), Value::String("null".into())]),
            );
        }
        Some(Value::String(_)) => {
            // typ == "null" — leave as-is (Python: no branch matches).
        }
        _ => {
            // "type" not in out -> anyOf [deepcopy(out), {"type":"null"}]
            // Build anyOf from a copy of the *current* out (without anyOf yet).
            let copy = Value::Object(obj.clone());
            obj.insert(
                "anyOf".into(),
                Value::Array(vec![copy, serde_json::json!({"type": "null"})]),
            );
        }
    }

    // enum handling: if list and None not in enum: enum + [None]
    if let Some(Value::Array(enum_arr)) = obj.get("enum").cloned() {
        let has_null = enum_arr.iter().any(|v| v.is_null());
        if !has_null {
            let mut new_enum = enum_arr;
            new_enum.push(Value::Null);
            obj.insert("enum".into(), Value::Array(new_enum));
        }
    }
    out
}

/// `strict_schema_for_responses(schema)` — convert a permissive tool schema into
/// the OpenAI strict-tool JSON Schema (every object property listed in
/// `required`, optional ones made nullable, `additionalProperties: false`).
pub fn strict_schema_for_responses(schema: &Value) -> Value {
    if !schema.is_object() {
        return schema.clone();
    }
    let mut out = schema.clone();
    {
        let obj = out.as_object_mut().expect("object");

        // recurse into anyOf / oneOf / allOf
        for key in ["anyOf", "oneOf", "allOf"] {
            if let Some(Value::Array(arr)) = obj.get(key).cloned() {
                let mapped: Vec<Value> =
                    arr.iter().map(strict_schema_for_responses).collect();
                obj.insert(key.into(), Value::Array(mapped));
            }
        }

        // items (object)
        if let Some(Value::Object(_)) = obj.get("items") {
            let items = obj.get("items").cloned().unwrap();
            obj.insert("items".into(), strict_schema_for_responses(&items));
        }
    }

    let props = out.get("properties").cloned();
    let typ = out.get("type").cloned();
    let is_object = matches!(&typ, Some(Value::String(s)) if s == "object")
        || matches!(&typ, Some(Value::Array(a)) if a.iter().any(|v| v.as_str() == Some("object")))
        || matches!(props, Some(Value::Object(_)));

    if is_object {
        let obj = out.as_object_mut().expect("object");
        // out["type"] = typ or "object"
        let new_type = match typ {
            Some(t) if is_truthy(&t) => t,
            _ => Value::String("object".into()),
        };
        obj.insert("type".into(), new_type);
        obj.insert("additionalProperties".into(), Value::Bool(false));

        if let Some(Value::Object(props_map)) = props {
            // original_required = set(out.get("required") or [])
            let original_required: Vec<String> = out_required(obj);
            let mut new_props = Map::new();
            for (name, prop) in &props_map {
                let mut child = strict_schema_for_responses(prop);
                if !original_required.iter().any(|r| r == name) {
                    child = nullable_schema(&child);
                }
                new_props.insert(name.clone(), child);
            }
            // required = list(props.keys()) — preserves insertion order.
            let required: Vec<Value> = props_map
                .keys()
                .map(|k| Value::String(k.clone()))
                .collect();
            obj.insert("properties".into(), Value::Object(new_props));
            obj.insert("required".into(), Value::Array(required));
        } else {
            obj.insert("properties".into(), Value::Object(Map::new()));
            obj.insert("required".into(), Value::Array(Vec::new()));
        }
    }
    out
}

fn out_required(obj: &Map<String, Value>) -> Vec<String> {
    match obj.get("required") {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
        _ => Vec::new(),
    }
}

/// `convert_tool_for_responses(tool)`.
pub fn convert_tool_for_responses(tool: &Value) -> Value {
    if tool.get("type").and_then(|v| v.as_str()) != Some("function") {
        return tool.clone();
    }
    // fn = tool.get("function") if isinstance(dict) else tool
    let fn_obj: &Value = match tool.get("function") {
        Some(v @ Value::Object(_)) => v,
        _ => tool,
    };
    // strict = bool(fn.get("strict", True))
    let strict = match fn_obj.get("strict") {
        Some(v) => is_truthy(v),
        None => true,
    };
    let source_parameters = match fn_obj.get("parameters") {
        Some(v) if is_truthy(v) => v.clone(),
        _ => Value::Object(Map::new()),
    };
    let parameters = if strict {
        strict_schema_for_responses(&source_parameters)
    } else {
        source_parameters
    };
    let mut out = Map::new();
    out.insert("type".into(), Value::String("function".into()));
    out.insert(
        "name".into(),
        Value::String(fn_obj.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string()),
    );
    out.insert(
        "description".into(),
        Value::String(fn_obj.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string()),
    );
    out.insert("parameters".into(), parameters);
    out.insert("strict".into(), Value::Bool(strict));
    Value::Object(out)
}

// --- response (non-stream) extraction ---------------------------------------

/// `extract_output_text(response)`.
pub fn extract_output_text(response: &Value) -> String {
    let mut text: Vec<String> = Vec::new();
    if let Some(Value::Array(items)) = response.get("output") {
        for item in items {
            if let Some(Value::Array(parts)) = item.get("content") {
                for part in parts {
                    if part.get("type").and_then(|v| v.as_str()) == Some("output_text") {
                        if let Some(Value::String(t)) = part.get("text") {
                            text.push(t.clone());
                        }
                    }
                }
            }
        }
    }
    if text.is_empty() {
        if let Some(Value::String(t)) = response.get("output_text") {
            text.push(t.clone());
        }
    }
    text.concat()
}

/// `extract_tool_calls(response)` -> `[{id, name, arguments(dict)}]`.
pub fn extract_tool_calls(response: &Value) -> Vec<Value> {
    let mut calls = Vec::new();
    if let Some(Value::Array(items)) = response.get("output") {
        for (index, item) in items.iter().enumerate() {
            if item.get("type").and_then(|v| v.as_str()) != Some("function_call") {
                continue;
            }
            let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
            if name.is_empty() {
                continue;
            }
            let raw_args = item.get("arguments").and_then(|v| v.as_str()).unwrap_or("{}");
            let args = if raw_args.is_empty() {
                loads("{}")
            } else {
                loads(raw_args)
            };
            let call_id = {
                let cid = item.get("call_id").and_then(|v| v.as_str()).unwrap_or("");
                let id = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
                if !cid.is_empty() {
                    cid.to_string()
                } else if !id.is_empty() {
                    id.to_string()
                } else {
                    format!("responses_call_{index}")
                }
            };
            let mut call = Map::new();
            call.insert("id".into(), Value::String(call_id));
            call.insert("name".into(), Value::String(name));
            call.insert("arguments".into(), Value::Object(args));
            calls.push(Value::Object(call));
        }
    }
    calls
}

// --- helpers ----------------------------------------------------------------

fn attr_str(obj: &Value, name: &str) -> String {
    obj.get(name).and_then(|v| v.as_str()).unwrap_or("").to_string()
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

fn value_to_py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(b) => if *b { "True" } else { "False" }.to_string(),
        Value::Null => "None".to_string(),
        Value::Number(n) => n.to_string(),
        _ => v.to_string(),
    }
}

fn python_strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn split_joins_all_systems_with_double_newline() {
        let messages = json!([
            {"role": "system", "content": "  GM_SYSTEM  "},
            {"role": "system", "content": "world setup"},
            {"role": "user", "content": "hello"},
        ]);
        let (instructions, items) = split_messages_for_responses(&messages);
        // strip() applied to each, joined with "\n\n"
        assert_eq!(instructions, "GM_SYSTEM\n\nworld setup");
        assert_eq!(items.len(), 1);
        assert_eq!(
            serde_json::to_string(&items[0]).unwrap(),
            r#"{"type":"message","role":"user","content":[{"type":"input_text","text":"hello"}]}"#
        );
    }

    #[test]
    fn split_empty_systems_omitted() {
        let messages = json!([
            {"role": "system", "content": "   "},
            {"role": "system", "content": "real"},
        ]);
        let (instructions, _items) = split_messages_for_responses(&messages);
        assert_eq!(instructions, "real");
    }

    #[test]
    fn split_tool_and_assistant_items() {
        let messages = json!([
            {"role": "assistant", "content": "ok",
             "tool_calls": [{"id": "c1", "type": "function",
                             "function": {"name": "roll", "arguments": "{\"n\":1}"}}]},
            {"role": "tool", "tool_call_id": "c1", "content": "result"},
        ]);
        let (instructions, items) = split_messages_for_responses(&messages);
        assert_eq!(instructions, "");
        // assistant text item, then function_call item, then tool output item
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].get("type").unwrap(), "message");
        assert_eq!(items[1].get("type").unwrap(), "function_call");
        assert_eq!(items[1].get("call_id").unwrap(), "c1");
        assert_eq!(items[1].get("name").unwrap(), "roll");
        assert_eq!(items[1].get("arguments").unwrap(), "{\"n\":1}");
        assert_eq!(items[2].get("type").unwrap(), "function_call_output");
        assert_eq!(items[2].get("call_id").unwrap(), "c1");
        assert_eq!(items[2].get("output").unwrap(), "result");
    }

    #[test]
    fn function_call_item_default_call_id() {
        let tc = json!({"name": "roll_dice", "arguments": {"notation": "1d20"}});
        let item = function_call_item(&tc).unwrap();
        assert_eq!(item.get("call_id").unwrap(), "call_roll_dice");
        // dict args -> compact json string, raw unicode
        assert_eq!(item.get("arguments").unwrap(), "{\"notation\":\"1d20\"}");
    }

    #[test]
    fn strict_schema_required_and_nullable() {
        let schema = json!({
            "type": "object",
            "properties": {
                "needed": {"type": "string"},
                "optional": {"type": "integer"}
            },
            "required": ["needed"]
        });
        let out = strict_schema_for_responses(&schema);
        // every property listed in required, in property insertion order
        assert_eq!(out.get("required").unwrap(), &json!(["needed", "optional"]));
        // additionalProperties false
        assert_eq!(out.get("additionalProperties").unwrap(), &json!(false));
        // optional made nullable: type -> ["integer","null"]
        let opt_type = out.pointer("/properties/optional/type").unwrap();
        assert_eq!(opt_type, &json!(["integer", "null"]));
        // required field NOT made nullable
        let needed_type = out.pointer("/properties/needed/type").unwrap();
        assert_eq!(needed_type, &json!("string"));
    }

    #[test]
    fn strict_schema_no_properties_object() {
        let schema = json!({"type": "object"});
        let out = strict_schema_for_responses(&schema);
        assert_eq!(out.get("properties").unwrap(), &json!({}));
        assert_eq!(out.get("required").unwrap(), &json!([]));
        assert_eq!(out.get("additionalProperties").unwrap(), &json!(false));
    }

    #[test]
    fn nullable_schema_enum_appends_null() {
        let s = json!({"type": "string", "enum": ["a", "b"]});
        let out = nullable_schema(&s);
        assert_eq!(out.get("type").unwrap(), &json!(["string", "null"]));
        assert_eq!(out.get("enum").unwrap(), &json!(["a", "b", null]));
    }

    #[test]
    fn nullable_schema_no_type_uses_anyof() {
        let s = json!({"description": "x"});
        let out = nullable_schema(&s);
        let anyof = out.get("anyOf").unwrap().as_array().unwrap();
        assert_eq!(anyof.len(), 2);
        assert_eq!(anyof[1], json!({"type": "null"}));
    }

    #[test]
    fn convert_tool_strict_default_true() {
        let tool = json!({
            "type": "function",
            "function": {
                "name": "roll_dice",
                "description": "roll",
                "parameters": {
                    "type": "object",
                    "properties": {"notation": {"type": "string"}},
                    "required": ["notation"]
                }
            }
        });
        let out = convert_tool_for_responses(&tool);
        assert_eq!(out.get("type").unwrap(), "function");
        assert_eq!(out.get("name").unwrap(), "roll_dice");
        assert_eq!(out.get("description").unwrap(), "roll");
        assert_eq!(out.get("strict").unwrap(), &json!(true));
        assert_eq!(out.get("parameters").unwrap().get("additionalProperties").unwrap(), &json!(false));
    }

    #[test]
    fn convert_tool_strict_false_passthrough_params() {
        let tool = json!({
            "type": "function",
            "function": {
                "name": "f", "strict": false,
                "parameters": {"type": "object", "properties": {"x": {"type": "string"}}}
            }
        });
        let out = convert_tool_for_responses(&tool);
        assert_eq!(out.get("strict").unwrap(), &json!(false));
        // non-strict params unchanged (no required injected, no additionalProperties)
        assert!(out.pointer("/parameters/additionalProperties").is_none());
    }

    #[test]
    fn convert_tool_non_function_passthrough() {
        let tool = json!({"type": "web_search"});
        assert_eq!(convert_tool_for_responses(&tool), tool);
    }

    #[test]
    fn extract_output_text_from_output_items() {
        let resp = json!({
            "output": [
                {"content": [{"type": "output_text", "text": "Hello "}]},
                {"content": [{"type": "output_text", "text": "world"}]}
            ]
        });
        assert_eq!(extract_output_text(&resp), "Hello world");
    }

    #[test]
    fn extract_output_text_fallback() {
        let resp = json!({"output": [], "output_text": "fallback"});
        assert_eq!(extract_output_text(&resp), "fallback");
    }

    #[test]
    fn extract_tool_calls_basic() {
        let resp = json!({
            "output": [
                {"type": "function_call", "call_id": "c1", "name": "roll",
                 "arguments": "{\"n\": 2}"}
            ]
        });
        let calls = extract_tool_calls(&resp);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].get("id").unwrap(), "c1");
        assert_eq!(calls[0].get("name").unwrap(), "roll");
        assert_eq!(calls[0].get("arguments").unwrap(), &json!({"n": 2}));
    }

    #[test]
    fn extract_tool_calls_default_id() {
        let resp = json!({
            "output": [{"type": "function_call", "name": "f", "arguments": "{}"}]
        });
        let calls = extract_tool_calls(&resp);
        assert_eq!(calls[0].get("id").unwrap(), "responses_call_0");
    }

    #[test]
    fn raw_tool_calls_shape() {
        let calls = vec![json!({"id": "c1", "name": "roll", "arguments": {"n": 1}})];
        let raw = raw_tool_calls(&calls);
        assert_eq!(raw.len(), 1);
        assert_eq!(raw[0].get("type").unwrap(), "function");
        assert_eq!(raw[0].pointer("/function/name").unwrap(), "roll");
        assert_eq!(raw[0].pointer("/function/arguments").unwrap(), "{\"n\":1}");
    }
}
