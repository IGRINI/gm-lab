//! Contract tests for ask_player engine-handling, ported from
//! `gm-lab/test_contracts.py`:
//!   - the direct ask_player tool: exact 'PLAYER OPTIONS' model + full text,
//!     <4-options rejection, non-terminal (Python ≈ 972-995),
//!   - run_turn engine handling: no gm_tool_call / tool_result events for
//!     ask_player; the GM's second request sees the PLAYER OPTIONS tool message;
//!     exactly one tool message carrying the next-step instruction
//!     (Python AskPlayerToolClient ≈ 2193-2322),
//!   - missing ask_player with options enabled -> "без ask_player" error, no
//!     player_options, no ask_player gm_tool_call (Python MissingAskPlayerClient),
//!   - missing-narration fallback: a bare ask_player call (no prelude content)
//!     still emits a prelude before player_options (Python
//!     AskPlayerWithoutNarrationClient ≈ 2236-2337).

use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::{json, Map, Value};

use gml_config::{Config, RuntimeSettings};
use gml_llm::backend::{
    channel, Backend, BackendError, ChatOutput, ChatStreamOutput, DeltaSink, JsonStreamOutput,
};
use gml_mock::{mock_stats, MockClient};
use gml_orchestrator::{run_tool_collect, run_turn, Session};
use gml_types::{Event, ParsedCall};

fn tokio_block_on<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
        .block_on(fut)
}

/// Settings with gm_suggest_options ENABLED (the ask_player gating regime).
fn settings_options_on() -> RuntimeSettings {
    settings_with(true)
}

fn settings_with(suggest_options: bool) -> RuntimeSettings {
    let mut cfg = Config::from_env();
    cfg.backend = "mock".to_string();
    let tmp = std::env::temp_dir().join(format!(
        "gml_orch_askplayer_settings_{}_{}.json",
        std::process::id(),
        suggest_options
    ));
    let _ = std::fs::remove_file(&tmp);
    let settings = RuntimeSettings::new(&cfg, tmp);
    let mut update = Map::new();
    update.insert(
        "gm_suggest_options".to_string(),
        Value::Bool(suggest_options),
    );
    settings.update(Some(&update));
    settings
}

fn session(client: Arc<dyn Backend>) -> Session {
    Session::new(client)
}

/// A scripted GM backend driven by a list of "moves", one per `chat_stream`
/// invocation that carries tools (the prelude call, with tools=None, is served
/// separately by a fixed prelude text when `prelude` is Some).
struct ScriptedGm {
    /// Each entry is the move for the Nth tool-carrying request.
    moves: Vec<Move>,
    calls: Mutex<usize>,
    /// Number of tool-carrying (tools=Some) requests served.
    tool_requests: Mutex<usize>,
    /// Prelude text returned when tools is None (None disables the prelude path).
    prelude: Option<String>,
    /// Record of message snapshots per request (for assertions).
    request_log: Mutex<Vec<Vec<Value>>>,
}

#[derive(Clone)]
enum Move {
    /// Stream this content as the final narration (no tool calls).
    Final(String),
    /// Emit `content` as prelude text + a single tool call.
    Tool {
        content: String,
        call: (String, Value),
    },
}

impl ScriptedGm {
    fn new(moves: Vec<Move>, prelude: Option<&str>) -> Self {
        ScriptedGm {
            moves,
            calls: Mutex::new(0),
            tool_requests: Mutex::new(0),
            prelude: prelude.map(String::from),
            request_log: Mutex::new(Vec::new()),
        }
    }
}

fn as_messages(v: &Value) -> Vec<Value> {
    match v {
        Value::Array(a) => a.clone(),
        _ => Vec::new(),
    }
}

