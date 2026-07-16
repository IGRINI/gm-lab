//! `CodexClient` — the ChatGPT Codex Responses API adapter implementing the
//! [`gml_llm::Backend`] trait.
//!
//! Faithful port of `codex_client.CodexClient`. Translates GM-Lab's
//! chat-completions-like interface into the ChatGPT Codex Responses endpoint
//! (`POST {CODEX_BASE_URL}/responses`), normalizes the SSE stream, tool calls,
//! model listing, and token usage back to GM-Lab's shapes.
//!
//! HTTP: uses `reqwest` (plain TLS, header-spoofing only — the Codex Responses
//! backend does NOT use JA3 impersonation today; see PORT_PLAN §1.3 / §3.2).

use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;
use serde_json::{json, Map, Value};

use gml_config::{Config, Role, RuntimeSettings};
use gml_llm::backend::{
    channel as llm_channel, Backend, BackendError, ChatOutput, ChatStreamOutput, DeltaSink,
    JsonStreamOutput,
};
use gml_llm::SessionIdentity;

use crate::oauth;
use crate::responses::{
    assistant_msg, clean, convert_tool_for_responses, loads, raw_tool_calls,
    split_messages_for_responses, think,
};
use crate::stream::{channel as sse_channel, StreamAccumulator, StreamResult};

const RESPONSES_LITE_HEADER: &str = "x-openai-internal-codex-responses-lite";

/// The Codex Responses API client.
pub struct CodexClient {
    responses_url: String,
    models_url: String,
    http: reqwest::Client,
    model: Mutex<String>,
    identity: SessionIdentity,
    /// `self._turn_state` — updated from the `x-codex-turn-state` response header.
    turn_state: Mutex<String>,
    /// `self.call_log`.
    call_log: Mutex<Vec<Map<String, Value>>>,
    cfg: Arc<Config>,
    settings: Arc<RuntimeSettings>,
}

impl CodexClient {
    /// Build the client (`CodexClient.__init__`).
    ///
    /// `installation_id` is a *persisted per-install* uuid (PORT_PLAN risk #9:
    /// "prefer a persisted per-install uuid") rather than a fresh per-process
    /// one. See [`crate::install_id::installation_id`].
    pub fn new(cfg: Arc<Config>, settings: Arc<RuntimeSettings>) -> Self {
        let base = cfg.codex_base_url.trim_end_matches('/').to_string();
        let responses_url = format!("{base}/responses");
        let models_url = format!("{base}/models");

        // httpx.Timeout(connect=10, read=None, write=60, pool=None): no read
        // timeout (streaming must run indefinitely). reqwest has no read timeout
        // by default; we only set the connect timeout.
        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .pool_idle_timeout(None)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        let model = {
            // self._model = config.CODEX_MODEL or config.MODEL

            if !cfg.codex_model.is_empty() {
                cfg.codex_model.clone()
            } else {
                cfg.model.clone()
            }
        };

        // SessionIdentity generates fresh session/thread/installation uuids; we
        // override installation_id with the persisted per-install value.
        let identity = SessionIdentity::new();

        CodexClient {
            responses_url,
            models_url,
            http,
            model: Mutex::new(model),
            identity,
            turn_state: Mutex::new(String::new()),
            call_log: Mutex::new(Vec::new()),
            cfg,
            settings,
        }
    }

    /// `self.session_id`.
    pub fn session_id(&self) -> String {
        self.identity.session_id()
    }

    /// `self.thread_id`.
    pub fn thread_id(&self) -> String {
        self.identity.thread_id()
    }

    /// Snapshot of the call log.
    pub fn call_log(&self) -> Vec<Map<String, Value>> {
        self.call_log.lock().expect("call_log lock").clone()
    }

    /// Effective prompt-cache key. A configured key is a namespace and the
    /// rotating thread id remains the per-history cache scope.
    pub fn prompt_cache_key(&self) -> String {
        self.identity
            .prompt_cache_key(&self.cfg.codex_prompt_cache_key)
    }

