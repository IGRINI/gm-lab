//! The turn loop: `run_turn`, `_drive`, `_run_tool`, `_ask_npc`, the pre-tool
//! prelude, and scene-delta sync — ports from `orchestrator.py`.
//!
//! PORT_PLAN §5.2: Python generators / `yield from` are flattened into ONE event
//! stream. Each helper takes a `&Sink` (an mpsc sender wrapper) and returns its
//! value (`ToolExecutionResult` / `String`). The whole turn is driven by
//! [`run_turn`], which returns a [`Vec<Event>`] (tests) and can also stream.

use std::collections::BTreeSet;
use std::time::Instant;

use serde_json::{json, Map, Value};
use tokio::sync::mpsc;

use gml_config::RuntimeSettings;
use gml_llm::{channel, BackendError, ChatStreamOutput, DeltaSink};
use gml_types::{event_kind, Event, NpcBeat, ParsedCall, Role, ToolExecutionResult};

use crate::compact::{
    add_total_context, context_usage, maybe_compact, maybe_compact_npc, meta, meta_total, round2,
};
use crate::helpers::{
    json_compact, player_facing_payload, tool_error, tool_reminder, tool_result,
    with_model_reminder, VISIBLE_CONTINUATION_REMINDER,
};
use crate::helpers::{model_player_options_text, model_roll_text};
use crate::memory_crystals::maybe_consolidate_memory_semantic;
use crate::model_text::{
    apply_scene_move, model_ask_npc_text, model_npc_profile_text,
    model_player_character_update_text, model_presence_text, model_scene_text, model_time_text,
    model_whereabouts_text, model_world_query_text, model_world_state_update_text,
    normalize_tool_args, player_options_payload,
};
use crate::session::Session;

const PRELUDE_CALLBRIEF_CHARS: usize = 4000;
const NPC_TOOL_HOPS: usize = 3;

/// `ev(kind, agent, data, sid)`.
fn ev(kind: &str, agent: Option<&str>, data: Value, sid: Option<&str>) -> Event {
    Event::new(kind, agent.map(String::from), data, sid.map(String::from))
}

/// The event sink — an mpsc sender. Each `emit` corresponds to one Python
/// generator `yield`.
pub struct Sink {
    tx: mpsc::UnboundedSender<Event>,
}

impl Sink {
    fn emit(&self, e: Event) {
        let _ = self.tx.send(e);
    }
}

// =========================================================================
// run_turn
// =========================================================================

/// `run_turn(session, player_text)` — collect all events into a `Vec<Event>`.
///
/// Drives the whole turn with an unbounded mpsc channel as the flattened event
/// sink, then drains it. The result is the exact ordered sequence Python's
/// generator yields, including the terminal `meta_total`.
pub async fn run_turn(
    session: &mut Session,
    settings: &RuntimeSettings,
    player_text: &str,
) -> Vec<Event> {
    let (tx, mut rx) = mpsc::unbounded_channel::<Event>();
    run_turn_into(session, settings, player_text, tx).await;
    let mut out = Vec::new();
    while let Some(e) = rx.recv().await {
        out.push(e);
    }
    out
}

/// `run_turn_into(session, settings, player_text, tx)` — the streaming variant.
///
/// Drives the whole turn, sending each [`Event`] into the caller-supplied
/// channel **as it is produced** (no buffering of the whole turn), then sends
/// the terminal `meta_total`. The event sequence is byte-for-byte identical to
/// [`run_turn`] — this is purely the "expose streaming" half of the same driver
/// (PORT_PLAN §5.2). The server (`gml-server`) drains the receiver and frames
/// each event as `data: {json}\n\n`, appending its own terminal `done` frame.
///
/// The channel is dropped when the turn ends, so the receiver sees `None` after
/// the final `meta_total`.
pub async fn run_turn_into(
    session: &mut Session,
    settings: &RuntimeSettings,
    player_text: &str,
    tx: mpsc::UnboundedSender<Event>,
) {
    let sink = Sink { tx };

    let t0 = Instant::now();
    let mut metas: Vec<Value> = Vec::new();
    drive(session, settings, player_text, &mut metas, &sink).await;
    let total_secs = round2(t0.elapsed().as_secs_f64());
    let mut total = meta_total(&metas, total_secs);
    add_total_context(&mut total, context_usage(session));
    let run = session.add_turn_usage(&total);
    if let Value::Object(ref mut m) = total {
        m.insert("run".to_string(), run);
    }
    sink.emit(ev(event_kind::META_TOTAL, None, total, None));
}

// =========================================================================
// _drive
// =========================================================================

async fn drive(
    session: &mut Session,
    settings: &RuntimeSettings,
    player_text: &str,
    metas: &mut Vec<Value>,
    sink: &Sink,
) {
    let include_player_options_tool = settings.gm_suggest_options_enabled(None);
    let stream_gm_content = settings.stream_gm_content_enabled(None);
    session.ensure_initial_tools(include_player_options_tool);
    session.turn += 1;
    session.last_player_action = player_text.to_string();
    session.turn_player_event = None;
    session.turn_time_advances = Vec::new();
    let mut turn_visible_output_seen = false;

    let gm_user =
        gml_agents::gm_user_message(&mut session.world, player_text, include_player_options_tool);
    session.gm_messages.push(gm_user);
    sink.emit(ev(
        event_kind::PLAYER,
        Some("Игрок"),
        Value::String(player_text.to_string()),
        None,
    ));

    let client = session.client.clone();
    maybe_compact(session, client.as_ref()).await;

    let mut fell_through = true;
    let max_tool_hops = settings.max_tool_hops(None);
    let mut tool_hops = 0_i64;
    let mut player_options_shown = false;

    while max_tool_hops <= 0 || tool_hops < max_tool_hops {
        tool_hops += 1;
        let sid = session.next_sid();

        // gm_turn_stream — buffer deltas while streaming.
        let mut content_deltas: Vec<String> = Vec::new();
        let stream_result = {
            let mut collector = GmDeltaCollector {
                sink,
                sid: &sid,
                stream_gm_content,
                content_deltas: &mut content_deltas,
            };
            gml_agents::gm_turn_stream(
                client.as_ref(),
                &session.world,
                &session.gm_messages,
                &session.gm_summary,
                Some(&session.loaded_gm_tools),
                include_player_options_tool,
                &mut collector,
            )
            .await
        };

        let ChatStreamOutput {
            thinking,
            content,
            calls,
            assistant_msg,
            stats,
        } = match stream_result {
            Ok(out) => out,
            Err(e) => {
                sink.emit(ev(
                    event_kind::ERROR,
                    Some("ГМ"),
                    Value::String(format!("Ошибка вызова модели: {e}")),
                    None,
                ));
                fell_through = false;
                break;
            }
        };

        if !thinking.trim().is_empty() {
            sink.emit(ev(
                event_kind::GM_THINKING,
                Some("ГМ"),
                Value::String(thinking.trim().to_string()),
                Some(&sid),
            ));
        }
        let label = if calls.is_empty() {
            "ГМ — нарратив"
        } else {
            "ГМ — решение"
        };
        let m = meta(label, &stats, "gm");
        metas.push(m.clone());
        sink.emit(ev(event_kind::META, Some("ГМ"), m, Some(&sid)));

        if calls.is_empty() {
            let final_text = content.trim().to_string();
            session.gm_messages.push(assistant_msg);
            if !final_text.is_empty() {
                if !content_deltas.is_empty() && !stream_gm_content {
                    sink.emit(ev(
                        event_kind::DELTA,
                        Some("ГМ"),
                        json!({"channel": "gm_narration", "text": final_text}),
                        Some(&sid),
                    ));
                }
                sink.emit(ev(
                    event_kind::GM_NARRATION,
                    Some("ГМ"),
                    Value::String(final_text.clone()),
                    Some(&sid),
                ));
                sync_scene_delta(session, &final_text, metas, sink).await;
            }
            fell_through = false;
            break;
        }

        let calls = normalize_tool_calls(&calls, &format!("gm_{sid}"));
        let mut assistant_msg = assistant_with_tool_calls(assistant_msg, &calls);
        let mut prelude_text = content.trim().to_string();
        let mut prelude_generated = false;
        if prelude_text.is_empty()
            && !turn_visible_output_seen
            && should_generate_tool_prelude(&calls)
        {
            prelude_text = generate_pre_tool_prelude(
                session,
                player_text,
                &calls,
                metas,
                stream_gm_content,
                sink,
            )
            .await;
            if !prelude_text.is_empty() {
                prelude_generated = true;
                if let Value::Object(ref mut m) = assistant_msg {
                    m.insert("content".to_string(), Value::String(prelude_text.clone()));
                }
            }
        }

        session.gm_messages.push(assistant_msg);
        if !prelude_text.is_empty() {
            if !prelude_generated {
                if !content_deltas.is_empty() && !stream_gm_content {
                    sink.emit(ev(
                        event_kind::DELTA,
                        Some("ГМ"),
                        json!({"channel": "gm_narration", "text": prelude_text}),
                        Some(&sid),
                    ));
                }
                sink.emit(ev(
                    event_kind::GM_NARRATION,
                    Some("ГМ"),
                    Value::String(prelude_text.clone()),
                    Some(&sid),
                ));
            }
            turn_visible_output_seen = true;
        }

        let mut terminal_after_tools = false;
        for call in &calls {
            let name = call.name.clone();
            let args = Value::Object(call.arguments.clone());
            let show_tool_event = name != "ask_player";
            if show_tool_event {
                sink.emit(ev(
                    event_kind::GM_TOOL_CALL,
                    Some("ГМ"),
                    json!({"name": name, "arguments": args}),
                    None,
                ));
            }
            let mut result = run_tool(session, &name, &args, metas, sink).await;
            if show_tool_event {
                sink.emit(ev(
                    event_kind::TOOL_RESULT,
                    Some(&name),
                    Value::String(result.full.clone()),
                    None,
                ));
            }
            let tool_visible_output = tool_emits_visible_output(&name, &result);
            let terminal_result = result.terminal;
            if (turn_visible_output_seen || tool_visible_output) && !terminal_result {
                result = with_model_reminder(result, VISIBLE_CONTINUATION_REMINDER);
            }
            session.gm_messages.push(json!({
                "role": "tool",
                "tool_call_id": call.id,
                "content": result.model,
            }));
            if tool_visible_output {
                turn_visible_output_seen = true;
            }
            if name == "ask_player" && result.full.starts_with("PLAYER OPTIONS") {
                player_options_shown = true;
            }
            if terminal_result {
                terminal_after_tools = true;
            }
        }
        if terminal_after_tools {
            fell_through = false;
            break;
        }
    }

    if fell_through {
        sink.emit(ev(
            event_kind::ERROR,
            Some("ГМ"),
            Value::String(format!(
                "Превышен лимит вызовов инструментов за ход: {max_tool_hops}."
            )),
            None,
        ));
    } else if include_player_options_tool && !player_options_shown {
        sink.emit(ev(
            event_kind::ERROR,
            Some("ГМ"),
            Value::String(
                "Модель завершила ход без ask_player, хотя варианты игрока включены.".to_string(),
            ),
            None,
        ));
    }

    if session.turn_player_event.is_none() {
        session.record_public("player", "speech", player_text, "");
    }

    finalize_turn_time(session);
    let compact_client = session.client.clone();
    session.commit_turn_without_memory_consolidation();
    maybe_consolidate_memory_semantic(session, compact_client.as_ref()).await;
}

// =========================================================================
// gm_turn_stream delta collector
// =========================================================================

struct GmDeltaCollector<'a> {
    sink: &'a Sink,
    sid: &'a str,
    stream_gm_content: bool,
    content_deltas: &'a mut Vec<String>,
}

impl DeltaSink for GmDeltaCollector<'_> {
    fn emit(&mut self, ch: &str, text: &str) {
        if ch == channel::THINKING {
            self.sink.emit(ev(
                event_kind::DELTA,
                Some("ГМ"),
                json!({"channel": "gm_thinking", "text": text}),
                Some(self.sid),
            ));
        } else if self.stream_gm_content {
            self.sink.emit(ev(
                event_kind::DELTA,
                Some("ГМ"),
                json!({"channel": "gm_narration", "text": text}),
                Some(self.sid),
            ));
        } else {
            self.content_deltas.push(text.to_string());
        }
    }
}

// =========================================================================
// prelude
// =========================================================================

const VISIBLE_PRELUDE_TOOLS: [&str; 7] = [
    "ask_npc",
    "ask_player",
    "move_npc",
    "set_npc_presence",
    "set_npc_whereabouts",
    "set_scene",
    "roll_dice",
];

fn should_generate_tool_prelude(calls: &[ParsedCall]) -> bool {
    calls
        .iter()
        .any(|c| VISIBLE_PRELUDE_TOOLS.contains(&c.name.as_str()))
}

