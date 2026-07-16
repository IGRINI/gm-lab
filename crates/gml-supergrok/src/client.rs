use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, AUTHORIZATION, USER_AGENT};
use reqwest::{Client, Response, StatusCode};
use serde_json::{json, Map, Value};

use gml_llm::{
    Backend, BackendError, ChatOutput, ChatStreamOutput, DeltaSink, JsonStreamOutput,
    SessionIdentity,
};

use crate::oauth::{OAuthCredential, OAuthError, SuperGrokOAuth};
use crate::protocol::{attach_reasoning_items, build_request, raw_tool_calls};
use crate::stream::{StreamAccumulator, StreamDelta, StreamResult};
use crate::{SuperGrokConfig, DEFAULT_MODEL_ID};

const MAX_TRANSIENT_RETRIES: usize = 3;
const MODEL_CATALOG_MAX_TRANSIENT_RETRIES: usize = 1;
const MODEL_CATALOG_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const BASE_RETRY_DELAY_MS: u64 = 500;
const MAX_RETRY_JITTER_MS: u64 = 250;
const MAX_RETRY_AFTER_SECONDS: u64 = 10;

pub struct SuperGrokClient {
    config: Arc<SuperGrokConfig>,
    http: Client,
    oauth: SuperGrokOAuth,
    model: Mutex<String>,
    identity: SessionIdentity,
    call_log: Mutex<Vec<Map<String, Value>>>,
}

impl SuperGrokClient {
    pub fn new(config: Arc<SuperGrokConfig>) -> Result<Self, OAuthError> {
        let http = Self::build_http_client()?;
        Self::with_http(config, http)
    }

    pub(crate) fn build_http_client() -> Result<Client, OAuthError> {
        Client::builder()
            .connect_timeout(Duration::from_secs(15))
            // Endpoint pinning is meaningless if reqwest may follow a redirect
            // and forward OAuth/API credentials to another host.
            .redirect(reqwest::redirect::Policy::none())
            // Responses streams may legitimately run for minutes. Request-level
            // OAuth calls have their own timeout in SuperGrokOAuth.
            .pool_idle_timeout(None)
            .build()
            .map_err(|error| OAuthError::Transport(error.to_string()))
    }

    pub fn with_http(config: Arc<SuperGrokConfig>, http: Client) -> Result<Self, OAuthError> {
        let oauth = SuperGrokOAuth::with_http(config.clone(), http.clone())?;
        Ok(Self::from_parts(config, http, oauth))
    }

    pub(crate) fn from_parts(
        config: Arc<SuperGrokConfig>,
        http: Client,
        oauth: SuperGrokOAuth,
    ) -> Self {
        let model = config.model.trim();
        let model = if model.is_empty() {
            DEFAULT_MODEL_ID
        } else {
            model
        };
        Self {
            model: Mutex::new(model.to_string()),
            config,
            http,
            oauth,
            identity: SessionIdentity::new(),
            call_log: Mutex::new(Vec::new()),
        }
    }

    pub fn oauth(&self) -> &SuperGrokOAuth {
        &self.oauth
    }

    pub fn call_log(&self) -> Vec<Map<String, Value>> {
        self.call_log.lock().expect("call log lock").clone()
    }

    pub fn prompt_cache_key(&self) -> String {
        self.identity
            .prompt_cache_key(self.config.prompt_cache_key.trim())
    }

    pub fn payload(
        &self,
        messages: &Value,
        tools: Option<&Value>,
        json_mode: bool,
        reasoning_role: &str,
    ) -> Value {
        let prompt_cache_key = self.prompt_cache_key();
        let reasoning_scope = self.thread_id();
        self.payload_with_scope(
            messages,
            tools,
            json_mode,
            reasoning_role,
            &prompt_cache_key,
            &reasoning_scope,
        )
    }

    fn payload_with_scope(
        &self,
        messages: &Value,
        tools: Option<&Value>,
        json_mode: bool,
        reasoning_role: &str,
        prompt_cache_key: &str,
        reasoning_scope: &str,
    ) -> Value {
        build_request(
            &self.model_for_role(reasoning_role),
            messages,
            tools,
            json_mode,
            prompt_cache_key,
            reasoning_scope,
        )
    }

    fn model_for_role(&self, reasoning_role: &str) -> String {
        if reasoning_role == "compact" && !self.config.compact_model.trim().is_empty() {
            self.config.compact_model.trim().to_string()
        } else {
            self.model.lock().expect("model lock").clone()
        }
    }

