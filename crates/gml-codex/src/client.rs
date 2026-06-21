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
use serde_json::{Map, Value};

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
            let m = if !cfg.codex_model.is_empty() {
                cfg.codex_model.clone()
            } else {
                cfg.model.clone()
            };
            m
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

    /// `prompt_cache_key` — `CODEX_PROMPT_CACHE_KEY or self.thread_id`.
    pub fn prompt_cache_key(&self) -> String {
        self.identity.prompt_cache_key(&self.cfg.codex_prompt_cache_key)
    }

    /// `_payload(messages, tools, think, schema, reasoning_role)` — build the
    /// Responses request body. Public for testing the exact shape.
    pub fn payload(
        &self,
        messages: &Value,
        tools: Option<&Value>,
        think_flag: Option<bool>,
        schema: Option<&Value>,
        reasoning_role: &str,
    ) -> Value {
        let settings = self.settings.get();
        let (instructions, input_items) = split_messages_for_responses(messages);

        // converted_tools = [convert_tool_for_responses(t) for t in (tools or [])]
        let converted_tools: Vec<Value> = match tools {
            Some(Value::Array(a)) => a.iter().map(convert_tool_for_responses).collect(),
            _ => Vec::new(),
        };
        let has_tools = !converted_tools.is_empty();
        let tool_choice = self.settings.tool_choice_for_request(has_tools);

        let mut payload = Map::new();
        payload.insert(
            "model".into(),
            Value::String(self.model.lock().expect("model lock").clone()),
        );
        payload.insert("instructions".into(), Value::String(instructions));
        payload.insert("input".into(), Value::Array(input_items));
        payload.insert("tools".into(), Value::Array(converted_tools));
        payload.insert("tool_choice".into(), Value::String(tool_choice.clone()));
        // parallel_tool_calls = parallel_tool_calls_for_request(has_tools) and tool_choice != "none"
        let parallel =
            self.settings.parallel_tool_calls_for_request(has_tools) && tool_choice != "none";
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
        if let Some(reasoning) = self.settings.reasoning_for_request(think_flag, reasoning_role) {
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
        if let Some(schema) = schema {
            // `if schema:` — Python truthiness; skip empty schemas.
            if is_truthy(schema) {
                let mut format = Map::new();
                format.insert("type".into(), Value::String("json_schema".into()));
                format.insert("name".into(), Value::String("gm_lab_json".into()));
                format.insert("strict".into(), Value::Bool(false));
                format.insert("schema".into(), schema.clone());
                text.insert("format".into(), Value::Object(format));
            }
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

    /// `_auth_headers(accept_sse)` — assemble the request headers, refreshing the
    /// OAuth credential first.
    async fn auth_headers(&self, accept_sse: bool) -> Result<reqwest::header::HeaderMap, BackendError> {
        let credential = oauth::ensure_fresh_credential(&self.http, &self.cfg)
            .await
            .map_err(|e| BackendError::new(e))?;

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
        put("x-codex-installation-id", crate::install_id::installation_id());
        if accept_sse {
            put("accept", "text/event-stream".to_string());
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
        let prompt = stats.get("prompt_eval_count").and_then(|v| v.as_i64()).unwrap_or(0);
        let eval = stats.get("eval_count").and_then(|v| v.as_i64()).unwrap_or(0);
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
        let headers = self.auth_headers(true).await?;
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
    fn model(&self) -> String {
        self.model.lock().expect("model lock").clone()
    }

    fn set_model(&self, model: &str) {
        let m = model.trim();
        if !m.is_empty() {
            *self.model.lock().expect("model lock") = m.to_string();
        }
    }

    fn set_session_identity(&self, session_id: Option<&str>, thread_id: Option<&str>) {
        self.identity.set(session_id, thread_id);
    }

    async fn list_models(&self) -> Vec<Value> {
        match self.list_models_inner().await {
            Ok(v) => v,
            // Python raises on failure; the Backend signature returns Vec.
            // Surface an empty list rather than panicking (callers treat empty
            // as "no models"). The error path is exercised only on live calls.
            Err(_) => Vec::new(),
        }
    }

    async fn chat(
        &self,
        messages: &Value,
        tools: Option<&Value>,
        think_flag: Option<bool>,
        reasoning_role: &str,
    ) -> Result<ChatOutput, BackendError> {
        let payload = self.payload(messages, tools, think_flag, None, reasoning_role);
        let mut sink = gml_llm::NullSink;
        let (result, elapsed_ms) = self.collect_stream(&payload, &mut sink, sse_channel::CONTENT).await?;
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
        schema: &Value,
        think_flag: Option<bool>,
        reasoning_role: &str,
    ) -> Result<Map<String, Value>, BackendError> {
        let payload = self.payload(messages, None, think_flag, Some(schema), reasoning_role);
        let mut sink = gml_llm::NullSink;
        let (result, elapsed_ms) = self.collect_stream(&payload, &mut sink, sse_channel::CONTENT).await?;
        self.remember("chat_json", result.usage.as_ref(), elapsed_ms);
        Ok(loads(&result.content))
    }

    async fn summarize(
        &self,
        text: &str,
        proper_nouns: &[String],
    ) -> Result<String, BackendError> {
        // names = [str(name).strip() for name in proper_nouns if str(name).strip()]
        let names: Vec<String> = proper_nouns
            .iter()
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty())
            .collect();
        // Codex-specific proper_nouns_line (DIFFERENT wording from llm_client's).
        let proper_nouns_line = if names.is_empty() {
            "Keep proper nouns exactly as written; never translate or transliterate them.".to_string()
        } else {
            format!(
                "Keep these proper nouns exactly as written: {}.",
                names.join(", ")
            )
        };
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
        Ok(out.content.trim_matches(|c: char| c.is_whitespace()).to_string())
    }

    async fn chat_stream(
        &self,
        messages: &Value,
        tools: Option<&Value>,
        think_flag: Option<bool>,
        reasoning_role: &str,
        sink: &mut (dyn DeltaSink + Send),
    ) -> Result<ChatStreamOutput, BackendError> {
        let payload = self.payload(messages, tools, think_flag, None, reasoning_role);
        let (result, elapsed_ms) = self.collect_stream(&payload, sink, sse_channel::CONTENT).await?;
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
        schema: &Value,
        think_flag: Option<bool>,
        reasoning_role: &str,
        sink: &mut (dyn DeltaSink + Send),
    ) -> Result<JsonStreamOutput, BackendError> {
        // chat_json_stream forwards only "content" deltas (content_channel="content").
        let payload = self.payload(messages, None, think_flag, Some(schema), reasoning_role);
        // Wrap the sink to drop thinking deltas (Python only yields "content").
        let mut filtered = ContentOnlySink { inner: sink };
        let (result, elapsed_ms) =
            self.collect_stream(&payload, &mut filtered, sse_channel::CONTENT).await?;
        let stats = self.remember("chat_json_stream", result.usage.as_ref(), elapsed_ms);
        Ok(JsonStreamOutput {
            data: loads(&result.content),
            stats,
        })
    }
}

impl CodexClient {
    /// `list_models()` — the live model listing (fallible, like Python).
    async fn list_models_inner(&self) -> Result<Vec<Value>, BackendError> {
        let headers = self.auth_headers(false).await?;
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
                let supported = obj
                    .get("supported_in_api")
                    .map(is_truthy)
                    .unwrap_or(true);
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
                    obj.get("default_reasoning_level").cloned().unwrap_or(Value::Null),
                );
                entry.insert(
                    "default_reasoning_summary".into(),
                    obj.get("default_reasoning_summary").cloned().unwrap_or(Value::Null),
                );
                entry.insert(
                    "supports_reasoning_summaries".into(),
                    obj.get("supports_reasoning_summaries").cloned().unwrap_or(Value::Null),
                );
                let levels = obj
                    .get("supported_reasoning_levels")
                    .filter(|v| is_truthy(v))
                    .or_else(|| obj.get("supported_reasoning_efforts").filter(|v| is_truthy(v)))
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
                models.push(Value::Object(entry));
            }
        }
        // models.sort(key=lambda m: (not m["supported"], -int(m["priority"] or 0), m["name"]))
        models.sort_by(|a, b| {
            let sa = a.get("supported").and_then(|v| v.as_bool()).unwrap_or(false);
            let sb = b.get("supported").and_then(|v| v.as_bool()).unwrap_or(false);
            // not supported sorts last: (!supported) ascending => supported first.
            (!sa)
                .cmp(&(!sb))
                .then_with(|| {
                    let pa = a.get("priority").and_then(|v| v.as_i64()).unwrap_or(0);
                    let pb = b.get("priority").and_then(|v| v.as_i64()).unwrap_or(0);
                    (-pa).cmp(&(-pb))
                })
                .then_with(|| {
                    let na = a.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let nb = b.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    na.cmp(nb)
                })
        });
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
        Ok(v) => Ok(if v.is_object() { v } else { Value::Object(Map::new()) }),
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
    fn prompt_cache_key_uses_configured_when_set() {
        let mut cfg = Config::from_env();
        cfg.codex_prompt_cache_key = "fixed-cache-key".to_string();
        let cfg = Arc::new(cfg);
        let settings = Arc::new(RuntimeSettings::new(
            &cfg,
            std::env::temp_dir().join("gml_codex_pck2.json"),
        ));
        let client = CodexClient::new(cfg, settings);
        assert_eq!(client.prompt_cache_key(), "fixed-cache-key");
        assert_ne!(client.prompt_cache_key(), client.thread_id());
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
        client.set_model("   ");
        assert_eq!(client.model(), orig);
        client.set_model("  gpt-test  ");
        assert_eq!(client.model(), "gpt-test");
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
        let p = client.payload(&messages, None, Some(false), None, Role::Gm.as_str());
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
        assert_eq!(p.pointer("/client_metadata/provider").unwrap(), "codex-oauth");
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
        let p = client.payload(&messages, None, Some(true), None, Role::Gm.as_str());
        assert!(p.get("reasoning").is_some());
        assert_eq!(
            p.get("include").unwrap(),
            &serde_json::json!(["reasoning.encrypted_content"])
        );
        // include empty when no reasoning (think=false)
        let p2 = client.payload(&messages, None, Some(false), None, Role::Gm.as_str());
        assert_eq!(p2.get("include").unwrap(), &serde_json::json!([]));
        assert!(p2.get("reasoning").is_none());
    }

    #[test]
    fn payload_schema_sets_text_format() {
        let cfg = Arc::new(Config::from_env());
        let settings = Arc::new(RuntimeSettings::new(
            &cfg,
            std::env::temp_dir().join("gml_codex_payload3.json"),
        ));
        let client = CodexClient::new(cfg, settings);
        let messages = serde_json::json!([{"role": "user", "content": "hi"}]);
        let schema = serde_json::json!({"type": "object", "properties": {}});
        let p = client.payload(&messages, None, Some(true), Some(&schema), Role::Gm.as_str());
        assert_eq!(p.pointer("/text/format/type").unwrap(), "json_schema");
        assert_eq!(p.pointer("/text/format/name").unwrap(), "gm_lab_json");
        assert_eq!(p.pointer("/text/format/strict").unwrap(), &Value::Bool(false));
        assert_eq!(p.pointer("/text/format/schema").unwrap(), &schema);
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
        assert_eq!(s.get("eval_duration").unwrap(), &Value::from(1_000_000_000i64));
        assert_eq!(s.get("total_duration").unwrap(), &Value::from(1_000_000_000i64));
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
