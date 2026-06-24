//! Payload builders — faithful ports of `server.py`'s `state()`,
//! `export_data()`, `debug_data()`, and the transcript-replay helpers.
//!
//! Each function takes a `&mut DialogRuntime` (so the underlying `World`
//! projection methods that mutate caches — `scene_export`, `entity_refs`,
//! `npc_whereabouts_export` — can run) plus the shared `RuntimeSettings`, and
//! returns the exact JSON shape the React frontend consumes (see
//! `tests/reference/server/{state,debug}.json`).

use serde_json::{json, Map, Value};

use gml_config::{Config, RuntimeSettings};
use gml_orchestrator::compact::context_usage;
use gml_orchestrator::Session;
use gml_persistence::{DialogRuntime, DEFAULT_CHAT_TITLE};
use gml_world::{public_gender, public_role, StateRecordQuery, WHEREABOUTS_STATUS_LABELS};

/// `config.BACKEND == "codex"` test, sourced from the shared [`Config`].
fn is_codex(cfg: &Config) -> bool {
    cfg.backend == "codex"
}

/// Convert a settings `BTreeMap` into a JSON value (object). Frontend
/// `JSON.parse`s the body, so key order is not load-bearing.
fn settings_to_value(map: gml_config::SettingsMap) -> Value {
    serde_json::to_value(map).unwrap_or(Value::Object(Map::new()))
}

/// `_default_model()`.
fn default_model(cfg: &Config) -> String {
    if cfg.backend == "codex" {
        if !cfg.codex_model.is_empty() {
            cfg.codex_model.clone()
        } else {
            cfg.model.clone()
        }
    } else if !cfg.model.is_empty() {
        cfg.model.clone()
    } else {
        "default".to_string()
    }
}

/// `_session_matches_backend(session)`.
fn session_matches_backend(session: &Session, cfg: &Config) -> bool {
    session.client_backend.is_empty() || session.client_backend == cfg.backend
}

/// `model = client.model or (session.client_model if matches) or _default_model()`.
fn resolve_model(session: &Session, cfg: &Config) -> String {
    let client_model = session.client.model();
    if !client_model.is_empty() {
        return client_model;
    }
    if session_matches_backend(session, cfg) && !session.client_model.is_empty() {
        return session.client_model.clone();
    }
    default_model(cfg)
}

/// `dict(world_mod.WHEREABOUTS_STATUS_LABELS)` — preserves insertion order.
fn status_labels() -> Value {
    let mut m = Map::new();
    for (k, v) in WHEREABOUTS_STATUS_LABELS {
        m.insert(k.to_string(), Value::String(v.to_string()));
    }
    Value::Object(m)
}

/// `_debug_state_records(world)` -> memory-backed StateRecord-shaped export.
fn debug_state_records(session: &mut Session) -> Vec<Value> {
    let mut query = StateRecordQuery::new("gm");
    query.active = None;
    session.world.state_memory_records_export(&query)
}

