//! `OpenAICompatClient` — the OpenAI-compatible `/v1/chat/completions` client.
//!
//! Faithful port of `llm_client.OpenAICompatClient`. Works with local llama.cpp
//! and external OpenAI-compatible providers; llama.cpp-only request fields are
//! sent only when `config.USE_LLAMA_TEMPLATE_KWARGS` is enabled.
//!
//! Byte-fidelity focus: [`build_payload`] reproduces the EXACT request body key
//! set, ordering, and conditional gating from Python `_payload`, with numeric
//! types matched (e.g. `min_p` / `top_k` as integers, sampling floats as floats)
//! so the wire bytes — and thus the prompt-cache prefix — match Python. The
//! `preserve_order` serde feature keeps insertion order on serialization.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{Map, Value};

use gml_config::{Config, RuntimeSettings, SamplingPreset};

use crate::backend::{
    channel, Backend, BackendError, ChatOutput, ChatStreamOutput, DeltaSink, JsonStreamOutput,
};
use crate::json_helpers::{loads_map, parse_tool_calls};
use crate::parsing::{assistant_msg, clean, proper_nouns_line, stats, think};

/// The OpenAI-compatible chat client.
///
/// Holds the resolved chat/models URLs, the reqwest client (no read timeout on
/// streaming — mirrors httpx `read=None`), the active model id, a call log, and
/// the shared [`Config`] / [`RuntimeSettings`] used to build request bodies.
pub struct OpenAICompatClient {
    chat_url: String,
    models_url: String,
    http: reqwest::Client,
    model: Mutex<String>,
    /// `self.call_log` — appended on every call via `_remember`.
    call_log: Mutex<Vec<Map<String, Value>>>,
    /// `self._last_stream_elapsed_ms` — set after each stream completes.
    last_stream_elapsed_ms: Mutex<Option<f64>>,
    cfg: Arc<Config>,
    settings: Arc<RuntimeSettings>,
}

impl OpenAICompatClient {
    /// Build the client. Mirrors `OpenAICompatClient.__init__`:
    /// - base = `(API_BASE or LLAMA_HOST).rstrip("/")`;
    /// - if base ends with `/v1` -> append `/chat/completions` and `/models`,
    ///   else append `/v1/chat/completions` and `/v1/models`;
    /// - `Authorization: Bearer <API_KEY>` header when API_KEY is set;
    /// - httpx Timeout(connect=10, read=None, write=60, pool=None) — no read
    ///   timeout (essential for long streaming generations);
    /// - model = `MODEL or _detect_model(base)`.
    ///
    /// The model detect performs a blocking GET, so construction is async to
    /// match (Python did this synchronously in `__init__`).
    pub async fn new(cfg: Arc<Config>, settings: Arc<RuntimeSettings>) -> Self {
        let base = {
            let b = if !cfg.api_base.is_empty() {
                cfg.api_base.clone()
            } else {
                cfg.llama_host.clone()
            };
            b.trim_end_matches('/').to_string()
        };
        let (chat_url, models_url) = if base.ends_with("/v1") {
            (format!("{base}/chat/completions"), format!("{base}/models"))
        } else {
            (
                format!("{base}/v1/chat/completions"),
                format!("{base}/v1/models"),
            )
        };

        let mut builder = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            // No overall/read timeout: streaming reads must be allowed to run
            // indefinitely (httpx read=None). reqwest has no read timeout by
            // default, so we simply do not set `.timeout(...)`.
            .pool_idle_timeout(None);
        if !cfg.api_key.is_empty() {
            let mut headers = reqwest::header::HeaderMap::new();
            if let Ok(val) = reqwest::header::HeaderValue::from_str(&format!(
                "Bearer {}",
                cfg.api_key
            )) {
                headers.insert(reqwest::header::AUTHORIZATION, val);
            }
            builder = builder.default_headers(headers);
        }
        let http = builder.build().unwrap_or_else(|_| reqwest::Client::new());

        let model = if !cfg.model.is_empty() {
            cfg.model.clone()
        } else {
            detect_model(&http, &models_url).await
        };

        OpenAICompatClient {
            chat_url,
            models_url,
            http,
            model: Mutex::new(model),
            call_log: Mutex::new(Vec::new()),
            last_stream_elapsed_ms: Mutex::new(None),
            cfg,
            settings,
        }
    }

