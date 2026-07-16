//! Provider-neutral injection of the selected response language.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Map, Value};

use gml_prompts::{response_language_instruction, RESPONSE_LANGUAGE_INSTRUCTION_PREFIX};

use crate::{Backend, BackendError, ChatOutput, ChatStreamOutput, DeltaSink, JsonStreamOutput};

/// Runtime source used to snapshot the selected language once per model call.
pub trait ResponseLanguageSource: Send + Sync {
    fn response_language(&self) -> String;
}

impl<F> ResponseLanguageSource for F
where
    F: Fn() -> String + Send + Sync,
{
    fn response_language(&self) -> String {
        self()
    }
}

/// Add one final leading system instruction without mutating persisted history.
///
/// A previous synthetic instruction is replaced defensively, so wrapping a
/// backend more than once cannot duplicate the rule. The original value is
/// never modified.
pub fn messages_with_response_language(messages: &Value, language_tag: &str) -> Value {
    let Value::Array(items) = messages else {
        return messages.clone();
    };

    let mut localized: Vec<Value> = items
        .iter()
        .filter(|message| !is_response_language_instruction(message))
        .cloned()
        .collect();
    let insert_at = localized
        .iter()
        .take_while(|message| message.get("role").and_then(Value::as_str) == Some("system"))
        .count();
    localized.insert(
        insert_at,
        json!({
            "role": "system",
            "content": response_language_instruction(language_tag),
        }),
    );
    Value::Array(localized)
}

fn is_response_language_instruction(message: &Value) -> bool {
    message.get("role").and_then(Value::as_str) == Some("system")
        && message
            .get("content")
            .and_then(Value::as_str)
            .is_some_and(|content| content.starts_with(RESPONSE_LANGUAGE_INSTRUCTION_PREFIX))
}

/// Backend decorator applied by the connector registry.
///
/// Connectors stay responsible only for provider transport. The application
/// core owns the language policy, while every current and future connector
/// receives the same already-localized message list.
pub struct ResponseLanguageBackend {
    inner: Arc<dyn Backend>,
    source: Arc<dyn ResponseLanguageSource>,
}

impl ResponseLanguageBackend {
    pub fn new(inner: Arc<dyn Backend>, source: Arc<dyn ResponseLanguageSource>) -> Self {
        Self { inner, source }
    }

    fn localize(&self, messages: &Value) -> Value {
        let language = self.source.response_language();
        messages_with_response_language(messages, &language)
    }
}

#[async_trait]
impl Backend for ResponseLanguageBackend {
    fn connector_id(&self) -> &str {
        self.inner.connector_id()
    }

    fn model(&self) -> String {
        self.inner.model()
    }

    fn supports_native_tool_search(&self) -> bool {
        self.inner.supports_native_tool_search()
    }

    fn set_model(&self, model: &str) {
        self.inner.set_model(model);
    }

    fn set_session_identity(&self, session_id: Option<&str>, thread_id: Option<&str>) {
        self.inner.set_session_identity(session_id, thread_id);
    }

    fn session_id(&self) -> String {
        self.inner.session_id()
    }

    fn thread_id(&self) -> String {
        self.inner.thread_id()
    }

    fn prompt_cache_key(&self) -> String {
        self.inner.prompt_cache_key()
    }

    async fn list_models(&self) -> Vec<Value> {
        self.inner.list_models().await
    }

    async fn chat(
        &self,
        messages: &Value,
        tools: Option<&Value>,
        think: Option<bool>,
        reasoning_role: &str,
    ) -> Result<ChatOutput, BackendError> {
        self.inner
            .chat(&self.localize(messages), tools, think, reasoning_role)
            .await
    }

    async fn chat_json(
        &self,
        messages: &Value,
        think: Option<bool>,
        reasoning_role: &str,
    ) -> Result<Map<String, Value>, BackendError> {
        self.inner
            .chat_json(&self.localize(messages), think, reasoning_role)
            .await
    }

    async fn summarize(&self, text: &str, proper_nouns: &[String]) -> Result<String, BackendError> {
        // Compaction summaries are internal context, not user-visible output.
        self.inner.summarize(text, proper_nouns).await
    }

    async fn chat_stream(
        &self,
        messages: &Value,
        tools: Option<&Value>,
        think: Option<bool>,
        reasoning_role: &str,
        sink: &mut (dyn DeltaSink + Send),
    ) -> Result<ChatStreamOutput, BackendError> {
        self.inner
            .chat_stream(&self.localize(messages), tools, think, reasoning_role, sink)
            .await
    }