async fn generate_pre_tool_prelude(
    session: &mut Session,
    player_text: &str,
    calls: &[ParsedCall],
    metas: &mut Vec<Value>,
    stream_gm_content: bool,
    sink: &Sink,
) -> String {
    let sid = session.next_sid();
    let calls_value: Vec<Value> = calls
        .iter()
        .map(|c| json!({"name": c.name, "arguments": Value::Object(c.arguments.clone())}))
        .collect();
    let client = session.client.clone();
    let stream_result = {
        let mut collector = PreludeDeltaCollector {
            sink,
            sid: &sid,
            stream_gm_content,
        };
        gml_agents::gm_prelude_stream(
            client.as_ref(),
            &mut session.world,
            player_text,
            &calls_value,
            PRELUDE_CALLBRIEF_CHARS,
            &mut collector,
        )
        .await
    };
    let ChatStreamOutput {
        thinking,
        content,
        stats,
        ..
    } = match stream_result {
        Ok(out) => out,
        Err(e) => {
            sink.emit(ev(
                event_kind::ERROR,
                Some("ГМ"),
                Value::String(format!("Ошибка прелюдии перед инструментом: {e}")),
                None,
            ));
            return String::new();
        }
    };
    if !thinking.trim().is_empty() {
        sink.emit(ev(
            event_kind::GM_THINKING,
            Some("ГМ"),
            Value::String(thinking.trim().to_string()),
            Some(&sid),
        ));
    }
    metas.push(meta("ГМ — прелюдия", &stats, "gm"));
    let final_text = content.trim().to_string();
    if !final_text.is_empty() {
        sink.emit(ev(
            event_kind::GM_NARRATION,
            Some("ГМ"),
            Value::String(final_text.clone()),
            Some(&sid),
        ));
    }
    final_text
}

struct PreludeDeltaCollector<'a> {
    sink: &'a Sink,
    sid: &'a str,
    stream_gm_content: bool,
}

impl DeltaSink for PreludeDeltaCollector<'_> {
    fn emit(&mut self, ch: &str, text: &str) {
        if ch == channel::THINKING {
            self.sink.emit(ev(
                event_kind::DELTA,
                Some("ГМ"),
                json!({"channel": "gm_thinking", "text": text}),
                Some(self.sid),
            ));
        } else if self.stream_gm_content {
            self.sink.emit(ev(
                event_kind::DELTA,
                Some("ГМ"),
                json!({"channel": "gm_narration", "text": text}),
                Some(self.sid),
            ));
        }
    }
}

// =========================================================================
// scene-delta sync
// =========================================================================

async fn sync_scene_delta(
    session: &mut Session,
    narration: &str,
    metas: &mut Vec<Value>,
    sink: &Sink,
) {
    if narration.trim().is_empty() {
        return;
    }
    let any_name = session
        .world
        .npcs
        .values()
        .any(|npc| !npc.name.is_empty() && narration.contains(&npc.name));
    if !any_name {
        return;
    }
    let client = session.client.clone();
    let delta =
        match gml_agents::extract_scene_delta(client.as_ref(), &mut session.world, narration).await
        {
            Ok(d) => d,
            Err(e) => {
                sink.emit(ev(
                    event_kind::ERROR,
                    Some("scene_sync"),
                    Value::String(format!("Scene state sync failed: {e}")),
                    None,
                ));
                return;
            }
        };
    // The scene-delta call is a mock chat_json call which records a stats row.
    // Python appends a "scene sync" meta from the client.call_log delta. The
    // mock client surfaces stats via mock_stats(); replicate the single meta.
    metas.push(meta("scene sync", &gml_llm::mock_stats(), "other"));
    let moves = match delta.get("moves") {
        Some(Value::Array(a)) => a.clone(),
        _ => Vec::new(),
    };
    for move_ in &moves {
        if !move_.is_object() {
            continue;
        }
        if let Some(payload) = apply_scene_move(&mut session.world, move_) {
            sink.emit(ev(
                event_kind::SCENE_UPDATE,
                Some("scene_sync"),
                payload,
                None,
            ));
        }
    }
}

// =========================================================================
// finalize turn time
// =========================================================================

fn finalize_turn_time(session: &mut Session) {
    let advances: Vec<Value> = session
        .turn_time_advances
        .iter()
        .filter(|r| r.is_object())
        .cloned()
        .collect();
    if advances.is_empty() {
        session.world.time.last_advance_minutes = 0;
        session.world.time.last_advance_reason = String::new();
        return;
    }
    let total: i64 = advances
        .iter()
        .map(|r| {
            r.get("minutes")
                .and_then(|v| v.as_i64())
                .unwrap_or(0)
                .max(0)
        })
        .sum();
    let reasons: Vec<String> = advances
        .iter()
        .filter_map(|r| {
            let reason = crate::helpers::clean_text(r.get("reason").unwrap_or(&Value::Null));
            if reason.is_empty() {
                None
            } else {
                Some(reason)
            }
        })
        .collect();
    session.world.time.last_advance_minutes = total;
    let joined: String = reasons.join("; ").chars().take(300).collect();
    session.world.time.last_advance_reason = joined;
}

// =========================================================================
// tool-call normalization (orchestrator-level)
// =========================================================================

/// `_normalize_tool_calls(calls, world, id_prefix)`.
fn normalize_tool_calls(calls: &[ParsedCall], id_prefix: &str) -> Vec<ParsedCall> {
    let mut out = Vec::new();
    for (idx, call) in calls.iter().enumerate() {
        let call_id = if call.id.trim().is_empty() {
            format!("{id_prefix}_{}", idx + 1)
        } else {
            call.id.clone()
        };
        let normalized_args =
            normalize_tool_args(&call.name, &Value::Object(call.arguments.clone()));
        let args_map = match normalized_args {
            Value::Object(m) => m,
            _ => Map::new(),
        };
        out.push(ParsedCall::new(call.name.clone(), args_map, call_id));
    }
    out
}

/// `_assistant_with_tool_calls(assistant_msg, calls)`.
fn assistant_with_tool_calls(assistant_msg: Value, calls: &[ParsedCall]) -> Value {
    let mut msg = match assistant_msg {
        Value::Object(m) => m,
        other => return other,
    };
    if calls.is_empty() {
        return Value::Object(msg);
    }
    let mut raw_calls = Vec::new();
    for call in calls {
        let name = call.name.trim();
        if name.is_empty() {
            continue;
        }
        raw_calls.push(json!({
            "id": call.id,
            "type": "function",
            "function": {
                "name": name,
                "arguments": json_compact(&Value::Object(call.arguments.clone())),
            },
        }));
    }
    if !raw_calls.is_empty() {
        msg.insert("tool_calls".to_string(), Value::Array(raw_calls));
    }
    Value::Object(msg)
}

/// `_tool_emits_visible_output(name, result)`.
fn tool_emits_visible_output(name: &str, result: &ToolExecutionResult) -> bool {
    if name != "ask_npc" {
        return false;
    }
    let payload: Value = match serde_json::from_str(&result.full) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let speech = crate::helpers::clean_text(payload.get("speech_ru").unwrap_or(&Value::Null));
    let action = crate::helpers::clean_text(payload.get("action_ru").unwrap_or(&Value::Null));
    !speech.is_empty() || !action.is_empty()
}

// =========================================================================
// _run_tool
// =========================================================================

/// Test-facing driver for a single tool dispatch — the Rust analogue of the
/// Python contract-test idiom `_drive(_run_tool(session, name, args, []))`.
///
/// Runs [`run_tool`] (exactly the dispatch the turn loop uses, before the
/// visible-continuation `with_model_reminder` wrap that `drive` applies), drains
/// the events the tool emitted, and returns `(events, ToolExecutionResult)`.
/// Both channels (`.full` / `.model`) and the `.terminal` flag are returned
/// untouched so contract tests can assert the two-channel split.
pub async fn run_tool_collect(
    session: &mut Session,
    name: &str,
    args: &Value,
) -> (Vec<Event>, ToolExecutionResult) {
    let (tx, mut rx) = mpsc::unbounded_channel::<Event>();
    let sink = Sink { tx };
    let mut metas: Vec<Value> = Vec::new();
    let result = run_tool(session, name, args, &mut metas, &sink).await;
    drop(sink);
    let mut events = Vec::new();
    while let Some(e) = rx.recv().await {
        events.push(e);
    }
    (events, result)
}

async fn run_tool(
    session: &mut Session,
    name: &str,
    args: &Value,
    metas: &mut Vec<Value>,
    sink: &Sink,
) -> ToolExecutionResult {
    let args = if args.is_object() {
        args.clone()
    } else {
        Value::Object(Map::new())
    };

    if name == "invoke_loaded_tool" {
        return run_invoke_loaded_tool(session, &args, metas, sink).await;
    }

    run_executable_tool(session, name, &args, metas, sink).await
}

async fn run_executable_tool(
    session: &mut Session,
    name: &str,
    args: &Value,
    metas: &mut Vec<Value>,
    sink: &Sink,
) -> ToolExecutionResult {
    match name {
        "tool_search" => run_tool_search(session, args, sink),
        "load_tool_schema" => run_load_tool_schema(session, args, sink),
        "ask_player" => run_ask_player(args, sink),
        "roll_dice" => run_roll_dice(session, args, sink),
        "get_world_fact" => run_get_world_fact(session, args, sink),
        "get_memory" => run_get_memory(session, args, sink),
        "note_memory" => run_note_memory(session, args, sink),
        "consolidate_memory" => run_consolidate_memory(session, args, sink),
        "get_npc_profile" => run_get_npc_profile(session, args, sink),
        "advance_time" => run_advance_time(session, args, sink),
        "update_player_character" => run_update_player_character(session, args, sink),
        "set_npc_whereabouts" => run_set_npc_whereabouts(session, args, sink),
        "move_npc" | "set_npc_presence" => run_move_npc(session, args, sink),
        "set_scene" => run_set_scene(session, args, sink),
        "move_player" => run_move_player(session, args, sink).await,
        "world_debug" => run_world_debug(session, args, sink),
        "generate_location" => run_generate_location(session, args, sink).await,
        "ask_npc" => run_ask_npc_tool(session, args, metas, sink).await,
        other => tool_error(
            if other.is_empty() { "unknown" } else { other },
            &format!("unknown tool: {other}"),
            None,
            "unknown_tool",
            &[],
        ),
    }
}

fn arg_str<'a>(args: &'a Value, key: &str) -> &'a str {
    args.get(key).and_then(Value::as_str).unwrap_or("")
}

fn situation_includes_room_witnesses(situation: &str) -> bool {
    let lower = situation.to_lowercase();
    let private_markers = [
        "не слышат",
        "не слышит",
        "не слышны",
        "не могут слышать",
        "не может слышать",
        "не громко",
        "тихо",
        "шепотом",
        "шёпотом",
        "шепчет",
        "шёпчет",
        "вполголоса",
        "негромко",
        "вплотную",
        "наедине",
        "частн",
        "только ему",
        "только ей",
        "only they hear",
        "only he hears",
        "only she hears",
        "quietly",
        "whisper",
        "private",
    ];
    if private_markers.iter().any(|marker| lower.contains(marker)) {
        return false;
    }

    let public_markers = [
        "громко",
        "вслух",
        "во весь голос",
        "на весь зал",
        "на всю комнату",
        "при всех",
        "все слышат",
        "слышат все",
        "чтобы все услышали",
        "публично",
        "loudly",
        "aloud",
        "publicly",
        "everyone hears",
        "the whole room hears",
    ];
    public_markers.iter().any(|marker| lower.contains(marker))
}

fn run_tool_search(session: &mut Session, args: &Value, sink: &Sink) -> ToolExecutionResult {
    let query = arg_str(args, "query");
    let max_results = args
        .get("max_results")
        .and_then(|v| v.as_i64())
        .unwrap_or(5);
    // Note: include_player_options_tool is read from settings in Python via
    // runtime_settings.gm_suggest_options_enabled(); we read it from the already
    // loaded tool set membership semantics. The agents layer takes the flag.
    let include_player_options = session.loaded_gm_tools.contains("ask_player");
    let payload = gml_agents::search_gm_tools(
        query,
        max_results,
        Some(&session.loaded_gm_tools),
        include_player_options,
    );
    let mut lines = vec![payload
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()];
    if let Some(Value::Array(matches)) = payload.get("matches") {
        if !matches.is_empty() {
            lines.push("Найдено:".to_string());
            for row in matches {
                let n = row.get("name").and_then(Value::as_str).unwrap_or("");
                let d = row.get("description").and_then(Value::as_str).unwrap_or("");
                lines.push(format!("- {n}: {d}"));
            }
        }
    }
    if let Some(Value::Array(missing)) = payload.get("missing") {
        if !missing.is_empty() {
            let names: Vec<String> = missing
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
            lines.push(format!("Не найдено: {}", names.join(", ")));
        }
    }
    let text = lines
        .into_iter()
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    sink.emit(ev(
        event_kind::TOOL_SEARCH,
        Some("ГМ"),
        Value::String(text),
        None,
    ));
    tool_result(
        &json_compact(&payload),
        Some(&crate::helpers::model_tool_search_text(&payload)),
        None,
        false,
    )
}

