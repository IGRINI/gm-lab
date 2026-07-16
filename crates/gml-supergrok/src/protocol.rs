use serde_json::{json, Map, Value};

pub(crate) const XAI_REASONING_STATE_FIELD: &str = "_gml_xai_reasoning";

pub(crate) fn build_request(
    model: &str,
    messages: &Value,
    tools: Option<&Value>,
    schema: Option<&Value>,
    prompt_cache_key: &str,
    reasoning_scope: &str,
) -> Value {
    let (instructions, input) = split_messages(messages, Some(reasoning_scope));
    let mut request = Map::new();
    request.insert("model".into(), Value::String(model.to_string()));
    if !instructions.is_empty() {
        request.insert("instructions".into(), Value::String(instructions));
    }
    request.insert("input".into(), Value::Array(input));
    request.insert("stream".into(), Value::Bool(true));
    request.insert("store".into(), Value::Bool(false));
    request.insert("include".into(), json!(["reasoning.encrypted_content"]));
    if !prompt_cache_key.is_empty() {
        request.insert(
            "prompt_cache_key".into(),
            Value::String(prompt_cache_key.to_string()),
        );
    }

    let converted = tools
        .and_then(Value::as_array)
        .map(|items| items.iter().map(convert_tool).collect::<Vec<_>>())
        .unwrap_or_default();
    if !converted.is_empty() {
        request.insert("tools".into(), Value::Array(converted));
        request.insert("tool_choice".into(), Value::String("auto".to_string()));
        request.insert("parallel_tool_calls".into(), Value::Bool(true));
    }
    if let Some(schema) = schema {
        request.insert(
            "text".into(),
            json!({
                "format": {
                    "type": "json_schema",
                    "name": "gm_lab_response",
                    "strict": true,
                    "schema": strict_schema(schema),
                }
            }),
        );
    }
    Value::Object(request)
}

fn split_messages(
    messages: &Value,
    expected_reasoning_scope: Option<&str>,
) -> (String, Vec<Value>) {
    let mut instructions = Vec::new();
    let mut input = Vec::new();
    let Some(messages) = messages.as_array() else {
        return (String::new(), input);
    };
    let reasoning_reset_boundary = messages.iter().rposition(|message| {
        reasoning_state(message, expected_reasoning_scope)
            .and_then(|state| state.get("reset_before"))
            .and_then(Value::as_bool)
            == Some(true)
    });
    for (message_index, message) in messages.iter().enumerate() {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let content = content_text(message.get("content"));
        match role {
            "system" => {
                let content = content.trim();
                if !content.is_empty() {
                    instructions.push(content.to_string());
                }
            }
            "user" if !content.trim().is_empty() => {
                input.push(message_item("user", "input_text", &content));
            }
            "assistant" => {
                if reasoning_reset_boundary.is_none_or(|boundary| message_index >= boundary) {
                    input.extend(reasoning_items_from_message(
                        message,
                        expected_reasoning_scope,
                    ));
                }
                if !content.trim().is_empty() {
                    input.push(message_item("assistant", "output_text", &content));
                }
                if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
                    input.extend(
                        calls
                            .iter()
                            .enumerate()
                            .filter_map(|(index, call)| function_call_item(call, index)),
                    );
                }
            }
            "tool" => {
                let call_id = message
                    .get("tool_call_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim();
                if !call_id.is_empty() {
                    input.push(json!({
                        "type": "function_call_output",
                        "call_id": call_id,
                        "output": content,
                    }));
                }
            }
            _ => {}
        }
    }
    (instructions.join("\n\n"), input)
}

pub(crate) fn attach_reasoning_items(
    message: &mut Value,
    scope: &str,
    items: &[Value],
    reset_before: bool,
) {
    let Some(message) = message.as_object_mut() else {
        return;
    };
    let items = items
        .iter()
        .filter_map(reasoning_replay_item)
        .collect::<Vec<_>>();
    if reset_before || !items.is_empty() {
        message.insert(
            XAI_REASONING_STATE_FIELD.to_string(),
            json!({
                "v": 1,
                "scope": scope,
                "reset_before": reset_before,
                "items": items,
            }),
        );
    }
}

fn reasoning_items_from_message(message: &Value, expected_scope: Option<&str>) -> Vec<Value> {
    let Some(state) = reasoning_state(message, expected_scope) else {
        return Vec::new();
    };
    state
        .get("items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(reasoning_replay_item)
        .collect()
}

fn reasoning_state<'a>(
    message: &'a Value,
    expected_scope: Option<&str>,
) -> Option<&'a Map<String, Value>> {
    let state = message
        .get(XAI_REASONING_STATE_FIELD)
        .and_then(Value::as_object)?;
    if state.get("v").and_then(Value::as_u64) != Some(1) {
        return None;
    }
    if expected_scope
        .is_some_and(|expected| state.get("scope").and_then(Value::as_str) != Some(expected))
    {
        return None;
    }
    Some(state)
}

