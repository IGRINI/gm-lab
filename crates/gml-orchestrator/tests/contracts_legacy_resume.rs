use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{json, Map, Value};
use tempfile::TempDir;

use gml_config::{Config, RuntimeSettings};
use gml_llm::{
    channel, Backend, BackendError, ChatOutput, ChatStreamOutput, DeltaSink, JsonStreamOutput,
};
use gml_mock::mock_stats;
use gml_orchestrator::{resume_turn_into, Session, TurnOutcome};
use gml_types::event_kind;

#[derive(Default)]
struct FinalBackend {
    requests: Mutex<Vec<Value>>,
    summarize_calls: AtomicUsize,
}

impl FinalBackend {
    fn output(&self) -> ChatOutput {
        let content = "Ход безопасно продолжен.".to_string();
        ChatOutput {
            thinking: String::new(),
            content: content.clone(),
            calls: Vec::new(),
            assistant_msg: json!({"role": "assistant", "content": content}),
        }
    }
}

#[async_trait]
impl Backend for FinalBackend {
    fn model(&self) -> String {
        "resume-test".to_string()
    }

    fn set_model(&self, _model: &str) {}

    async fn list_models(&self) -> Vec<Value> {
        Vec::new()
    }

    async fn chat(
        &self,
        _messages: &Value,
        _tools: Option<&Value>,
        _think: Option<bool>,
        _reasoning_role: &str,
    ) -> Result<ChatOutput, BackendError> {
        Ok(self.output())
    }

    async fn chat_json(
        &self,
        _messages: &Value,
        _think: Option<bool>,
        _reasoning_role: &str,
    ) -> Result<Map<String, Value>, BackendError> {
        Ok(json!({"moves": []})
            .as_object()
            .expect("moves payload")
            .clone())
    }

    async fn summarize(
        &self,
        _text: &str,
        _proper_nouns: &[String],
    ) -> Result<String, BackendError> {
        self.summarize_calls.fetch_add(1, Ordering::SeqCst);
        Ok("unexpected compaction".to_string())
    }

    async fn chat_stream(
        &self,
        messages: &Value,
        _tools: Option<&Value>,
        _think: Option<bool>,
        _reasoning_role: &str,
        sink: &mut (dyn DeltaSink + Send),
    ) -> Result<ChatStreamOutput, BackendError> {
        self.requests
            .lock()
            .expect("request log")
            .push(messages.clone());
        let output = self.output();
        sink.emit(channel::CONTENT, &output.content);
        Ok(ChatStreamOutput {
            thinking: output.thinking,
            content: output.content,
            calls: output.calls,
            assistant_msg: output.assistant_msg,
            stats: mock_stats(),
        })
    }

    async fn chat_json_stream(
        &self,
        _messages: &Value,
        _think: Option<bool>,
        _reasoning_role: &str,
        _sink: &mut (dyn DeltaSink + Send),
    ) -> Result<JsonStreamOutput, BackendError> {
        Ok(JsonStreamOutput {
            data: Map::new(),
            stats: mock_stats(),
        })
    }
}

fn settings() -> (TempDir, RuntimeSettings) {
    let mut config = Config::from_env();
    config.backend = "mock".to_string();
    let temp = tempfile::tempdir().expect("settings tempdir");
    let settings = RuntimeSettings::new(&config, temp.path().join("settings.json"));
    settings.update(Some(
        json!({
            "gm_suggest_options": false,
            "stream_gm_content": false,
            "max_tool_hops": 4
        })
        .as_object()
        .expect("settings object"),
    ));
    (temp, settings)
}

fn prepare_legacy_turn(session: &mut Session, player_text: &str) -> usize {
    session.ensure_initial_tools(false);
    session.turn = 1;
    session.last_player_action = player_text.to_string();
    session.turn_time_advances.clear();

    let recent = session.recent_contact_ids();
    let snapshot = gml_agents::gm_world_snapshot(&mut session.world, &recent, false);
    session
        .gm_messages
        .push(gml_agents::gm_snapshot_message(&snapshot));
    session.snapshot_options_state = Some(false);
    session
        .gm_messages
        .push(gml_agents::gm_user_message(player_text));

    session.record_public("player", "speech", player_text, "");
    session.events.len() - 1
}

fn empty_failed_total(secs: f64) -> Value {
    json!({
        "calls": [],
        "in": 0,
        "out": 0,
        "cached": 0,
        "tokens": 0,
        "peak_context": 0,
        "secs": secs,
        "run": {"turns": 3}
    })
}