fn run_load_tool_schema(session: &mut Session, args: &Value, sink: &Sink) -> ToolExecutionResult {
    let name = arg_str(args, "name");
    let include_player_options = session.loaded_gm_tools.contains("ask_player");
    let payload = gml_agents::load_gm_tool_schema(
        name,
        Some(&session.loaded_gm_tools),
        include_player_options,
    );

    let status = payload
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let message = payload.get("message").and_then(Value::as_str).unwrap_or("");
    let loaded_schema = payload
        .get("loaded_schema")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut lines = vec![format!("Статус: {status}")];
    if !loaded_schema.is_empty() {
        lines.push(format!("Схема: {loaded_schema}"));
    }
    if !message.is_empty() {
        lines.push(message.to_string());
    }
    sink.emit(ev(
        event_kind::TOOL_SEARCH,
        Some("ГМ"),
        Value::String(lines.join("\n")),
        None,
    ));
    tool_result(
        &json_compact(&payload),
        Some(&crate::helpers::model_load_tool_schema_text(&payload)),
        None,
        false,
    )
}

async fn run_invoke_loaded_tool(
    session: &mut Session,
    args: &Value,
    metas: &mut Vec<Value>,
    sink: &Sink,
) -> ToolExecutionResult {
    let requested = arg_str(args, "name").trim();
    if requested.is_empty() {
        return tool_error(
            "invoke_loaded_tool",
            "missing loaded tool name",
            None,
            "missing_name",
            &[],
        );
    }
    if matches!(
        requested,
        "tool_search" | "load_tool_schema" | "invoke_loaded_tool"
    ) {
        return tool_error(
            "invoke_loaded_tool",
            &format!("cannot invoke loader tool through invoke_loaded_tool: {requested}"),
            None,
            "blocked_loader_tool",
            &[("name", Value::String(requested.to_string()))],
        );
    }
    let invocation_args = match args.get("arguments") {
        Some(Value::Object(m)) => Value::Object(m.clone()),
        _ => {
            return tool_error(
                "invoke_loaded_tool",
                "arguments must be a JSON object matching the loaded schema",
                None,
                "invalid_arguments",
                &[("name", Value::String(requested.to_string()))],
            );
        }
    };

    let include_player_options = session.loaded_gm_tools.contains("ask_player");
    let schema_payload = gml_agents::load_gm_tool_schema(
        requested,
        Some(&session.loaded_gm_tools),
        include_player_options,
    );
    let status = schema_payload
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("");
    if status == "missing" || status == "invalid" || schema_payload.get("schema").is_none() {
        let full = json_compact(&schema_payload);
        return tool_error(
            "invoke_loaded_tool",
            &format!("tool schema is not loadable: {requested}"),
            Some(&full),
            "schema_not_loadable",
            &[("name", Value::String(requested.to_string()))],
        );
    }
    let canonical = schema_payload
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or(requested);

    run_executable_tool(session, canonical, &invocation_args, metas, sink).await
}

fn run_ask_player(args: &Value, sink: &Sink) -> ToolExecutionResult {
    let (payload, error) = player_options_payload(args);
    if !error.is_empty() {
        sink.emit(ev(
            event_kind::ERROR,
            Some("ГМ"),
            Value::String(error.clone()),
            None,
        ));
        let count = match args.get("options") {
            Some(Value::Array(a)) => a.len() as i64,
            _ => 0,
        };
        return tool_error(
            "ask_player",
            &error,
            None,
            "not_enough_options",
            &[("count", json!(count))],
        );
    }
    sink.emit(ev(
        event_kind::PLAYER_OPTIONS,
        Some("ГМ"),
        payload.clone(),
        None,
    ));
    let model_text = model_player_options_text(&payload);
    tool_result(&model_text, Some(&model_text), None, false)
}

fn run_roll_dice(session: &mut Session, args: &Value, sink: &Sink) -> ToolExecutionResult {
    let notation = {
        let n = arg_str(args, "notation");
        if n.is_empty() {
            "1d20"
        } else {
            n
        }
    };
    let target_number = args.get("target_number");
    let mut payload = session.world.roll_outcome_payload(
        notation,
        target_number,
        arg_str(args, "target_kind"),
        arg_str(args, "roll_kind"),
    );
    let note = arg_str(args, "modifier_note").trim().to_string();
    if !note.is_empty() && note.to_lowercase() != "none/unknown" {
        if let Value::Object(ref mut m) = payload {
            m.insert("modifier_note".to_string(), Value::String(note));
        }
    }
    let detail = payload
        .get("detail")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    sink.emit(ev(event_kind::DICE, Some("ГМ"), payload.clone(), None));
    session.record_public("gm", "dice", "", &detail);
    tool_result(
        &detail,
        Some(&model_roll_text(&payload)),
        Some(tool_reminder("roll_dice")),
        false,
    )
}