    /// A snapshot of the call log (`self.call_log`).
    pub fn call_log(&self) -> Vec<Map<String, Value>> {
        self.call_log.lock().expect("call_log lock").clone()
    }

    /// `_payload(...)` — build the request body. Public for testing the exact
    /// key set / order / gating. See [`build_payload`].
    pub fn payload(
        &self,
        messages: &Value,
        tools: Option<&Value>,
        think_flag: Option<bool>,
        response_format: Option<&Value>,
        stream: bool,
        reasoning_role: &str,
    ) -> Value {
        let model = self.model.lock().expect("model lock").clone();
        build_payload(
            &self.cfg,
            &self.settings,
            &model,
            messages,
            tools,
            think_flag,
            response_format,
            stream,
            reasoning_role,
        )
    }

    /// `_remember(label, usage, timings, elapsed_ms)` — normalize stats, append a
    /// call-log row, return the stats.
    ///
    /// Python:
    /// ```python
    /// def _remember(self, label, usage, timings, elapsed_ms=None):
    ///     if not timings and elapsed_ms is not None:
    ///         timings = {"prompt_ms": 0, "predicted_ms": elapsed_ms}
    ///     stats = _stats(usage, timings)
    ///     row = {"label": label, **stats,
    ///            "tokens": stats.get("prompt_eval_count", 0) + stats.get("eval_count", 0)}
    ///     self.call_log.append(row)
    ///     return stats
    /// ```
    fn remember(
        &self,
        label: &str,
        usage: Option<&Value>,
        timings: Option<&Value>,
        elapsed_ms: Option<f64>,
    ) -> Map<String, Value> {
        let synth;
        let timings_ref = if !is_truthy_opt(timings) {
            if let Some(ms) = elapsed_ms {
                synth = serde_json::json!({"prompt_ms": 0, "predicted_ms": ms});
                Some(&synth)
            } else {
                timings
            }
        } else {
            timings
        };
        let st = stats(usage, timings_ref);

        // row = {"label": label, **stats, "tokens": ...}
        let mut row = Map::new();
        row.insert("label".to_string(), Value::String(label.to_string()));
        for (k, v) in &st {
            row.insert(k.clone(), v.clone());
        }
        let prompt = st.get("prompt_eval_count").and_then(|v| v.as_i64()).unwrap_or(0);
        let eval = st.get("eval_count").and_then(|v| v.as_i64()).unwrap_or(0);
        row.insert("tokens".to_string(), Value::from(prompt + eval));
        self.call_log.lock().expect("call_log lock").push(row);
        st
    }

    /// `_post(payload)` — POST and parse JSON, attaching `_client_elapsed_ms`.
    async fn post(&self, payload: &Value) -> Result<Value, BackendError> {
        let t0 = Instant::now();
        let resp = self
            .http
            .post(&self.chat_url)
            .json(payload)
            .send()
            .await
            .map_err(BackendError::new)?;
        let resp = resp.error_for_status().map_err(BackendError::new)?;
        let mut data: Value = resp.json().await.map_err(BackendError::new)?;
        let elapsed = t0.elapsed().as_secs_f64() * 1000.0;
        if let Value::Object(ref mut m) = data {
            m.insert("_client_elapsed_ms".to_string(), Value::from(elapsed));
        }
        Ok(data)
    }