    pub(crate) async fn list_models_inner(&self) -> Result<Vec<Value>, BackendError> {
        let response = self.send_models_request().await?;
        let body: Value = response.json().await.map_err(BackendError::new)?;
        let items = body
            .get("data")
            .or_else(|| body.get("models"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut models = Vec::new();
        for item in items {
            let Some(id) = item
                .get("id")
                .or_else(|| item.get("model"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty())
            else {
                continue;
            };
            if !is_responses_language_model(&item, id) {
                continue;
            }
            let name = item
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.trim().is_empty())
                .unwrap_or(id);
            let mut model = Map::new();
            model.insert("id".into(), Value::String(id.to_string()));
            model.insert("name".into(), Value::String(name.to_string()));
            model.insert("supported".into(), Value::Bool(true));
            if let Some(context) = item
                .get("context_window")
                .or_else(|| item.get("max_context_window"))
                .or_else(|| item.get("context_length"))
                .cloned()
            {
                model.insert("context_window".into(), context);
            }
            models.push(Value::Object(model));
        }
        Ok(models)
    }

    async fn send_models_request(&self) -> Result<Response, BackendError> {
        let mut rejected_access_token = None;
        for attempt in 0..2 {
            let auth_epoch = self.oauth.begin_request().map_err(BackendError::new)?;
            let credential = match rejected_access_token.as_deref() {
                Some(rejected) => self.oauth.refresh_after_unauthorized(rejected).await,
                None => self.oauth.ensure_fresh(false).await,
            }
            .map_err(BackendError::new)?;
            let url = self.config.models_url();
            let headers = self.headers(&credential, false)?;
            let response = self
                .send_with_transient_retry(
                    "model catalog",
                    MODEL_CATALOG_MAX_TRANSIENT_RETRIES,
                    auth_epoch,
                    || {
                        self.http
                            .get(&url)
                            .headers(headers.clone())
                            .timeout(MODEL_CATALOG_REQUEST_TIMEOUT)
                    },
                )
                .await?;
            if response.status() == StatusCode::UNAUTHORIZED && attempt == 0 {
                rejected_access_token = Some(credential.access_token);
                continue;
            }
            if !response.status().is_success() {
                return Err(provider_response_error(response).await);
            }
            return Ok(response);
        }
        Err(BackendError::new("SuperGrok authentication failed"))
    }

    async fn send_responses_request(
        &self,
        payload: &Value,
    ) -> Result<(Response, bool), BackendError> {
        let mut rejected_access_token = None;
        let mut authentication_retry_attempted = false;
        let mut reasoning_retry_attempted = false;
        let mut request_payload = payload.clone();
        loop {
            let auth_epoch = self.oauth.begin_request().map_err(BackendError::new)?;
            let credential = match rejected_access_token.as_deref() {
                Some(rejected) => self.oauth.refresh_after_unauthorized(rejected).await,
                None => self.oauth.ensure_fresh(false).await,
            }
            .map_err(BackendError::new)?;
            let url = self.config.responses_url();
            let headers = self.headers(&credential, true)?;
            let response = self
                .send_with_transient_retry("response", MAX_TRANSIENT_RETRIES, auth_epoch, || {
                    self.http
                        .post(&url)
                        .headers(headers.clone())
                        .json(&request_payload)
                })
                .await?;
            if response.status() == StatusCode::UNAUTHORIZED && !authentication_retry_attempted {
                authentication_retry_attempted = true;
                rejected_access_token = Some(credential.access_token);
                continue;
            }
            if response.status().is_success() {
                return Ok((response, reasoning_retry_attempted));
            }
            if response.status() == StatusCode::BAD_REQUEST
                && !reasoning_retry_attempted
                && has_reasoning_replay(&request_payload)
            {
                let status = response.status();
                let retry_after = retry_after_header(&response);
                let body = response.text().await.unwrap_or_default();
                if invalid_encrypted_content(status, &body) {
                    request_payload = without_reasoning_replay(&request_payload);
                    reasoning_retry_attempted = true;
                    continue;
                }
                return Err(provider_error_from_body(status, retry_after, &body));
            }
            return Err(provider_response_error(response).await);
        }
    }

    /// Retry failures that occur before an inference stream starts. Once this
    /// returns a successful response, the caller owns the SSE body and never
    /// replays it after observing an event.
    async fn send_with_transient_retry<F>(
        &self,
        operation: &'static str,
        max_retries: usize,
        auth_epoch: u64,
        mut request: F,
    ) -> Result<Response, BackendError>
    where
        F: FnMut() -> reqwest::RequestBuilder,
    {
        let mut retries = 0usize;
        loop {
            self.ensure_request_epoch(auth_epoch)?;
            let result = request().send().await;
            self.ensure_request_epoch(auth_epoch)?;
            match result {
                Ok(response) => {
                    let Some(delay) = response_retry_delay(&response, retries) else {
                        return Ok(response);
                    };
                    if retries >= max_retries {
                        return Ok(response);
                    }
                    tracing::warn!(
                        operation,
                        status = response.status().as_u16(),
                        retry = retries + 1,
                        delay_ms = delay.as_millis(),
                        "retrying transient SuperGrok response"
                    );
                    retries += 1;
                    tokio::time::sleep(delay).await;
                }
                Err(error) => {
                    if retries >= max_retries || !is_retryable_transport_error(&error) {
                        return Err(BackendError::new(error));
                    }
                    let delay = retry_backoff(retries, retry_jitter_ms());
                    tracing::warn!(
                        operation,
                        error = %error,
                        retry = retries + 1,
                        delay_ms = delay.as_millis(),
                        "retrying transient SuperGrok transport failure"
                    );
                    retries += 1;
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }

    fn ensure_request_epoch(&self, auth_epoch: u64) -> Result<(), BackendError> {
        if self.oauth.request_epoch_is_current(auth_epoch) {
            Ok(())
        } else {
            Err(BackendError::new(
                "SuperGrok authentication changed while the request was waiting; retry the action",
            ))
        }
    }

    fn headers(
        &self,
        credential: &OAuthCredential,
        stream: bool,
    ) -> Result<HeaderMap, BackendError> {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", credential.access_token.trim()))
                .map_err(BackendError::new)?,
        );
        headers.insert(
            USER_AGENT,
            HeaderValue::from_str(&self.config.user_agent).map_err(BackendError::new)?,
        );
        headers.insert(
            HeaderName::from_static("x-grok-conv-id"),
            HeaderValue::from_str(&self.identity.session_id()).map_err(BackendError::new)?,
        );
        if stream {
            headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        } else {
            headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        }
        Ok(headers)
    }

    async fn collect_stream(
        &self,
        payload: &Value,
        sink: &mut (dyn DeltaSink + Send),
        content_only: bool,
    ) -> Result<(StreamResult, f64), BackendError> {
        let started = Instant::now();
        let (response, reset_reasoning_before) = self.send_responses_request(payload).await?;
        let is_sse = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.to_ascii_lowercase().contains("text/event-stream"));
        if !is_sse {
            let response_value: Value = response.json().await.map_err(BackendError::new)?;
            let mut accumulator = StreamAccumulator::default();
            accumulator
                .handle(&json!({"type": "response.completed", "response": response_value}))
                .map_err(BackendError::new)?;
            let mut result = accumulator.finish();
            result.reset_reasoning_before = reset_reasoning_before;
            return Ok((result, started.elapsed().as_secs_f64() * 1_000.0));
        }

        let mut accumulator = StreamAccumulator::default();
        let mut stream = response.bytes_stream();
        let mut buffer = Vec::<u8>::new();
        let mut data_lines = Vec::<String>::new();
        let mut saw_event = false;
        let mut ended = false;

        'body: while let Some(chunk) = stream.next().await {
            buffer.extend_from_slice(&chunk.map_err(BackendError::new)?);
            while let Some(position) = buffer.iter().position(|byte| *byte == b'\n') {
                let raw = buffer.drain(..=position).collect::<Vec<_>>();
                let line = trim_sse_line(&raw);
                if handle_sse_line(
                    line,
                    &mut data_lines,
                    &mut accumulator,
                    sink,
                    content_only,
                    &mut saw_event,
                )? {
                    ended = true;
                    break 'body;
                }
            }
        }
        if !ended && !buffer.is_empty() {
            let line = trim_sse_line(&buffer);
            ended = handle_sse_line(
                line,
                &mut data_lines,
                &mut accumulator,
                sink,
                content_only,
                &mut saw_event,
            )?;
        }
        if !ended && !data_lines.is_empty() {
            ended = flush_sse_event(
                &mut data_lines,
                &mut accumulator,
                sink,
                content_only,
                &mut saw_event,
            )?;
        }
        let _ = ended;
        if !saw_event {
            return Err(BackendError::new(
                "SuperGrok returned an empty Responses stream",
            ));
        }
        if !ended {
            return Err(BackendError::new(
                "SuperGrok Responses stream ended before completion",
            ));
        }
        let mut result = accumulator.finish();
        result.reset_reasoning_before = reset_reasoning_before;
        Ok((result, started.elapsed().as_secs_f64() * 1_000.0))
    }

    fn remember(&self, label: &str, usage: Option<&Value>, elapsed_ms: f64) -> Map<String, Value> {
        let stats = usage_stats(usage, elapsed_ms);
        let mut row = stats.clone();
        row.insert("label".into(), Value::String(label.to_string()));
        let tokens = row
            .get("prompt_eval_count")
            .and_then(Value::as_i64)
            .unwrap_or(0)
            + row.get("eval_count").and_then(Value::as_i64).unwrap_or(0);
        row.insert("tokens".into(), Value::from(tokens));
        self.call_log.lock().expect("call log lock").push(row);
        stats
    }
}

#[async_trait]
impl Backend for SuperGrokClient {
    fn connector_id(&self) -> &str {
        "xai"
    }

    fn model(&self) -> String {
        self.model.lock().expect("model lock").clone()
    }

    fn set_model(&self, model: &str) {
        let model = model.trim();
        if model.is_empty() {
            return;
        }
        let changed = {
            let mut current = self.model.lock().expect("model lock");
            if *current == model {
                false
            } else {
                *current = model.to_string();
                true
            }
        };
        if changed {
            self.identity.reset_cache_scope();
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
        SuperGrokClient::prompt_cache_key(self)
    }

    async fn list_models(&self) -> Vec<Value> {
        match self.list_models_inner().await {
            Ok(models) if !models.is_empty() => models,
            _ => {
                let model = self.model();
                vec![json!({"id": model, "name": model, "supported": true})]
            }
        }
    }

    async fn chat(
        &self,
        messages: &Value,
        tools: Option<&Value>,
        _think: Option<bool>,
        reasoning_role: &str,
    ) -> Result<ChatOutput, BackendError> {
        let prompt_cache_key = self.prompt_cache_key();
        let reasoning_scope = self.thread_id();
        let payload = self.payload_with_scope(
            messages,
            tools,
            false,
            reasoning_role,
            &prompt_cache_key,
            &reasoning_scope,
        );
        let mut sink = gml_llm::NullSink;
        let (result, elapsed_ms) = self.collect_stream(&payload, &mut sink, false).await?;
        self.remember("chat", result.usage.as_ref(), elapsed_ms);
        Ok(chat_output(result, &reasoning_scope))
    }

    async fn chat_json(
        &self,
        messages: &Value,
        _think: Option<bool>,
        reasoning_role: &str,
    ) -> Result<Map<String, Value>, BackendError> {
        let payload = self.payload(messages, None, true, reasoning_role);
        let mut sink = gml_llm::NullSink;
        let (result, elapsed_ms) = self.collect_stream(&payload, &mut sink, true).await?;
        self.remember("chat_json", result.usage.as_ref(), elapsed_ms);
        Ok(object_from_text(&result.content))
    }

    async fn summarize(&self, text: &str, proper_nouns: &[String]) -> Result<String, BackendError> {
        let noun_rule = gml_prompts::gm_compact_connector_proper_nouns_line(proper_nouns);
        let system = gml_prompts::render_gm_compact_system(&noun_rule);
        let clipped = text
            .chars()
            .take(self.config.compact_input_chars)
            .collect::<String>();
        let output = self
            .chat(
                &json!([
                    {"role": "system", "content": system},
                    {"role": "user", "content": clipped}
                ]),
                None,
                Some(true),
                "compact",
            )
            .await?;
        Ok(output.content.trim().to_string())
    }

    async fn chat_stream(
        &self,
        messages: &Value,
        tools: Option<&Value>,
        _think: Option<bool>,
        reasoning_role: &str,
        sink: &mut (dyn DeltaSink + Send),
    ) -> Result<ChatStreamOutput, BackendError> {
        let prompt_cache_key = self.prompt_cache_key();
        let reasoning_scope = self.thread_id();
        let payload = self.payload_with_scope(
            messages,
            tools,
            false,
            reasoning_role,
            &prompt_cache_key,
            &reasoning_scope,
        );
        let (result, elapsed_ms) = self.collect_stream(&payload, sink, false).await?;
        let stats = self.remember("chat_stream", result.usage.as_ref(), elapsed_ms);
        let output = chat_output(result, &reasoning_scope);
        Ok(ChatStreamOutput {
            thinking: output.thinking,
            content: output.content,
            calls: output.calls,
            assistant_msg: output.assistant_msg,
            stats,
        })
    }

    async fn chat_json_stream(
        &self,
        messages: &Value,
        _think: Option<bool>,
        reasoning_role: &str,
        sink: &mut (dyn DeltaSink + Send),
    ) -> Result<JsonStreamOutput, BackendError> {
        let payload = self.payload(messages, None, true, reasoning_role);
        let (result, elapsed_ms) = self.collect_stream(&payload, sink, true).await?;
        let stats = self.remember("chat_json_stream", result.usage.as_ref(), elapsed_ms);
        Ok(JsonStreamOutput {
            data: object_from_text(&result.content),
            stats,
        })
    }
}

fn chat_output(result: StreamResult, reasoning_scope: &str) -> ChatOutput {
    let raw = raw_tool_calls(&result.calls);
    let raw_value = Value::Array(raw);
    let mut assistant_msg = gml_llm::assistant_msg(&result.content, Some(&raw_value));
    attach_reasoning_items(
        &mut assistant_msg,
        reasoning_scope,
        &result.reasoning_items,
        result.reset_reasoning_before,
    );
    ChatOutput {
        thinking: gml_llm::think(Some(&result.thinking)),
        content: gml_llm::clean(&result.content),
        calls: gml_llm::parse_tool_calls(Some(&raw_value)),
        assistant_msg,
    }
}

fn object_from_text(text: &str) -> Map<String, Value> {
    gml_llm::loads_value(text)
        .as_object()
        .cloned()
        .unwrap_or_default()
}

fn trim_sse_line(raw: &[u8]) -> &str {
    let raw = raw.strip_suffix(b"\n").unwrap_or(raw);
    let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
    std::str::from_utf8(raw).unwrap_or_default()
}

fn handle_sse_line(
    line: &str,
    data_lines: &mut Vec<String>,
    accumulator: &mut StreamAccumulator,
    sink: &mut (dyn DeltaSink + Send),
    content_only: bool,
    saw_event: &mut bool,
) -> Result<bool, BackendError> {
    if line.is_empty() {
        return flush_sse_event(data_lines, accumulator, sink, content_only, saw_event);
    }
    if let Some(data) = line.strip_prefix("data:") {
        data_lines.push(data.strip_prefix(' ').unwrap_or(data).to_string());
    }
    Ok(false)
}

fn flush_sse_event(
    data_lines: &mut Vec<String>,
    accumulator: &mut StreamAccumulator,
    sink: &mut (dyn DeltaSink + Send),
    content_only: bool,
    saw_event: &mut bool,
) -> Result<bool, BackendError> {
    if data_lines.is_empty() {
        return Ok(false);
    }
    let payload = data_lines.join("\n");
    data_lines.clear();
    if payload == "[DONE]" {
        if accumulator.done {
            return Ok(true);
        }
        return Err(BackendError::new(
            "SuperGrok ended the Responses stream before response.completed",
        ));
    }
    let event: Value = serde_json::from_str(&payload).map_err(|error| {
        BackendError::new(format!("SuperGrok returned invalid SSE JSON: {error}"))
    })?;
    if !event.is_object() {
        return Err(BackendError::new(
            "SuperGrok returned a non-object SSE event",
        ));
    }
    *saw_event = true;
    for delta in accumulator.handle(&event).map_err(BackendError::new)? {
        match delta {
            StreamDelta::Thinking(text) if !content_only => {
                sink.emit(gml_llm::channel::THINKING, &text)
            }
            StreamDelta::Content(text) => sink.emit(gml_llm::channel::CONTENT, &text),
            StreamDelta::Thinking(_) => {}
        }
    }
    Ok(accumulator.done)
}

fn has_reasoning_replay(payload: &Value) -> bool {
    payload
        .get("input")
        .and_then(Value::as_array)
        .is_some_and(|items| {
            items
                .iter()
                .any(|item| item.get("type").and_then(Value::as_str) == Some("reasoning"))
        })
}

fn without_reasoning_replay(payload: &Value) -> Value {
    let mut payload = payload.clone();
    let Some(input) = payload.get_mut("input").and_then(Value::as_array_mut) else {
        return payload;
    };
    input.retain(|item| item.get("type").and_then(Value::as_str) != Some("reasoning"));
    payload
}

fn invalid_encrypted_content(status: StatusCode, body: &str) -> bool {
    if status != StatusCode::BAD_REQUEST {
        return false;
    }
    let normalized = body.to_ascii_lowercase();
    normalized.contains("invalid_encrypted_content")
        || (normalized.contains("encrypted content for item")
            && normalized.contains("could not be verified"))
        || normalized.contains("could not decrypt the provided encrypted_content")
}

fn retry_after_header(response: &Response) -> Option<String> {
    response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

fn is_retryable_gateway_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::BAD_GATEWAY | StatusCode::SERVICE_UNAVAILABLE | StatusCode::GATEWAY_TIMEOUT
    ) || status.as_u16() == 529
}

fn is_retryable_transport_error(error: &reqwest::Error) -> bool {
    error.is_connect() || error.is_timeout() || error.is_request()
}

fn response_retry_delay(response: &Response, retry: usize) -> Option<Duration> {
    retry_delay(
        response.status(),
        retry_after_header(response).as_deref(),
        retry,
        retry_jitter_ms(),
    )
}

fn retry_delay(
    status: StatusCode,
    retry_after: Option<&str>,
    retry: usize,
    jitter_ms: u64,
) -> Option<Duration> {
    if status == StatusCode::TOO_MANY_REQUESTS {
        return bounded_retry_after(retry_after);
    }
    if !is_retryable_gateway_status(status) {
        return None;
    }
    match retry_after {
        Some(value) => bounded_retry_after(Some(value)),
        None => Some(retry_backoff(retry, jitter_ms)),
    }
}

fn bounded_retry_after(value: Option<&str>) -> Option<Duration> {
    let seconds = value?.trim().parse::<u64>().ok()?;
    (seconds <= MAX_RETRY_AFTER_SECONDS).then(|| Duration::from_secs(seconds))
}

fn retry_backoff(retry: usize, jitter_ms: u64) -> Duration {
    let multiplier = 1u64 << retry.min(2);
    let base_ms = BASE_RETRY_DELAY_MS.saturating_mul(multiplier);
    Duration::from_millis(base_ms.saturating_add(jitter_ms.min(MAX_RETRY_JITTER_MS)))
}

fn retry_jitter_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64
        % (MAX_RETRY_JITTER_MS + 1)
}

async fn provider_response_error(response: Response) -> BackendError {
    let status = response.status();
    let retry_after = retry_after_header(&response);
    let body = response.text().await.unwrap_or_default();
    provider_error_from_body(status, retry_after, &body)
}

fn provider_error_from_body(
    status: StatusCode,
    retry_after: Option<String>,
    body: &str,
) -> BackendError {
    let message = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("error")
                .and_then(|error| {
                    error
                        .get("message")
                        .and_then(Value::as_str)
                        .or_else(|| error.as_str())
                })
                .or_else(|| value.get("message").and_then(Value::as_str))
                .map(str::to_string)
        })
        .unwrap_or_else(|| body.split_whitespace().collect::<Vec<_>>().join(" "));
    let message = message.chars().take(2_000).collect::<String>();
    let suffix = retry_after
        .map(|value| format!("; retry after {value}"))
        .unwrap_or_default();
    BackendError::new(format!(
        "SuperGrok API error {}: {}{}",
        status.as_u16(),
        message,
        suffix
    ))
}