fn run_get_world_fact(session: &mut Session, args: &Value, sink: &Sink) -> ToolExecutionResult {
    let query = arg_str(args, "query").to_string();
    let retrieval_report =
        crate::rag::retrieve_world_fact_report(&mut session.world, "player", &query);
    let fact = match retrieval_report.fact {
        Some(fact) => gml_world::WorldFact::new(fact.status, fact.text, fact.sources),
        None => session.world.fact(&query, "player", None),
    };
    let mut payload = fact.as_tool_payload();
    if let Value::Object(ref mut payload_map) = payload {
        payload_map.insert("retrieval".to_string(), retrieval_report.status);
    }
    let scope_key = crate::worldstate::query_scope_key("fact", "");
    let (payload, _delivered) =
        crate::query_dedup::filter_new_fact_payload(session, &scope_key, payload, &query);
    let mut event_payload = match payload.clone() {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    event_payload.insert("query".to_string(), Value::String(query.clone()));
    sink.emit(ev(
        event_kind::WORLD_FACT,
        Some("ГМ"),
        Value::Object(event_payload),
        None,
    ));
    tool_result(
        &json_compact(&payload),
        Some(&crate::helpers::model_world_fact_text(&payload)),
        Some(tool_reminder("get_world_fact")),
        false,
    )
}

fn run_get_memory(session: &mut Session, args: &Value, sink: &Sink) -> ToolExecutionResult {
    let payload = crate::worldstate::get_memory(session, args);
    if let Some(err) = payload.get("error").and_then(Value::as_str) {
        if !err.is_empty() {
            sink.emit(ev(
                event_kind::ERROR,
                Some("ГМ"),
                Value::String(err.to_string()),
                None,
            ));
        }
    } else {
        sink.emit(ev(
            event_kind::WORLD_QUERY,
            Some("ГМ"),
            payload.clone(),
            None,
        ));
    }
    tool_result(
        &json_compact(&payload),
        Some(&model_world_query_text(&payload)),
        Some(tool_reminder("get_memory")),
        false,
    )
}

fn run_note_memory(session: &mut Session, args: &Value, sink: &Sink) -> ToolExecutionResult {
    let payload = crate::worldstate::note_memory(session, args);
    if let Some(err) = payload.get("error").and_then(Value::as_str) {
        if !err.is_empty() {
            sink.emit(ev(
                event_kind::ERROR,
                Some("ГМ"),
                Value::String(err.to_string()),
                None,
            ));
        }
    }
    sink.emit(ev(
        event_kind::WORLD_STATE_UPDATE,
        Some("ГМ"),
        payload.clone(),
        None,
    ));
    tool_result(
        &json_compact(&payload),
        Some(&model_world_state_update_text(&payload)),
        Some(tool_reminder("note_memory")),
        false,
    )
}

fn run_consolidate_memory(session: &mut Session, args: &Value, sink: &Sink) -> ToolExecutionResult {
    let payload = crate::worldstate::consolidate_memory(session, args);
    if let Some(err) = payload.get("error").and_then(Value::as_str) {
        if !err.is_empty() {
            sink.emit(ev(
                event_kind::ERROR,
                Some("ГМ"),
                Value::String(err.to_string()),
                None,
            ));
        }
    }
    sink.emit(ev(
        event_kind::WORLD_STATE_UPDATE,
        Some("ГМ"),
        payload.clone(),
        None,
    ));
    tool_result(
        &json_compact(&payload),
        Some(&model_world_state_update_text(&payload)),
        Some(tool_reminder("consolidate_memory")),
        false,
    )
}

fn run_get_npc_profile(session: &mut Session, args: &Value, sink: &Sink) -> ToolExecutionResult {
    let payload = match session.world.npc_profile(
        arg_str(args, "npc_id"),
        {
            let p = arg_str(args, "preset");
            if p.is_empty() {
                "visible"
            } else {
                p
            }
        },
        args.get("fields").unwrap_or(&Value::Null),
    ) {
        Ok(p) => {
            sink.emit(ev(event_kind::NPC_PROFILE, Some("ГМ"), p.clone(), None));
            p
        }
        Err(e) => {
            sink.emit(ev(
                event_kind::ERROR,
                Some("ГМ"),
                Value::String(e.clone()),
                None,
            ));
            json!({"status": "error", "error": e, "npc_id": arg_str(args, "npc_id")})
        }
    };
    let compact = crate::model_text::compact_npc_profile_payload(&payload);
    tool_result(
        &json_compact(&compact),
        Some(&model_npc_profile_text(&payload)),
        Some(tool_reminder("get_npc_profile")),
        false,
    )
}

fn run_advance_time(session: &mut Session, args: &Value, sink: &Sink) -> ToolExecutionResult {
    let minutes = args.get("minutes").cloned().unwrap_or(json!(0));
    let reason = arg_str(args, "reason").to_string();
    let payload = match session.world.advance_time(&minutes, &reason) {
        Ok(p) => {
            if p.get("ok").and_then(Value::as_bool).unwrap_or(false) {
                session.turn_time_advances.push(json!({
                    "minutes": p.get("elapsed_minutes").and_then(|v| v.as_i64()).unwrap_or(0),
                    "reason": p.get("reason").cloned().unwrap_or(json!("")),
                }));
            }
            sink.emit(ev(event_kind::TIME, Some("ГМ"), p.clone(), None));
            p
        }
        Err(e) => {
            let p = json!({"ok": false, "error": e});
            sink.emit(ev(event_kind::ERROR, Some("ГМ"), Value::String(e), None));
            p
        }
    };
    tool_result(
        &json_compact(&payload),
        Some(&model_time_text(&payload)),
        None,
        false,
    )
}

fn run_update_player_character(
    session: &mut Session,
    args: &Value,
    sink: &Sink,
) -> ToolExecutionResult {
    let payload = session.world.update_player_character(
        args.get("fields").unwrap_or(&Value::Null),
        arg_str(args, "reason"),
    );
    sink.emit(ev(
        event_kind::PLAYER_CHARACTER_UPDATE,
        Some("ГМ"),
        payload.clone(),
        None,
    ));
    tool_result(
        &json_compact(&payload),
        Some(&model_player_character_update_text(&payload)),
        Some(tool_reminder("update_player_character")),
        false,
    )
}

fn run_set_npc_whereabouts(
    session: &mut Session,
    args: &Value,
    sink: &Sink,
) -> ToolExecutionResult {
    match session.world.set_npc_whereabouts(
        arg_str(args, "npc_id"),
        arg_str(args, "location_id"),
        arg_str(args, "location_name"),
        arg_str(args, "status"),
        arg_str(args, "details"),
        arg_str(args, "source"),
    ) {
        Ok(payload) => {
            let player_payload = player_facing_payload(&session.world, &payload);
            sink.emit(ev(
                event_kind::NPC_WHEREABOUTS,
                Some("ГМ"),
                player_payload,
                None,
            ));
            tool_result(
                &json_compact(&payload),
                Some(&model_whereabouts_text(&payload)),
                Some(tool_reminder("set_npc_whereabouts")),
                false,
            )
        }
        Err(e) => {
            sink.emit(ev(
                event_kind::ERROR,
                Some("ГМ"),
                Value::String(e.clone()),
                None,
            ));
            tool_error(
                "set_npc_whereabouts",
                &e,
                None,
                "unknown_npc",
                &[("npc_id", json!(arg_str(args, "npc_id")))],
            )
        }
    }
}

fn run_move_npc(session: &mut Session, args: &Value, sink: &Sink) -> ToolExecutionResult {
    match session.world.set_npc_presence(
        arg_str(args, "npc_id"),
        args.get("present").map(crate::truthy).unwrap_or(false),
        arg_str(args, "location"),
        args.get("visible").map(crate::truthy).unwrap_or(true),
        args.get("can_hear").map(crate::truthy).unwrap_or(true),
        arg_str(args, "activity"),
        arg_str(args, "attitude"),
    ) {
        Ok(payload) => {
            let player_payload = player_facing_payload(&session.world, &payload);
            sink.emit(ev(
                event_kind::SCENE_UPDATE,
                Some("ГМ"),
                player_payload,
                None,
            ));
            tool_result(
                &json_compact(&payload),
                Some(&model_presence_text(&payload)),
                Some(tool_reminder("move_npc")),
                false,
            )
        }
        Err(e) => {
            sink.emit(ev(
                event_kind::ERROR,
                Some("ГМ"),
                Value::String(e.clone()),
                None,
            ));
            tool_error(
                "move_npc",
                &e,
                None,
                "unknown_npc",
                &[("npc_id", json!(arg_str(args, "npc_id")))],
            )
        }
    }
}

fn run_set_scene(session: &mut Session, args: &Value, sink: &Sink) -> ToolExecutionResult {
    use gml_world::canon::{engine, ids, travel, Action, ProposedAction, Scope, WorldCanon};
    use gml_world::helpers::{as_list, as_str, safe_id};
    use gml_world::NpcWhereabouts;

    session.world.ensure_npc_whereabouts();
    let title = nonempty_string(arg_str(args, "title").trim().to_string(), "Новая сцена");
    let description = nonempty_string(arg_str(args, "description").trim().to_string(), &title);
    let fallback_id = format!("scene_{}", set_scene_ord_sum(&title) % 100000);
    let dest_id = safe_id(
        &nonempty_string(arg_str(args, "location_id").trim().to_string(), &title),
        &fallback_id,
    );
    let reason = nonempty_string(arg_str(args, "reason").trim().to_string(), "set_scene");

    if session.world.world_canon.is_empty() {
        let seed = session.world.dice_seed.to_string();
        session.world.world_canon = WorldCanon::from_scene(&session.world.scene, &seed);
    }
    let from_place = session.world.world_canon.player_place_id.clone();

    if !from_place.is_empty() && from_place != dest_id {
        if let Some(existing) = session
            .world
            .world_canon
            .exits_from(&from_place)
            .into_iter()
            .find(|transition| {
                transition.to_place == dest_id
                    || (transition.to_place.is_empty()
                        && (set_scene_shell_text_matches(
                            &transition.destination_hint,
                            &dest_id,
                            &title,
                        ) || set_scene_shell_text_matches(&transition.label, &dest_id, &title)))
            })
        {
            let msg = format!(
                "set_scene cannot bypass existing transition '{}'; call move_player with that transition_id.",
                existing.transition_id
            );
            sink.emit(ev(
                event_kind::ERROR,
                Some("ГМ"),
                Value::String(msg.clone()),
                None,
            ));
            return tool_error(
                "set_scene",
                &msg,
                None,
                "use_move_player",
                &[("transition_id", json!(existing.transition_id))],
            );
        }
    }

    let present_raw = args.get("present_npcs").unwrap_or(&Value::Null);
    let mut present = BTreeSet::new();
    let mut dropped_present_npcs = Vec::new();
    for raw_id in as_list(present_raw) {
        let npc_id = safe_id(&as_str(&raw_id), "");
        if npc_id.is_empty() || !session.world.npcs.contains_key(&npc_id) {
            let raw_label = as_str(&raw_id);
            if !raw_label.is_empty() {
                dropped_present_npcs.push(raw_label);
            }
        } else {
            present.insert(npc_id);
        }
    }

    let coerced_items = coerce_set_scene_items(args.get("items").unwrap_or(&Value::Null));
    let coerced_exits = coerce_set_scene_exits(args.get("exits").unwrap_or(&Value::Null));
    let constraints: Vec<String> = as_list(args.get("constraints").unwrap_or(&Value::Null))
        .iter()
        .map(as_str)
        .filter(|text| !text.is_empty())
        .collect();
    let tension = arg_str(args, "tension").trim().to_string();

    let mut actions: Vec<(Action, String)> = Vec::new();
    if session.world.world_canon.place(&dest_id).is_some() {
        actions.push((
            Action::UpdatePlace {
                place_id: dest_id.clone(),
                name: title.clone(),
                kind: "scene".to_string(),
                description: description.clone(),
                features: Vec::new(),
                visited: true,
            },
            reason.clone(),
        ));
    } else {
        actions.push((
            Action::CreatePlace {
                place_id: dest_id.clone(),
                name: title.clone(),
                kind: "scene".to_string(),
                parent: String::new(),
                region_id: String::new(),
                description: description.clone(),
                features: Vec::new(),
                visited: true,
                shell: false,
            },
            reason.clone(),
        ));
    }

    let mut move_transition_id = String::new();
    if !from_place.is_empty() && from_place != dest_id {
        move_transition_id = ids::stable_id(
            &session.world.world_canon.world_seed,
            &from_place,
            "transition",
            &dest_id,
        );
        actions.push((
            Action::CreateTransition {
                transition_id: move_transition_id.clone(),
                from_place: from_place.clone(),
                to_place: dest_id.clone(),
                destination_hint: title.clone(),
                label: nonempty_string(title.clone(), "Переход"),
                kind: "scene".to_string(),
                visible: Some(true),
                passable: Some(true),
                blocked_by: String::new(),
                time_cost: travel::infer_time_cost("scene", &title, &title),
                risk: travel::infer_risk("scene", &title, &title),
            },
            "set_scene transition".to_string(),
        ));

        let back_id = ids::stable_id(
            &session.world.world_canon.world_seed,
            &dest_id,
            "transition",
            &from_place,
        );
        if !session.world.world_canon.transitions.contains_key(&back_id) {
            let back_label = session
                .world
                .world_canon
                .place(&from_place)
                .map(|place| place.name.clone())
                .unwrap_or_else(|| "Назад".to_string());
            actions.push((
                Action::CreateTransition {
                    transition_id: back_id,
                    from_place: dest_id.clone(),
                    to_place: from_place.clone(),
                    destination_hint: String::new(),
                    label: nonempty_string(back_label.clone(), "Назад"),
                    kind: "back".to_string(),
                    visible: Some(true),
                    passable: Some(true),
                    blocked_by: String::new(),
                    time_cost: travel::infer_time_cost("back", &back_label, ""),
                    risk: travel::infer_risk("back", &back_label, ""),
                },
                "set_scene return path".to_string(),
            ));
        }
    }

    let mut planned_transition_ids = BTreeSet::new();
    for exit in &coerced_exits {
        let base = if exit.exit_id.is_empty() {
            safe_id(&exit.name, "exit")
        } else {
            exit.exit_id.clone()
        };
        let mut transition_id = format!("{dest_id}_{base}");
        let mut n = 2;
        while session
            .world
            .world_canon
            .transitions
            .contains_key(&transition_id)
            || planned_transition_ids.contains(&transition_id)
        {
            transition_id = format!("{dest_id}_{base}_{n}");
            n += 1;
        }
        planned_transition_ids.insert(transition_id.clone());
        actions.push((
            Action::CreateTransition {
                transition_id,
                from_place: dest_id.clone(),
                to_place: String::new(),
                destination_hint: exit.destination.clone(),
                label: exit.name.clone(),
                kind: String::new(),
                visible: Some(exit.visible),
                passable: Some(exit.blocked_by.is_empty()),
                blocked_by: exit.blocked_by.clone(),
                time_cost: travel::infer_time_cost("", &exit.name, &exit.destination),
                risk: travel::infer_risk("", &exit.name, &exit.destination),
            },
            "set_scene exit".to_string(),
        ));
    }

    for npc_id in &present {
        let npc = &session.world.npcs[npc_id];
        if session.world.world_canon.actor(npc_id).is_some() {
            actions.push((
                Action::MoveActor {
                    actor_id: npc_id.clone(),
                    to_place: dest_id.clone(),
                },
                "set_scene present npc".to_string(),
            ));
        } else {
            actions.push((
                Action::CreateActor {
                    actor_id: npc_id.clone(),
                    public_label: if npc.public_label.is_empty() {
                        npc.name.clone()
                    } else {
                        npc.public_label.clone()
                    },
                    place_id: dest_id.clone(),
                    role: npc.role.clone(),
                    faction_id: String::new(),
                },
                "set_scene present npc".to_string(),
            ));
        }
    }

    if !move_transition_id.is_empty() {
        actions.push((
            Action::MovePlayer {
                transition_id: move_transition_id.clone(),
            },
            reason.clone(),
        ));
    }

    let mut effects = vec![
        format!("authored_place:{dest_id}"),
        format!("player_at:{dest_id}"),
    ];
    if !present.is_empty() {
        effects.push(format!(
            "present:{}",
            present.iter().cloned().collect::<Vec<_>>().join(",")
        ));
    }
    actions.push((
        Action::CreateEvent {
            kind: "set_scene".to_string(),
            place_id: dest_id.clone(),
            actors: present.iter().cloned().collect(),
            causes: vec![reason.clone()],
            effects,
            visible_to_player: true,
            scope: Scope::Player,
            traces: Vec::new(),
        },
        "set_scene authored place + move".to_string(),
    ));

    let mut staged = session.world.world_canon.clone();
    let mut committed_events = Vec::new();
    for (action, action_reason) in actions {
        let mut proposed = ProposedAction::new(action, "gm", &action_reason);
        proposed.scope = Scope::Player;
        match engine::apply(&mut staged, &proposed, session.turn) {
            Ok(mut events) => committed_events.append(&mut events),
            Err(rejection) => {
                let msg = format!("set_scene rejected: {}", rejection.reason);
                sink.emit(ev(
                    event_kind::ERROR,
                    Some("ГМ"),
                    Value::String(msg.clone()),
                    None,
                ));
                return tool_error(
                    "set_scene",
                    &msg,
                    None,
                    &rejection.code,
                    &[("place_id", json!(dest_id))],
                );
            }
        }
    }

    let before_time = session.world.time_export();
    let elapsed_minutes = committed_events
        .iter()
        .find(|event| event.kind == "move_player")
        .and_then(|event| {
            event.effects.iter().find_map(|effect| {
                effect
                    .strip_prefix("elapsed_minutes:")
                    .and_then(|raw| raw.parse::<i64>().ok())
            })
        })
        .unwrap_or(0);
    session.world.world_canon = staged;
    session.world.time.absolute_minutes = session.world.world_canon.clock_minutes;
    session.world.time.last_advance_minutes = elapsed_minutes;
    session.world.time.last_advance_reason = reason.clone();
    if elapsed_minutes > 0 {
        session.turn_time_advances.push(json!({
            "minutes": elapsed_minutes,
            "reason": reason,
        }));
        sink.emit(ev(
            event_kind::TIME,
            Some("ГМ"),
            json!({
                "before": before_time,
                "after": session.world.time_export(),
                "minutes": elapsed_minutes,
                "reason": session.world.time.last_advance_reason,
            }),
            None,
        ));
    }

    session.world.scene.scene_id = dest_id.clone();
    session.world.scene.items = coerced_items;
    session.world.scene.constraints = constraints;
    session.world.scene.tension = tension;
    session.world.scene.player_seen = vec![description];
    session.world.refresh_scene_from_canon();
    for npc_id in &present {
        session.world.npc_whereabouts.insert(
            npc_id.clone(),
            NpcWhereabouts {
                npc_id: npc_id.clone(),
                location_id: dest_id.clone(),
                location_name: title.clone(),
                status: "present".to_string(),
                details: "в текущей сцене".to_string(),
                source: "set_scene".to_string(),
            },
        );
    }

    let mut payload = session.world.scene_export();
    if !dropped_present_npcs.is_empty() {
        if let Value::Object(ref mut m) = payload {
            m.insert(
                "dropped_present_npcs".to_string(),
                json!(dropped_present_npcs),
            );
            m.insert(
                "repair_hint".to_string(),
                json!(format!(
                    "Ignored unknown present_npcs ids: {}. Use npc_ids from the current roster in CURRENT TURN CONTEXT.",
                    m.get("dropped_present_npcs")
                        .and_then(Value::as_array)
                        .map(|items| items.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "))
                        .unwrap_or_default()
                )),
            );
        }
    }
    if let Some(hint) = payload.get("repair_hint").and_then(Value::as_str) {
        if !hint.is_empty() {
            sink.emit(ev(
                event_kind::ERROR,
                Some("ГМ"),
                Value::String(hint.to_string()),
                None,
            ));
        }
    }
    sink.emit(ev(
        event_kind::SCENE_UPDATE,
        Some("ГМ"),
        payload.clone(),
        None,
    ));
    tool_result(
        &json_compact(&payload),
        Some(&model_scene_text(&payload)),
        Some(tool_reminder("set_scene")),
        false,
    )
}

fn set_scene_ord_sum(s: &str) -> u64 {
    s.chars().map(|c| c as u64).sum()
}

fn set_scene_shell_text_matches(text: &str, dest_id: &str, title: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }
    trimmed.eq_ignore_ascii_case(dest_id)
        || trimmed.eq_ignore_ascii_case(title.trim())
        || gml_world::helpers::safe_id(trimmed, "") == dest_id
}

fn set_scene_bool(value: Option<&Value>, default: bool) -> bool {
    match value {
        Some(Value::Bool(value)) => *value,
        Some(Value::Number(number)) => number.as_i64().map(|value| value != 0).unwrap_or(default),
        Some(Value::String(text)) => {
            let lower = text.trim().to_lowercase();
            if matches!(lower.as_str(), "" | "false" | "0" | "no" | "нет") {
                false
            } else if matches!(lower.as_str(), "true" | "1" | "yes" | "да") {
                true
            } else {
                default
            }
        }
        Some(Value::Null) | None => default,
        Some(_) => default,
    }
}

fn coerce_set_scene_items(raw: &Value) -> Vec<gml_world::SceneItem> {
    use gml_world::helpers::{as_list, as_str, get_str, safe_id};

    let mut items = Vec::new();
    for (idx, item) in as_list(raw).iter().enumerate() {
        let number = idx + 1;
        match item {
            Value::Object(map) => {
                let name = nonempty_string(get_str(map, "name"), &format!("предмет {number}"));
                items.push(gml_world::SceneItem {
                    item_id: safe_id(&get_str(map, "id"), &format!("item_{number}")),
                    name,
                    location: nonempty_string(get_str(map, "location"), "в сцене"),
                    visible: set_scene_bool(map.get("visible"), true),
                    portable: set_scene_bool(map.get("portable"), false),
                    owner: get_str(map, "owner"),
                    details: get_str(map, "details"),
                });
            }
            other => {
                let name = as_str(other);
                if !name.is_empty() {
                    items.push(gml_world::SceneItem {
                        item_id: safe_id(&name, &format!("item_{number}")),
                        name,
                        location: "в сцене".to_string(),
                        visible: true,
                        portable: false,
                        owner: String::new(),
                        details: String::new(),
                    });
                }
            }
        }
    }
    items
}

fn coerce_set_scene_exits(raw: &Value) -> Vec<gml_world::SceneExit> {
    use gml_world::helpers::{as_list, as_str, get_str, safe_id};

    let mut exits = Vec::new();
    for (idx, exit) in as_list(raw).iter().enumerate() {
        let number = idx + 1;
        match exit {
            Value::Object(map) => {
                let name = nonempty_string(get_str(map, "name"), &format!("выход {number}"));
                exits.push(gml_world::SceneExit {
                    exit_id: safe_id(&get_str(map, "id"), &format!("exit_{number}")),
                    name,
                    destination: nonempty_string(
                        get_str(map, "destination"),
                        "неизвестное направление",
                    ),
                    visible: set_scene_bool(map.get("visible"), true),
                    blocked_by: get_str(map, "blocked_by"),
                });
            }
            other => {
                let name = as_str(other);
                if !name.is_empty() {
                    exits.push(gml_world::SceneExit {
                        exit_id: safe_id(&name, &format!("exit_{number}")),
                        name,
                        destination: "unknown destination".to_string(),
                        visible: true,
                        blocked_by: String::new(),
                    });
                }
            }
        }
    }
    exits
}

// =========================================================================
// Canon / living-world tools (LOCKED DECISIONS #2/#3). These route through the
// validator-gated engine on `session.world.world_canon`; on a successful canon
// mutation the live scene is rebuilt FROM the canon so scene_export and the GM
// scene_context reflect canon.player_place_id, not a stale legacy scene.
// =========================================================================

/// `move_player(transition_id, reason)` — commit a player traversal through the
/// canon engine. An invalid move (unknown transition, not starting at the
/// player's place, hidden, or blocked) is REJECTED and mutates NOTHING. On
/// success the canon is mutated, the live scene is rebuilt from the canon, and
/// the committed canon events are summarised back.
async fn run_move_player(session: &mut Session, args: &Value, sink: &Sink) -> ToolExecutionResult {
    use gml_world::canon::{engine, Action, ProposedAction};

    let transition_id = arg_str(args, "transition_id").trim().to_string();
    let reason = arg_str(args, "reason").trim().to_string();

    if transition_id.is_empty() {
        let msg = "move_player requires a non-empty `transition_id` naming a visible exit \
from the player's current place.";
        sink.emit(ev(
            event_kind::ERROR,
            Some("ГМ"),
            Value::String(msg.to_string()),
            None,
        ));
        return tool_error("move_player", msg, None, "missing_transition_id", &[]);
    }

    let proposed = ProposedAction::new(
        Action::MovePlayer {
            transition_id: transition_id.clone(),
        },
        "gm",
        &reason,
    );

    match engine::apply(&mut session.world.world_canon, &proposed, session.turn) {
        Ok(events) => {
            // LOCKED DECISION #2: rebuild the live scene FROM the canon.
            let before_time = session.world.time_export();
            let elapsed_minutes = events
                .iter()
                .find(|e| e.kind == "move_player")
                .and_then(|e| {
                    e.effects.iter().find_map(|effect| {
                        effect
                            .strip_prefix("elapsed_minutes:")
                            .and_then(|raw| raw.parse::<i64>().ok())
                    })
                })
                .unwrap_or_else(|| {
                    let before = session.world.time.absolute_minutes;
                    (session.world.world_canon.clock_minutes - before).max(0)
                });
            session.world.time.absolute_minutes = session.world.world_canon.clock_minutes;
            session.world.time.last_advance_minutes = elapsed_minutes;
            session.world.time.last_advance_reason = if reason.is_empty() {
                "travel".to_string()
            } else {
                reason.clone()
            };
            session
                .world
                .spread_rumors_on_transition("player", &transition_id, elapsed_minutes);
            let after_time = session.world.time_export();
            if elapsed_minutes > 0 {
                let advance_reason = session.world.time.last_advance_reason.clone();
                session.turn_time_advances.push(json!({
                    "minutes": elapsed_minutes,
                    "reason": advance_reason,
                }));
                sink.emit(ev(
                    event_kind::TIME,
                    Some("ГМ"),
                    json!({
                        "ok": true,
                        "elapsed_minutes": elapsed_minutes,
                        "reason": session.world.time.last_advance_reason.clone(),
                        "before": before_time,
                        "current": after_time,
                        "summary": session.world.time_summary(),
                    }),
                    None,
                ));
            }
            session.world.refresh_scene_from_canon();
            let generated_situation =
                auto_generate_travel_situation(session, &events, &transition_id, &reason, sink)
                    .await;
            session.world.refresh_scene_from_canon();
            let place_id = session.world.world_canon.player_place_id.clone();
            let title = session
                .world
                .world_canon
                .place(&place_id)
                .map(|p| p.name.clone())
                .unwrap_or_default();
            let effects: Vec<String> = events
                .iter()
                .flat_map(|e| e.effects.iter().cloned())
                .collect();
            let payload = json!({
                "ok": true,
                "status": "moved",
                "transition_id": transition_id,
                "place_id": place_id,
                "title": title,
                "events": events.len(),
                "elapsed_minutes": elapsed_minutes,
                "clock_minutes": session.world.world_canon.clock_minutes,
                "effects": effects,
                "generated_situation": generated_situation,
            });
            sink.emit(ev(
                event_kind::SCENE_UPDATE,
                Some("ГМ"),
                session.world.scene_export(),
                None,
            ));
            let model = format!(
                "Игрок перешёл в '{}' ({}); зафиксировано событий: {}.",
                if title.is_empty() {
                    place_id.as_str()
                } else {
                    title.as_str()
                },
                place_id,
                events.len(),
            );
            tool_result(
                &json_compact(&payload),
                Some(&model),
                Some(tool_reminder("move_player")),
                false,
            )
        }
        Err(rejection) => {
            let msg = format!("move rejected: {} ({})", rejection.reason, rejection.code);
            sink.emit(ev(
                event_kind::ERROR,
                Some("ГМ"),
                Value::String(msg.clone()),
                None,
            ));
            tool_error(
                "move_player",
                &msg,
                None,
                &rejection.code,
                &[("transition_id", json!(transition_id))],
            )
        }
    }
}

/// `world_debug(causal_log_only)` — read-only canon debug/replay dump (TZ §12).
fn run_world_debug(session: &mut Session, args: &Value, sink: &Sink) -> ToolExecutionResult {
    use gml_world::canon::engine;

    let causal_only = args
        .get("causal_log_only")
        .map(crate::truthy)
        .unwrap_or(false);
    let canon = &session.world.world_canon;
    let causal = engine::causal_log(canon);

    let payload = if causal_only {
        json!({"ok": true, "status": "debug", "causal_log": causal})
    } else {
        json!({
            "ok": true,
            "status": "debug",
            "canon": engine::debug_dump(canon),
            "causal_log": causal,
        })
    };

    sink.emit(ev(
        event_kind::WORLD_DEBUG,
        Some("ГМ"),
        json!({"world_debug": true, "causal_log": causal}),
        None,
    ));
    let model = format!(
        "Снимок канона: мест={}, переходов={}, актёров={}, событий={}.",
        canon.places.len(),
        canon.transitions.len(),
        canon.actors.len(),
        canon.event_log.events.len(),
    );
    tool_result(&json_compact(&payload), Some(&model), None, false)
}

fn effect_suffix<'a>(effect: &'a str, prefix: &str) -> Option<&'a str> {
    effect.strip_prefix(prefix).map(str::trim)
}