    /// `_stream(payload)` — POST with streaming, parse SSE `data:` lines, invoke
    /// `on_chunk` for each parsed JSON chunk. Sets `last_stream_elapsed_ms` at
    /// the end.
    ///
    /// Python:
    /// ```python
    /// with self._http.stream("POST", url, json=payload) as r:
    ///     r.raise_for_status()
    ///     for line in r.iter_lines():
    ///         if not line or not line.startswith("data:"):
    ///             continue
    ///         chunk = line[5:].strip()
    ///         if chunk == "[DONE]":
    ///             break
    ///         try:
    ///             yield json.loads(chunk)
    ///         except Exception:
    ///             continue
    /// self._last_stream_elapsed_ms = (time.perf_counter() - t0) * 1000
    /// ```
    async fn stream<F: FnMut(Value)>(
        &self,
        payload: &Value,
        mut on_chunk: F,
    ) -> Result<(), BackendError> {
        use futures_util::StreamExt;

        let t0 = Instant::now();
        let resp = self
            .http
            .post(&self.chat_url)
            .json(payload)
            .send()
            .await
            .map_err(BackendError::new)?;
        let resp = resp.error_for_status().map_err(BackendError::new)?;

        let mut stream = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();

        while let Some(item) = stream.next().await {
            let bytes = item.map_err(BackendError::new)?;
            buf.extend_from_slice(&bytes);
            // iter_lines() splits on line boundaries. Process complete lines.
            loop {
                let Some(pos) = buf.iter().position(|&b| b == b'\n') else {
                    break;
                };
                let mut line: Vec<u8> = buf.drain(..=pos).collect();
                // drop trailing \n and optional \r
                if line.last() == Some(&b'\n') {
                    line.pop();
                }
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                process_sse_line(&line, &mut on_chunk);
            }
        }
        // Process any trailing partial line without a newline (httpx iter_lines
        // would also yield a final line at EOF).
        if !buf.is_empty() {
            process_sse_line(&buf, &mut on_chunk);
        }

        *self.last_stream_elapsed_ms.lock().expect("elapsed lock") =
            Some(t0.elapsed().as_secs_f64() * 1000.0);
        Ok(())
    }
}

/// Decide whether an SSE line is a `data:` chunk and forward the parsed JSON.
/// Returns `true` if `[DONE]` was seen (caller may stop, though we keep reading
/// to drain — matching the practical effect of Python's `break`).
fn process_sse_line<F: FnMut(Value)>(line: &[u8], on_chunk: &mut F) {
    let Ok(text) = std::str::from_utf8(line) else {
        return;
    };
    // `if not line or not line.startswith("data:"): continue`
    if text.is_empty() || !text.starts_with("data:") {
        return;
    }
    // chunk = line[5:].strip()
    let chunk = text[5..].trim_matches(|c: char| c.is_whitespace());
    if chunk == "[DONE]" {
        return;
    }
    if let Ok(v) = serde_json::from_str::<Value>(chunk) {
        on_chunk(v);
    }
    // else: continue (skip malformed chunk)
}

/// `_detect_model(base)` — GET /v1/models, return `data[0].id`, else `"default"`.
async fn detect_model(http: &reqwest::Client, models_url: &str) -> String {
    let attempt = async {
        let resp = http
            .get(models_url)
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .ok()?;
        let data: Value = resp.json().await.ok()?;
        data.get("data")?
            .get(0)?
            .get("id")?
            .as_str()
            .map(|s| s.to_string())
    };
    attempt.await.unwrap_or_else(|| "default".to_string())
}