#[async_trait]
impl Backend for ScriptedGm {
    fn model(&self) -> String {
        "mock".to_string()
    }
    fn set_model(&self, _m: &str) {}
    async fn list_models(&self) -> Vec<Value> {
        vec![]
    }
    async fn chat(
        &self,
        _m: &Value,
        _t: Option<&Value>,
        _th: Option<bool>,
        _r: &str,
    ) -> Result<ChatOutput, BackendError> {
        Ok(ChatOutput {
            thinking: String::new(),
            content: String::new(),
            calls: vec![],
            assistant_msg: json!({"role": "assistant", "content": ""}),
        })
    }
    async fn chat_json(
        &self,
        _m: &Value,
        _th: Option<bool>,
        _r: &str,
    ) -> Result<Map<String, Value>, BackendError> {
        // Scene-delta sync asks for {"moves": []}.
        Ok(json!({"moves": []}).as_object().unwrap().clone())
    }
    async fn summarize(&self, _t: &str, _p: &[String]) -> Result<String, BackendError> {
        Ok(String::new())
    }
    async fn chat_stream(
        &self,
        messages: &Value,
        tools: Option<&Value>,
        _think: Option<bool>,
        _reasoning_role: &str,
        sink: &mut (dyn DeltaSink + Send),
    ) -> Result<ChatStreamOutput, BackendError> {
        *self.calls.lock().unwrap() += 1;
        self.request_log.lock().unwrap().push(as_messages(messages));

        // The prelude stream is invoked with tools=None.
        if tools.is_none() {
            if let Some(p) = &self.prelude {
                for w in p.split_whitespace() {
                    sink.emit(channel::CONTENT, &format!("{w} "));
                }
                return Ok(ChatStreamOutput {
                    thinking: String::new(),
                    content: p.clone(),
                    calls: vec![],
                    assistant_msg: json!({"role": "assistant", "content": p}),
                    stats: mock_stats(),
                });
            }
            // No prelude configured: empty content.
            return Ok(ChatStreamOutput {
                thinking: String::new(),
                content: String::new(),
                calls: vec![],
                assistant_msg: json!({"role": "assistant", "content": ""}),
                stats: mock_stats(),
            });
        }

        // Tool-carrying request -> serve the next move.
        let idx = {
            let mut tr = self.tool_requests.lock().unwrap();
            let i = *tr;
            *tr += 1;
            i
        };
        let mv = self
            .moves
            .get(idx)
            .cloned()
            .unwrap_or_else(|| Move::Final("(no more moves)".to_string()));
        match mv {
            Move::Final(text) => {
                for w in text.split_whitespace() {
                    sink.emit(channel::CONTENT, &format!("{w} "));
                }
                Ok(ChatStreamOutput {
                    thinking: String::new(),
                    content: text.clone(),
                    calls: vec![],
                    assistant_msg: json!({"role": "assistant", "content": text}),
                    stats: mock_stats(),
                })
            }
            Move::Tool { content, call } => {
                let (name, args) = call;
                let parsed = ParsedCall::new(
                    name.clone(),
                    args.as_object().cloned().unwrap_or_default(),
                    "call_1",
                );
                if !content.is_empty() {
                    for w in content.split_whitespace() {
                        sink.emit(channel::CONTENT, &format!("{w} "));
                    }
                }
                let assistant = json!({"role": "assistant", "content": content});
                Ok(ChatStreamOutput {
                    thinking: String::new(),
                    content,
                    calls: vec![parsed],
                    assistant_msg: assistant,
                    stats: mock_stats(),
                })
            }
        }
    }
    async fn chat_json_stream(
        &self,
        _m: &Value,
        _th: Option<bool>,
        _r: &str,
        _sink: &mut (dyn DeltaSink + Send),
    ) -> Result<JsonStreamOutput, BackendError> {
        Ok(JsonStreamOutput {
            data: Map::new(),
            stats: mock_stats(),
        })
    }
}

fn four_options() -> Value {
    json!([
        {"label": "Спросить", "message": "Спрашиваю ближайшего свидетеля."},
        {"label": "Осмотреть", "message": "Осматриваю место вокруг себя."},
        {"label": "Выйти", "message": "Выхожу проверить соседний проход."},
        {"label": "Подождать", "message": "Замираю и смотрю, кто первым отреагирует."},
    ])
}