#[tokio::test]
async fn resume_reuses_player_turn_and_replaces_failed_usage() {
    let backend = Arc::new(FinalBackend::default());
    let mut session = Session::new(backend.clone());
    let player_text = "Осматриваюсь";
    let player_event_index = prepare_legacy_turn(&mut session, player_text);
    let player_seq = session.events[player_event_index].seq;
    let player_source = format!("world_event_{player_seq}");

    let baseline_usage = json!({
        "turns": 2,
        "calls": 5,
        "in": 100,
        "out": 40,
        "cached": 20,
        "tokens": 140,
        "secs": 3.23,
        "peak_context": 100,
        "gm_calls": 3,
        "gm_tokens": 80,
        "npc_calls": 1,
        "npc_tokens": 40,
        "other_calls": 1,
        "other_tokens": 20
    });
    session.set_run_usage(&baseline_usage);
    let failed_total = empty_failed_total(0.77);
    session.add_turn_usage(&failed_total);
    assert_eq!(session.run_usage["turns"], 3);
    assert_eq!(session.run_usage["secs"], 4.0);
    assert!(session.remove_empty_failed_turn_usage(&failed_total));
    assert_eq!(
        session.run_usage,
        baseline_usage.as_object().unwrap().clone()
    );

    let player_memory_before = session
        .world
        .world_canon
        .memory
        .units
        .values()
        .filter(|unit| unit.source_event_ids.contains(&player_source))
        .count();
    let gm_user = gml_agents::gm_user_message(player_text);
    let gm_user_before = session
        .gm_messages
        .iter()
        .filter(|message| **message == gm_user)
        .count();

    // A fresh turn would compact under these thresholds. Resume must not run
    // that pre-turn phase.
    session.compaction.gm_history_tokens = 0;
    session.compaction.gm_keep_turns = 1;
    session.gm_summary = "legacy summary".to_string();

    let (_temp, settings) = settings();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let outcome =
        resume_turn_into(&mut session, &settings, player_text, player_event_index, tx).await;
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }

    assert_eq!(outcome, TurnOutcome::Completed);
    assert_eq!(session.turn, 1);
    assert_eq!(session.turn_player_event, Some(player_event_index));
    assert_eq!(session.events.len(), 1);
    assert_eq!(
        session
            .events
            .iter()
            .filter(|event| event.actor == "player" && event.turn == 1)
            .count(),
        1
    );
    assert_eq!(
        session
            .gm_messages
            .iter()
            .filter(|message| **message == gm_user)
            .count(),
        gm_user_before
    );
    assert_eq!(
        session
            .world
            .world_canon
            .memory
            .units
            .values()
            .filter(|unit| unit.source_event_ids.contains(&player_source))
            .count(),
        player_memory_before
    );
    assert!(!events.iter().any(|event| event.kind == event_kind::PLAYER));
    assert_eq!(session.gm_summary, "legacy summary");
    assert_eq!(backend.summarize_calls.load(Ordering::SeqCst), 0);
    assert_eq!(session.run_usage["turns"], 3);
    assert_eq!(backend.requests.lock().expect("request log").len(), 1);
}

#[tokio::test]
async fn invalid_resume_does_not_mutate_session() {
    let backend = Arc::new(FinalBackend::default());
    let mut session = Session::new(backend);
    let player_event_index = prepare_legacy_turn(&mut session, "Осматриваюсь");
    let before = session.to_payload();
    let before_usage = session.run_usage.clone();
    let before_player_event = session.turn_player_event;
    let (_temp, settings) = settings();

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let outcome = resume_turn_into(
        &mut session,
        &settings,
        "Другое действие",
        player_event_index,
        tx,
    )
    .await;
    let events = std::iter::from_fn(|| rx.try_recv().ok()).collect::<Vec<_>>();

    assert!(matches!(
        outcome,
        TurnOutcome::Failed {
            retryable: false,
            ..
        }
    ));
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, event_kind::ERROR);
    assert_eq!(session.to_payload(), before);
    assert_eq!(session.run_usage, before_usage);
    assert_eq!(session.turn_player_event, before_player_event);
}

#[test]
fn failed_usage_removal_is_strict_and_atomic() {
    let mut session = Session::new(Arc::new(FinalBackend::default()));
    session.set_run_usage(&json!({
        "turns": 1,
        "calls": 7,
        "secs": 2.0,
        "tokens": 99
    }));
    let before = session.run_usage.clone();

    let mut non_empty = empty_failed_total(0.5);
    non_empty["tokens"] = json!(1);
    assert!(!session.remove_empty_failed_turn_usage(&non_empty));
    assert_eq!(session.run_usage, before);

    let too_slow = empty_failed_total(2.01);
    assert!(!session.remove_empty_failed_turn_usage(&too_slow));
    assert_eq!(session.run_usage, before);

    assert!(session.remove_empty_failed_turn_usage(&empty_failed_total(0.5)));
    assert_eq!(session.run_usage["turns"], 0);
    assert_eq!(session.run_usage["secs"], 1.5);
    assert_eq!(session.run_usage["calls"], before["calls"]);
    assert_eq!(session.run_usage["tokens"], before["tokens"]);
}