pub(crate) fn extract_encrypted_reasoning_items(response: &Value) -> Vec<Value> {
    response
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| is_encrypted_reasoning_item(item))
        .cloned()
        .collect()
}

fn is_encrypted_reasoning_item(item: &Value) -> bool {
    item.get("type").and_then(Value::as_str) == Some("reasoning")
        && item
            .get("encrypted_content")
            .and_then(Value::as_str)
            .is_some_and(|content| !content.trim().is_empty())
}

fn reasoning_replay_item(item: &Value) -> Option<Value> {
    if !is_encrypted_reasoning_item(item) {
        return None;
    }
    if item
        .get("id")
        .and_then(Value::as_str)
        .is_some_and(|id| id.starts_with("rs_tmp_"))
    {
        return None;
    }
    let encrypted_content = item.get("encrypted_content")?.as_str()?;
    let summary = item
        .get("summary")
        .filter(|summary| summary.is_array())
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));
    Some(json!({
        "type": "reasoning",
        "encrypted_content": encrypted_content,
        "summary": summary,
    }))
}

fn message_item(role: &str, content_type: &str, text: &str) -> Value {
    json!({
        "type": "message",
        "role": role,
        "content": [{"type": content_type, "text": text}],
    })
}

fn function_call_item(call: &Value, index: usize) -> Option<Value> {
    let function = call.get("function").and_then(Value::as_object);
    let name = call
        .get("name")
        .and_then(Value::as_str)
        .or_else(|| {
            function
                .and_then(|item| item.get("name"))
                .and_then(Value::as_str)
        })
        .unwrap_or_default()
        .trim();
    if name.is_empty() {
        return None;
    }
    let call_id = call
        .get("id")
        .and_then(Value::as_str)
        .or_else(|| call.get("call_id").and_then(Value::as_str))
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("call_{name}_{index}"));
    let arguments = call
        .get("arguments")
        .or_else(|| function.and_then(|item| item.get("arguments")))
        .map(arguments_string)
        .unwrap_or_else(|| "{}".to_string());
    Some(json!({
        "type": "function_call",
        "call_id": call_id,
        "name": name,
        "arguments": arguments,
    }))
}

fn arguments_string(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Object(_) => serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string()),
        _ => "{}".to_string(),
    }
}

fn content_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| {
                part.as_str()
                    .map(str::to_string)
                    .or_else(|| part.get("text").and_then(Value::as_str).map(str::to_string))
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

pub(crate) fn convert_tool(tool: &Value) -> Value {
    if tool.get("type").and_then(Value::as_str) != Some("function") {
        return tool.clone();
    }
    let function = tool
        .get("function")
        .filter(|value| value.is_object())
        .unwrap_or(tool);
    let strict = function
        .get("strict")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let parameters = function
        .get("parameters")
        .cloned()
        .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
    json!({
        "type": "function",
        "name": function.get("name").and_then(Value::as_str).unwrap_or_default(),
        "description": function.get("description").and_then(Value::as_str).unwrap_or_default(),
        "parameters": if strict { strict_schema(&parameters) } else { parameters },
        "strict": strict,
    })
}

fn strict_schema(schema: &Value) -> Value {
    let Some(source) = schema.as_object() else {
        return schema.clone();
    };
    let mut output = source.clone();

    for keyword in ["anyOf", "oneOf", "allOf"] {
        if let Some(items) = source.get(keyword).and_then(Value::as_array) {
            output.insert(
                keyword.to_string(),
                Value::Array(items.iter().map(strict_schema).collect()),
            );
        }
    }
    if let Some(items) = source.get("items") {
        output.insert("items".to_string(), strict_schema(items));
    }

    let properties = source.get("properties").and_then(Value::as_object);
    let is_object =
        source.get("type").and_then(Value::as_str) == Some("object") || properties.is_some();
    if !is_object {
        return Value::Object(output);
    }
    output.insert("type".to_string(), Value::String("object".to_string()));
    output.insert("additionalProperties".to_string(), Value::Bool(false));
    let original_required = source
        .get("required")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).collect::<Vec<_>>())
        .unwrap_or_default();
    let mut converted = Map::new();
    let mut required = Vec::new();
    for (name, property) in properties.into_iter().flatten() {
        let child = strict_schema(property);
        converted.insert(
            name.clone(),
            if original_required.contains(&name.as_str()) {
                child
            } else {
                nullable(child)
            },
        );
        required.push(Value::String(name.clone()));
    }
    output.insert("properties".to_string(), Value::Object(converted));
    output.insert("required".to_string(), Value::Array(required));
    Value::Object(output)
}