fn usage_stats(usage: Option<&Value>, elapsed_ms: f64) -> Map<String, Value> {
    let usage = usage.cloned().unwrap_or_else(|| json!({}));
    let input = integer_field(&usage, &["input_tokens", "prompt_tokens"]);
    let output = integer_field(&usage, &["output_tokens", "completion_tokens"]);
    let cached = usage
        .get("input_tokens_details")
        .or_else(|| usage.get("prompt_tokens_details"))
        .and_then(|details| details.get("cached_tokens"))
        .and_then(|value| value.as_i64())
        .unwrap_or(0);
    let elapsed_ns = (elapsed_ms * 1_000_000.0).max(0.0) as i64;
    Map::from_iter([
        ("prompt_eval_count".to_string(), Value::from(input)),
        ("eval_count".to_string(), Value::from(output)),
        ("cached_tokens".to_string(), Value::from(cached)),
        ("prompt_eval_duration".to_string(), Value::from(0)),
        ("eval_duration".to_string(), Value::from(elapsed_ns)),
        ("total_duration".to_string(), Value::from(elapsed_ns)),
        ("load_duration".to_string(), Value::from(0)),
    ])
}

fn integer_field(value: &Value, keys: &[&str]) -> i64 {
    keys.iter()
        .find_map(|key| {
            value
                .get(*key)
                .and_then(|value| value.as_i64().or_else(|| value.as_f64().map(|v| v as i64)))
        })
        .unwrap_or(0)
}