/// `_payload(...)` — free function so it can be unit-tested without a live
/// client. Reproduces the Python body byte-for-byte (key set, order, gating,
/// numeric types).
#[allow(clippy::too_many_arguments)]
pub fn build_payload(
    cfg: &Config,
    settings: &RuntimeSettings,
    model: &str,
    messages: &Value,
    tools: Option<&Value>,
    think_flag: Option<bool>,
    response_format: Option<&Value>,
    stream: bool,
    reasoning_role: &str,
) -> Value {
    let mut p = Map::new();
    // p = {"model": ..., "messages": ..., "stream": ...}
    p.insert("model".to_string(), Value::String(model.to_string()));
    p.insert("messages".to_string(), messages.clone());
    p.insert("stream".to_string(), Value::Bool(stream));

    // if config.PROMPT_CACHE_KEY: p["prompt_cache_key"] = ...
    if !cfg.prompt_cache_key.is_empty() {
        p.insert(
            "prompt_cache_key".to_string(),
            Value::String(cfg.prompt_cache_key.clone()),
        );
    }
    // if config.PROMPT_CACHE_RETENTION: p["prompt_cache_retention"] = ...
    if !cfg.prompt_cache_retention.is_empty() {
        p.insert(
            "prompt_cache_retention".to_string(),
            Value::String(cfg.prompt_cache_retention.clone()),
        );
    }

    // max_output_tokens = runtime_settings.max_output_tokens()
    let max_output_tokens = settings.max_output_tokens();
    if max_output_tokens > 0 {
        p.insert("max_tokens".to_string(), Value::from(max_output_tokens));
    }

    // if tools: ...
    let tools_truthy = matches!(tools, Some(Value::Array(a)) if !a.is_empty())
        || matches!(tools, Some(v) if is_truthy(v) && !v.is_array())
        || matches!(tools, Some(Value::Object(o)) if !o.is_empty());
    if tools_truthy {
        if let Some(t) = tools {
            p.insert("tools".to_string(), t.clone());
        }
        let tool_choice = settings.tool_choice_for_request(true);
        p.insert("tool_choice".to_string(), Value::String(tool_choice.clone()));
        // parallel_tool_calls = parallel_tool_calls_for_request(True) and tool_choice != "none"
        let parallel = settings.parallel_tool_calls_for_request(true) && tool_choice != "none";
        p.insert("parallel_tool_calls".to_string(), Value::Bool(parallel));
    }

    // if response_format: p["response_format"] = response_format
    if let Some(rf) = response_format {
        if is_truthy(rf) {
            p.insert("response_format".to_string(), rf.clone());
        }
    }

    // if think is not None: ...
    if let Some(think_val) = think_flag {
        // effective_think = runtime_settings.reasoning_enabled(think, reasoning_role)
        let effective_think = settings.reasoning_enabled(Some(think_val), reasoning_role);
        let sampling: SamplingPreset = if effective_think {
            gml_config::SAMPLING_THINK
        } else {
            gml_config::SAMPLING_PLAIN
        };
        if cfg.use_llama_template_kwargs {
            // p["chat_template_kwargs"] = {"enable_thinking": bool(effective_think)}
            let mut ctk = Map::new();
            ctk.insert(
                "enable_thinking".to_string(),
                Value::Bool(effective_think),
            );
            p.insert("chat_template_kwargs".to_string(), Value::Object(ctk));
            // p.update(sampling) — insert sampling fields in dict order:
            // temperature, top_p, top_k, min_p, presence_penalty (with exact types)
            insert_sampling(&mut p, &sampling);
            // if config.LLAMA_CACHE_REUSE > 0: p["n_cache_reuse"] = LLAMA_CACHE_REUSE
            if cfg.llama_cache_reuse > 0 {
                p.insert("n_cache_reuse".to_string(), Value::from(cfg.llama_cache_reuse));
            }
        } else {
            // Keep only widely-supported fields: temperature, top_p, presence_penalty
            for key in ["temperature", "top_p", "presence_penalty"] {
                if let Some(v) = sampling_field(&sampling, key) {
                    p.insert(key.to_string(), v);
                }
            }
        }
    }

    // if stream: p["stream_options"] = {"include_usage": True}
    if stream {
        let mut so = Map::new();
        so.insert("include_usage".to_string(), Value::Bool(true));
        p.insert("stream_options".to_string(), Value::Object(so));
    }

    Value::Object(p)
}