/// `state(dialog)` — the shared chat state the SPA renders on first load and
/// after each mutation.
pub fn state(runtime: &mut DialogRuntime, cfg: &Config, settings: &RuntimeSettings) -> Value {
    let model = resolve_model(&runtime.session, cfg);
    let settings_map = settings.get();
    let stream_gm_content = settings.stream_gm_content_enabled(Some(&settings_map));
    let context = context_usage(&mut runtime.session);
    let run_usage = Value::Object(runtime.session.run_usage.clone());

    let session = &mut runtime.session;
    let w = &mut session.world;

    let story_id = w.story_id.clone();
    let story_title = w.story_title.clone();
    let story_brief = w.story_brief.clone();
    let public = w.public.clone();
    let time = w.time_export();
    let player_character = w.player_character_export(true);
    let scene = w.scene_export();
    let entities = w.entity_refs();

    // Public NPC projection (`public_npc(npc)`), in roster order.
    let npc_ids: Vec<String> = w.npcs.keys().cloned().collect();
    let mut npcs: Vec<Value> = Vec::with_capacity(npc_ids.len());
    for npc_id in &npc_ids {
        let label = w.npc_player_label(npc_id, "player");
        let known_name = w.npc_known_name(npc_id, "player");
        let npc = &w.npcs[npc_id];
        npcs.push(json!({
            "id": npc.npc_id,
            "name": label,
            "label": label,
            "known_name": known_name,
            "public_label": npc.public_label,
            "role": public_role(&npc.role),
            "pronouns": public_gender(&npc.pronouns),
            "color": npc.color,
            "physical_type": npc.physical_type,
            "distinctive_features": npc.distinctive_features,
            "condition": npc.condition,
            "life_status": npc.life_status,
        }));
    }

    let mut data = Map::new();
    data.insert("model".to_string(), Value::String(model));
    data.insert("backend".to_string(), Value::String(cfg.backend.clone()));
    data.insert(
        "stream_gm_content".to_string(),
        Value::Bool(stream_gm_content),
    );
    data.insert("settings".to_string(), settings_to_value(settings_map));
    data.insert(
        "settings_options".to_string(),
        Value::Object(settings.options()),
    );
    data.insert("run_usage".to_string(), run_usage);
    data.insert("context_usage".to_string(), context);
    data.insert("story_id".to_string(), Value::String(story_id));
    data.insert(
        "story_title".to_string(),
        Value::String(story_title.clone()),
    );
    data.insert(
        "story_brief".to_string(),
        json!({
            "title": story_title,
            "text": story_brief,
        }),
    );
    data.insert("public".to_string(), Value::String(public));
    data.insert("time".to_string(), time);
    data.insert("player_character".to_string(), player_character);
    data.insert("scene".to_string(), scene);
    data.insert("entities".to_string(), entities);
    data.insert("status_labels".to_string(), status_labels());
    data.insert("npcs".to_string(), Value::Array(npcs));
    if is_codex(cfg) {
        data.insert(
            "codex_auth".to_string(),
            Value::Object(gml_codex::auth_status()),
        );
    }
    Value::Object(data)
}

/// `_chat_response(dialog, active)`.
pub fn chat_response(runtime: &DialogRuntime, active: bool) -> Value {
    let title = if runtime.title.is_empty() {
        DEFAULT_CHAT_TITLE.to_string()
    } else {
        runtime.title.clone()
    };
    json!({
        "id": runtime.chat_id,
        "title": title,
        "preview": runtime.preview,
        "turn_count": runtime.turn_count.max(0),
        "created_at": runtime.created_at,
        "updated_at": runtime.updated_at,
        "active": active,
    })
}

// =========================================================================
// transcript replay (`replay_events` + NPC-name sanitization)
// =========================================================================

const NPC_AGENT_KINDS: [&str; 3] = ["npc_start", "npc_speech", "gm_reject"];
const NPC_DATA_NAME_KINDS: [&str; 2] = ["scene_update", "npc_whereabouts"];

/// `_npc_label_maps(world)` — (by_id, by_name).
fn npc_label_maps(session: &mut Session) -> (Map<String, Value>, Map<String, Value>) {
    let mut by_id = Map::new();
    let mut by_name = Map::new();
    let npc_ids: Vec<String> = session.world.npcs.keys().cloned().collect();
    for npc_id in &npc_ids {
        let label = session.world.npc_player_label(npc_id, "player");
        if !label.is_empty() {
            by_id.insert(npc_id.clone(), Value::String(label.clone()));
        }
        let npc = &session.world.npcs[npc_id];
        for raw in [npc.name.clone(), npc.public_label.clone(), label.clone()] {
            let key = raw.trim().to_lowercase();
            if !key.is_empty() {
                by_name
                    .entry(key)
                    .or_insert_with(|| Value::String(npc_id.clone()));
            }
        }
    }
    (by_id, by_name)
}