fn event_effect_str(events: &[gml_world::canon::CanonEvent], prefix: &str) -> String {
    events
        .iter()
        .flat_map(|event| event.effects.iter())
        .find_map(|effect| effect_suffix(effect, prefix))
        .unwrap_or("")
        .to_string()
}

fn event_effect_i64(events: &[gml_world::canon::CanonEvent], prefix: &str) -> i64 {
    event_effect_str(events, prefix).parse::<i64>().unwrap_or(0)
}

async fn auto_generate_travel_situation(
    session: &mut Session,
    events: &[gml_world::canon::CanonEvent],
    route_transition_id: &str,
    reason: &str,
    sink: &Sink,
) -> Option<Value> {
    let travel_event = events
        .iter()
        .find(|event| event.kind == "travel_situation")?;
    let place_id = travel_event.place_id.trim().to_string();
    if place_id.is_empty() {
        return None;
    }

    let situation_type = event_effect_str(events, "situation_type:");
    let rarity = event_effect_str(events, "rarity:");
    let elapsed_minutes = event_effect_i64(events, "elapsed_minutes:");
    let remaining_minutes = event_effect_i64(events, "remaining_minutes:");
    let roll = event_effect_i64(events, "roll:");
    let chance = event_effect_i64(events, "chance_percent:");
    let visible_seed = travel_event
        .effects
        .iter()
        .filter(|effect| !effect.contains(':') || effect.starts_with("На дороге"))
        .cloned()
        .collect::<Vec<_>>()
        .join("; ");
    let request = format!(
        "Сгенерируй содержимое дорожной ситуации для уже созданной точки {place_id}. \
         Исходный бросок: тип={situation_type}, редкость={rarity}, шанс={chance}%, \
         выпало={roll}, прошло={elapsed_minutes} мин., осталось={remaining_minutes} мин. \
         Видимая завязка: {visible_seed}. Причина перехода: {reason}. \
         Дай игроку конкретные зацепки и варианты взаимодействия, но скрытую правду держи в hidden-полях."
    );
    let generator_args = json!({
        "purpose": "travel_situation",
        "request": request,
        "target_place_id": place_id.clone(),
        "route_transition_id": route_transition_id,
        "commit": true,
        "elapsed_minutes": elapsed_minutes,
        "remaining_minutes": remaining_minutes,
        "route_time_minutes": elapsed_minutes + remaining_minutes,
        "situation_type": situation_type,
        "rarity": rarity,
    });
    let result = run_generate_location(session, &generator_args, sink).await;
    let parsed = serde_json::from_str::<Value>(&result.full).unwrap_or_else(|_| {
        json!({
            "ok": false,
            "status": "unparsed_generator_result",
        })
    });
    Some(json!({
        "ok": parsed.get("ok").and_then(Value::as_bool).unwrap_or(true),
        "place_id": place_id,
        "applied": parsed.get("applied").cloned().unwrap_or(Value::Null),
    }))
}