/// Insert the sampling preset fields in Python dict order with exact JSON number
/// types. `min_p` and `top_k` are integers in the Python dicts; the rest are
/// floats. Reproducing the numeric type matters for wire byte-identity.
fn insert_sampling(p: &mut Map<String, Value>, s: &SamplingPreset) {
    p.insert("temperature".to_string(), float_value(s.temperature));
    p.insert("top_p".to_string(), float_value(s.top_p));
    p.insert("top_k".to_string(), Value::from(s.top_k));
    p.insert("min_p".to_string(), min_p_value(s.min_p));
    p.insert("presence_penalty".to_string(), float_value(s.presence_penalty));
}

/// One sampling field by key, with exact numeric type (used for the
/// non-llama subset path).
fn sampling_field(s: &SamplingPreset, key: &str) -> Option<Value> {
    match key {
        "temperature" => Some(float_value(s.temperature)),
        "top_p" => Some(float_value(s.top_p)),
        "presence_penalty" => Some(float_value(s.presence_penalty)),
        _ => None,
    }
}

/// Serialize a float the way Python `json.dumps` would for these preset values
/// (e.g. `0.6`, `0.95`, `0.8`, `1.5`). serde_json's f64 formatting matches.
fn float_value(x: f64) -> Value {
    Value::from(x)
}