/// `_sanitize_player_name(event, by_id, by_name)`.
fn sanitize_player_name(
    event: &Value,
    by_id: &Map<String, Value>,
    by_name: &Map<String, Value>,
) -> Value {
    let kind = event.get("kind").and_then(Value::as_str).unwrap_or("");
    if NPC_AGENT_KINDS.contains(&kind) {
        let data = event.get("data");
        let mut npc_id = data
            .and_then(|d| d.as_object())
            .and_then(|o| o.get("npc_id"))
            .and_then(Value::as_str)
            .map(String::from)
            .filter(|s| !s.is_empty());
        if npc_id.is_none() {
            let agent_key = event
                .get("agent")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_lowercase();
            npc_id = by_name
                .get(&agent_key)
                .and_then(Value::as_str)
                .map(String::from);
        }
        let label = npc_id
            .as_deref()
            .and_then(|id| by_id.get(id))
            .and_then(Value::as_str);
        if let Some(label) = label {
            if Some(label) != event.get("agent").and_then(Value::as_str) {
                let mut e = event.clone();
                if let Value::Object(ref mut m) = e {
                    m.insert("agent".to_string(), Value::String(label.to_string()));
                }
                return e;
            }
        }
        return event.clone();
    }
    if NPC_DATA_NAME_KINDS.contains(&kind) {
        if let Some(Value::Object(data)) = event.get("data") {
            let mut npc_id = data
                .get("npc_id")
                .and_then(Value::as_str)
                .map(String::from)
                .filter(|s| !s.is_empty());
            if npc_id.is_none() {
                let name_key = data
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_lowercase();
                npc_id = by_name
                    .get(&name_key)
                    .and_then(Value::as_str)
                    .map(String::from);
            }
            let label = npc_id
                .as_deref()
                .and_then(|id| by_id.get(id))
                .and_then(Value::as_str);
            if let Some(label) = label {
                if Some(label) != data.get("name").and_then(Value::as_str) {
                    let mut e = event.clone();
                    if let Value::Object(ref mut m) = e {
                        let mut new_data = data.clone();
                        new_data.insert("name".to_string(), Value::String(label.to_string()));
                        m.insert("data".to_string(), Value::Object(new_data));
                    }
                    return e;
                }
            }
        }
        return event.clone();
    }
    event.clone()
}

/// `replay_events(dialog)` — drop `delta` rows, sanitize NPC names to the labels
/// the player currently knows.
pub fn replay_events(runtime: &mut DialogRuntime) -> Vec<Value> {
    let (by_id, by_name) = npc_label_maps(&mut runtime.session);
    let mut events = Vec::new();
    for row in &runtime.transcript {
        let event = match row.get("event") {
            Some(Value::Object(_)) => row.get("event").unwrap(),
            _ => continue,
        };
        if event.get("kind").and_then(Value::as_str) == Some("delta") {
            continue;
        }
        events.push(sanitize_player_name(event, &by_id, &by_name));
    }
    events
}

// =========================================================================
// export (`export_data`) + debug (`debug_data`)
// =========================================================================

/// `_ser_messages(msgs)` — each message dict passes through; Rust messages are
/// already `Value`.
fn ser_messages(msgs: &[Value]) -> Vec<Value> {
    msgs.to_vec()
}