    async fn chat_json_stream(
        &self,
        messages: &Value,
        think: Option<bool>,
        reasoning_role: &str,
        sink: &mut (dyn DeltaSink + Send),
    ) -> Result<JsonStreamOutput, BackendError> {
        self.inner
            .chat_json_stream(&self.localize(messages), think, reasoning_role, sink)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct RecordingBackend {
        messages: Arc<Mutex<Option<Value>>>,
    }

    #[async_trait]
    impl Backend for RecordingBackend {
        fn model(&self) -> String {
            "recording".to_string()
        }

        fn set_model(&self, _model: &str) {}

        async fn list_models(&self) -> Vec<Value> {
            Vec::new()
        }

        async fn chat(
            &self,
            messages: &Value,
            _tools: Option<&Value>,
            _think: Option<bool>,
            _reasoning_role: &str,
        ) -> Result<ChatOutput, BackendError> {
            *self.messages.lock().unwrap() = Some(messages.clone());
            Ok(ChatOutput {
                thinking: String::new(),
                content: String::new(),
                calls: Vec::new(),
                assistant_msg: json!({"role": "assistant", "content": ""}),
            })
        }

        async fn chat_json(
            &self,
            _messages: &Value,
            _think: Option<bool>,
            _reasoning_role: &str,
        ) -> Result<Map<String, Value>, BackendError> {
            unreachable!("not used by this test")
        }

        async fn summarize(
            &self,
            _text: &str,
            _proper_nouns: &[String],
        ) -> Result<String, BackendError> {
            unreachable!("not used by this test")
        }

        async fn chat_stream(
            &self,
            _messages: &Value,
            _tools: Option<&Value>,
            _think: Option<bool>,
            _reasoning_role: &str,
            _sink: &mut (dyn DeltaSink + Send),
        ) -> Result<ChatStreamOutput, BackendError> {
            unreachable!("not used by this test")
        }

        async fn chat_json_stream(
            &self,
            _messages: &Value,
            _think: Option<bool>,
            _reasoning_role: &str,
            _sink: &mut (dyn DeltaSink + Send),
        ) -> Result<JsonStreamOutput, BackendError> {
            unreachable!("not used by this test")
        }
    }

    #[test]
    fn appends_one_final_leading_system_instruction_without_changing_input() {
        let messages = json!([
            {"role": "system", "content": "stable"},
            {"role": "user", "content": "hello"}
        ]);
        let localized = messages_with_response_language(&messages, "en-US");

        assert_eq!(messages.as_array().unwrap().len(), 2);
        let localized = localized.as_array().unwrap();
        assert_eq!(localized.len(), 3);
        assert_eq!(localized[0], messages[0]);
        assert_eq!(localized[1]["role"], "system");
        assert_eq!(localized[2], messages[1]);
        assert!(localized[1]["content"]
            .as_str()
            .unwrap()
            .starts_with("<gml-response-language code=\"en-us\">"));
    }

    #[test]
    fn replaces_an_existing_synthetic_instruction() {
        let once = messages_with_response_language(&json!([]), "ru");
        let twice = messages_with_response_language(&once, "en");
        let items = twice.as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert!(items[0]["content"]
            .as_str()
            .unwrap()
            .starts_with("<gml-response-language code=\"en\">"));
    }

    #[tokio::test]
    async fn backend_snapshots_language_and_localizes_each_request() {
        let recorded = Arc::new(Mutex::new(None));
        let backend = Arc::new(RecordingBackend {
            messages: recorded.clone(),
        });
        let localized = ResponseLanguageBackend::new(backend, Arc::new(|| "en".to_string()));
        let original = json!([
            {"role": "system", "content": "Write in Russian."},
            {"role": "user", "content": "Continue"}
        ]);

        localized
            .chat(&original, None, Some(false), "gm")
            .await
            .unwrap();

        let captured = recorded.lock().unwrap().clone().unwrap();
        let messages = captured.as_array().unwrap();
        assert_eq!(messages[0], original[0]);
        assert!(messages[1]["content"]
            .as_str()
            .unwrap()
            .starts_with("<gml-response-language code=\"en\">"));
        assert_eq!(messages[2], original[1]);
        assert_eq!(original.as_array().unwrap().len(), 2);
    }
}
