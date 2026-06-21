//! The turn loop: `run_turn`, `_drive`, `_run_tool`, `_ask_npc`, the pre-tool
//! prelude, and scene-delta sync — ports from `orchestrator.py`.
//!
//! PORT_PLAN §5.2: Python generators / `yield from` are flattened into ONE event
//! stream. Each helper takes a `&Sink` (an mpsc sender wrapper) and returns its
//! value (`ToolExecutionResult` / `String`). The whole turn is driven by
//! [`run_turn`], which returns a [`Vec<Event>`] (tests) and can also stream.

use std::time::Instant;

use serde_json::{json, Map, Value};
use tokio::sync::mpsc;

use gml_config::RuntimeSettings;
use gml_llm::{channel, ChatStreamOutput, DeltaSink};
use gml_types::{event_kind, Event, ParsedCall, ToolExecutionResult};

use crate::compact::{
    add_total_context, context_usage, maybe_compact, maybe_compact_npc, meta, meta_total,
    round2,
};
use crate::helpers::{
    json_compact, player_facing_payload, tool_error, tool_reminder, tool_result,
    with_model_reminder, VISIBLE_CONTINUATION_REMINDER,
};
use crate::helpers::{model_player_options_text, model_roll_text};
use crate::model_text::{
    apply_scene_move, model_ask_npc_text, model_npc_profile_text, model_player_character_update_text,
    model_presence_text, model_scene_text, model_time_text, model_whereabouts_text,
    model_world_query_text, model_world_state_update_text, normalize_tool_args,
    player_options_payload,
};
use crate::session::Session;

const PRELUDE_CALLBRIEF_CHARS: usize = 4000;

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

    drop(sink);
    let mut out = Vec::new();
    while let Some(e) = rx.recv().await {
        out.push(e);
    }
    out
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

    let gm_user = gml_agents::gm_user_message(&mut session.world, player_text, include_player_options_tool);
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
        let label = if calls.is_empty() { "ГМ — нарратив" } else { "ГМ — решение" };
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
    session.commit_turn();
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
    calls.iter().any(|c| VISIBLE_PRELUDE_TOOLS.contains(&c.name.as_str()))
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