    /// `_payload(messages, tools, think, json_mode, reasoning_role)` — build the
    /// Responses request body. Public for testing the exact shape.
    pub fn payload(
        &self,
        messages: &Value,
        tools: Option<&Value>,
        think_flag: Option<bool>,
        json_mode: bool,
        reasoning_role: &str,
    ) -> Value {
        let settings = self.settings.get();
        let model = self.model_for_role(reasoning_role);
        let use_responses_lite = model_uses_responses_lite(&model);
        let (instructions, mut input_items) = split_messages_for_responses(messages);

        // json_object text.format requires the word "json" somewhere in the INPUT
        // messages (the Responses API does not scan `instructions`, where the
        // system prompt lands). Append a trailing hint message when absent —
        // appending keeps the shared prompt-cache prefix byte-identical.
        if json_mode && !input_items_mention_json(&input_items) {
            let json_hint =
                gml_prompts::render_prompt(gml_prompts::PromptId::JsonObjectInputHint, json!({}))
                    .expect("embedded JSON-object input hint must render");
            input_items.push(json!({
                "type": "message",
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": json_hint
                }],
            }));
        }

        // converted_tools = [convert_tool_for_responses(t) for t in (tools or [])]
        let converted_tools: Vec<Value> = match tools {
            Some(Value::Array(a)) => a.iter().map(convert_tool_for_responses).collect(),
            _ => Vec::new(),
        };
        let has_tools = !converted_tools.is_empty();
        let tool_choice = self.settings.tool_choice_for_request(has_tools);

        if use_responses_lite {
            let mut lite_input = Vec::with_capacity(input_items.len() + 2);
            lite_input.push(json!({
                "type": "additional_tools",
                "role": "developer",
                "tools": converted_tools,
            }));
            if !instructions.is_empty() {
                lite_input.push(json!({
                    "type": "message",
                    "role": "developer",
                    "content": [{
                        "type": "input_text",
                        "text": instructions,
                    }],
                }));
            }
            lite_input.append(&mut input_items);
            input_items = lite_input;
        }

        let mut payload = Map::new();
        payload.insert("model".into(), Value::String(model));
        payload.insert("input".into(), Value::Array(input_items));
        if !use_responses_lite {
            payload.insert("instructions".into(), Value::String(instructions));
            payload.insert("tools".into(), Value::Array(converted_tools));
        }
        payload.insert("tool_choice".into(), Value::String(tool_choice.clone()));
        // parallel_tool_calls = parallel_tool_calls_for_request(has_tools) and tool_choice != "none"
        let parallel = !use_responses_lite
            && self.settings.parallel_tool_calls_for_request(has_tools)
            && tool_choice != "none";
        payload.insert("parallel_tool_calls".into(), Value::Bool(parallel));
        payload.insert("store".into(), Value::Bool(false));
        payload.insert("stream".into(), Value::Bool(true));
        payload.insert("include".into(), Value::Array(Vec::new()));
        let mut client_metadata = Map::new();
        client_metadata.insert("application".into(), Value::String("gm-lab".into()));
        client_metadata.insert("provider".into(), Value::String("codex-oauth".into()));
        payload.insert("client_metadata".into(), Value::Object(client_metadata));

        // payload["prompt_cache_key"] = config.CODEX_PROMPT_CACHE_KEY or self._thread_id
        payload.insert(
            "prompt_cache_key".into(),
            Value::String(self.prompt_cache_key()),
        );

        // reasoning = runtime_settings.reasoning_for_request(think, reasoning_role)
        // Responses Lite requires all_turns even when effort/summary are disabled.
        let mut reasoning = self
            .settings
            .reasoning_for_request(think_flag, reasoning_role)
            .unwrap_or_default();
        if use_responses_lite {
            reasoning.insert("context".into(), Value::String("all_turns".into()));
        }
        if !reasoning.is_empty() {
            payload.insert("reasoning".into(), Value::Object(reasoning));
            payload.insert(
                "include".into(),
                Value::Array(vec![Value::String("reasoning.encrypted_content".into())]),
            );
        }

        // text: {verbosity?, format?}
        let mut text = Map::new();
        let verbosity = settings
            .get("text_verbosity")
            .and_then(|v| v.as_str())
            .unwrap_or("default");
        if verbosity != "default" {
            text.insert("verbosity".into(), Value::String(verbosity.to_string()));
        }
        if json_mode {
            // The expected shape belongs in the prompt. The provider receives
            // only loose JSON-object mode, never a response schema.
            let mut format = Map::new();
            format.insert("type".into(), Value::String("json_object".into()));
            text.insert("format".into(), Value::Object(format));
        }
        if !text.is_empty() {
            payload.insert("text".into(), Value::Object(text));
        }

        let max_output_tokens = self.settings.max_output_tokens();
        if max_output_tokens > 0 {
            payload.insert("max_output_tokens".into(), Value::from(max_output_tokens));
        }

        Value::Object(payload)
    }

    fn model_for_role(&self, reasoning_role: &str) -> String {
        if reasoning_role == Role::Compact.as_str() {
            let compact_model = self.cfg.codex_compact_model.trim();
            if !compact_model.is_empty() {
                return compact_model.to_string();
            }
        }
        self.model.lock().expect("model lock").clone()
    }

    /// `_auth_headers(accept_sse)` — assemble the request headers, refreshing the
    /// OAuth credential first.
    async fn auth_headers(
        &self,
        accept_sse: bool,
        use_responses_lite: bool,
    ) -> Result<reqwest::header::HeaderMap, BackendError> {
        let credential = oauth::ensure_fresh_credential(&self.http, &self.cfg)
            .await
            .map_err(BackendError::new)?;

        let mut headers = reqwest::header::HeaderMap::new();
        let mut put = |name: &'static str, value: String| {
            if let Ok(v) = reqwest::header::HeaderValue::from_str(&value) {
                // Custom (non-standard) header names need HeaderName::from_static.
                if let Ok(hn) = reqwest::header::HeaderName::from_bytes(name.as_bytes()) {
                    headers.insert(hn, v);
                }
            }
        };
        put(
            "authorization",
            format!("Bearer {}", credential.access_token.trim()),
        );
        put("originator", self.cfg.codex_originator.clone());
        put("user-agent", self.cfg.codex_user_agent.clone());
        put("version", self.cfg.codex_client_version.clone());
        put("session-id", self.identity.session_id());
        put("thread-id", self.identity.thread_id());
        put("x-client-request-id", self.identity.thread_id());
        put(
            "x-codex-installation-id",
            crate::install_id::installation_id(),
        );
        if accept_sse {
            put("accept", "text/event-stream".to_string());
        }
        if use_responses_lite {
            put(RESPONSES_LITE_HEADER, "true".to_string());
        }
        if let Some(account_id) = &credential.account_id {
            put("chatgpt-account-id", account_id.clone());
        }
        let turn_state = self.turn_state.lock().expect("turn_state lock").clone();
        if !turn_state.is_empty() {
            put("x-codex-turn-state", turn_state);
        }
        Ok(headers)
    }

    /// `_remember(label, usage, elapsed_ms)`.
    fn remember(&self, label: &str, usage: Option<&Value>, elapsed_ms: f64) -> Map<String, Value> {
        let stats = usage_stats(usage, Some(elapsed_ms));
        let mut row = Map::new();
        row.insert("label".into(), Value::String(label.to_string()));
        for (k, v) in &stats {
            row.insert(k.clone(), v.clone());
        }
        let prompt = stats
            .get("prompt_eval_count")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let eval = stats
            .get("eval_count")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        row.insert("tokens".into(), Value::from(prompt + eval));
        self.call_log.lock().expect("call_log lock").push(row);
        stats
    }

    /// `_iter_events(payload)` driver — POST the streaming request, parse the SSE
    /// frames, and feed each event to the accumulator via `handle`, forwarding
    /// `(channel, delta)` pairs to `sink`. Returns the final [`StreamResult`] and
    /// elapsed milliseconds.
    async fn collect_stream(
        &self,
        payload: &Value,
        sink: &mut (dyn DeltaSink + Send),
        content_channel: &str,
    ) -> Result<(StreamResult, f64), BackendError> {
        use futures_util::StreamExt;

        let t0 = Instant::now();
        let use_responses_lite = payload
            .get("model")
            .and_then(Value::as_str)
            .is_some_and(model_uses_responses_lite);
        let headers = self.auth_headers(true, use_responses_lite).await?;
        let resp = self
            .http
            .post(&self.responses_url)
            .headers(headers)
            .json(payload)
            .send()
            .await
            .map_err(BackendError::new)?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(BackendError::new(redacted_provider_error(status, &body)));
        }

        // turn_state = r.headers.get("x-codex-turn-state")
        if let Some(ts) = resp.headers().get("x-codex-turn-state") {
            if let Ok(s) = ts.to_str() {
                if !s.is_empty() {
                    *self.turn_state.lock().expect("turn_state lock") = s.to_string();
                }
            }
        }

        let mut acc = StreamAccumulator::new();
        let mut byte_stream = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        let mut data_lines: Vec<String> = Vec::new();
        let mut done = false;

        // Faithful port of `_iter_events`: iterate over stripped lines; an empty
        // line flushes the accumulated `data:` lines as one event payload.
        'outer: while let Some(item) = byte_stream.next().await {
            let bytes = item.map_err(BackendError::new)?;
            buf.extend_from_slice(&bytes);
            loop {
                let Some(pos) = buf.iter().position(|&b| b == b'\n') else {
                    break;
                };
                let mut raw: Vec<u8> = buf.drain(..=pos).collect();
                if raw.last() == Some(&b'\n') {
                    raw.pop();
                }
                if raw.last() == Some(&b'\r') {
                    raw.pop();
                }
                let line = String::from_utf8_lossy(&raw);
                let line = line.trim_matches(|c: char| c.is_whitespace());
                if line.is_empty() {
                    if !data_lines.is_empty() {
                        let payload_text = data_lines.join("\n");
                        data_lines.clear();
                        if payload_text == "[DONE]" {
                            done = true;
                            break 'outer;
                        }
                        let event = json_event(&payload_text)?;
                        forward_event(&mut acc, &event, sink, content_channel)?;
                        if acc.done {
                            done = true;
                            break 'outer;
                        }
                    }
                    continue;
                }
                if let Some(rest) = line.strip_prefix("data:") {
                    data_lines.push(rest.trim_matches(|c: char| c.is_whitespace()).to_string());
                }
            }
        }

        // Trailing flush (no terminating blank line at EOF).
        if !done && !data_lines.is_empty() {
            let payload_text = data_lines.join("\n");
            if payload_text != "[DONE]" {
                let event = json_event(&payload_text)?;
                forward_event(&mut acc, &event, sink, content_channel)?;
            }
        }

        let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
        Ok((acc.finish(), elapsed_ms))
    }
}

