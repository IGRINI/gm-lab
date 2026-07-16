use serde_json::{json, Map, Value};

use crate::protocol::{extract_encrypted_reasoning_items, extract_output_text, extract_tool_calls};

#[derive(Debug)]
pub(crate) enum StreamDelta {
    Thinking(String),
    Content(String),
}

pub(crate) struct StreamResult {
    pub thinking: String,
    pub content: String,
    pub calls: Vec<Value>,
    pub reasoning_items: Vec<Value>,
    pub reset_reasoning_before: bool,
    pub usage: Option<Value>,
}

#[derive(Default)]
pub(crate) struct StreamAccumulator {
    thinking: String,
    content: String,
    calls: Vec<CallAccumulator>,
    completed_calls: Vec<Value>,
    reasoning_items: Vec<Value>,
    usage: Option<Value>,
    pub done: bool,
}

#[derive(Default)]
struct CallAccumulator {
    output_index: i64,
    item_id: String,
    call_id: String,
    name: String,
    arguments: String,
}

impl StreamAccumulator {
    pub fn handle(&mut self, event: &Value) -> Result<Vec<StreamDelta>, String> {
        let kind = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match kind {
            "response.output_text.delta" => {
                let delta = event
                    .get("delta")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if delta.is_empty() {
                    Ok(Vec::new())
                } else {
                    self.content.push_str(delta);
                    Ok(vec![StreamDelta::Content(delta.to_string())])
                }
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                let delta = event
                    .get("delta")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if delta.is_empty() {
                    Ok(Vec::new())
                } else {
                    self.thinking.push_str(delta);
                    Ok(vec![StreamDelta::Thinking(delta.to_string())])
                }
            }
            "response.output_item.added" | "response.output_item.done" => {
                self.merge_item(event);
                Ok(Vec::new())
            }
            "response.function_call_arguments.delta" => {
                let index = output_index(event);
                let item_id = event
                    .get("item_id")
                    .and_then(Value::as_str)
                    .or_else(|| event.get("call_id").and_then(Value::as_str));
                let delta = event
                    .get("delta")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                self.call(index, item_id).arguments.push_str(delta);
                Ok(Vec::new())
            }
            "response.function_call_arguments.done" => {
                let index = output_index(event);
                let item_id = event
                    .get("item_id")
                    .and_then(Value::as_str)
                    .or_else(|| event.get("call_id").and_then(Value::as_str));
                let call = self.call(index, item_id);
                if let Some(value) = event.get("call_id").and_then(Value::as_str) {
                    call.call_id = value.to_string();
                }
                if let Some(value) = event.get("name").and_then(Value::as_str) {
                    call.name = value.to_string();
                }
                if let Some(value) = event.get("arguments").and_then(Value::as_str) {
                    call.arguments = value.to_string();
                }
                Ok(Vec::new())
            }
            "response.completed" => {
                let response = event
                    .get("response")
                    .filter(|value| value.is_object())
                    .cloned()
                    .unwrap_or_else(|| Value::Object(Map::new()));
                if self.content.is_empty() {
                    self.content = extract_output_text(&response);
                }
                self.completed_calls = extract_tool_calls(&response);
                for item in extract_encrypted_reasoning_items(&response) {
                    self.merge_reasoning_item(&item);
                }
                self.usage = response
                    .get("usage")
                    .filter(|value| value.is_object())
                    .cloned();
                self.done = true;
                Ok(Vec::new())
            }
            "response.failed" | "response.incomplete" | "error" => Err(event_error(event)),
            _ => Ok(Vec::new()),
        }
    }

    pub fn finish(mut self) -> StreamResult {
        self.calls.sort_by_key(|call| call.output_index);
        let calls = self
            .calls
            .into_iter()
            .filter_map(|call| {
                let name = call.name.trim();
                if name.is_empty() {
                    return None;
                }
                let id = if !call.call_id.is_empty() {
                    call.call_id
                } else if !call.item_id.is_empty() {
                    call.item_id
                } else {
                    format!("responses_call_{}", call.output_index)
                };
                let arguments = if call.arguments.is_empty() {
                    Map::new()
                } else {
                    gml_llm::loads_value(&call.arguments)
                        .as_object()
                        .cloned()
                        .unwrap_or_default()
                };
                Some(json!({"id": id, "name": name, "arguments": arguments}))
            })
            .collect::<Vec<_>>();
        StreamResult {
            thinking: self.thinking,
            content: self.content,
            calls: if calls.is_empty() {
                self.completed_calls
            } else {
                calls
            },
            reasoning_items: self.reasoning_items,
            reset_reasoning_before: false,
            usage: self.usage,
        }
    }

    fn merge_item(&mut self, event: &Value) {
        let Some(item) = event.get("item").filter(|item| item.is_object()) else {
            return;
        };
        match item.get("type").and_then(Value::as_str) {
            Some("reasoning") => {
                self.merge_reasoning_item(item);
                return;
            }
            Some("function_call") => {}
            _ => return,
        }
        let index = output_index(event);
        let item_id = item.get("id").and_then(Value::as_str);
        let call = self.call(index, item_id);
        if let Some(value) = item_id {
            call.item_id = value.to_string();
        }
        if let Some(value) = item.get("call_id").and_then(Value::as_str) {
            call.call_id = value.to_string();
        }
        if let Some(value) = item.get("name").and_then(Value::as_str) {
            call.name = value.to_string();
        }
        if let Some(value) = item.get("arguments").and_then(Value::as_str) {
            call.arguments = value.to_string();
        }
    }