// =========================================================================
// Direct ask_player tool: exact PLAYER OPTIONS text + non-terminal
// =========================================================================

#[test]
fn ask_player_tool_exact_text_and_non_terminal() {
    let mut s = session(Arc::new(MockClient::new()));
    let (events, result) = tokio_block_on(run_tool_collect(
        &mut s,
        "ask_player",
        &json!({
            "question": "Что дальше?",
            "options": [
                {"label": "Спросить", "message": "Спросить Борина о шуме за дверью."},
                {"label": "Осмотреть", "message": "Осмотреть общий зал и ближайшие столы."},
                {"label": "Выйти", "message": "Выйти на улицу и проверить переулок."},
                {"label": "Ждать", "message": "Остаться у стойки и подождать развития событий."},
            ],
        }),
    ));
    assert!(!result.terminal, "ask_player result must be non-terminal");
    let expected = "PLAYER OPTIONS\n\
status: buttons shown to player\n\
shown: 4\n\
next: write the final player-facing narration now, then stop; do not call ask_player again.";
    assert_eq!(result.full, expected);
    // ask_player has no system reminder -> model == full.
    assert_eq!(result.model, result.full);
    // A player_options event is emitted (the engine surfaces the buttons).
    assert!(events.iter().any(|e| e.kind == "player_options"));
}

#[test]
fn ask_player_rejects_fewer_than_four_options() {
    let mut s = session(Arc::new(MockClient::new()));
    let (_events, result) = tokio_block_on(run_tool_collect(
        &mut s,
        "ask_player",
        &json!({
            "question": "Что дальше?",
            "options": [{"label": "Спросить", "message": "Спросить Борина."}],
        }),
    ));
    assert!(!result.terminal);
    let plain = result
        .model
        .split("\n\n<system-reminder>")
        .next()
        .unwrap_or("")
        .to_string();
    assert!(
        plain.contains("not_enough_options"),
        "model: {}",
        result.model
    );
}

// =========================================================================
// run_turn: ask_player is engine-handled (no gm_tool_call/tool_result events)
// =========================================================================

fn has_ask_player_tool_event(events: &[Event]) -> bool {
    events.iter().any(|e| {
        e.kind == "gm_tool_call" && e.data.get("name").and_then(Value::as_str) == Some("ask_player")
    })
}

#[test]
fn run_turn_ask_player_engine_handled() {
    let client = Arc::new(ScriptedGm::new(
        vec![
            Move::Tool {
                content: "Перед тобой остаются несколько явных ходов.".to_string(),
                call: (
                    "ask_player".to_string(),
                    json!({"question": "Что дальше?", "options": four_options()}),
                ),
            },
            Move::Final(
                "Кнопки уже появились над вводом; сцена закрывается на твоём следующем выборе."
                    .to_string(),
            ),
        ],
        None,
    ));
    let mut s = Session::new(client.clone());
    let settings = settings_options_on();
    let events = tokio_block_on(run_turn(&mut s, &settings, "Что можно сделать?"));

    // player_options is emitted, but NO ask_player gm_tool_call / tool_result.
    let options_idx = events
        .iter()
        .position(|e| e.kind == "player_options")
        .expect("player_options emitted");
    let final_idx = events
        .iter()
        .position(|e| {
            e.kind == "gm_narration"
                && e.data
                    .as_str()
                    .map(|s| s.contains("сцена закрывается"))
                    .unwrap_or(false)
        })
        .expect("final narration emitted");
    assert!(options_idx < final_idx);
    assert!(
        !has_ask_player_tool_event(&events),
        "no gm_tool_call for ask_player"
    );
    assert!(
        !events
            .iter()
            .any(|e| e.kind == "tool_result" && e.agent.as_deref() == Some("ask_player")),
        "no tool_result for ask_player"
    );

    // Exactly one tool message in GM history, carrying the next-step instruction.
    let tool_messages: Vec<&Value> = s
        .gm_messages
        .iter()
        .filter(|m| m.get("role").and_then(Value::as_str) == Some("tool"))
        .collect();
    assert_eq!(tool_messages.len(), 1);
    let content = tool_messages[0]["content"].as_str().unwrap();
    assert!(content.contains("write the final player-facing narration now"));
    assert!(content.contains("do not call ask_player again"));

    // The GM's SECOND request saw the PLAYER OPTIONS tool message.
    let log = client.request_log.lock().unwrap();
    let second_req = log.iter().rev().find(|req| {
        req.iter().any(|m| {
            m.get("role").and_then(Value::as_str) == Some("tool")
                && m.get("content")
                    .and_then(Value::as_str)
                    .map(|c| c.contains("PLAYER OPTIONS"))
                    .unwrap_or(false)
        })
    });
    assert!(
        second_req.is_some(),
        "second GM request must see PLAYER OPTIONS tool message"
    );
}