/// Feed one event to the accumulator and forward its `(channel, delta)` yields.
fn forward_event(
    acc: &mut StreamAccumulator,
    event: &Value,
    sink: &mut (dyn DeltaSink + Send),
    content_channel: &str,
) -> Result<(), BackendError> {
    let yields = acc.handle(event).map_err(BackendError::new)?;
    for (channel, text) in yields {
        match channel {
            sse_channel::THINKING => sink.emit(llm_channel::THINKING, &text),
            sse_channel::CONTENT => sink.emit(content_channel, &text),
            _ => {}
        }
    }
    Ok(())
}

#[async_trait]
impl Backend for CodexClient {
    fn connector_id(&self) -> &str {
        "codex"
    }

    fn model(&self) -> String {
        self.model.lock().expect("model lock").clone()
    }

    fn supports_native_tool_search(&self) -> bool {
        model_supports_native_tool_search(&self.model())
    }

    fn set_model(&self, model: &str) {
        let m = model.trim();
        if !m.is_empty() {
            let changed = {
                let mut current = self.model.lock().expect("model lock");
                if *current == m {
                    false
                } else {
                    *current = m.to_string();
                    true
                }
            };
            if changed {
                self.turn_state.lock().expect("turn_state lock").clear();
                self.identity.reset_cache_scope();
            }
        }
    }

    fn set_session_identity(&self, session_id: Option<&str>, thread_id: Option<&str>) {
        self.identity.set(session_id, thread_id);
    }

    fn session_id(&self) -> String {
        self.identity.session_id()
    }

    fn thread_id(&self) -> String {
        self.identity.thread_id()
    }

    fn prompt_cache_key(&self) -> String {
        CodexClient::prompt_cache_key(self)
    }

    async fn list_models(&self) -> Vec<Value> {
        // Python raises on failure; the Backend signature returns Vec. Surface an
        // empty list rather than panicking (callers treat empty as "no models").
        // The error path is exercised only on live calls.
        self.list_models_inner().await.unwrap_or_default()
    }

    async fn chat(
        &self,
        messages: &Value,
        tools: Option<&Value>,
        think_flag: Option<bool>,
        reasoning_role: &str,
    ) -> Result<ChatOutput, BackendError> {
        let payload = self.payload(messages, tools, think_flag, false, reasoning_role);
        let mut sink = gml_llm::NullSink;
        let (result, elapsed_ms) = self
            .collect_stream(&payload, &mut sink, sse_channel::CONTENT)
            .await?;
        self.remember("chat", result.usage.as_ref(), elapsed_ms);
        let raw = raw_tool_calls(&result.calls);
        Ok(ChatOutput {
            thinking: think(&result.thinking),
            content: clean(&result.content),
            calls: gml_llm::parse_tool_calls(Some(&Value::Array(raw.clone()))),
            assistant_msg: assistant_msg(&result.content, &raw),
        })
    }