fn is_responses_language_model(item: &Value, id: &str) -> bool {
    if let Some(supported) = item.get("supports_responses").and_then(Value::as_bool) {
        return supported;
    }
    if let Some(output_modalities) = item.get("output_modalities").and_then(Value::as_array) {
        return output_modalities.iter().any(|modality| {
            modality
                .as_str()
                .is_some_and(|value| value.eq_ignore_ascii_case("text"))
        });
    }

    let normalized = id.to_ascii_lowercase();
    let explicitly_non_text = [
        "imagine",
        "image",
        "video",
        "embedding",
        "embed",
        "audio",
        "voice",
    ]
    .iter()
    .any(|marker| normalized.contains(marker));
    if explicitly_non_text {
        return false;
    }

    item.get("completion_text_token_price")
        .is_some_and(Value::is_number)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn payload_uses_model_cache_key_and_xai_storage_policy() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = SuperGrokConfig::new(dir.path().join("auth.json"));
        config.model = "grok-test".to_string();
        let client = SuperGrokClient::new(Arc::new(config)).unwrap();
        client.set_session_identity(Some("conversation"), Some("cache-scope"));
        let payload = client.payload(
            &json!([{"role":"user","content":"hello"}]),
            None,
            false,
            "gm",
        );
        assert_eq!(payload["model"], "grok-test");
        assert_eq!(payload["prompt_cache_key"], "cache-scope");
        assert_eq!(payload["store"], false);
    }

    #[test]
    fn model_change_rotates_provider_cache_identity() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = SuperGrokConfig::new(dir.path().join("auth.json"));
        config.prompt_cache_key = "configured-namespace".to_string();
        let client = SuperGrokClient::new(Arc::new(config)).unwrap();
        client.set_session_identity(Some("conversation"), Some("thread"));
        let original_cache_key = client.prompt_cache_key();
        client.set_model(" grok-next ");
        assert_eq!(client.model(), "grok-next");
        assert_ne!(client.session_id(), "conversation");
        assert_ne!(client.thread_id(), "thread");
        assert_ne!(client.prompt_cache_key(), original_cache_key);
    }

    #[test]
    fn sse_frame_parser_handles_multiline_data_and_done() {
        struct Sink(Vec<(String, String)>);
        impl DeltaSink for Sink {
            fn emit(&mut self, channel: &str, delta: &str) {
                self.0.push((channel.to_string(), delta.to_string()));
            }
        }
        let mut lines = Vec::new();
        let mut accumulator = StreamAccumulator::default();
        let mut sink = Sink(Vec::new());
        let mut saw = false;
        handle_sse_line(
            "data: {\"type\":\"response.output_text.delta\",",
            &mut lines,
            &mut accumulator,
            &mut sink,
            false,
            &mut saw,
        )
        .unwrap();
        handle_sse_line(
            "data: \"delta\":\"hi\"}",
            &mut lines,
            &mut accumulator,
            &mut sink,
            false,
            &mut saw,
        )
        .unwrap();
        assert!(
            !handle_sse_line("", &mut lines, &mut accumulator, &mut sink, false, &mut saw,)
                .unwrap()
        );
        assert_eq!(sink.0[0].1, "hi");
        lines.push("[DONE]".to_string());
        let error =
            flush_sse_event(&mut lines, &mut accumulator, &mut sink, false, &mut saw).unwrap_err();
        assert!(error.to_string().contains("before response.completed"));

        accumulator
            .handle(&json!({"type":"response.completed","response":{}}))
            .unwrap();
        lines.push("[DONE]".to_string());
        assert!(
            flush_sse_event(&mut lines, &mut accumulator, &mut sink, false, &mut saw,).unwrap()
        );
    }

    #[test]
    fn model_catalog_excludes_non_text_models() {
        assert!(is_responses_language_model(
            &json!({"completion_text_token_price": 0.00001}),
            "grok-4"
        ));
        assert!(is_responses_language_model(
            &json!({"output_modalities":["text"]}),
            "future-model"
        ));
        assert!(!is_responses_language_model(
            &json!({"output_modalities":["image"]}),
            "grok-imagine-image"
        ));
        assert!(!is_responses_language_model(
            &json!({"supports_responses":false,"output_modalities":["text"]}),
            "disabled-model"
        ));
        assert!(!is_responses_language_model(
            &json!({"image_price": 0.02}),
            "grok-imagine-image"
        ));
        assert!(!is_responses_language_model(&json!({}), "unknown-model"));
    }

    #[test]
    fn invalid_reasoning_recovery_is_narrow_and_one_shot_ready() {
        let payload = json!({
            "input": [
                {"type":"reasoning","encrypted_content":"stale","summary":[]},
                {"type":"message","role":"user","content":[]}
            ]
        });
        assert!(has_reasoning_replay(&payload));
        assert!(invalid_encrypted_content(
            StatusCode::BAD_REQUEST,
            r#"{"error":{"code":"invalid_encrypted_content","message":"could not verify"}}"#
        ));
        assert!(!invalid_encrypted_content(
            StatusCode::INTERNAL_SERVER_ERROR,
            "invalid_encrypted_content"
        ));

        let retry = without_reasoning_replay(&payload);
        assert!(!has_reasoning_replay(&retry));
        assert_eq!(retry["input"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn chat_output_keeps_connector_reasoning_state_private() {
        let output = chat_output(
            StreamResult {
                thinking: String::new(),
                content: "answer".to_string(),
                calls: Vec::new(),
                reasoning_items: vec![json!({
                    "type":"reasoning",
                    "id":"provider-item",
                    "encrypted_content":"secret"
                })],
                reset_reasoning_before: false,
                usage: None,
            },
            "thread",
        );
        let state = &output.assistant_msg[crate::protocol::XAI_REASONING_STATE_FIELD];
        assert_eq!(state["v"], 1);
        assert_eq!(state["scope"], "thread");
        assert_eq!(state["reset_before"], false);
        assert_eq!(state["items"][0]["encrypted_content"], "secret");
        assert!(state["items"][0].get("id").is_none());
    }

    #[test]
    fn recovery_output_persists_a_reset_boundary_without_new_reasoning() {
        let output = chat_output(
            StreamResult {
                thinking: String::new(),
                content: "answer".to_string(),
                calls: Vec::new(),
                reasoning_items: Vec::new(),
                reset_reasoning_before: true,
                usage: None,
            },
            "thread",
        );
        let state = &output.assistant_msg[crate::protocol::XAI_REASONING_STATE_FIELD];
        assert_eq!(state["reset_before"], true);
        assert_eq!(state["items"], json!([]));
    }

    #[test]
    fn usage_normalization_preserves_cached_tokens() {
        let stats = usage_stats(
            Some(&json!({
                "input_tokens": 10,
                "output_tokens": 4,
                "input_tokens_details": {"cached_tokens": 6}
            })),
            12.5,
        );
        assert_eq!(stats["prompt_eval_count"], 10);
        assert_eq!(stats["eval_count"], 4);
        assert_eq!(stats["cached_tokens"], 6);
        assert_eq!(stats["total_duration"], 12_500_000);
    }

    #[test]
    fn transient_retry_policy_is_bounded() {
        assert!(is_retryable_gateway_status(StatusCode::BAD_GATEWAY));
        assert!(is_retryable_gateway_status(StatusCode::SERVICE_UNAVAILABLE));
        assert!(is_retryable_gateway_status(StatusCode::GATEWAY_TIMEOUT));
        assert!(is_retryable_gateway_status(
            StatusCode::from_u16(529).unwrap()
        ));
        assert!(!is_retryable_gateway_status(StatusCode::BAD_REQUEST));

        assert_eq!(bounded_retry_after(Some("3")), Some(Duration::from_secs(3)));
        assert_eq!(bounded_retry_after(Some("11")), None);
        assert_eq!(bounded_retry_after(Some("not-a-number")), None);
        assert_eq!(
            retry_delay(StatusCode::SERVICE_UNAVAILABLE, None, 0, 0),
            Some(Duration::from_millis(500))
        );
        assert_eq!(
            retry_delay(StatusCode::SERVICE_UNAVAILABLE, Some("3"), 0, 0),
            Some(Duration::from_secs(3))
        );
        assert_eq!(
            retry_delay(StatusCode::SERVICE_UNAVAILABLE, Some("11"), 0, 0),
            None
        );
        assert_eq!(
            retry_delay(
                StatusCode::SERVICE_UNAVAILABLE,
                Some("Wed, 21 Oct 2030 07:28:00 GMT"),
                0,
                0
            ),
            None
        );
        assert_eq!(retry_delay(StatusCode::TOO_MANY_REQUESTS, None, 0, 0), None);
        assert_eq!(retry_backoff(0, 0), Duration::from_millis(500));
        assert_eq!(retry_backoff(1, 0), Duration::from_millis(1_000));
        assert_eq!(retry_backoff(2, 250), Duration::from_millis(2_250));
        assert_eq!(retry_backoff(9, 999), Duration::from_millis(2_250));
    }

    #[tokio::test]
    async fn transient_response_is_retried_before_streaming_starts() {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for response in [
                "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nRetry-After: 0\r\nConnection: close\r\n\r\n",
                "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK",
            ] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = [0u8; 1_024];
                let _ = socket.read(&mut request).await.unwrap();
                socket.write_all(response.as_bytes()).await.unwrap();
                socket.shutdown().await.unwrap();
            }
        });

        let directory = tempfile::tempdir().unwrap();
        let client = SuperGrokClient::new(Arc::new(SuperGrokConfig::new(
            directory.path().join("auth.json"),
        )))
        .unwrap();
        let url = format!("http://{address}/probe");
        let auth_epoch = client.oauth.begin_request().unwrap();
        let response = client
            .send_with_transient_retry("test", MAX_TRANSIENT_RETRIES, auth_epoch, || {
                client.http.get(&url)
            })
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn logout_stops_retries_before_another_bearer_request() {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 1_024];
            let _ = socket.read(&mut request).await.unwrap();
            socket
                .write_all(
                    b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nRetry-After: 1\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
            socket.shutdown().await.unwrap();
        });

        let directory = tempfile::tempdir().unwrap();
        let client = SuperGrokClient::new(Arc::new(SuperGrokConfig::new(
            directory.path().join("auth.json"),
        )))
        .unwrap();
        let url = format!("http://{address}/probe");
        let auth_epoch = client.oauth.begin_request().unwrap();
        let request =
            client.send_with_transient_retry("test", MAX_TRANSIENT_RETRIES, auth_epoch, || {
                client.http.get(&url)
            });
        let logout = async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            client.oauth.logout().await.unwrap();
        };
        let (result, ()) = tokio::join!(request, logout);

        assert!(result
            .unwrap_err()
            .to_string()
            .contains("authentication changed"));
        server.await.unwrap();
    }
}