async fn run_generate_location(
    session: &mut Session,
    args: &Value,
    sink: &Sink,
) -> ToolExecutionResult {
    let purpose = arg_str(args, "purpose").trim();
    let request = arg_str(args, "request").trim();
    if purpose.is_empty() || request.is_empty() {
        let msg = "generate_location requires non-empty `purpose` and `request`.";
        sink.emit(ev(
            event_kind::ERROR,
            Some("ГМ"),
            Value::String(msg.to_string()),
            None,
        ));
        return tool_error(
            "generate_location",
            msg,
            None,
            "missing_generator_request",
            &[],
        );
    }
    const ALLOWED_PURPOSES: &[&str] = &[
        "place",
        "local_place",
        "room",
        "travel_situation",
        "city_point",
        "village_point",
        "dungeon_point",
    ];
    if !ALLOWED_PURPOSES.contains(&purpose) {
        let msg = format!("generate_location purpose is not supported: {purpose}");
        sink.emit(ev(
            event_kind::ERROR,
            Some("ГМ"),
            Value::String(msg.clone()),
            None,
        ));
        return tool_error(
            "generate_location",
            &msg,
            None,
            "unsupported_generator_purpose",
            &[],
        );
    }

    let current_place_id = session.world.world_canon.player_place_id.clone();
    let target_place_id = arg_str(args, "target_place_id").trim().to_string();
    let parent_place_id = arg_str(args, "parent_place_id").trim().to_string();
    let route_transition_id = arg_str(args, "route_transition_id").trim().to_string();
    let commit = args.get("commit").map(crate::truthy).unwrap_or(true);
    let transition = if route_transition_id.is_empty() {
        None
    } else {
        session.world.world_canon.transition(&route_transition_id)
    };
    let road_risk = transition.map(|t| t.risk.clone()).unwrap_or_default();
    let route_time_minutes = args
        .get("route_time_minutes")
        .and_then(Value::as_i64)
        .or_else(|| transition.map(|t| t.time_cost))
        .unwrap_or(0);
    let request_payload = json!({
        "purpose": purpose,
        "request": request,
        "commit": commit,
        "target_place_id": target_place_id,
        "parent_place_id": parent_place_id,
        "current_place_id": current_place_id,
        "route_transition_id": route_transition_id,
        "elapsed_minutes": args.get("elapsed_minutes").and_then(Value::as_i64).unwrap_or(0),
        "remaining_minutes": args.get("remaining_minutes").and_then(Value::as_i64).unwrap_or(0),
        "route_time_minutes": route_time_minutes,
        "situation_type": arg_str(args, "situation_type"),
        "rarity": arg_str(args, "rarity"),
        "road_risk": road_risk,
    });

    let client = session.ensure_location_generator_client();
    let recent = session.location_generator_anti_repeat.clone();
    let history = session.location_generator_messages.clone();
    let generated = match gml_agents::generate_location(
        client.as_ref(),
        &mut session.world,
        &request_payload,
        &recent,
        &history,
    )
    .await
    {
        Ok(data) => data,
        Err(e) => {
            sink.emit(ev(
                event_kind::ERROR,
                Some("location_generator"),
                Value::String(format!("Location generator failed: {e}")),
                None,
            ));
            return tool_error(
                "generate_location",
                &format!("Location generator failed: {e}"),
                None,
                "generator_failed",
                &[],
            );
        }
    };
    session.location_generator_client = Some(client);
    session.remember_location_generator_client();

    let generated_value = Value::Object(generated.clone());
    session.record_location_generator_exchange(&request_payload, &generated_value);
    let applied = if commit {
        commit_generated_location(session, args, &generated_value)
    } else {
        json!({"ok": true, "status": "preview", "committed": false})
    };
    let applied_ok = applied.get("ok").and_then(Value::as_bool).unwrap_or(true);
    if applied_ok || !commit {
        if let Some(key) = generated
            .get("anti_repeat_key")
            .and_then(Value::as_str)
            .filter(|key| !key.trim().is_empty())
        {
            session.note_location_anti_repeat_key(key);
        }
    }

    let payload = json!({
        "ok": applied_ok,
        "status": "generated",
        "committed": commit && applied_ok,
        "request": request_payload,
        "generated": generated_value,
        "applied": applied,
    });
    let player_observed = payload
        .get("applied")
        .and_then(|v| v.get("player_observed"))
        .and_then(Value::as_bool)
        .unwrap_or(!commit);
    let public_payload = public_generate_location_payload(&payload, player_observed);
    if commit && !applied_ok {
        let code = payload
            .get("applied")
            .and_then(|v| v.get("code"))
            .and_then(Value::as_str)
            .unwrap_or("rejected");
        let reason = payload
            .get("applied")
            .and_then(|v| v.get("reason"))
            .and_then(Value::as_str)
            .unwrap_or("location commit rejected");
        sink.emit(ev(
            event_kind::ERROR,
            Some("location_generator"),
            Value::String(format!("Location commit rejected ({code}): {reason}")),
            None,
        ));
    }
    sink.emit(ev(
        event_kind::WORLD_STATE_UPDATE,
        Some("location_generator"),
        public_payload.clone(),
        None,
    ));
    if commit && applied_ok {
        session.world.refresh_scene_from_canon();
        sink.emit(ev(
            event_kind::SCENE_UPDATE,
            Some("location_generator"),
            session.world.scene_export(),
            None,
        ));
    }
    let name = payload
        .get("generated")
        .and_then(|v| v.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("локация");
    let model_message = if applied_ok {
        format!("Генератор подготовил: {name}.")
    } else {
        "Генератор подготовил локацию, но канон отклонил коммит.".to_string()
    };
    tool_result(
        &json_compact(&public_payload),
        Some(&model_message),
        Some(tool_reminder("generate_location")),
        false,
    )
}

fn public_generate_location_payload(payload: &Value, player_observed: bool) -> Value {
    let mut public = payload.clone();
    if let Value::Object(ref mut root) = public {
        if let Some(generated) = root.get("generated").cloned() {
            root.insert(
                "generated".to_string(),
                public_generated_location(generated, player_observed),
            );
        }
        if let Some(applied) = root.get("applied").cloned() {
            root.insert(
                "applied".to_string(),
                public_generated_location_applied(applied, player_observed),
            );
        }
    }
    public
}

fn public_generated_location(generated: Value, player_observed: bool) -> Value {
    let Value::Object(map) = generated else {
        return generated;
    };
    if !player_observed {
        return json!({
            "redacted": true,
            "reason": "not_discovered_by_player",
        });
    }
    let mut public = Map::new();
    for key in [
        "name",
        "kind",
        "visible_summary",
        "description",
        "features",
        "sensory_details",
        "choices",
        "consequences",
        "transitions",
    ] {
        if let Some(value) = map.get(key) {
            public.insert(key.to_string(), value.clone());
        }
    }
    Value::Object(public)
}

fn public_generated_location_applied(applied: Value, player_observed: bool) -> Value {
    let Value::Object(map) = applied else {
        return applied;
    };
    let mut public = Map::new();
    for key in ["ok", "status", "committed", "code", "reason"] {
        if let Some(value) = map.get(key) {
            public.insert(key.to_string(), value.clone());
        }
    }
    if player_observed {
        for key in [
            "place_id",
            "name",
            "kind",
            "event_id",
            "event_seq",
            "transitions_added",
            "entry_transition_id",
            "entered",
            "current_place_id",
        ] {
            if let Some(value) = map.get(key) {
                public.insert(key.to_string(), value.clone());
            }
        }
    } else if map.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        public.insert("redacted".to_string(), Value::Bool(true));
    }
    Value::Object(public)
}

fn generated_str(generated: &Value, key: &str) -> String {
    generated
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string()
}