    async fn chat_json(
        &self,
        messages: &Value,
        think_flag: Option<bool>,
        reasoning_role: &str,
    ) -> Result<Map<String, Value>, BackendError> {
        let payload = self.payload(messages, None, think_flag, true, reasoning_role);
        let mut sink = gml_llm::NullSink;
        let (result, elapsed_ms) = self
            .collect_stream(&payload, &mut sink, sse_channel::CONTENT)
            .await?;
        self.remember("chat_json", result.usage.as_ref(), elapsed_ms);
        Ok(loads(&result.content))
    }

    async fn summarize(&self, text: &str, proper_nouns: &[String]) -> Result<String, BackendError> {
        let proper_nouns_line = gml_prompts::gm_compact_connector_proper_nouns_line(proper_nouns);
        let sys = gml_prompts::render_gm_compact_system(&proper_nouns_line);
        // text[:config.COMPACT_INPUT_CHARS] — clip by chars (Unicode scalars).
        let clipped: String = text
            .chars()
            .take(self.cfg.compact_input_chars.max(0) as usize)
            .collect();
        let messages = serde_json::json!([
            {"role": "system", "content": sys},
            {"role": "user", "content": clipped},
        ]);
        let out = self
            .chat(&messages, None, Some(true), Role::Compact.as_str())
            .await?;
        Ok(out
            .content
            .trim_matches(|c: char| c.is_whitespace())
            .to_string())
    }

    async fn chat_stream(
        &self,
        messages: &Value,
        tools: Option<&Value>,
        think_flag: Option<bool>,
        reasoning_role: &str,
        sink: &mut (dyn DeltaSink + Send),
    ) -> Result<ChatStreamOutput, BackendError> {
        let payload = self.payload(messages, tools, think_flag, false, reasoning_role);
        let (result, elapsed_ms) = self
            .collect_stream(&payload, sink, sse_channel::CONTENT)
            .await?;
        let stats = self.remember("chat_stream", result.usage.as_ref(), elapsed_ms);
        let raw = raw_tool_calls(&result.calls);
        Ok(ChatStreamOutput {
            thinking: think(&result.thinking),
            content: clean(&result.content),
            calls: gml_llm::parse_tool_calls(Some(&Value::Array(raw.clone()))),
            assistant_msg: assistant_msg(&result.content, &raw),
            stats,
        })
    }

    async fn chat_json_stream(
        &self,
        messages: &Value,
        think_flag: Option<bool>,
        reasoning_role: &str,
        sink: &mut (dyn DeltaSink + Send),
    ) -> Result<JsonStreamOutput, BackendError> {
        // chat_json_stream forwards only "content" deltas (content_channel="content").
        let payload = self.payload(messages, None, think_flag, true, reasoning_role);
        // Wrap the sink to drop thinking deltas (Python only yields "content").
        let mut filtered = ContentOnlySink { inner: sink };
        let (result, elapsed_ms) = self
            .collect_stream(&payload, &mut filtered, sse_channel::CONTENT)
            .await?;
        let stats = self.remember("chat_json_stream", result.usage.as_ref(), elapsed_ms);
        Ok(JsonStreamOutput {
            data: loads(&result.content),
            stats,
        })
    }
}

fn model_supports_native_tool_search(model: &str) -> bool {
    let model = model.to_ascii_lowercase();
    model.contains("gpt-5.4")
        || model.contains("gpt-5.5")
        || model.contains("gpt-5.6")
        || model.starts_with("gpt-6")
}

fn model_uses_responses_lite(model: &str) -> bool {
    model.trim().to_ascii_lowercase().starts_with("gpt-5.6")
}

fn sort_models_for_picker(models: &mut [Value]) {
    models.sort_by(|a, b| {
        let sa = a.get("supported").and_then(Value::as_bool).unwrap_or(false);
        let sb = b.get("supported").and_then(Value::as_bool).unwrap_or(false);

        (!sa)
            .cmp(&(!sb))
            .then_with(|| {
                let pa = a.get("priority").and_then(Value::as_i64).unwrap_or(0);
                let pb = b.get("priority").and_then(Value::as_i64).unwrap_or(0);
                pa.cmp(&pb)
            })
            .then_with(|| {
                let na = a.get("name").and_then(Value::as_str).unwrap_or("");
                let nb = b.get("name").and_then(Value::as_str).unwrap_or("");
                na.cmp(nb)
            })
    });
}