    fn merge_reasoning_item(&mut self, item: &Value) {
        let Some(source) = item.as_object() else {
            return;
        };
        let item_id = source
            .get("id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty());
        let encrypted_content = source
            .get("encrypted_content")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty());
        let position = self.reasoning_items.iter().position(|current| {
            item_id.is_some_and(|id| current.get("id").and_then(Value::as_str) == Some(id))
                || encrypted_content.is_some_and(|content| {
                    current.get("encrypted_content").and_then(Value::as_str) == Some(content)
                })
        });
        if let Some(position) = position {
            let Some(current) = self.reasoning_items[position].as_object_mut() else {
                self.reasoning_items[position] = item.clone();
                return;
            };
            current.extend(source.clone());
        } else {
            self.reasoning_items.push(item.clone());
        }
    }

    fn call(&mut self, output_index: i64, item_id: Option<&str>) -> &mut CallAccumulator {
        let position = item_id
            .and_then(|id| {
                self.calls
                    .iter()
                    .position(|call| call.item_id == id || call.call_id == id)
            })
            .or_else(|| {
                self.calls.iter().position(|call| {
                    call.output_index == output_index
                        && (item_id.is_none() || call.item_id.is_empty())
                })
            });
        let position = position.unwrap_or_else(|| {
            self.calls.push(CallAccumulator {
                output_index,
                item_id: item_id.unwrap_or_default().to_string(),
                ..CallAccumulator::default()
            });
            self.calls.len() - 1
        });
        &mut self.calls[position]
    }
}

fn output_index(event: &Value) -> i64 {
    event
        .get("output_index")
        .or_else(|| event.get("index"))
        .and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
        })
        .unwrap_or(0)
}

fn event_error(event: &Value) -> String {
    event
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| {
            event
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
        })
        .or_else(|| {
            event
                .get("response")
                .and_then(|response| response.get("error"))
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
        })
        .filter(|message| !message.trim().is_empty())
        .unwrap_or("SuperGrok stream failed")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accumulates_content_reasoning_and_tool_arguments() {
        let mut stream = StreamAccumulator::default();
        stream
            .handle(&json!({"type":"response.reasoning_text.delta","delta":"think"}))
            .unwrap();
        stream
            .handle(&json!({"type":"response.output_text.delta","delta":"hello"}))
            .unwrap();
        stream
            .handle(
                &json!({"type":"response.output_item.added","output_index":0,"item":{
                    "type":"function_call","id":"item-1","call_id":"call-1","name":"roll"
                }}),
            )
            .unwrap();
        stream
            .handle(&json!({"type":"response.function_call_arguments.delta","output_index":0,"item_id":"item-1","delta":"{\"n\":"}))
            .unwrap();
        stream
            .handle(&json!({"type":"response.function_call_arguments.delta","output_index":0,"item_id":"item-1","delta":"2}"}))
            .unwrap();
        stream
            .handle(&json!({"type":"response.completed","response":{"usage":{"input_tokens":3}}}))
            .unwrap();
        let result = stream.finish();
        assert_eq!(result.thinking, "think");
        assert_eq!(result.content, "hello");
        assert_eq!(result.calls[0]["arguments"]["n"], 2);
        assert_eq!(result.usage.unwrap()["input_tokens"], 3);
    }

    #[test]
    fn completed_payload_is_fallback_when_no_deltas_arrive() {
        let mut stream = StreamAccumulator::default();
        stream
            .handle(&json!({"type":"response.completed","response":{"output":[
                {"content":[{"type":"output_text","text":"fallback"}]},
                {"type":"function_call","call_id":"c","name":"tool","arguments":"{}"},
                {"type":"reasoning","encrypted_content":"completed-secret","summary":[]}
            ]}}))
            .unwrap();
        let result = stream.finish();
        assert_eq!(result.content, "fallback");
        assert_eq!(result.calls[0]["id"], "c");
        assert_eq!(
            result.reasoning_items[0]["encrypted_content"],
            "completed-secret"
        );
    }

    #[test]
    fn captures_completed_encrypted_reasoning_without_duplicates() {
        let mut stream = StreamAccumulator::default();
        stream
            .handle(&json!({
                "type":"response.output_item.added",
                "output_index":0,
                "item":{"type":"reasoning","id":"r1"}
            }))
            .unwrap();
        stream
            .handle(&json!({
                "type":"response.output_item.done",
                "output_index":0,
                "item":{
                    "type":"reasoning",
                    "id":"r1",
                    "encrypted_content":"secret",
                    "summary":[]
                }
            }))
            .unwrap();
        stream
            .handle(&json!({
                "type":"response.completed",
                "response":{"output":[{
                    "type":"reasoning",
                    "id":"r1",
                    "encrypted_content":"secret",
                    "summary":[]
                }]}
            }))
            .unwrap();

        let result = stream.finish();
        assert_eq!(result.reasoning_items.len(), 1);
        assert_eq!(result.reasoning_items[0]["encrypted_content"], "secret");
    }

    #[test]
    fn provider_failure_is_not_silenced() {
        let mut stream = StreamAccumulator::default();
        let error = stream
            .handle(&json!({"type":"response.failed","error":{"message":"denied"}}))
            .unwrap_err();
        assert_eq!(error, "denied");
    }

    #[test]
    fn calls_without_output_indexes_stay_separate_by_item_id() {
        let mut stream = StreamAccumulator::default();
        stream
            .handle(&json!({"type":"response.output_item.added","item":{
                "type":"function_call","id":"item-1","call_id":"call-1","name":"first"
            }}))
            .unwrap();
        stream
            .handle(&json!({"type":"response.output_item.added","item":{
                "type":"function_call","id":"item-2","call_id":"call-2","name":"second"
            }}))
            .unwrap();
        let result = stream.finish();
        assert_eq!(result.calls.len(), 2);
        assert_eq!(result.calls[0]["id"], "call-1");
        assert_eq!(result.calls[1]["id"], "call-2");
    }
}