// =========================================================================
// run_turn: missing ask_player with options enabled -> error, no options
// =========================================================================

#[test]
fn run_turn_missing_ask_player_errors() {
    // GM ends the turn with final narration, never calling ask_player.
    let client = Arc::new(ScriptedGm::new(
        vec![Move::Final(
            "Ты оставляешь себе секунду на выбор следующего шага.".to_string(),
        )],
        None,
    ));
    let mut s = Session::new(client);
    let settings = settings_options_on();
    let events = tokio_block_on(run_turn(&mut s, &settings, "Жду."));

    // No player_options payloads, no ask_player gm_tool_call.
    assert!(!events.iter().any(|e| e.kind == "player_options"));
    assert!(!has_ask_player_tool_event(&events));
    // An error event mentions the missing ask_player ("без ask_player").
    assert!(
        events.iter().any(|e| e.kind == "error"
            && e.data
                .as_str()
                .map(|s| s.contains("без ask_player"))
                .unwrap_or(false)),
        "expected a 'без ask_player' error event"
    );
}

// =========================================================================
// run_turn: bare ask_player (no narration) -> prelude fallback before options
// =========================================================================

#[test]
fn run_turn_bare_ask_player_emits_prelude_before_options() {
    // First tool move has EMPTY content (no prelude inline) -> the engine must
    // generate a pre-tool prelude (tools=None path) before surfacing options.
    let client = Arc::new(ScriptedGm::new(
        vec![
            Move::Tool {
                content: String::new(),
                call: (
                    "ask_player".to_string(),
                    json!({"question": "Что дальше?", "options": four_options()}),
                ),
            },
            Move::Final("После короткой паузы остаётся только выбрать следующий ход.".to_string()),
        ],
        Some("Ты задерживаешь взгляд на сцене и видишь несколько безопасных ходов."),
    ));
    let mut s = Session::new(client.clone());
    let settings = settings_options_on();
    let events = tokio_block_on(run_turn(&mut s, &settings, "Что можно сделать?"));

    let prelude_idx = events
        .iter()
        .position(|e| {
            e.kind == "gm_narration"
                && e.data
                    .as_str()
                    .map(|s| s.contains("несколько безопасных ходов"))
                    .unwrap_or(false)
        })
        .expect("prelude narration emitted");
    let options_idx = events
        .iter()
        .position(|e| e.kind == "player_options")
        .expect("player_options emitted");
    let final_idx = events
        .iter()
        .position(|e| {
            e.kind == "gm_narration"
                && e.data
                    .as_str()
                    .map(|s| s.contains("выбрать следующий ход"))
                    .unwrap_or(false)
        })
        .expect("final narration emitted");
    assert!(prelude_idx < options_idx, "prelude must precede options");
    assert!(
        options_idx < final_idx,
        "options must precede final narration"
    );
    // Still no ask_player gm_tool_call event.
    assert!(!has_ask_player_tool_event(&events));
}