impl CodexClient {
    /// `list_models()` — the live model listing (fallible, like Python).
    async fn list_models_inner(&self) -> Result<Vec<Value>, BackendError> {
        let headers = self.auth_headers(false, false).await?;
        let resp = self
            .http
            .get(&self.models_url)
            .headers(headers)
            .query(&[("client_version", &self.cfg.codex_client_version)])
            .timeout(std::time::Duration::from_secs(20))
            .send()
            .await
            .map_err(BackendError::new)?;
        if !resp.status().is_success() {
            return Err(BackendError::new(format!(
                "Codex models endpoint failed with status {}",
                resp.status().as_u16()
            )));
        }
        let data: Value = resp.json().await.map_err(BackendError::new)?;
        // raw_models = data.get("models") or data.get("data") or []
        let raw_models = data
            .get("models")
            .filter(|v| is_truthy(v))
            .or_else(|| data.get("data").filter(|v| is_truthy(v)));
        let self_model = self.model.lock().expect("model lock").clone();
        let mut models: Vec<Value> = Vec::new();
        if let Some(Value::Array(items)) = raw_models {
            for raw in items {
                let Some(obj) = raw.as_object() else { continue };
                // slug = raw.get("slug") or raw.get("id") or raw.get("model")
                let slug = obj
                    .get("slug")
                    .filter(|v| is_truthy(v))
                    .or_else(|| obj.get("id").filter(|v| is_truthy(v)))
                    .or_else(|| obj.get("model").filter(|v| is_truthy(v)))
                    .and_then(|v| v.as_str());
                let Some(slug) = slug else { continue };
                let visibility = obj
                    .get("visibility")
                    .filter(|v| is_truthy(v))
                    .and_then(|v| v.as_str())
                    .unwrap_or("list")
                    .to_string();
                if visibility != "list" && slug != self_model {
                    continue;
                }
                let name = obj
                    .get("display_name")
                    .filter(|v| is_truthy(v))
                    .and_then(|v| v.as_str())
                    .unwrap_or(slug);
                let supported = obj.get("supported_in_api").map(is_truthy).unwrap_or(true);
                let mut entry = Map::new();
                entry.insert("id".into(), Value::String(slug.to_string()));
                entry.insert("slug".into(), Value::String(slug.to_string()));
                entry.insert("name".into(), Value::String(name.to_string()));
                entry.insert(
                    "description".into(),
                    Value::String(
                        obj.get("description")
                            .filter(|v| is_truthy(v))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                    ),
                );
                entry.insert("supported".into(), Value::Bool(supported));
                entry.insert("visibility".into(), Value::String(visibility));
                entry.insert(
                    "priority".into(),
                    obj.get("priority").cloned().unwrap_or(Value::from(0)),
                );
                entry.insert(
                    "context_window".into(),
                    obj.get("context_window").cloned().unwrap_or(Value::Null),
                );
                entry.insert(
                    "default_reasoning_level".into(),
                    obj.get("default_reasoning_level")
                        .cloned()
                        .unwrap_or(Value::Null),
                );
                entry.insert(
                    "default_reasoning_summary".into(),
                    obj.get("default_reasoning_summary")
                        .cloned()
                        .unwrap_or(Value::Null),
                );
                entry.insert(
                    "supports_reasoning_summaries".into(),
                    obj.get("supports_reasoning_summaries")
                        .cloned()
                        .unwrap_or(Value::Null),
                );
                let levels = obj
                    .get("supported_reasoning_levels")
                    .filter(|v| is_truthy(v))
                    .or_else(|| {
                        obj.get("supported_reasoning_efforts")
                            .filter(|v| is_truthy(v))
                    })
                    .cloned()
                    .unwrap_or(Value::Array(Vec::new()));
                entry.insert("supported_reasoning_levels".into(), levels);
                entry.insert(
                    "default_verbosity".into(),
                    obj.get("default_verbosity").cloned().unwrap_or(Value::Null),
                );
                entry.insert(
                    "support_verbosity".into(),
                    obj.get("support_verbosity").cloned().unwrap_or(Value::Null),
                );
                entry.insert(
                    "use_responses_lite".into(),
                    obj.get("use_responses_lite")
                        .cloned()
                        .unwrap_or(Value::Bool(false)),
                );
                entry.insert(
                    "tool_mode".into(),
                    obj.get("tool_mode").cloned().unwrap_or(Value::Null),
                );
                models.push(Value::Object(entry));
            }
        }
        // Match the current Codex picker: supported models first, then lower
        // server priority values first (the 5.6 family is currently 1..3).
        sort_models_for_picker(&mut models);
        Ok(models)
    }
}

/// A [`DeltaSink`] wrapper that forwards only `content` deltas, dropping
/// `thinking` ones — matching `chat_json_stream`'s `if channel == "content"`.
struct ContentOnlySink<'a> {
    inner: &'a mut (dyn DeltaSink + Send),
}

impl DeltaSink for ContentOnlySink<'_> {
    fn emit(&mut self, channel: &str, delta: &str) {
        if channel == llm_channel::CONTENT {
            self.inner.emit(channel, delta);
        }
    }
}

/// `_usage_stats(usage, elapsed_ms)` — normalize Responses usage to the `_meta`
/// shape (durations in nanoseconds).
pub fn usage_stats(usage: Option<&Value>, elapsed_ms: Option<f64>) -> Map<String, Value> {
    let empty = Value::Object(Map::new());
    let usage = usage.unwrap_or(&empty);
    // prompt = int(usage.get("input_tokens") or usage.get("prompt_tokens") or 0)
    let prompt = first_int(usage, &["input_tokens", "prompt_tokens"]);
    let output = first_int(usage, &["output_tokens", "completion_tokens"]);
    // details = usage.get("input_tokens_details") or usage.get("prompt_tokens_details") or {}
    let details = usage
        .get("input_tokens_details")
        .filter(|v| is_truthy(v))
        .or_else(|| usage.get("prompt_tokens_details").filter(|v| is_truthy(v)));
    let cached = match details {
        Some(Value::Object(d)) => d
            .get("cached_tokens")
            .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)))
            .unwrap_or(0),
        _ => 0,
    };
    let elapsed_ns = ((elapsed_ms.unwrap_or(0.0)) * 1e6) as i64;
    let mut s = Map::new();
    s.insert("prompt_eval_count".into(), Value::from(prompt));
    s.insert("eval_count".into(), Value::from(output));
    s.insert("cached_tokens".into(), Value::from(cached));
    s.insert("prompt_eval_duration".into(), Value::from(0));
    s.insert("eval_duration".into(), Value::from(elapsed_ns));
    s.insert("total_duration".into(), Value::from(elapsed_ns));
    s.insert("load_duration".into(), Value::from(0));
    s
}

/// `_json_event(text)` — parse one SSE JSON payload.
fn json_event(text: &str) -> Result<Value, BackendError> {
    match serde_json::from_str::<Value>(text) {
        Ok(v) => Ok(if v.is_object() {
            v
        } else {
            Value::Object(Map::new())
        }),
        Err(e) => Err(BackendError::new(format!(
            "Codex returned invalid SSE JSON: {e}"
        ))),
    }
}

/// `_redacted_provider_error(status_code, body)`.
fn redacted_provider_error(status_code: u16, body: &str) -> String {
    let compact = body.replace('\n', " ");
    let compact = compact.trim();
    let compact = if compact.chars().count() > 2000 {
        let truncated: String = compact.chars().take(2000).collect();
        format!("{truncated}...")
    } else {
        compact.to_string()
    };
    format!("Codex API error {status_code}: {compact}")
}

fn first_int(obj: &Value, keys: &[&str]) -> i64 {
    for key in keys {
        if let Some(v) = obj.get(*key) {
            if is_truthy(v) {
                if let Some(n) = v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)) {
                    return n;
                }
            }
        }
    }
    0
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