/// `export_data(dialog)` — the downloadable JSON snapshot.
pub fn export_data(runtime: &mut DialogRuntime, cfg: &Config) -> Value {
    let model = {
        let m = runtime.session.client.model();
        if m.is_empty() {
            cfg.model.clone()
        } else {
            m
        }
    };
    let turn_count = runtime.turn_count;
    let state_records = debug_state_records(&mut runtime.session);
    let session = &mut runtime.session;
    let run_usage = Value::Object(session.run_usage.clone());
    let commitments = btreemap_strvec_to_value(&session.commitments);
    let npc_summaries = btreemap_str_to_value(&session.npc_summaries);
    let npc_messages = btreemap_vec_to_value(&session.npc_messages);
    let npc_client_state = npc_client_state_to_value(session);
    let gm_messages = ser_messages(&session.gm_messages);
    let transcript = runtime.transcript.clone();

    let w = &mut session.world;
    let story_id = w.story_id.clone();
    let story_title = w.story_title.clone();
    let story_brief = w.story_brief.clone();
    let public = w.public.clone();
    let time = w.time_export();
    let player_character = w.player_character_export(false);
    let constraints: Vec<Value> = w.constraints.iter().cloned().map(Value::String).collect();
    let scene = w.scene_export();
    let rumors: Vec<Value> = w.rumors.iter().map(rumor_to_export_value).collect();
    let events: Vec<Value> = session.events.iter().map(event_to_export_value).collect();

    json!({
        "meta": {
            "model": model,
            "backend": cfg.backend,
            "turns": turn_count,
            "run_usage": run_usage,
            "story_id": story_id,
            "story_title": story_title,
            "story_brief": story_brief,
        },
        "world": {
            "story_id": story_id,
            "story_title": story_title,
            "story_brief": story_brief,
            "public": public,
            "time": time,
            "player_character": player_character,
            "constraints": constraints,
            "scene": scene,
            "rumors": rumors,
            "state_records": state_records,
            "npc_commitments": commitments,
            "npc_summaries": npc_summaries,
            "npc_messages": npc_messages,
            "npc_client_state": npc_client_state,
            "events": events,
        },
        "transcript": transcript,
        "gm_messages": gm_messages,
    })
}

fn rumor_to_export_value(r: &gml_world::Rumor) -> Value {
    json!({
        "seq": r.seq,
        "turn": r.turn,
        "speaker": r.speaker,
        "text": r.text,
        "confirmed": r.confirmed,
        "witnesses": sorted_strings(&r.witnesses),
    })
}

fn event_to_export_value(e: &gml_world::WorldEvent) -> Value {
    json!({
        "seq": e.seq,
        "turn": e.turn,
        "actor": e.actor,
        "kind": e.kind,
        "speech": e.speech,
        "action": e.action,
        "witnesses": sorted_strings(&e.witnesses),
    })
}

fn sorted_strings(set: &std::collections::BTreeSet<String>) -> Value {
    Value::Array(set.iter().cloned().map(Value::String).collect())
}

fn btreemap_str_to_value(m: &std::collections::BTreeMap<String, String>) -> Value {
    let mut out = Map::new();
    for (k, v) in m {
        out.insert(k.clone(), Value::String(v.clone()));
    }
    Value::Object(out)
}

fn btreemap_strvec_to_value(m: &std::collections::BTreeMap<String, Vec<String>>) -> Value {
    let mut out = Map::new();
    for (k, v) in m {
        out.insert(
            k.clone(),
            Value::Array(v.iter().cloned().map(Value::String).collect()),
        );
    }
    Value::Object(out)
}

fn btreemap_vec_to_value(m: &std::collections::BTreeMap<String, Vec<Value>>) -> Value {
    let mut out = Map::new();
    for (k, v) in m {
        out.insert(k.clone(), Value::Array(v.clone()));
    }
    Value::Object(out)
}

fn npc_client_state_to_value(session: &Session) -> Value {
    let mut out = Map::new();
    for (k, st) in &session.npc_client_state {
        out.insert(
            k.clone(),
            json!({
                "model": st.model,
                "session_id": st.session_id,
                "thread_id": st.thread_id,
            }),
        );
    }
    Value::Object(out)
}

/// `_debug_event(event)`.
fn debug_event(e: &gml_world::WorldEvent) -> Value {
    json!({
        "seq": e.seq,
        "turn": e.turn,
        "actor": e.actor,
        "kind": e.kind,
        "speech": e.speech,
        "action": e.action,
        "witnesses": sorted_strings(&e.witnesses),
    })
}