fn nullable(mut schema: Value) -> Value {
    let Some(object) = schema.as_object_mut() else {
        return json!({"anyOf": [schema, {"type": "null"}]});
    };
    match object.get("type").cloned() {
        Some(Value::String(kind)) if kind != "null" => {
            object.insert("type".to_string(), json!([kind, "null"]));
        }
        Some(Value::Array(mut kinds)) => {
            if !kinds.iter().any(|kind| kind == "null") {
                kinds.push(Value::String("null".to_string()));
            }
            object.insert("type".to_string(), Value::Array(kinds));
        }
        Some(_) => {}
        None => return json!({"anyOf": [Value::Object(object.clone()), {"type": "null"}]}),
    }
    if let Some(Value::Array(mut values)) = object.get("enum").cloned() {
        if !values.iter().any(Value::is_null) {
            values.push(Value::Null);
            object.insert("enum".to_string(), Value::Array(values));
        }
    }
    schema
}

pub(crate) fn extract_output_text(response: &Value) -> String {
    let mut output = Vec::new();
    if let Some(items) = response.get("output").and_then(Value::as_array) {
        for item in items {
            if let Some(parts) = item.get("content").and_then(Value::as_array) {
                for part in parts {
                    if part.get("type").and_then(Value::as_str) == Some("output_text") {
                        if let Some(text) = part.get("text").and_then(Value::as_str) {
                            output.push(text);
                        }
                    }
                }
            }
        }
    }
    if output.is_empty() {
        response
            .get("output_text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    } else {
        output.concat()
    }
}

pub(crate) fn extract_tool_calls(response: &Value) -> Vec<Value> {
    response
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(index, item)| {
            if item.get("type").and_then(Value::as_str) != Some("function_call") {
                return None;
            }
            let name = item.get("name").and_then(Value::as_str)?.trim();
            if name.is_empty() {
                return None;
            }
            let id = item
                .get("call_id")
                .and_then(Value::as_str)
                .or_else(|| item.get("id").and_then(Value::as_str))
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| format!("responses_call_{index}"));
            let arguments = item
                .get("arguments")
                .and_then(Value::as_str)
                .and_then(|raw| gml_llm::loads_value(raw).as_object().cloned())
                .unwrap_or_default();
            Some(json!({"id": id, "name": name, "arguments": arguments}))
        })
        .collect()
}