/// `min_p` is the Python int `0`. SamplingPreset stores it as f64 `0.0`; emit it
/// as an integer to match the Python wire bytes (`0`, not `0.0`).
fn min_p_value(x: f64) -> Value {
    if x.fract() == 0.0 && x.abs() < (i64::MAX as f64) {
        Value::from(x as i64)
    } else {
        Value::from(x)
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

fn is_truthy_opt(v: Option<&Value>) -> bool {
    v.map(is_truthy).unwrap_or(false)
}

#[async_trait]
impl Backend for OpenAICompatClient {
    fn model(&self) -> String {
        self.model.lock().expect("model lock").clone()
    }

    fn set_model(&self, model: &str) {
        let m = model.trim();
        if !m.is_empty() {
            *self.model.lock().expect("model lock") = m.to_string();
        }
    }

    async fn list_models(&self) -> Vec<Value> {
        let fallback = || {
            let m = self.model.lock().expect("model lock").clone();
            vec![serde_json::json!({"id": m, "name": m, "supported": true})]
        };
        let data: Value = match self
            .http
            .get(&self.models_url)
            .timeout(Duration::from_secs(10))
            .send()
            .await
        {
            Ok(r) => match r.error_for_status() {
                Ok(r) => match r.json().await {
                    Ok(v) => v,
                    Err(_) => return fallback(),
                },
                Err(_) => return fallback(),
            },
            Err(_) => return fallback(),
        };
        // raw_models = data.get("data") or data.get("models") or []
        let raw_models = data
            .get("data")
            .filter(|v| is_truthy(v))
            .or_else(|| data.get("models").filter(|v| is_truthy(v)));
        let mut models = Vec::new();
        if let Some(Value::Array(items)) = raw_models {
            for raw in items {
                let Some(obj) = raw.as_object() else {
                    continue;
                };
                // model_id = id or slug or model
                let model_id = obj
                    .get("id")
                    .filter(|v| is_truthy(v))
                    .or_else(|| obj.get("slug").filter(|v| is_truthy(v)))
                    .or_else(|| obj.get("model").filter(|v| is_truthy(v)))
                    .and_then(|v| v.as_str());
                if let Some(model_id) = model_id {
                    let name = obj
                        .get("name")
                        .filter(|v| is_truthy(v))
                        .and_then(|v| v.as_str())
                        .unwrap_or(model_id);
                    models.push(serde_json::json!({
                        "id": model_id,
                        "name": name,
                        "supported": true
                    }));
                }
            }
        }
        if models.is_empty() {
            fallback()
        } else {
            models
        }
    }

    async fn chat(
        &self,
        messages: &Value,
        tools: Option<&Value>,
        think_flag: Option<bool>,
        reasoning_role: &str,
    ) -> Result<ChatOutput, BackendError> {
        let payload = self.payload(messages, tools, think_flag, None, false, reasoning_role);
        let data = self.post(&payload).await?;
        self.remember(
            "chat",
            data.get("usage"),
            data.get("timings"),
            data.get("_client_elapsed_ms").and_then(|v| v.as_f64()),
        );
        let msg = data
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .cloned()
            .ok_or_else(|| BackendError::new("missing choices[0].message"))?;
        let raw = msg.get("tool_calls");
        let content = msg.get("content").and_then(|v| v.as_str()).unwrap_or("");
        Ok(ChatOutput {
            thinking: think(msg.get("reasoning_content").and_then(|v| v.as_str())),
            content: clean(content),
            calls: parse_tool_calls(raw),
            assistant_msg: assistant_msg(content, raw),
        })
    }

    async fn chat_json(
        &self,
        messages: &Value,
        _schema: &Value,
        think_flag: Option<bool>,
        reasoning_role: &str,
    ) -> Result<Map<String, Value>, BackendError> {
        // First attempt: free text, think as given.
        let payload = self.payload(messages, None, think_flag, None, false, reasoning_role);
        let data = self.post(&payload).await?;
        self.remember(
            "chat_json",
            data.get("usage"),
            data.get("timings"),
            data.get("_client_elapsed_ms").and_then(|v| v.as_f64()),
        );
        let content = data
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let out = loads_map(content);
        if !out.is_empty() {
            return Ok(out);
        }
        // Fallback: response_format json_object, think=False.
        let rf = serde_json::json!({"type": "json_object"});
        let payload2 = self.payload(messages, None, Some(false), Some(&rf), false, reasoning_role);
        let data2 = self.post(&payload2).await?;
        self.remember(
            "chat_json_fallback",
            data2.get("usage"),
            data2.get("timings"),
            data2.get("_client_elapsed_ms").and_then(|v| v.as_f64()),
        );
        let content2 = data2
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|v| v.as_str())
            .unwrap_or("{}");
        Ok(loads_map(content2))
    }

    async fn summarize(
        &self,
        text: &str,
        proper_nouns: &[String],
    ) -> Result<String, BackendError> {
        let sys = gml_prompts::render_gm_compact_system(&proper_nouns_line(proper_nouns));
        // text[:config.COMPACT_INPUT_CHARS] — clip by CHARS (Unicode scalars).
        let clipped: String = text
            .chars()
            .take(self.cfg.compact_input_chars.max(0) as usize)
            .collect();
        let messages = serde_json::json!([
            {"role": "system", "content": sys},
            {"role": "user", "content": clipped},
        ]);
        let out = self
            .chat(
                &messages,
                None,
                Some(true),
                gml_config::Role::Compact.as_str(),
            )
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
        let payload = self.payload(messages, tools, think_flag, None, true, reasoning_role);

        let mut t_parts: Vec<String> = Vec::new();
        let mut c_parts: Vec<String> = Vec::new();
        // tool_acc: index -> {id, name, args}
        let mut tool_acc: std::collections::BTreeMap<i64, ToolAcc> =
            std::collections::BTreeMap::new();
        let mut usage: Option<Value> = None;
        let mut timings: Option<Value> = None;

        self.stream(&payload, |obj| {
            if let Some(u) = obj.get("usage").filter(|v| is_truthy(v)) {
                usage = Some(u.clone());
            }
            if let Some(t) = obj.get("timings").filter(|v| is_truthy(v)) {
                timings = Some(t.clone());
            }
            let ch = obj.get("choices").and_then(|c| c.get(0));
            let Some(ch) = ch else {
                return;
            };
            let delta = ch.get("delta");
            let Some(delta) = delta else {
                return;
            };
            if let Some(t) = delta.get("reasoning_content").and_then(|v| v.as_str()) {
                if !t.is_empty() {
                    t_parts.push(t.to_string());
                    sink.emit(channel::THINKING, t);
                }
            }
            if let Some(c) = delta.get("content").and_then(|v| v.as_str()) {
                if !c.is_empty() {
                    c_parts.push(c.to_string());
                    sink.emit(channel::CONTENT, c);
                }
            }
            if let Some(Value::Array(tcs)) = delta.get("tool_calls") {
                for tc in tcs {
                    let idx = tc.get("index").and_then(|v| v.as_i64()).unwrap_or(0);
                    let acc = tool_acc.entry(idx).or_default();
                    if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                        if !id.is_empty() {
                            acc.id = id.to_string();
                        }
                    }
                    if let Some(func) = tc.get("function") {
                        if let Some(name) = func.get("name").and_then(|v| v.as_str()) {
                            if !name.is_empty() {
                                acc.name = name.to_string();
                            }
                        }
                        if let Some(args) = func.get("arguments").and_then(|v| v.as_str()) {
                            if !args.is_empty() {
                                acc.args.push_str(args);
                            }
                        }
                    }
                }
            }
        })
        .await?;

        // raw = [{id, type:"function", function:{name, arguments}} for ... if name]
        let raw: Vec<Value> = tool_acc
            .values()
            .filter(|a| !a.name.is_empty())
            .map(|a| {
                serde_json::json!({
                    "id": a.id,
                    "type": "function",
                    "function": {"name": a.name, "arguments": a.args},
                })
            })
            .collect();
        let raw_val = Value::Array(raw);

        let stats = self.remember(
            "chat_stream",
            usage.as_ref(),
            timings.as_ref(),
            *self.last_stream_elapsed_ms.lock().expect("elapsed lock"),
        );

        let content_joined: String = c_parts.concat();
        let thinking_joined: String = t_parts.concat();
        Ok(ChatStreamOutput {
            thinking: think(Some(&thinking_joined)),
            content: clean(&content_joined),
            calls: parse_tool_calls(Some(&raw_val)),
            assistant_msg: assistant_msg(&content_joined, Some(&raw_val)),
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
        let payload = self.payload(messages, None, think_flag, None, true, reasoning_role);

        let mut parts: Vec<String> = Vec::new();
        let mut usage: Option<Value> = None;
        let mut timings: Option<Value> = None;

        self.stream(&payload, |obj| {
            if let Some(u) = obj.get("usage").filter(|v| is_truthy(v)) {
                usage = Some(u.clone());
            }
            if let Some(t) = obj.get("timings").filter(|v| is_truthy(v)) {
                timings = Some(t.clone());
            }
            let ch = obj.get("choices").and_then(|c| c.get(0));
            if let Some(ch) = ch {
                if let Some(c) = ch
                    .get("delta")
                    .and_then(|d| d.get("content"))
                    .and_then(|v| v.as_str())
                {
                    if !c.is_empty() {
                        parts.push(c.to_string());
                        sink.emit(channel::CONTENT, c);
                    }
                }
            }
        })
        .await?;

        let joined: String = parts.concat();
        let mut data = loads_map(&joined);
        let stats = self.remember(
            "chat_json_stream",
            usage.as_ref(),
            timings.as_ref(),
            *self.last_stream_elapsed_ms.lock().expect("elapsed lock"),
        );
        // if not data: data = self.chat_json(messages, schema, reasoning_role=...)
        if data.is_empty() {
            // Python fallback passes think defaulted (True) — chat_json default
            // think=True; only reasoning_role is overridden.
            data = self.chat_json(messages, schema, Some(true), reasoning_role).await?;
        }
        Ok(JsonStreamOutput { data, stats })
    }
}

/// Accumulator for a streamed tool call (Python `tool_acc[index]`).
#[derive(Default)]
struct ToolAcc {
    id: String,
    name: String,
    args: String,
}