/// Does any Responses `input` item mention "json" (case-insensitive)? Scans
/// message text parts and `function_call_output` payloads — the fields the
/// Responses API json_object validation reads.
fn input_items_mention_json(items: &[Value]) -> bool {
    items.iter().any(|item| {
        if let Some(output) = item.get("output").and_then(Value::as_str) {
            if output.to_lowercase().contains("json") {
                return true;
            }
        }
        match item.get("content") {
            Some(Value::Array(parts)) => parts.iter().any(|part| {
                part.get("text")
                    .and_then(Value::as_str)
                    .is_some_and(|t| t.to_lowercase().contains("json"))
            }),
            _ => false,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn test_client() -> CodexClient {
        let cfg = Arc::new(Config::from_env());
        let settings = Arc::new(RuntimeSettings::new(
            &cfg,
            std::env::temp_dir().join("gml_codex_test_settings.json"),
        ));
        CodexClient::new(cfg, settings)
    }

    #[test]
    fn prompt_cache_key_uses_thread_id_when_unset() {
        let mut cfg = Config::from_env();
        cfg.codex_prompt_cache_key = String::new();
        let cfg = Arc::new(cfg);
        let settings = Arc::new(RuntimeSettings::new(
            &cfg,
            std::env::temp_dir().join("gml_codex_pck1.json"),
        ));
        let client = CodexClient::new(cfg, settings);
        assert_eq!(client.prompt_cache_key(), client.thread_id());
    }

    #[test]
    fn configured_prompt_cache_namespace_keeps_history_scope() {
        let mut cfg = Config::from_env();
        cfg.codex_prompt_cache_key = "fixed-cache-key".to_string();
        let cfg = Arc::new(cfg);
        let settings = Arc::new(RuntimeSettings::new(
            &cfg,
            std::env::temp_dir().join("gml_codex_pck2.json"),
        ));
        let client = CodexClient::new(cfg, settings);
        assert_eq!(
            client.prompt_cache_key(),
            format!("fixed-cache-key:{}", client.thread_id())
        );
        assert_ne!(client.prompt_cache_key(), client.thread_id());

        let before = client.prompt_cache_key();
        client.set_model("gpt-cache-scope-rotated");
        assert_ne!(client.prompt_cache_key(), before);
    }

    #[test]
    fn set_session_identity_overrides_only_nonempty() {
        let client = test_client();
        let orig_session = client.session_id();
        let orig_thread = client.thread_id();
        client.set_session_identity(Some(""), Some("   "));
        assert_eq!(client.session_id(), orig_session);
        assert_eq!(client.thread_id(), orig_thread);
        client.set_session_identity(Some("sess-x"), Some("thr-y"));
        assert_eq!(client.session_id(), "sess-x");
        assert_eq!(client.thread_id(), "thr-y");
        // prompt_cache_key follows the restored thread_id
        assert_eq!(client.prompt_cache_key(), "thr-y");
    }

    #[test]
    fn set_model_trims_and_ignores_empty() {
        let client = test_client();
        let orig = client.model();
        let original_session = client.session_id();
        let original_thread = client.thread_id();
        client.set_model("   ");
        assert_eq!(client.model(), orig);
        assert_eq!(client.session_id(), original_session);
        assert_eq!(client.thread_id(), original_thread);
        client.set_model("  gpt-test  ");
        assert_eq!(client.model(), "gpt-test");
        assert_ne!(client.session_id(), original_session);
        assert_ne!(client.thread_id(), original_thread);
        let changed_session = client.session_id();
        let changed_thread = client.thread_id();
        client.set_model("gpt-test");
        assert_eq!(client.session_id(), changed_session);
        assert_eq!(client.thread_id(), changed_thread);
    }

    #[test]
    fn native_tool_search_capability_is_model_gated() {
        assert!(model_supports_native_tool_search("gpt-5.4"));
        assert!(model_supports_native_tool_search("gpt-5.4-mini"));
        assert!(model_supports_native_tool_search("gpt-5.5"));
        assert!(model_supports_native_tool_search("gpt-5.6-sol"));
        assert!(model_supports_native_tool_search("gpt-5.6-terra"));
        assert!(model_supports_native_tool_search("gpt-5.6-luna"));
        assert!(model_supports_native_tool_search("gpt-6"));
        assert!(!model_supports_native_tool_search("gpt-5.3"));
        assert!(!model_supports_native_tool_search("codex-mini-latest"));
    }

    #[test]
    fn payload_basic_shape_and_cache_key() {
        let mut cfg = Config::from_env();
        cfg.codex_prompt_cache_key = String::new();
        cfg.codex_model = "gpt-test".to_string();
        let cfg = Arc::new(cfg);
        let settings = Arc::new(RuntimeSettings::new(
            &cfg,
            std::env::temp_dir().join("gml_codex_payload1.json"),
        ));
        let client = CodexClient::new(cfg, settings);
        let messages = serde_json::json!([
            {"role": "system", "content": "S1"},
            {"role": "system", "content": "S2"},
            {"role": "user", "content": "hi"},
        ]);
        let p = client.payload(&messages, None, Some(false), false, Role::Gm.as_str());
        assert_eq!(p.get("model").unwrap(), "gpt-test");
        assert_eq!(p.get("instructions").unwrap(), "S1\n\nS2");
        assert_eq!(p.get("store").unwrap(), &Value::Bool(false));
        assert_eq!(p.get("stream").unwrap(), &Value::Bool(true));
        assert_eq!(
            p.get("prompt_cache_key").unwrap(),
            &Value::String(client.thread_id())
        );
        // no tools -> tool_choice none, parallel false
        assert_eq!(p.get("tool_choice").unwrap(), "none");
        assert_eq!(p.get("parallel_tool_calls").unwrap(), &Value::Bool(false));
        // client_metadata
        assert_eq!(p.pointer("/client_metadata/application").unwrap(), "gm-lab");
        assert_eq!(
            p.pointer("/client_metadata/provider").unwrap(),
            "codex-oauth"
        );
    }

    #[test]
    fn responses_lite_payload_embeds_tools_and_instructions_in_input() {
        let mut cfg = Config::from_env();
        cfg.codex_prompt_cache_key = String::new();
        cfg.codex_model = "gpt-5.6-sol".to_string();
        let cfg = Arc::new(cfg);
        let settings = Arc::new(RuntimeSettings::new(
            &cfg,
            std::env::temp_dir().join("gml_codex_responses_lite_payload.json"),
        ));
        let client = CodexClient::new(cfg, settings);
        let messages = serde_json::json!([
            {"role": "system", "content": "GM instructions"},
            {"role": "user", "content": "hello"},
        ]);
        let tools = serde_json::json!([{
            "type": "function",
            "function": {
                "name": "roll_dice",
                "description": "Roll dice",
                "parameters": {"type": "object", "properties": {}}
            }
        }]);

        let payload = client.payload(
            &messages,
            Some(&tools),
            Some(true),
            false,
            Role::Gm.as_str(),
        );

        assert!(payload.get("instructions").is_none());
        assert!(payload.get("tools").is_none());
        assert_eq!(
            payload.get("parallel_tool_calls"),
            Some(&Value::Bool(false))
        );
        assert_eq!(payload.pointer("/reasoning/context").unwrap(), "all_turns");

        let input = payload.get("input").and_then(Value::as_array).unwrap();
        assert_eq!(input[0].get("type").unwrap(), "additional_tools");
        assert_eq!(input[0].get("role").unwrap(), "developer");
        assert_eq!(input[0].pointer("/tools/0/name").unwrap(), "roll_dice");
        assert_eq!(input[1].get("role").unwrap(), "developer");
        assert_eq!(
            input[1].pointer("/content/0/text").unwrap(),
            "GM instructions"
        );
        assert_eq!(input[2].get("role").unwrap(), "user");
    }

    #[test]
    fn responses_lite_is_limited_to_gpt_5_6_family() {
        assert!(model_uses_responses_lite("gpt-5.6"));
        assert!(model_uses_responses_lite("gpt-5.6-terra"));
        assert!(!model_uses_responses_lite("gpt-5.5"));
        assert!(!model_uses_responses_lite("gpt-6"));
    }

    #[test]
    fn responses_lite_keeps_required_context_when_reasoning_is_disabled() {
        let mut cfg = Config::from_env();
        cfg.codex_model = "gpt-5.6-luna".to_string();
        let cfg = Arc::new(cfg);
        let settings = Arc::new(RuntimeSettings::new(
            &cfg,
            std::env::temp_dir().join("gml_codex_responses_lite_no_reasoning.json"),
        ));
        let client = CodexClient::new(cfg, settings);
        let messages = serde_json::json!([{"role": "user", "content": "hello"}]);

        let payload = client.payload(&messages, None, Some(false), false, Role::Npc.as_str());

        assert_eq!(payload.pointer("/reasoning/context").unwrap(), "all_turns");
        assert!(payload.pointer("/reasoning/effort").is_none());
        assert!(payload.pointer("/reasoning/summary").is_none());
        assert_eq!(
            payload.pointer("/include/0").unwrap(),
            "reasoning.encrypted_content"
        );
    }

    #[test]
    fn model_catalog_uses_current_codex_priority_order() {
        let mut models = vec![
            json!({"id": "legacy", "name": "legacy", "supported": true, "priority": 16}),
            json!({"id": "terra", "name": "terra", "supported": true, "priority": 2}),
            json!({"id": "sol", "name": "sol", "supported": true, "priority": 1}),
            json!({"id": "unsupported", "name": "unsupported", "supported": false, "priority": 0}),
        ];

        sort_models_for_picker(&mut models);

        let ids: Vec<&str> = models
            .iter()
            .filter_map(|model| model.get("id").and_then(Value::as_str))
            .collect();
        assert_eq!(ids, vec!["sol", "terra", "legacy", "unsupported"]);
    }

    #[test]
    fn legacy_models_keep_the_standard_responses_shape() {
        let mut cfg = Config::from_env();
        cfg.codex_model = "gpt-5.5".to_string();
        let cfg = Arc::new(cfg);
        let settings = Arc::new(RuntimeSettings::new(
            &cfg,
            std::env::temp_dir().join("gml_codex_standard_responses_payload.json"),
        ));
        let client = CodexClient::new(cfg, settings);
        let messages = serde_json::json!([
            {"role": "system", "content": "GM instructions"},
            {"role": "user", "content": "hello"},
        ]);
        let tools = serde_json::json!([{
            "type": "function",
            "function": {
                "name": "roll_dice",
                "description": "Roll dice",
                "parameters": {"type": "object", "properties": {}}
            }
        }]);

        let payload = client.payload(
            &messages,
            Some(&tools),
            Some(false),
            false,
            Role::Gm.as_str(),
        );

        assert_eq!(payload.get("instructions").unwrap(), "GM instructions");
        assert_eq!(payload.pointer("/tools/0/name").unwrap(), "roll_dice");
        assert_eq!(payload.pointer("/input/0/role").unwrap(), "user");
        assert!(payload
            .get("input")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .all(|item| item.get("type").and_then(Value::as_str) != Some("additional_tools")));
    }

    #[test]
    fn payload_uses_compact_model_only_for_compact_role() {
        let mut cfg = Config::from_env();
        cfg.codex_prompt_cache_key = String::new();
        cfg.codex_model = "gpt-main".to_string();
        cfg.codex_compact_model = "gpt-mini-compact".to_string();
        let cfg = Arc::new(cfg);
        let settings = Arc::new(RuntimeSettings::new(
            &cfg,
            std::env::temp_dir().join("gml_codex_compact_model.json"),
        ));
        let client = CodexClient::new(cfg, settings);
        let messages = serde_json::json!([{"role": "user", "content": "hi"}]);

        let gm = client.payload(&messages, None, Some(false), false, Role::Gm.as_str());
        assert_eq!(gm.get("model").unwrap(), "gpt-main");

        let compact = client.payload(&messages, None, Some(true), false, Role::Compact.as_str());
        assert_eq!(compact.get("model").unwrap(), "gpt-mini-compact");
    }

    #[test]
    fn payload_reasoning_sets_include_encrypted() {
        let cfg = Arc::new(Config::from_env());
        let settings = Arc::new(RuntimeSettings::new(
            &cfg,
            std::env::temp_dir().join("gml_codex_payload2.json"),
        ));
        let client = CodexClient::new(cfg, settings);
        let messages = serde_json::json!([{"role": "user", "content": "hi"}]);
        // think=true with GM role (default effort low) -> reasoning present
        let p = client.payload(&messages, None, Some(true), false, Role::Gm.as_str());
        assert!(p.get("reasoning").is_some());
        assert_eq!(
            p.get("include").unwrap(),
            &serde_json::json!(["reasoning.encrypted_content"])
        );
        // include empty when no reasoning (think=false)
        let p2 = client.payload(&messages, None, Some(false), false, Role::Gm.as_str());
        assert_eq!(p2.get("include").unwrap(), &serde_json::json!([]));
        assert!(p2.get("reasoning").is_none());
    }

    #[test]
    fn payload_json_mode_sets_json_object_text_format() {
        let cfg = Arc::new(Config::from_env());
        let settings = Arc::new(RuntimeSettings::new(
            &cfg,
            std::env::temp_dir().join("gml_codex_payload3.json"),
        ));
        let client = CodexClient::new(cfg, settings);
        let messages = serde_json::json!([{"role": "user", "content": "hi"}]);
        let p = client.payload(&messages, None, Some(true), true, Role::Gm.as_str());
        assert_eq!(p.pointer("/text/format/type").unwrap(), "json_object");
        assert!(p.pointer("/text/format/name").is_none());
        assert!(p.pointer("/text/format/strict").is_none());
        assert!(p.pointer("/text/format/schema").is_none());
        // "hi" carries no "json": the client must append the input hint the
        // Responses API requires for json_object mode (it ignores instructions).
        let input = p.get("input").unwrap().as_array().unwrap();
        assert_eq!(input.len(), 2);
        let hint = input.last().unwrap();
        assert_eq!(hint.pointer("/role").unwrap(), "user");
        assert_eq!(
            hint.pointer("/content/0/text").unwrap(),
            "Return the result strictly as a JSON object."
        );
    }

    #[test]
    fn payload_json_mode_skips_hint_when_input_mentions_json() {
        let cfg = Arc::new(Config::from_env());
        let settings = Arc::new(RuntimeSettings::new(
            &cfg,
            std::env::temp_dir().join("gml_codex_payload4.json"),
        ));
        let client = CodexClient::new(cfg, settings);
        let messages = serde_json::json!([
            {"role": "system", "content": "Return JSON only."},
            {"role": "user", "content": "Верни ответ как JSON-объект: {\"moves\":[]}"},
        ]);
        let p = client.payload(&messages, None, Some(false), true, Role::Gm.as_str());
        let input = p.get("input").unwrap().as_array().unwrap();
        assert_eq!(input.len(), 1);
    }

    #[test]
    fn payload_without_json_mode_never_appends_json_hint() {
        let cfg = Arc::new(Config::from_env());
        let settings = Arc::new(RuntimeSettings::new(
            &cfg,
            std::env::temp_dir().join("gml_codex_payload5.json"),
        ));
        let client = CodexClient::new(cfg, settings);
        let messages = serde_json::json!([{"role": "user", "content": "hi"}]);
        let p = client.payload(&messages, None, Some(false), false, Role::Gm.as_str());
        let input = p.get("input").unwrap().as_array().unwrap();
        assert_eq!(input.len(), 1);
    }

    #[test]
    fn usage_stats_shape() {
        let usage = serde_json::json!({
            "input_tokens": 100,
            "output_tokens": 20,
            "input_tokens_details": {"cached_tokens": 64}
        });
        let s = usage_stats(Some(&usage), Some(1000.0));
        assert_eq!(s.get("prompt_eval_count").unwrap(), &Value::from(100));
        assert_eq!(s.get("eval_count").unwrap(), &Value::from(20));
        assert_eq!(s.get("cached_tokens").unwrap(), &Value::from(64));
        assert_eq!(s.get("prompt_eval_duration").unwrap(), &Value::from(0));
        // elapsed_ns = 1000ms * 1e6 = 1_000_000_000
        assert_eq!(
            s.get("eval_duration").unwrap(),
            &Value::from(1_000_000_000i64)
        );
        assert_eq!(
            s.get("total_duration").unwrap(),
            &Value::from(1_000_000_000i64)
        );
        assert_eq!(s.get("load_duration").unwrap(), &Value::from(0));
        // key order
        let out = serde_json::to_string(&Value::Object(s)).unwrap();
        assert_eq!(
            out,
            r#"{"prompt_eval_count":100,"eval_count":20,"cached_tokens":64,"prompt_eval_duration":0,"eval_duration":1000000000,"total_duration":1000000000,"load_duration":0}"#
        );
    }

    #[test]
    fn usage_stats_prompt_tokens_fallback() {
        let usage = serde_json::json!({"prompt_tokens": 5, "completion_tokens": 2});
        let s = usage_stats(Some(&usage), None);
        assert_eq!(s.get("prompt_eval_count").unwrap(), &Value::from(5));
        assert_eq!(s.get("eval_count").unwrap(), &Value::from(2));
        assert_eq!(s.get("cached_tokens").unwrap(), &Value::from(0));
    }

    #[test]
    fn redacted_error_truncates() {
        let body = "x".repeat(2500);
        let msg = redacted_provider_error(500, &body);
        assert!(msg.starts_with("Codex API error 500: "));
        assert!(msg.ends_with("..."));
    }
}