async fn sync_scene_delta(session: &mut Session, narration: &str, metas: &mut Vec<Value>, sink: &Sink) {
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
    let delta = match gml_agents::extract_scene_delta(client.as_ref(), &mut session.world, narration).await {
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
            sink.emit(ev(event_kind::SCENE_UPDATE, Some("scene_sync"), payload, None));
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
        .map(|r| r.get("minutes").and_then(|v| v.as_i64()).unwrap_or(0).max(0))
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
        let normalized_args = normalize_tool_args(&call.name, &Value::Object(call.arguments.clone()));
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

    match name {
        "tool_search" => run_tool_search(session, &args, sink),
        "ask_player" => run_ask_player(&args, sink),
        "roll_dice" => run_roll_dice(session, &args, sink),
        "get_world_fact" => run_get_world_fact(session, &args, sink),
        "update_world_state" => run_update_world_state(session, &args, sink),
        "query_world_state" => run_query_world_state(session, &args, sink),
        "get_npc_profile" => run_get_npc_profile(session, &args, sink),
        "advance_time" => run_advance_time(session, &args, sink),
        "update_player_character" => run_update_player_character(session, &args, sink),
        "set_npc_whereabouts" => run_set_npc_whereabouts(session, &args, sink),
        "move_npc" | "set_npc_presence" => run_move_npc(session, &args, sink),
        "set_scene" => run_set_scene(session, &args, sink),
        "ask_npc" => run_ask_npc_tool(session, &args, metas, sink).await,
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

fn run_tool_search(session: &mut Session, args: &Value, sink: &Sink) -> ToolExecutionResult {
    let query = arg_str(args, "query");
    let max_results = args.get("max_results").and_then(|v| v.as_i64()).unwrap_or(5);
    // Note: include_player_options_tool is read from settings in Python via
    // runtime_settings.gm_suggest_options_enabled(); we read it from the already
    // loaded tool set membership semantics. The agents layer takes the flag.
    let include_player_options = session
        .loaded_gm_tools
        .contains("ask_player");
    let payload = gml_agents::search_gm_tools(
        query,
        max_results,
        Some(&session.loaded_gm_tools),
        include_player_options,
    );
    if let Some(Value::Array(loaded)) = payload.get("loaded_tools") {
        for tool_name in loaded {
            if let Some(s) = tool_name.as_str() {
                session.loaded_gm_tools.insert(s.to_string());
            }
        }
    }
    let mut lines = vec![payload
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()];
    if let Some(Value::Array(matches)) = payload.get("matches") {
        if !matches.is_empty() {
            lines.push("Загружено:".to_string());
            for row in matches {
                let n = row.get("name").and_then(Value::as_str).unwrap_or("");
                let d = row.get("description").and_then(Value::as_str).unwrap_or("");
                lines.push(format!("- {n}: {d}"));
            }
        }
    }
    if let Some(Value::Array(missing)) = payload.get("missing") {
        if !missing.is_empty() {
            let names: Vec<String> = missing.iter().filter_map(|v| v.as_str().map(String::from)).collect();
            lines.push(format!("Не найдено: {}", names.join(", ")));
        }
    }
    let text = lines.into_iter().filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n");
    sink.emit(ev(event_kind::TOOL_SEARCH, Some("ГМ"), Value::String(text), None));
    tool_result(
        &json_compact(&payload),
        Some(&crate::helpers::model_tool_search_text(&payload)),
        None,
        false,
    )
}

fn run_ask_player(args: &Value, sink: &Sink) -> ToolExecutionResult {
    let (payload, error) = player_options_payload(args);
    if !error.is_empty() {
        sink.emit(ev(event_kind::ERROR, Some("ГМ"), Value::String(error.clone()), None));
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
    sink.emit(ev(event_kind::PLAYER_OPTIONS, Some("ГМ"), payload.clone(), None));
    let model_text = model_player_options_text(&payload);
    tool_result(&model_text, Some(&model_text), None, false)
}

fn run_roll_dice(session: &mut Session, args: &Value, sink: &Sink) -> ToolExecutionResult {
    let notation = {
        let n = arg_str(args, "notation");
        if n.is_empty() { "1d20" } else { n }
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
    let detail = payload.get("detail").and_then(Value::as_str).unwrap_or("").to_string();
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
    let retriever = crate::rag::build_retriever(&mut session.world, "player", &query);
    let fact = session
        .world
        .fact(&query, "player", retriever.as_ref().map(|r| r.as_dyn()));
    let payload = fact.as_tool_payload();
    let scope_key = crate::worldstate::query_scope_key("fact", "");
    let (payload, _delivered) =
        crate::query_dedup::filter_new_fact_payload(session, &scope_key, payload, &query);
    let mut event_payload = match payload.clone() {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    event_payload.insert("query".to_string(), Value::String(query.clone()));
    sink.emit(ev(event_kind::WORLD_FACT, Some("ГМ"), Value::Object(event_payload), None));
    tool_result(
        &json_compact(&payload),
        Some(&crate::helpers::model_world_fact_text(&payload)),
        Some(tool_reminder("get_world_fact")),
        false,
    )
}

fn run_update_world_state(session: &mut Session, args: &Value, sink: &Sink) -> ToolExecutionResult {
    let payload = crate::worldstate::apply_world_state_batch(session, args);
    if let Some(Value::Array(errors)) = payload.get("errors") {
        for error in errors {
            let msg = error
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("world-state update failed");
            sink.emit(ev(event_kind::ERROR, Some("ГМ"), Value::String(msg.to_string()), None));
        }
    }
    sink.emit(ev(event_kind::WORLD_STATE_UPDATE, Some("ГМ"), payload.clone(), None));
    tool_result(
        &json_compact(&payload),
        Some(&model_world_state_update_text(&payload)),
        Some(tool_reminder("update_world_state")),
        false,
    )
}

fn run_query_world_state(session: &mut Session, args: &Value, sink: &Sink) -> ToolExecutionResult {
    let payload = crate::worldstate::query_world_state(session, args);
    if let Some(err) = payload.get("error").and_then(Value::as_str) {
        if !err.is_empty() {
            sink.emit(ev(event_kind::ERROR, Some("ГМ"), Value::String(err.to_string()), None));
        } else {
            sink.emit(ev(event_kind::WORLD_QUERY, Some("ГМ"), payload.clone(), None));
        }
    } else {
        sink.emit(ev(event_kind::WORLD_QUERY, Some("ГМ"), payload.clone(), None));
    }
    tool_result(
        &json_compact(&payload),
        Some(&model_world_query_text(&payload)),
        Some(tool_reminder("query_world_state")),
        false,
    )
}

fn run_get_npc_profile(session: &mut Session, args: &Value, sink: &Sink) -> ToolExecutionResult {
    let payload = match session.world.npc_profile(
        arg_str(args, "npc_id"),
        {
            let p = arg_str(args, "preset");
            if p.is_empty() { "visible" } else { p }
        },
        args.get("fields").unwrap_or(&Value::Null),
    ) {
        Ok(p) => {
            sink.emit(ev(event_kind::NPC_PROFILE, Some("ГМ"), p.clone(), None));
            p
        }
        Err(e) => {
            sink.emit(ev(event_kind::ERROR, Some("ГМ"), Value::String(e.clone()), None));
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
    tool_result(&json_compact(&payload), Some(&model_time_text(&payload)), None, false)
}

fn run_update_player_character(session: &mut Session, args: &Value, sink: &Sink) -> ToolExecutionResult {
    let payload = session
        .world
        .update_player_character(args.get("fields").unwrap_or(&Value::Null), arg_str(args, "reason"));
    sink.emit(ev(event_kind::PLAYER_CHARACTER_UPDATE, Some("ГМ"), payload.clone(), None));
    tool_result(
        &json_compact(&payload),
        Some(&model_player_character_update_text(&payload)),
        Some(tool_reminder("update_player_character")),
        false,
    )
}

fn run_set_npc_whereabouts(session: &mut Session, args: &Value, sink: &Sink) -> ToolExecutionResult {
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
            sink.emit(ev(event_kind::NPC_WHEREABOUTS, Some("ГМ"), player_payload, None));
            tool_result(
                &json_compact(&payload),
                Some(&model_whereabouts_text(&payload)),
                Some(tool_reminder("set_npc_whereabouts")),
                false,
            )
        }
        Err(e) => {
            sink.emit(ev(event_kind::ERROR, Some("ГМ"), Value::String(e.clone()), None));
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
            sink.emit(ev(event_kind::SCENE_UPDATE, Some("ГМ"), player_payload, None));
            tool_result(
                &json_compact(&payload),
                Some(&model_presence_text(&payload)),
                Some(tool_reminder("move_npc")),
                false,
            )
        }
        Err(e) => {
            sink.emit(ev(event_kind::ERROR, Some("ГМ"), Value::String(e.clone()), None));
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
    let null = Value::Null;
    let payload = session.world.set_scene(
        arg_str(args, "title"),
        arg_str(args, "description"),
        arg_str(args, "location_id"),
        args.get("present_npcs").unwrap_or(&null),
        args.get("items").unwrap_or(&null),
        args.get("exits").unwrap_or(&null),
        args.get("constraints").unwrap_or(&null),
        arg_str(args, "tension"),
    );
    if let Some(hint) = payload.get("repair_hint").and_then(Value::as_str) {
        if !hint.is_empty() {
            sink.emit(ev(event_kind::ERROR, Some("ГМ"), Value::String(hint.to_string()), None));
        }
    }
    sink.emit(ev(event_kind::SCENE_UPDATE, Some("ГМ"), payload.clone(), None));
    tool_result(
        &json_compact(&payload),
        Some(&model_scene_text(&payload)),
        Some(tool_reminder("set_scene")),
        false,
    )
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
        sink.emit(ev(event_kind::ERROR, Some("ГМ"), Value::String(msg.to_string()), None));
        return tool_error(
            "ask_npc",
            msg,
            None,
            "missing_situation",
            &[("npc_id", json!(npc_id))],
        );
    }
    let mut correction = args.get("correction").and_then(Value::as_str).map(String::from);
    if let Some(c) = &correction {
        if !c.is_empty() && !session.pending.contains_key(&npc_id) {
            correction = None;
        }
    }
    ask_npc(session, &npc_id, &situation, correction.as_deref(), metas, sink).await
}

// =========================================================================
// _ask_npc
// =========================================================================

async fn ask_npc(
    session: &mut Session,
    npc_id: &str,
    situation: &str,
    correction: Option<&str>,
    metas: &mut Vec<Value>,
    sink: &Sink,
) -> ToolExecutionResult {
    let correction = correction.map(|c| c.trim().to_string()).filter(|c| !c.is_empty());

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
        sink.emit(ev(event_kind::ERROR, Some("ГМ"), Value::String(msg.clone()), None));
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
        sink.emit(ev(event_kind::GM_REJECT, Some(&npc_name), Value::String(c.clone()), None));
    }

    let exchange_witnesses = session.record_player_for(&resolved);
    let sid = session.next_sid();
    let npc_label = session.world.npc_player_label(&resolved, "player");
    sink.emit(ev(
        event_kind::NPC_START,
        Some(&npc_label),
        json!({"npc_id": resolved}),
        Some(&sid),
    ));

    let observations = session.observations(&resolved);
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
    let history = session.npc_messages.get(&resolved).cloned().unwrap_or_default();
    let summary = session.npc_summaries.get(&resolved).cloned().unwrap_or_default();

    // Build the user_message exactly as agents.npc_user_message (for history).
    let user_message = gml_agents::npc_user_message(
        &brief,
        &observations,
        &commitments,
        correction.as_deref(),
        &constraints,
        &scene_slice,
    );

    // npc_turn_stream — stream speech deltas.
    let mut emitted = 0usize;
    let stream_result = {
        let mut collector = NpcSpeechCollector {
            sink,
            sid: &sid,
            npc_label: &npc_label,
            buf: String::new(),
            emitted: &mut emitted,
        };
        gml_agents::npc_turn_stream(
            npc_client.as_ref(),
            &npc,
            &brief,
            &observations,
            &commitments,
            correction.as_deref(),
            &constraints,
            &scene_slice,
            &history,
            &summary,
            &mut collector,
        )
        .await
    };

    let (out, stats) = match stream_result {
        Ok(v) => v,
        Err(e) => {
            if correction.is_some() {
                session.pending.remove(&resolved);
            }
            sink.emit(ev(event_kind::ERROR, Some(&npc_name), Value::String(format!("Ошибка NPC: {e}")), None));
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

    let reasoning = out.get("reasoning").and_then(Value::as_str).unwrap_or("").to_string();
    let speech = out.get("speech").and_then(Value::as_str).unwrap_or("").to_string();
    let action = out.get("action").and_then(Value::as_str).unwrap_or("").to_string();
    let claims: Vec<String> = match out.get("claims") {
        Some(Value::Array(a)) => a.iter().filter_map(|v| v.as_str().map(String::from)).collect(),
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
        json!({"speech": speech, "action": action, "claims": claims, "npc_id": resolved}),
        Some(&sid),
    ));
    let m = meta(&npc_name, &stats, "npc");
    metas.push(m.clone());
    sink.emit(ev(event_kind::META, Some(&npc_name), m, Some(&sid)));

    session.remember_npc_client(&resolved);
    let assistant_message = assistant_json_message(&out);
    session.draft(
        &resolved,
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
        if let (Some(val), _done) = gml_llm::extract_json_string(&self.buf, "speech") {
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