pub(crate) fn raw_tool_calls(calls: &[Value]) -> Vec<Value> {
    calls
        .iter()
        .filter_map(|call| {
            let name = call.get("name").and_then(Value::as_str)?.trim();
            if name.is_empty() {
                return None;
            }
            let id = call.get("id").and_then(Value::as_str).unwrap_or_default();
            let arguments = call
                .get("arguments")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            Some(json!({
                "id": id,
                "type": "function",
                "function": {
                    "name": name,
                    "arguments": serde_json::to_string(&arguments).unwrap_or_else(|_| "{}".to_string()),
                }
            }))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_keeps_stable_prefix_and_cache_scope() {
        let request = build_request(
            "grok-test",
            &json!([
                {"role":"system","content":" first "},
                {"role":"system","content":"second"},
                {"role":"user","content":"hello"}
            ]),
            None,
            None,
            "conversation-1",
            "conversation-1",
        );
        assert_eq!(request["instructions"], "first\n\nsecond");
        assert_eq!(request["prompt_cache_key"], "conversation-1");
        assert_eq!(request["store"], false);
        assert_eq!(request["stream"], true);
        assert_eq!(request["include"], json!(["reasoning.encrypted_content"]));
    }

    #[test]
    fn encrypted_reasoning_round_trips_only_inside_its_cache_scope() {
        let mut assistant = json!({"role":"assistant","content":"answer"});
        attach_reasoning_items(
            &mut assistant,
            "thread-a",
            &[json!({
                "type": "reasoning",
                "id": "reasoning-item-id",
                "status": "completed",
                "encrypted_content": " encrypted-state ",
                "summary": [{"type":"summary_text","text":"short"}],
            })],
            false,
        );

        let same_scope = build_request(
            "grok-test",
            &Value::Array(vec![assistant.clone()]),
            None,
            None,
            "fixed-cache-key",
            "thread-a",
        );
        assert_eq!(
            same_scope["input"][0],
            json!({
                "type": "reasoning",
                "encrypted_content": " encrypted-state ",
                "summary": [{"type":"summary_text","text":"short"}],
            })
        );
        assert!(same_scope["input"][0].get("id").is_none());
        assert!(same_scope["input"][0].get("status").is_none());

        let different_scope = build_request(
            "grok-test",
            &Value::Array(vec![assistant]),
            None,
            None,
            "fixed-cache-key",
            "thread-b",
        );
        assert_eq!(different_scope["input"].as_array().unwrap().len(), 1);
        assert_eq!(different_scope["input"][0]["type"], "message");
    }

    #[test]
    fn reasoning_without_summary_uses_an_empty_summary_array() {
        let mut assistant = json!({"role":"assistant","content":"answer"});
        attach_reasoning_items(
            &mut assistant,
            "thread",
            &[json!({"type":"reasoning","encrypted_content":"secret"})],
            false,
        );
        let request = build_request(
            "grok-test",
            &json!([assistant]),
            None,
            None,
            "thread",
            "thread",
        );
        assert_eq!(request["input"][0]["summary"], json!([]));
    }

    #[test]
    fn empty_reasoning_blob_does_not_create_connector_state() {
        let mut assistant = json!({"role":"assistant","content":"answer"});
        attach_reasoning_items(
            &mut assistant,
            "thread",
            &[json!({"type":"reasoning","encrypted_content":"   "})],
            false,
        );
        assert!(assistant.get(XAI_REASONING_STATE_FIELD).is_none());
    }

    #[test]
    fn transient_reasoning_item_does_not_create_replay_state() {
        let mut assistant = json!({"role":"assistant","content":"answer"});
        attach_reasoning_items(
            &mut assistant,
            "thread",
            &[json!({
                "type":"reasoning",
                "id":"rs_tmp_123",
                "encrypted_content":"temporary-secret"
            })],
            false,
        );
        assert!(assistant.get(XAI_REASONING_STATE_FIELD).is_none());
    }

    #[test]
    fn reasoning_reset_boundary_survives_serialization() {
        let mut old_assistant = json!({"role":"assistant","content":"old answer"});
        attach_reasoning_items(
            &mut old_assistant,
            "thread",
            &[json!({"type":"reasoning","encrypted_content":"old-secret"})],
            false,
        );
        let mut reset_assistant = json!({"role":"assistant","content":"fresh answer"});
        attach_reasoning_items(
            &mut reset_assistant,
            "thread",
            &[json!({"type":"reasoning","encrypted_content":"fresh-secret"})],
            true,
        );
        let serialized = serde_json::to_string(&json!([old_assistant, reset_assistant])).unwrap();
        let restored: Value = serde_json::from_str(&serialized).unwrap();
        let request = build_request(
            "grok-test",
            &restored,
            None,
            None,
            "fixed-cache-key",
            "thread",
        );
        let replayed = request["input"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["type"] == "reasoning")
            .collect::<Vec<_>>();
        assert_eq!(replayed.len(), 1);
        assert_eq!(replayed[0]["encrypted_content"], "fresh-secret");
    }

    #[test]
    fn assistant_call_and_result_round_trip_to_responses_items() {
        let (_, items) = split_messages(
            &json!([
                {"role":"assistant","content":"", "tool_calls":[{
                    "id":"c1","type":"function","function":{"name":"roll","arguments":"{\"n\":2}"}
                }]},
                {"role":"tool","tool_call_id":"c1","content":"7"}
            ]),
            None,
        );
        assert_eq!(items[0]["type"], "function_call");
        assert_eq!(items[0]["name"], "roll");
        assert_eq!(items[1]["type"], "function_call_output");
    }

    #[test]
    fn generated_call_ids_are_unique_inside_one_assistant_turn() {
        let (_, items) = split_messages(
            &json!([{"role":"assistant","tool_calls":[
                {"function":{"name":"roll","arguments":"{}"}},
                {"function":{"name":"roll","arguments":"{}"}}
            ]}]),
            None,
        );
        assert_ne!(items[0]["call_id"], items[1]["call_id"]);
    }

    #[test]
    fn strict_tool_schema_requires_nullable_optional_properties() {
        let tool = convert_tool(&json!({"type":"function","function":{
            "name":"lookup","parameters":{"type":"object","properties":{
                "required":{"type":"string"},"optional":{"type":"integer"}
            },"required":["required"]}
        }}));
        assert_eq!(tool["parameters"]["additionalProperties"], false);
        assert_eq!(
            tool["parameters"]["required"],
            json!(["required", "optional"])
        );
        assert_eq!(
            tool["parameters"]["properties"]["optional"]["type"],
            json!(["integer", "null"])
        );
    }

    #[test]
    fn completed_response_extracts_text_and_calls() {
        let response = json!({"output":[
            {"type":"message","content":[{"type":"output_text","text":"done"}]},
            {"type":"function_call","call_id":"c1","name":"roll","arguments":"{\"n\":1}"}
        ]});
        assert_eq!(extract_output_text(&response), "done");
        assert_eq!(extract_tool_calls(&response)[0]["arguments"]["n"], 1);
    }
}