fn generated_string_list(generated: &Value, key: &str) -> Vec<String> {
    generated
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn generated_i64(item: &Value, key: &str) -> i64 {
    item.get(key).and_then(Value::as_i64).unwrap_or(0)
}

fn nonempty_string(primary: String, fallback: &str) -> String {
    if primary.trim().is_empty() {
        fallback.trim().to_string()
    } else {
        primary
    }
}

fn commit_generated_location(session: &mut Session, args: &Value, generated: &Value) -> Value {
    use gml_world::canon::{
        engine, ids, travel, Action, MemoryInjectionState, MemoryTier, MemoryTruthStatus,
        MemoryUnit, ProposedAction, Scope,
    };

    let purpose = arg_str(args, "purpose").trim().to_string();
    let request = arg_str(args, "request").trim().to_string();
    let current_place_id = session.world.world_canon.player_place_id.clone();
    let explicit_target = arg_str(args, "target_place_id").trim().to_string();
    let explicit_parent = arg_str(args, "parent_place_id").trim().to_string();
    let player_observed_requested = args
        .get("player_observed")
        .map(crate::truthy)
        .unwrap_or(false);
    let enter_after_commit = args
        .get("enter_after_commit")
        .map(crate::truthy)
        .unwrap_or(false);
    let parent_place_id = if !explicit_parent.is_empty() {
        explicit_parent
    } else {
        current_place_id.clone()
    };
    let world_seed = if session.world.world_canon.world_seed.is_empty() {
        session.world.dice_seed.to_string()
    } else {
        session.world.world_canon.world_seed.clone()
    };

    let name = nonempty_string(generated_str(generated, "name"), "Безымянное место");
    let kind = nonempty_string(generated_str(generated, "kind"), "generated_place");
    let visible_summary = generated_str(generated, "visible_summary");
    let description = nonempty_string(generated_str(generated, "description"), &visible_summary);
    let hidden_summary = generated_str(generated, "hidden_summary");
    let memory_note = nonempty_string(generated_str(generated, "memory_note"), &visible_summary);
    let anti_repeat_key = generated_str(generated, "anti_repeat_key");
    let mut features = generated_string_list(generated, "features");
    for detail in generated_string_list(generated, "sensory_details") {
        if !features.contains(&detail) {
            features.push(detail);
        }
    }

    let place_id = if !explicit_target.is_empty() {
        explicit_target
    } else {
        ids::stable_id(
            &world_seed,
            if parent_place_id.is_empty() {
                "world"
            } else {
                &parent_place_id
            },
            "generated_place",
            &format!("{purpose}|{name}|{anti_repeat_key}|{request}"),
        )
    };

    let created_place = session.world.world_canon.place(&place_id).is_none();
    let mut transitions_added: Vec<String> = Vec::new();
    let region_id = session
        .world
        .world_canon
        .place(&parent_place_id)
        .map(|p| p.region_id.clone())
        .unwrap_or_default();
    let mut actions: Vec<(Action, String)> = Vec::new();
    let existing_was_visited = session
        .world
        .world_canon
        .place(&place_id)
        .map(|place| place.is_visited())
        .unwrap_or(false);
    let player_observed = enter_after_commit
        || player_observed_requested
        || purpose == "travel_situation"
        || place_id == current_place_id
        || existing_was_visited;
    let event_scope = if player_observed {
        Scope::Player
    } else {
        Scope::GmPrivate
    };
    let visited = enter_after_commit
        || purpose == "travel_situation"
        || place_id == current_place_id
        || existing_was_visited;
    if created_place {
        actions.push((
            Action::CreatePlace {
                place_id: place_id.clone(),
                name: name.clone(),
                kind: kind.clone(),
                parent: parent_place_id.clone(),
                region_id,
                description: description.clone(),
                features: features.clone(),
                visited,
                shell: false,
            },
            request.clone(),
        ));
    } else {
        actions.push((
            Action::UpdatePlace {
                place_id: place_id.clone(),
                name: name.clone(),
                kind: kind.clone(),
                description: description.clone(),
                features: features.clone(),
                visited,
            },
            request.clone(),
        ));
    }

    let canon = &session.world.world_canon;
    let mut planned_transition_ids = BTreeSet::new();
    let mut entry_transition_id = if !parent_place_id.is_empty()
        && parent_place_id != place_id
        && canon.place(&parent_place_id).is_some()
    {
        canon
            .exits_from(&parent_place_id)
            .iter()
            .find(|transition| transition.to_place == place_id)
            .map(|transition| transition.transition_id.clone())
    } else {
        None
    };
    if !parent_place_id.is_empty()
        && parent_place_id != place_id
        && canon.place(&parent_place_id).is_some()
        && !canon
            .exits_from(&parent_place_id)
            .iter()
            .any(|t| t.to_place == place_id)
    {
        let forward_id = ids::stable_id(&world_seed, &parent_place_id, "transition", &place_id);
        planned_transition_ids.insert(forward_id.clone());
        transitions_added.push(forward_id.clone());
        entry_transition_id = Some(forward_id.clone());
        actions.push((
            Action::CreateTransition {
                transition_id: forward_id,
                from_place: parent_place_id.clone(),
                to_place: place_id.clone(),
                destination_hint: name.clone(),
                label: format!("К {name}"),
                kind: "path".to_string(),
                visible: Some(true),
                passable: Some(true),
                blocked_by: String::new(),
                time_cost: travel::infer_time_cost("path", &name, ""),
                risk: travel::infer_risk("path", &name, ""),
            },
            "link generated place".to_string(),
        ));
    }

    if !parent_place_id.is_empty()
        && parent_place_id != place_id
        && canon.place(&parent_place_id).is_some()
        && !canon
            .exits_from(&place_id)
            .iter()
            .any(|t| t.to_place == parent_place_id)
    {
        let back_id = ids::stable_id(&world_seed, &place_id, "transition", &parent_place_id);
        planned_transition_ids.insert(back_id.clone());
        transitions_added.push(back_id.clone());
        actions.push((
            Action::CreateTransition {
                transition_id: back_id,
                from_place: place_id.clone(),
                to_place: parent_place_id.clone(),
                destination_hint: String::new(),
                label: "Назад".to_string(),
                kind: "back".to_string(),
                visible: Some(true),
                passable: Some(true),
                blocked_by: String::new(),
                time_cost: travel::infer_time_cost("back", "Назад", ""),
                risk: travel::infer_risk("back", "Назад", ""),
            },
            "return from generated place".to_string(),
        ));
    }

    if let Some(items) = generated.get("transitions").and_then(Value::as_array) {
        for (idx, item) in items.iter().enumerate() {
            let label = generated_str(item, "label");
            if label.is_empty() {
                continue;
            }
            let destination_hint = generated_str(item, "destination_hint");
            let back_to_parent = if !parent_place_id.is_empty() && parent_place_id != place_id {
                let haystack = format!("{label} {destination_hint}").to_lowercase();
                let parent_id = parent_place_id.to_lowercase();
                let parent_name = canon
                    .place(&parent_place_id)
                    .map(|place| place.name.to_lowercase())
                    .unwrap_or_default();
                haystack.contains("назад")
                    || haystack.contains("обратно")
                    || haystack.contains("возврат")
                    || haystack.contains(&parent_id)
                    || (!parent_name.is_empty() && haystack.contains(&parent_name))
            } else {
                false
            };
            if back_to_parent {
                continue;
            }
            let edge_kind = nonempty_string(generated_str(item, "kind"), "path");
            let edge_id = ids::stable_id(
                &world_seed,
                &place_id,
                "transition",
                &format!("{idx}|{label}|{destination_hint}"),
            );
            if canon.transitions.contains_key(&edge_id) || planned_transition_ids.contains(&edge_id)
            {
                continue;
            }
            planned_transition_ids.insert(edge_id.clone());
            transitions_added.push(edge_id.clone());
            actions.push((
                Action::CreateTransition {
                    transition_id: edge_id,
                    from_place: place_id.clone(),
                    to_place: String::new(),
                    destination_hint,
                    label,
                    kind: edge_kind,
                    visible: Some(true),
                    passable: Some(true),
                    blocked_by: String::new(),
                    time_cost: generated_i64(item, "time_cost_minutes"),
                    risk: generated_str(item, "risk"),
                },
                "generated exit hook".to_string(),
            ));
        }
    }

    if enter_after_commit && place_id != current_place_id {
        if parent_place_id != current_place_id {
            return json!({
                "ok": false,
                "status": "rejected",
                "committed": false,
                "code": "cannot_enter_from_non_current_parent",
                "reason": "enter_after_commit requires the generated place to be linked from the player's current place",
                "place_id": place_id,
            });
        }
        if entry_transition_id.is_none() {
            return json!({
                "ok": false,
                "status": "rejected",
                "committed": false,
                "code": "missing_entry_transition",
                "reason": "enter_after_commit requires a passable transition from the current place to the generated place",
                "place_id": place_id,
            });
        }
    }

    let mut effects = vec![
        format!("place:{place_id}"),
        format!("name:{name}"),
        format!("kind:{kind}"),
        format!("features:{}", features.len()),
    ];
    if !transitions_added.is_empty() {
        effects.push(format!("transitions:{}", transitions_added.join(",")));
    }
    if enter_after_commit {
        effects.push(format!("player_enters:{place_id}"));
    }
    if let Some(entry_transition_id) = entry_transition_id
        .as_ref()
        .filter(|_| enter_after_commit && place_id != current_place_id)
    {
        actions.push((
            Action::MovePlayer {
                transition_id: entry_transition_id.clone(),
            },
            format!("enter generated place: {name}"),
        ));
    }
    actions.push((
        Action::CreateEvent {
            kind: "generate_location".to_string(),
            place_id: place_id.clone(),
            actors: Vec::new(),
            causes: vec![purpose.clone()],
            effects,
            visible_to_player: player_observed,
            scope: event_scope.clone(),
            traces: features.iter().take(3).cloned().collect(),
        },
        request.clone(),
    ));

    let mut staged = session.world.world_canon.clone();
    let mut committed_events = Vec::new();
    for (action, reason) in actions {
        let mut proposed = ProposedAction::new(action, "location_generator", &reason);
        proposed.scope = event_scope.clone();
        match engine::apply(&mut staged, &proposed, session.turn) {
            Ok(mut events) => committed_events.append(&mut events),
            Err(rejection) => {
                return json!({
                    "ok": false,
                    "status": "rejected",
                    "committed": false,
                    "code": rejection.code,
                    "reason": rejection.reason,
                    "place_id": place_id,
                });
            }
        }
    }

    let Some(location_event) = committed_events
        .iter()
        .find(|event| event.kind == "generate_location")
    else {
        return json!({
            "ok": false,
            "status": "rejected",
            "committed": false,
            "code": "missing_generate_location_event",
            "reason": "location generation did not commit its canon event",
            "place_id": place_id,
        });
    };
    let event_id = location_event.event_id.clone();
    let event_seq = location_event.seq;
    session.world.world_canon = staged;

    let mut memory_ids = Vec::new();
    if !memory_note.is_empty() {
        let mut visibility_scopes = vec![format!("place:{place_id}")];
        if player_observed {
            visibility_scopes.push("player".to_string());
        }
        let memory_id = session.world.add_memory_unit(MemoryUnit {
            tier: MemoryTier::Raw,
            owner_scope: format!("place:{place_id}"),
            visibility_scopes,
            summary: memory_note,
            details: description.clone(),
            source_event_ids: vec![event_id.clone()],
            time_start: session.world.world_canon.clock_minutes,
            time_end: session.world.world_canon.clock_minutes,
            place_ids: vec![place_id.clone()],
            topic_tags: vec![purpose.clone(), kind.clone()],
            truth_status: MemoryTruthStatus::Actual,
            injection_state: MemoryInjectionState::Hot,
            created_by: "location_generator".to_string(),
            ..Default::default()
        });
        memory_ids.push(memory_id);
    }

    let hidden_clues = generated_string_list(generated, "hidden_clues");
    let knows_more = generated_string_list(generated, "knows_more");
    let mut hidden_details = Vec::new();
    if !hidden_summary.is_empty() {
        hidden_details.push(hidden_summary.clone());
    }
    hidden_details.extend(hidden_clues);
    hidden_details.extend(knows_more);
    if !hidden_details.is_empty() {
        let memory_id = session.world.add_memory_unit(MemoryUnit {
            tier: MemoryTier::Raw,
            owner_scope: "gm_private".to_string(),
            visibility_scopes: Vec::new(),
            summary: format!("Скрытое по локации {name}: {}", hidden_details[0]),
            details: hidden_details.join("\n"),
            source_event_ids: vec![event_id.clone()],
            time_start: session.world.world_canon.clock_minutes,
            time_end: session.world.world_canon.clock_minutes,
            place_ids: vec![place_id.clone()],
            topic_tags: vec![purpose.clone(), kind.clone(), "hidden".to_string()],
            truth_status: MemoryTruthStatus::Actual,
            injection_state: MemoryInjectionState::Warm,
            created_by: "location_generator".to_string(),
            ..Default::default()
        });
        memory_ids.push(memory_id);
    }

    json!({
        "ok": true,
        "status": if created_place { "created" } else { "updated" },
        "committed": true,
        "place_id": place_id,
        "name": name,
        "kind": kind,
        "event_id": event_id,
        "event_seq": event_seq,
        "transitions_added": transitions_added,
        "entry_transition_id": entry_transition_id.unwrap_or_default(),
        "entered": enter_after_commit && session.world.world_canon.player_place_id == place_id,
        "current_place_id": session.world.world_canon.player_place_id.clone(),
        "memory_ids": memory_ids,
        "player_observed": player_observed,
    })
}

async fn run_ask_npc_tool(
    session: &mut Session,
    args: &Value,
    metas: &mut Vec<Value>,
    sink: &Sink,
) -> ToolExecutionResult {
    let npc_id = arg_str(args, "npc_id").to_string();
    let situation = arg_str(args, "situation").trim().to_string();
    if situation.is_empty() {
        let msg = "ask_npc requires a non-empty `situation`; call ask_npc again with \
`npc_id` and a neutral third-person situation.";
        sink.emit(ev(
            event_kind::ERROR,
            Some("ГМ"),
            Value::String(msg.to_string()),
            None,
        ));
        return tool_error(
            "ask_npc",
            msg,
            None,
            "missing_situation",
            &[("npc_id", json!(npc_id))],
        );
    }
    let mut correction = args
        .get("correction")
        .and_then(Value::as_str)
        .map(String::from);
    if let Some(c) = &correction {
        if !c.is_empty() && !session.pending.contains_key(&npc_id) {
            correction = None;
        }
    }
    ask_npc(
        session,
        &npc_id,
        &situation,
        correction.as_deref(),
        metas,
        sink,
    )
    .await
}

// =========================================================================
// _ask_npc
// =========================================================================

fn merge_llm_stats(total: &mut Map<String, Value>, stats: &Map<String, Value>) {
    for key in [
        "prompt_eval_count",
        "eval_count",
        "cached_tokens",
        "eval_duration",
        "prompt_eval_duration",
        "total_duration",
        "load_duration",
    ] {
        let current = total.get(key).and_then(Value::as_i64).unwrap_or(0);
        let next = stats.get(key).and_then(Value::as_i64).unwrap_or(0);
        let sum = current + next;
        if sum != 0 {
            total.insert(key.to_string(), Value::from(sum));
        }
    }
}

fn run_npc_tool(
    session: &mut Session,
    npc_id: &str,
    name: &str,
    args: &Value,
    sink: &Sink,
) -> ToolExecutionResult {
    match name {
        "remember" => {
            let mut tool_args = args.as_object().cloned().unwrap_or_default();
            tool_args.insert("npc_id".to_string(), Value::String(npc_id.to_string()));
            let payload = crate::worldstate::npc_memory_recall(session, &Value::Object(tool_args));
            if let Some(err) = payload.get("error").and_then(Value::as_str) {
                if !err.is_empty() {
                    sink.emit(ev(
                        event_kind::ERROR,
                        Some(npc_id),
                        Value::String(err.to_string()),
                        None,
                    ));
                }
            }
            tool_result(
                &json_compact(&payload),
                Some(&model_world_query_text(&payload)),
                Some(tool_reminder("remember")),
                false,
            )
        }
        "npc_note_memory" => {
            let mut tool_args = args.as_object().cloned().unwrap_or_default();
            tool_args.insert("npc_id".to_string(), Value::String(npc_id.to_string()));
            let payload = crate::worldstate::npc_note_memory(session, &Value::Object(tool_args));
            if let Some(err) = payload.get("error").and_then(Value::as_str) {
                if !err.is_empty() {
                    sink.emit(ev(
                        event_kind::ERROR,
                        Some(npc_id),
                        Value::String(err.to_string()),
                        None,
                    ));
                }
            }
            let public_payload = json!({
                "ok": payload.get("ok").cloned().unwrap_or(Value::Bool(false)),
                "scope": "npc",
                "npc_id": npc_id,
                "status": payload
                    .get("status")
                    .cloned()
                    .unwrap_or_else(|| Value::String("stored".to_string())),
                "memory_id": payload.get("memory_id").cloned().unwrap_or(Value::Null),
                "privacy_note": payload
                    .get("privacy_note")
                    .cloned()
                    .unwrap_or_else(|| Value::String(
                        "Stored as this NPC's private memory.".to_string()
                    )),
            });
            let model_payload = if payload.get("error").is_some() {
                payload
            } else {
                public_payload.clone()
            };
            tool_result(
                &json_compact(&public_payload),
                Some(&json_compact(&model_payload)),
                Some(tool_reminder("note_memory")),
                false,
            )
        }
        "npc_recall_relationship" => {
            let mut tool_args = args.as_object().cloned().unwrap_or_default();
            tool_args.insert("npc_id".to_string(), Value::String(npc_id.to_string()));
            let payload =
                crate::worldstate::npc_recall_relationship(session, &Value::Object(tool_args));
            if let Some(err) = payload.get("error").and_then(Value::as_str) {
                if !err.is_empty() {
                    sink.emit(ev(
                        event_kind::ERROR,
                        Some(npc_id),
                        Value::String(err.to_string()),
                        None,
                    ));
                }
            }
            tool_result(
                &json_compact(&payload),
                Some(&model_world_query_text(&payload)),
                Some(tool_reminder("remember")),
                false,
            )
        }
        other => tool_error(
            "npc_tool",
            &format!("NPC tool `{other}` is not available to this role"),
            Some("That tool is not available to you."),
            "unknown_npc_tool",
            &[("tool", json!(other))],
        ),
    }
}

#[allow(clippy::too_many_arguments)]
async fn npc_turn_stream_with_tools(
    session: &mut Session,
    npc_client: &dyn gml_llm::Backend,
    npc_id: &str,
    npc_label: &str,
    sid: &str,
    npc: &gml_world::Npc,
    history: &[Value],
    summary: &str,
    user_message: &Value,
    sink: &Sink,
    emitted: &mut usize,
) -> Result<(Map<String, Value>, Map<String, Value>), BackendError> {
    let mut messages = gml_agents::npc_request_messages(npc, history, summary, user_message);
    let tools = Value::Array(gml_agents::build_npc_tools());
    let mut total_stats = Map::new();

    for hop in 0..=NPC_TOOL_HOPS {
        let request_messages = Value::Array(messages.clone());
        let output = {
            let mut collector = NpcSpeechCollector {
                sink,
                sid,
                npc_label,
                buf: String::new(),
                emitted,
            };
            npc_client
                .chat_stream(
                    &request_messages,
                    Some(&tools),
                    Some(true),
                    Role::Npc.as_str(),
                    &mut collector,
                )
                .await?
        };
        merge_llm_stats(&mut total_stats, &output.stats);

        if output.calls.is_empty() {
            let data = gml_llm::loads_map(&output.content);
            return Ok((
                gml_agents::norm_npc_with_reasoning(&Value::Object(data), &output.thinking),
                total_stats,
            ));
        }

        if hop == NPC_TOOL_HOPS {
            return Err(BackendError::new(format!(
                "NPC exceeded tool-call limit ({NPC_TOOL_HOPS})"
            )));
        }

        let calls = normalize_tool_calls(&output.calls, &format!("npc_{sid}_{hop}"));
        messages.push(assistant_with_tool_calls(output.assistant_msg, &calls));

        for call in &calls {
            let args = Value::Object(call.arguments.clone());
            sink.emit(ev(
                event_kind::NPC_TOOL_CALL,
                Some(npc_label),
                json!({"npc_id": npc_id, "name": call.name.clone(), "arguments": args.clone()}),
                Some(sid),
            ));
            let result = run_npc_tool(session, npc_id, &call.name, &args, sink);
            sink.emit(ev(
                event_kind::NPC_TOOL_RESULT,
                Some(npc_label),
                Value::String(result.full.clone()),
                Some(sid),
            ));
            messages.push(json!({
                "role": "tool",
                "tool_call_id": call.id,
                "content": result.model,
            }));
        }
    }

    Err(BackendError::new(
        "NPC tool loop ended without a final response",
    ))
}

async fn ask_npc(
    session: &mut Session,
    npc_id: &str,
    situation: &str,
    correction: Option<&str>,
    metas: &mut Vec<Value>,
    sink: &Sink,
) -> ToolExecutionResult {
    let correction = correction
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty());

    let resolved = match session.world.resolve(npc_id) {
        Ok(id) => id,
        Err(e) => {
            sink.emit(ev(event_kind::ERROR, Some("ГМ"), Value::String(e), None));
            return tool_error(
                "ask_npc",
                &format!("no such NPC: {npc_id}"),
                Some(&format!("(no such NPC: {npc_id})")),
                "unknown_npc",
                &[("npc_id", json!(npc_id))],
            );
        }
    };
    let npc_name = session.world.npcs[&resolved].name.clone();

    if !session.world.npc_can_react(&resolved) {
        let whereabouts = session.world.npc_whereabouts_summary(&resolved);
        let mut msg = format!(
            "{npc_name} is not present and able to hear in the current scene. \
Do not invent their reaction here. Do not write speech/action for any other \
named NPC unless you first call ask_npc for that exact present NPC. Narrate \
only absence, travel/search, or generic scene response."
        );
        if !whereabouts.is_empty() {
            msg.push_str(&format!(" Known whereabouts: {whereabouts}"));
        }
        sink.emit(ev(
            event_kind::ERROR,
            Some("ГМ"),
            Value::String(msg.clone()),
            None,
        ));
        let label = session.world.npc_player_label(&resolved, "player");
        return tool_error(
            "ask_npc",
            &msg,
            None,
            "npc_not_present",
            &[
                ("npc_id", json!(resolved)),
                ("npc_label", json!(label)),
                ("whereabouts", json!(whereabouts)),
            ],
        );
    }

    if let Some(c) = &correction {
        sink.emit(ev(
            event_kind::GM_REJECT,
            Some(&npc_name),
            Value::String(c.clone()),
            None,
        ));
    }

    let exchange_witnesses = if situation_includes_room_witnesses(situation) {
        session.record_player_for(&resolved)
    } else {
        session.record_player_for_direct(&resolved)
    };
    let sid = session.next_sid();
    let npc_label = session.world.npc_player_label(&resolved, "player");
    sink.emit(ev(
        event_kind::NPC_START,
        Some(&npc_label),
        json!({"npc_id": resolved}),
        Some(&sid),
    ));

    let observations = session.observations(&resolved);
    let last_contact = session.npc_last_contact_text(&resolved);
    let commitments = session.commit_text(&resolved);
    session.snapshot_shown(&resolved);
    let brief = situation.trim().to_string();
    // brief is guaranteed non-empty (checked by caller), but Python re-checks.

    let npc_client = session
        .ensure_npc_client(&resolved)
        .unwrap_or_else(|| session.client.clone());
    maybe_compact_npc(session, &resolved, npc_client.as_ref()).await;

    sink.emit(ev(
        event_kind::NPC_HISTORY,
        Some(&npc_name),
        json!({
            "npc_id": resolved,
            "messages": session.npc_messages.get(&resolved).map(|m| m.len()).unwrap_or(0),
            "has_summary": session.npc_summaries.get(&resolved).map(|s| !s.trim().is_empty()).unwrap_or(false),
            "text": session.npc_history_text(&resolved, 6),
        }),
        None,
    ));

    let npc = session.world.npcs[&resolved].clone();
    let constraints = session.world.constraints.clone();
    let scene_slice = session.world.npc_scene_slice(&resolved);
    let history = session
        .npc_messages
        .get(&resolved)
        .cloned()
        .unwrap_or_default();
    let summary = session
        .npc_summaries
        .get(&resolved)
        .cloned()
        .unwrap_or_default();

    // Build the user_message exactly as agents.npc_user_message (for history).
    let user_message = gml_agents::npc_user_message_with_contact(
        &brief,
        &last_contact,
        &observations,
        &commitments,
        correction.as_deref(),
        &constraints,
        &scene_slice,
    );

    let mut emitted = 0usize;
    let stream_result = npc_turn_stream_with_tools(
        session,
        npc_client.as_ref(),
        &resolved,
        &npc_label,
        &sid,
        &npc,
        &history,
        &summary,
        &user_message,
        sink,
        &mut emitted,
    )
    .await;

    let (out, stats) = match stream_result {
        Ok(v) => v,
        Err(e) => {
            if correction.is_some() {
                session.pending.remove(&resolved);
            }
            sink.emit(ev(
                event_kind::ERROR,
                Some(&npc_name),
                Value::String(format!("Ошибка NPC: {e}")),
                None,
            ));
            let label = session.world.npc_player_label(&resolved, "player");
            return tool_error(
                "ask_npc",
                &format!("NPC generation failed: {e}"),
                Some(&format!("({npc_name} stays silent)")),
                "npc_generation_failed",
                &[("npc_id", json!(resolved)), ("npc_label", json!(label))],
            );
        }
    };

    let reasoning = out
        .get("reasoning")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let speech = out
        .get("speech")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let action = out
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let response = out
        .get("response")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| {
            [action.as_str(), speech.as_str()]
                .into_iter()
                .filter(|s| !s.trim().is_empty())
                .collect::<Vec<_>>()
                .join(" ")
        });
    let beats = out.get("beats").cloned().unwrap_or_else(|| json!([]));
    let beat_rows: Vec<NpcBeat> = serde_json::from_value(beats.clone()).unwrap_or_default();
    let claims: Vec<String> = match out.get("claims") {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
        _ => Vec::new(),
    };

    sink.emit(ev(
        event_kind::NPC_THINKING,
        Some(&npc_name),
        Value::String(reasoning),
        Some(&sid),
    ));
    sink.emit(ev(
        event_kind::NPC_SPEECH,
        Some(&npc_label),
        json!({
            "response": response.clone(),
            "beats": beats.clone(),
            "speech": speech,
            "action": action,
            "claims": claims.clone(),
            "npc_id": resolved,
        }),
        Some(&sid),
    ));
    let m = meta(&npc_name, &stats, "npc");
    metas.push(m.clone());
    sink.emit(ev(event_kind::META, Some(&npc_name), m, Some(&sid)));

    session.remember_npc_client(&resolved);
    session.mark_npc_contact(&resolved);
    let assistant_message = assistant_json_message(&out);
    session.draft_with_response(
        &resolved,
        &response,
        beat_rows,
        &speech,
        &action,
        claims,
        Some(user_message),
        Some(assistant_message),
        Some(exchange_witnesses),
    );

    let label = session.world.npc_player_label(&resolved, "player");
    let payload = json!({
        "npc_id": resolved,
        "npc_name": npc_name,
        "npc_label": label,
        "response_ru": response,
        "beats": beats,
        "speech_ru": speech,
        "action_ru": action,
        "gm_instruction":
            "This exact NPC speech/action has already been emitted to the player by the \
    engine. If more NPCs should react, call ask_npc for them now. In final \
    narration, do not rewrite, retell, embellish, or paraphrase this NPC \
    speech/action. Do not mention this NPC's name, body, speech, action, \
    expression, posture, gesture, or emotion again. Final narration should be \
    only 0-2 short sentences about surrounding scene consequences. If there is no \
    new non-NPC consequence, produce empty final narration. Do not add another \
    named NPC's reaction; call ask_npc for that NPC if you need it.",
    });
    tool_result(
        &json_compact(&payload),
        Some(&model_ask_npc_text(&payload)),
        Some(tool_reminder("ask_npc")),
        false,
    )
}