/// `_debug_rumor(rumor)`.
fn debug_rumor(r: &gml_world::Rumor) -> Value {
    json!({
        "seq": r.seq,
        "turn": r.turn,
        "speaker": r.speaker,
        "text": r.text,
        "witnesses": sorted_strings(&r.witnesses),
        "confirmed": r.confirmed,
    })
}

/// `_debug_pending(pending)`.
fn debug_pending(session: &Session) -> Value {
    let mut out = Map::new();
    for (npc_id, d) in &session.pending {
        out.insert(
            npc_id.clone(),
            json!({
                "seq": d.seq,
                "speech": d.speech,
                "action": d.action,
                "claims": Value::Array(d.claims.iter().cloned().map(Value::String).collect()),
                "witnesses": sorted_strings(&d.witnesses),
            }),
        );
    }
    Value::Object(out)
}

/// `vars(presence)` for an NPC's `Presence`.
fn presence_value(p: &gml_world::Presence) -> Value {
    json!({
        "npc_id": p.npc_id,
        "location": p.location,
        "visible": p.visible,
        "can_hear": p.can_hear,
        "activity": p.activity,
        "attitude": p.attitude,
    })
}

/// `debug_data(dialog)` — the full debug-panel state dump.
pub fn debug_data(runtime: &mut DialogRuntime, cfg: &Config, settings: &RuntimeSettings) -> Value {
    let model = resolve_model(&runtime.session, cfg);
    let context = context_usage(&mut runtime.session);
    let turn_count = runtime.turn_count;
    let settings_map = settings.get();
    let state_records = debug_state_records(&mut runtime.session);

    let session = &mut runtime.session;
    let run_usage = Value::Object(session.run_usage.clone());
    let thread_id = session.client_thread_id.clone();
    let prompt_cache_key = if !cfg.codex_prompt_cache_key.is_empty() {
        cfg.codex_prompt_cache_key.clone()
    } else {
        thread_id.clone()
    };
    let gm_summary = session.gm_summary.clone();
    let gm_messages_len = session.gm_messages.len() as i64;
    let loaded_gm_tools: Vec<Value> = session
        .loaded_gm_tools
        .iter()
        .cloned()
        .map(Value::String)
        .collect();
    let events: Vec<Value> = {
        let evs = &session.events;
        let start = evs.len().saturating_sub(80);
        evs[start..].iter().map(debug_event).collect()
    };
    let pending = debug_pending(session);
    let delivered = {
        let mut m = Map::new();
        for (k, v) in &session.delivered {
            m.insert(k.clone(), json!(v));
        }
        Value::Object(m)
    };

    // Facts.
    let facts: Vec<Value> = session
        .world
        .fact_records
        .iter()
        .map(|r| {
            json!({
                "id": r.fact_id,
                "kind": r.kind,
                "text": r.text,
                "keywords": Value::Array(r.keywords.iter().cloned().map(Value::String).collect()),
                "source": r.source,
                "confirmed": r.confirmed,
            })
        })
        .collect();

    let story_id = session.world.story_id.clone();
    let story_title = session.world.story_title.clone();
    let story_brief = session.world.story_brief.clone();
    let public_intro = session.world.public.clone();
    let hidden_truth = session.world.canon.clone();
    let story_constraints: Vec<Value> = session
        .world
        .constraints
        .iter()
        .cloned()
        .map(Value::String)
        .collect();
    let hidden_events: Vec<Value> = session
        .world
        .hidden_events
        .iter()
        .cloned()
        .map(Value::String)
        .collect();
    let scene = session.world.scene_export();
    let time = session.world.time_export();
    let player_character = session.world.player_character_export(false);
    let roll_next = session.world.forced_die_next;
    let roll_all = session.world.forced_die_all;
    let rumors: Vec<Value> = session.world.rumors.iter().map(debug_rumor).collect();

    // NPC debug rows, sorted by id (Python `sorted(w.npcs.items())`).
    let npc_ids: Vec<String> = session.world.npcs.keys().cloned().collect();
    let mut npcs: Vec<Value> = Vec::with_capacity(npc_ids.len());
    for npc_id in &npc_ids {
        let player_label = session.world.npc_player_label(npc_id, "player");
        let known_name = session.world.npc_known_name(npc_id, "player");
        let whereabouts = session.world.npc_whereabouts_export(Some(npc_id));
        let summary = session
            .npc_summaries
            .get(npc_id)
            .cloned()
            .unwrap_or_default();
        let commitments: Vec<Value> = session
            .commitments
            .get(npc_id)
            .map(|v| v.iter().cloned().map(Value::String).collect())
            .unwrap_or_default();
        let messages = session
            .npc_messages
            .get(npc_id)
            .map(|m| m.len() as i64)
            .unwrap_or(0);
        let history = session.npc_history_text(npc_id, 6);
        let presence = session.world.scene.presence.get(npc_id).map(presence_value);
        let present = session.world.scene.present_npcs.contains(npc_id);
        let npc = &session.world.npcs[npc_id];
        npcs.push(json!({
            "id": npc_id,
            "name": npc.name,
            "player_label": player_label,
            "known_name": known_name,
            "color": npc.color,
            "role": npc.role,
            "pronouns": npc.pronouns,
            "public_label": npc.public_label,
            "age": npc.age,
            "physical_type": npc.physical_type,
            "distinctive_features": npc.distinctive_features,
            "life_status": npc.life_status,
            "life_status_note": npc.life_status_note,
            "condition": npc.condition,
            "card_revision": npc.card_revision,
            "present": present,
            "presence": presence.unwrap_or(Value::Null),
            "whereabouts": whereabouts,
            "persona": npc.persona,
            "personality": npc.personality,
            "values": npc.values,
            "habits": npc.habits,
            "pressure_response": npc.pressure_response,
            "boundaries": npc.boundaries,
            "voice": npc.voice,
            "goals": npc.goals,
            "knowledge": npc.knowledge,
            "secret": npc.secret,
            "mechanics": {
                "abilities": Value::Object(npc.abilities.clone()),
                "skills": Value::Object(npc.skills.clone()),
                "saving_throws": Value::Object(npc.saving_throws.clone()),
                "passive_perception": npc.passive_perception.map(|v| json!(v)).unwrap_or(Value::Null),
                "ac": npc.ac.clone(),
                "hp": Value::Object(npc.hp.clone()),
                "speed": npc.speed,
                "senses": npc.senses,
                "languages": npc.languages,
            },
            "summary": summary,
            "commitments": commitments,
            "messages": messages,
            "history": history,
        }));
    }

    json!({
        "ok": true,
        "meta": {
            "model": model,
            "backend": cfg.backend,
            "turns": turn_count,
            "run_usage": run_usage,
            "context_usage": context,
        },
        "runtime": {
            "settings": settings_to_value(settings_map),
            "cache": {
                "prompt_cache_key": prompt_cache_key,
                "thread_id": thread_id,
                "store": false,
            },
        },
        "story": {
            "id": story_id,
            "title": story_title,
            "brief": story_brief,
            "objective": "Вести игрока к раскрытию скрытой правды истории через действия, улики, свидетелей и последствия, не выдавая секреты без игрового основания.",
            "public_intro": public_intro,
            "hidden_truth": hidden_truth,
            "constraints": story_constraints,
            "hidden_events": hidden_events,
        },
        "scene": scene,
        "time": time,
        "player_character": player_character,
        "roll_override": {
            "next": roll_next.map(|v| json!(v)).unwrap_or(Value::Null),
            "all": roll_all.map(|v| json!(v)).unwrap_or(Value::Null),
        },
        "status_labels": status_labels(),
        "facts": facts,
        "state_records": state_records,
        "rumors": rumors,
        "npcs": npcs,
        "memory": {
            "gm_summary": gm_summary,
            "gm_messages": gm_messages_len,
            "loaded_gm_tools": loaded_gm_tools,
            "events": events,
            "pending": pending,
            "delivered": delivered,
        },
    })
}