/// `_assistant_json_message(out)` — `{"role":"assistant","content":json.dumps(out)}`.
/// NOTE: Python uses `json.dumps(out, ensure_ascii=False)` — DEFAULT separators
/// (`", "` / `": "`), not compact — for the stored assistant message.
fn assistant_json_message(out: &Map<String, Value>) -> Value {
    let content = py_json_dumps_default(&Value::Object(out.clone()));
    json!({"role": "assistant", "content": content})
}

/// `json.dumps(data, ensure_ascii=False)` — default separators with spaces.
fn py_json_dumps_default(v: &Value) -> String {
    let compact = serde_json::to_string(v).expect("json serialize");
    let mut out = String::with_capacity(compact.len() + compact.len() / 8);
    let mut in_string = false;
    let mut escaped = false;
    for c in compact.chars() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            ',' => {
                out.push(',');
                out.push(' ');
            }
            ':' => {
                out.push(':');
                out.push(' ');
            }
            _ => out.push(c),
        }
    }
    out
}

/// Streams NPC speech deltas (mirrors the `extract_json_string("speech")` loop).
struct NpcSpeechCollector<'a> {
    sink: &'a Sink,
    sid: &'a str,
    npc_label: &'a str,
    buf: String,
    emitted: &'a mut usize,
}

impl DeltaSink for NpcSpeechCollector<'_> {
    fn emit(&mut self, ch: &str, text: &str) {
        if ch != channel::CONTENT {
            return;
        }
        self.buf.push_str(text);
        if let (Some(val), _done) = gml_llm::extract_json_string(&self.buf, "response") {
            let disp = gml_llm::json_unescape(&val);
            let disp_len = disp.chars().count();
            if disp_len > *self.emitted {
                let new_part: String = disp.chars().skip(*self.emitted).collect();
                self.sink.emit(ev(
                    event_kind::DELTA,
                    Some(self.npc_label),
                    json!({"channel": "npc_speech", "text": new_part}),
                    Some(self.sid),
                ));
                *self.emitted = disp_len;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::situation_includes_room_witnesses;

    #[test]
    fn situation_witness_mode_prefers_private_markers_over_loud_words() {
        assert!(!situation_includes_room_witnesses(
            "Дарра говорит негромко, остальные не слышат точных слов."
        ));
        assert!(!situation_includes_room_witnesses(
            "Игрок стоит вплотную и шепчет только ей."
        ));
        assert!(situation_includes_room_witnesses(
            "Игрок громко говорит при всех, чтобы весь зал услышал."
        ));
    }
}
