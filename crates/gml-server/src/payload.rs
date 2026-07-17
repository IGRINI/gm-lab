//! Payload builders — faithful ports of `server.py`'s `state()`,
//! `export_data()`, `debug_data()`, and the transcript-replay helpers.
//!
//! Each function takes a `&mut DialogRuntime` (so the underlying `World`
//! projection methods that mutate caches — `scene_export`, `entity_refs`,
//! `npc_whereabouts_export` — can run) plus the shared `RuntimeSettings`, and
//! returns the exact JSON shape the React frontend consumes (see
//! `tests/reference/server/{state,debug}.json`).

use std::collections::BTreeSet;

use serde_json::{json, Map, Value};

use gml_config::{Config, RuntimeSettings};
use gml_orchestrator::compact::context_usage;
use gml_orchestrator::Session;
use gml_persistence::{DialogRuntime, DialogVisualAssets, DEFAULT_CHAT_TITLE};
use gml_world::{
    canon::{Place, WorldCanon},
    public_gender, public_role, StateRecordQuery, WHEREABOUTS_STATUS_LABELS,
};

/// Convert a settings `BTreeMap` into a JSON value (object). Frontend
/// `JSON.parse`s the body, so key order is not load-bearing.
fn settings_to_value(map: gml_config::SettingsMap) -> Value {
    serde_json::to_value(map).unwrap_or(Value::Object(Map::new()))
}

fn resolve_model(session: &Session, _cfg: &Config) -> String {
    session.model_binding().model_id().to_string()
}

/// `dict(world_mod.WHEREABOUTS_STATUS_LABELS)` — preserves insertion order.
fn status_labels() -> Value {
    let mut m = Map::new();
    for (k, v) in WHEREABOUTS_STATUS_LABELS {
        m.insert(k.to_string(), Value::String(v.to_string()));
    }
    Value::Object(m)
}

fn json_text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

fn visible_scene_rows(scene: &Value, key: &str) -> Vec<Value> {
    scene
        .get(key)
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter(|row| {
                    row.is_object() && row.get("visible").and_then(Value::as_bool) != Some(false)
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

fn location_image_url(
    place: &Place,
    current_scene: Option<&Value>,
    visual_assets: &DialogVisualAssets,
) -> Option<String> {
    current_scene
        .and_then(|scene| json_text(scene, "image_url"))
        .and_then(crate::safe_dialog_visual_url)
        .or_else(|| {
            [place.place_id.as_str(), place.name.as_str()]
                .into_iter()
                .find_map(|key| {
                    visual_assets
                        .locations
                        .get(key)
                        .and_then(|asset| crate::safe_dialog_visual_url(&asset.url))
                })
        })
}

fn insert_scene_image(scene: &mut Value, image_url: Option<&str>) {
    if let (Some(scene), Some(image_url)) = (scene.as_object_mut(), image_url) {
        scene.insert(
            "image_url".to_string(),
            Value::String(image_url.to_string()),
        );
    }
}

fn current_location_scene(
    place: &Place,
    current_scene: &Value,
    title: &str,
    description: &str,
    image_url: Option<&str>,
) -> Value {
    let scene_id = json_text(current_scene, "scene_id").unwrap_or(&place.place_id);
    let present_npcs = current_scene
        .get("present_npcs")
        .filter(|value| value.is_array())
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));
    let npc_whereabouts = current_scene
        .get("npc_whereabouts")
        .filter(|value| value.is_object())
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    let mut scene = json!({
        "scene_id": scene_id,
        "location_id": place.place_id,
        "title": title,
        "description": description,
        "present_npcs": present_npcs,
        "npc_whereabouts": npc_whereabouts,
        "exits": visible_scene_rows(current_scene, "exits"),
        "items": visible_scene_rows(current_scene, "items"),
    });
    insert_scene_image(&mut scene, image_url);
    scene
}

fn historical_location_scene(
    canon: &WorldCanon,
    place: &Place,
    visible_place_ids: &BTreeSet<String>,
    image_url: Option<&str>,
) -> Value {
    let exits = canon
        .exits_from(&place.place_id)
        .into_iter()
        .filter(|transition| transition.visible)
        .map(|transition| {
            let exit_id = if transition.source_exit_id.trim().is_empty() {
                transition.transition_id.as_str()
            } else {
                transition.source_exit_id.as_str()
            };
            let destination = visible_place_ids
                .contains(&transition.to_place)
                .then(|| canon.places.get(&transition.to_place))
                .flatten()
                .map(|target| target.name.as_str())
                .unwrap_or(transition.destination_hint.as_str());
            json!({
                "exit_id": exit_id,
                "name": transition.label,
                "destination": destination,
                "visible": true,
                "blocked_by": transition.blocked_by,
            })
        })
        .collect::<Vec<_>>();
    let mut scene = json!({
        "scene_id": place.place_id,
        "location_id": place.place_id,
        "title": place.name,
        "description": place.default_description,
        "present_npcs": [],
        "npc_whereabouts": {},
        "exits": exits,
        // Canon stores item links, not a durable player-visible item snapshot.
        // Returning ids here could leak hidden generation data and would not be
        // useful to WorldDetailModal, so historical items stay empty.
        "items": [],
    });
    insert_scene_image(&mut scene, image_url);
    scene
}

/// Player-safe projection of the persistent canonical place graph.
///
/// Only visited places (plus the current place) become full nodes. A visible
/// exit whose target is still unknown to the player remains an edge-local
/// placeholder, so pre-generated canon cannot leak through the state API.
fn player_location_graph(
    canon: &WorldCanon,
    current_scene: &Value,
    visual_assets: &DialogVisualAssets,
) -> Value {
    let player_place_id = canon.player_place_id.trim();
    let current = (!player_place_id.is_empty() && canon.places.contains_key(player_place_id))
        .then(|| player_place_id.to_string());

    let visible_place_ids: BTreeSet<String> = canon
        .places
        .values()
        .filter(|place| place.is_visited() || current.as_deref() == Some(place.place_id.as_str()))
        .map(|place| place.place_id.clone())
        .collect();

    let root = location_graph_root(canon, &visible_place_ids, current.as_deref());
    let nodes = visible_place_ids
        .iter()
        .filter_map(|place_id| canon.places.get(place_id))
        .map(|place| {
            let is_current = current.as_deref() == Some(place.place_id.as_str());
            let live_scene = is_current.then_some(current_scene);
            let name = live_scene
                .and_then(|scene| json_text(scene, "title"))
                .unwrap_or(&place.name);
            let description = live_scene
                .and_then(|scene| json_text(scene, "description"))
                .unwrap_or(&place.default_description);
            let image_url = location_image_url(place, live_scene, visual_assets);
            let scene = if is_current {
                current_location_scene(
                    place,
                    current_scene,
                    name,
                    description,
                    image_url.as_deref(),
                )
            } else {
                historical_location_scene(canon, place, &visible_place_ids, image_url.as_deref())
            };
            let mut node = json!({
                "id": place.place_id,
                "name": name,
                "description": description,
                "kind": place.kind,
                "scene": scene,
            });
            if let (Some(node), Some(image_url)) = (node.as_object_mut(), image_url) {
                node.insert("image_url".to_string(), Value::String(image_url));
            }
            node
        })
        .collect::<Vec<_>>();

    let mut edges = Vec::new();
    for from_place_id in &visible_place_ids {
        for transition in canon.exits_from(from_place_id) {
            if !transition.visible {
                continue;
            }
            let parsed_risk = gml_world::canon::travel::TravelRisk::parse(&transition.risk);
            let profile_is_valid = transition.has_explicit_passage_profile()
                && !transition.kind.trim().is_empty()
                && transition.time_cost > 0
                && parsed_risk.is_some()
                && !gml_world::canon::travel::has_asymmetric_reciprocal_profile(
                    canon,
                    &transition.transition_id,
                );
            let known_target = visible_place_ids
                .contains(&transition.to_place)
                .then(|| transition.to_place.clone());
            let passable = transition.passable && transition.blocked_by.trim().is_empty();
            let mut edge = json!({
                "id": transition.transition_id,
                "from": from_place_id,
                "to": known_target,
                "label": transition.label,
                "description": transition.destination_hint,
                "kind": profile_is_valid.then_some(transition.kind.as_str()),
                "passable": passable,
                "blocked_by": transition.blocked_by,
                "time_cost_minutes": profile_is_valid.then_some(transition.time_cost),
            });
            if transition.has_explicit_passage_profile() {
                let edge_object = edge
                    .as_object_mut()
                    .expect("location graph edge is an object");
                edge_object.insert(
                    "passage_id".to_string(),
                    Value::String(transition.passage_id.clone()),
                );
                edge_object.insert(
                    "directionality".to_string(),
                    json!(transition.directionality),
                );
            }
            if let Some(risk) = parsed_risk
                .filter(|_| profile_is_valid)
                .filter(|risk| *risk != gml_world::canon::travel::TravelRisk::Certain)
            {
                edge["risk"] = json!(risk.as_str());
            }
            if edge.get("to").is_some_and(Value::is_null) {
                let placeholder_name = if transition.label.trim().is_empty() {
                    transition.destination_hint.trim()
                } else {
                    transition.label.trim()
                };
                edge.as_object_mut()
                    .expect("location graph edge is an object")
                    .insert(
                        "placeholder".to_string(),
                        json!({
                            "id": format!("exit:{}", transition.transition_id),
                            "name": placeholder_name,
                            "hint": transition.destination_hint,
                        }),
                    );
            }
            edges.push(edge);
        }
    }

    json!({
        "current": current,
        "root": root,
        "nodes": nodes,
        "edges": edges,
    })
}

fn location_graph_root(
    canon: &WorldCanon,
    visible_place_ids: &BTreeSet<String>,
    current: Option<&str>,
) -> Option<String> {
    // Once the player has moved, the source of the first committed traversal
    // is the stable campaign entry point even when all worldgen places share
    // the same creation turn and parent settlement.
    let first_traversal_source = canon
        .event_log
        .events
        .iter()
        .filter(|event| event.kind == "move_player")
        .flat_map(|event| event.effects.iter())
        .filter_map(|effect| effect.strip_prefix("via:"))
        .filter_map(|transition_id| canon.transitions.get(transition_id))
        .map(|transition| transition.from_place.as_str())
        .find(|place_id| visible_place_ids.contains(*place_id));
    if let Some(place_id) = first_traversal_source {
        return Some(place_id.to_string());
    }

    canon
        .places
        .values()
        .find(|place| {
            visible_place_ids.contains(&place.place_id) && place.provenance.origin == "seed"
        })
        .map(|place| place.place_id.clone())
        .or_else(|| current.map(str::to_string))
        .or_else(|| visible_place_ids.first().cloned())
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
    let visual_assets = runtime.visual_assets.clone();
    let model = resolve_model(&runtime.session, cfg);
    let settings_map = settings.get();
    let stream_gm_content = settings.stream_gm_content_enabled(Some(&settings_map));
    let context = context_usage(&mut runtime.session);
    let run_usage = Value::Object(runtime.session.run_usage.clone());
    let model_binding = runtime.session.model_binding().clone();

    let session = &mut runtime.session;
    let w = &mut session.world;

    let story_id = w.story_id.clone();
    let story_title = w.story_title.clone();
    let story_brief = w.story_brief.clone();
    let public = w.public.clone();
    let time = w.time_export();
    let mut player_character = w.player_character_export(true);
    if let Some(asset) = visual_assets.characters.get(crate::PLAYER_VISUAL_ID) {
        if let (Some(player), Some(url)) = (
            player_character.as_object_mut(),
            crate::safe_dialog_visual_url(&asset.url),
        ) {
            player.insert("portrait_url".to_string(), Value::String(url));
        }
    }
    let mut scene = w.scene_export();
    if let Some(key) = crate::scene_visual_key(&w.scene) {
        if let Some(asset) = visual_assets.locations.get(&key) {
            if let (Some(scene), Some(url)) = (
                scene.as_object_mut(),
                crate::safe_dialog_visual_url(&asset.url),
            ) {
                scene.insert("image_url".to_string(), Value::String(url));
            }
        }
    }
    let location_graph = player_location_graph(&w.world_canon, &scene, &visual_assets);
    let entities = w.entity_refs();

    // Public NPC projection (`public_npc(npc)`), in roster order.
    let npc_ids: Vec<String> = w.npcs.keys().cloned().collect();
    let mut npcs: Vec<Value> = Vec::with_capacity(npc_ids.len());
    for npc_id in &npc_ids {
        let label = w.npc_player_label(npc_id, "player");
        let known_name = w.npc_known_name(npc_id, "player");
        let npc = &w.npcs[npc_id];
        let mut row = json!({
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
            "current_appearance": npc.current_appearance,
            "condition": npc.condition,
            "life_status": npc.life_status,
        });
        if let Some(asset) = visual_assets.characters.get(npc_id) {
            if let Some(url) = crate::safe_dialog_visual_url(&asset.url) {
                row.as_object_mut()
                    .expect("NPC public projection is an object")
                    .insert("portrait_url".to_string(), Value::String(url));
            }
        }
        npcs.push(row);
    }

    let mut data = Map::new();
    data.insert("model".to_string(), Value::String(model));
    data.insert(
        "backend".to_string(),
        Value::String(model_binding.connector_id().as_str().to_string()),
    );
    data.insert(
        "model_binding".to_string(),
        serde_json::to_value(&model_binding).unwrap_or(Value::Null),
    );
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
    // K1 (§К1.5): surface the launched CHARACTER package provenance so the
    // player-facing "save hero" control can offer "update the source" only when
    // `char_ref` resolves. Emitted only when `Some` (mirrors the additive
    // byte-identity discipline of `world_to_payload`); absent -> the UI treats
    // it as null and offers "save as new" only.
    if let Some(char_ref) = &w.char_ref {
        data.insert(
            "char_ref".to_string(),
            json!({ "id": char_ref.id, "version": char_ref.version }),
        );
    }
    // Surface the launched WORLD package provenance (mirrors char_ref) so the
    // game-context bar can resolve the world name/genre/tone from the loaded
    // worlds list. Emitted only when `Some`; absent -> UI shows a generic label.
    if let Some(world_ref) = &w.world_ref {
        data.insert(
            "world_ref".to_string(),
            json!({ "id": world_ref.id, "version": world_ref.version }),
        );
    }
    data.insert("scene".to_string(), scene);
    data.insert("location_graph".to_string(), location_graph);
    data.insert("entities".to_string(), entities);
    data.insert("status_labels".to_string(), status_labels());
    data.insert("npcs".to_string(), Value::Array(npcs));
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
        "model_binding": runtime.session.model_binding(),
        "rewindable_turns": runtime.rewindable_turns,
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
        let mut replayed = sanitize_player_name(event, &by_id, &by_name);
        if let Some(replayed) = replayed.as_object_mut() {
            if let Some(turn) = row.get("turn").and_then(Value::as_i64) {
                replayed.insert("turn".to_string(), json!(turn));
                if event.get("kind").and_then(Value::as_str) == Some("player") {
                    replayed.insert(
                        "rewindable".to_string(),
                        Value::Bool(runtime.rewindable_turns.contains(&turn)),
                    );
                }
            }
            if event.get("kind").and_then(Value::as_str) == Some("player") {
                if let Some(request_id) = row.get("request_id").and_then(Value::as_str) {
                    replayed.insert("message_id".to_string(), json!(request_id));
                    replayed.insert("request_id".to_string(), json!(request_id));
                }
            }
        }
        events.push(replayed);
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
            "backend": session.model_binding().connector_id().as_str(),
            "model_binding": session.model_binding(),
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
    let prompt_cache_key = session.client.prompt_cache_key();
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
            "current_appearance": npc.current_appearance,
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
            "backend": session.model_binding().connector_id().as_str(),
            "model_binding": session.model_binding(),
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

#[cfg(test)]
mod location_graph_tests {
    use std::collections::BTreeSet;

    use gml_persistence::{DialogVisualAsset, DialogVisualAssets};
    use gml_world::canon::{PassageDirectionality, Place, Provenance, Transition, WorldCanon};
    use serde_json::{json, Value};

    use super::player_location_graph;

    fn place(id: &str, name: &str, visited: bool, provenance: Provenance) -> Place {
        let mut state_flags = BTreeSet::new();
        if visited {
            state_flags.insert("visited".to_string());
        }
        Place {
            place_id: id.to_string(),
            name: name.to_string(),
            kind: "room".to_string(),
            default_description: format!("Описание: {name}"),
            state_flags,
            provenance,
            ..Default::default()
        }
    }

    fn transition(
        id: &str,
        from: &str,
        to: &str,
        label: &str,
        hint: &str,
        visible: bool,
    ) -> Transition {
        Transition {
            transition_id: id.to_string(),
            from_place: from.to_string(),
            to_place: to.to_string(),
            label: label.to_string(),
            destination_hint: hint.to_string(),
            kind: "door".to_string(),
            visible,
            passable: true,
            time_cost: 1,
            risk: "none".to_string(),
            ..Default::default()
        }
    }

    fn graph_node<'a>(graph: &'a Value, id: &str) -> &'a Value {
        graph["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .find(|node| node["id"] == id)
            .expect("graph node")
    }

    fn graph_edge<'a>(graph: &'a Value, id: &str) -> Option<&'a Value> {
        graph["edges"]
            .as_array()
            .expect("edges")
            .iter()
            .find(|edge| edge["id"] == id)
    }

    #[test]
    fn graph_exposes_visited_and_current_places_without_leaking_unknown_targets() {
        let mut canon = WorldCanon::default();
        canon.insert_place(place("kitchen", "Кухня", true, Provenance::seed()));
        canon.insert_place(place(
            "hall",
            "Коридор",
            false,
            Provenance::by("worldgen", "prepared", 0),
        ));
        canon.insert_place(place(
            "yard",
            "Тайный двор с контрабандистами",
            false,
            Provenance::by("worldgen", "hidden", 0),
        ));
        canon.player_place_id = "hall".to_string();
        canon.insert_transition(transition(
            "kitchen_hall",
            "kitchen",
            "hall",
            "В коридор",
            "коридор",
            true,
        ));
        canon
            .transitions
            .get_mut("kitchen_hall")
            .expect("hall transition")
            .risk = "low".to_string();
        let kitchen_hall = canon
            .transitions
            .get_mut("kitchen_hall")
            .expect("hall transition");
        kitchen_hall.passage_id = "kitchen_hall_passage".to_string();
        kitchen_hall.directionality = PassageDirectionality::OneWay;
        canon.insert_transition(transition(
            "kitchen_yard",
            "kitchen",
            "yard",
            "Выход в задний двор",
            "задний двор",
            true,
        ));
        canon.insert_transition(transition(
            "hall_unknown",
            "hall",
            "",
            "Дверь вниз",
            "нижний этаж",
            true,
        ));
        canon.insert_transition(transition(
            "hidden_tunnel",
            "kitchen",
            "yard",
            "Скрытый тоннель",
            "тайный путь",
            false,
        ));
        let yard_exit = canon
            .transitions
            .get_mut("kitchen_yard")
            .expect("yard exit");
        yard_exit.passable = false;
        yard_exit.blocked_by = "ржавая цепь".to_string();
        yard_exit.time_cost = 7;
        yard_exit.conditions = vec!["gm-only: тайный знак".to_string()];
        yard_exit.risk = "gm-only: засада контрабандистов".to_string();

        let current_scene = json!({
            "scene_id": "hall-live",
            "location_id": "hall",
            "title": "Закопчённый коридор",
            "description": "На стенах ещё свежая копоть.",
            "present_npcs": ["guard"],
            "npc_whereabouts": {},
            "exits": [
                {
                    "exit_id": "live_exit",
                    "name": "Дверь в зал",
                    "destination": "зал",
                    "visible": true,
                    "blocked_by": "",
                },
                {
                    "exit_id": "secret_exit",
                    "name": "Секретный лаз",
                    "destination": "тайник",
                    "visible": false,
                    "blocked_by": "",
                },
            ],
            "items": [
                {"item_id": "lamp", "name": "Фонарь", "visible": true},
                {"item_id": "key", "name": "Спрятанный ключ", "visible": false},
            ],
        });

        let graph = player_location_graph(&canon, &current_scene, &DialogVisualAssets::default());

        assert_eq!(graph["current"], "hall");
        assert_eq!(graph["root"], "kitchen");
        assert_eq!(graph["nodes"].as_array().expect("nodes").len(), 2);
        assert!(graph_node(&graph, "kitchen").is_object());
        let hall = graph_node(&graph, "hall");
        assert_eq!(hall["name"], "Закопчённый коридор");
        assert_eq!(hall["scene"]["scene_id"], "hall-live");
        assert_eq!(hall["scene"]["exits"].as_array().unwrap().len(), 1);
        assert_eq!(hall["scene"]["items"].as_array().unwrap().len(), 1);
        let kitchen_scene = &graph_node(&graph, "kitchen")["scene"];
        assert_eq!(kitchen_scene["title"], "Кухня");
        assert_eq!(kitchen_scene["items"], json!([]));
        assert_eq!(kitchen_scene["exits"].as_array().unwrap().len(), 2);
        assert_eq!(kitchen_scene["exits"][1]["destination"], "задний двор");
        assert!(graph_edge(&graph, "hidden_tunnel").is_none());
        assert_eq!(graph_edge(&graph, "kitchen_hall").unwrap()["to"], "hall");
        assert_eq!(graph_edge(&graph, "kitchen_hall").unwrap()["risk"], "low");
        assert_eq!(
            graph_edge(&graph, "kitchen_hall").unwrap()["passage_id"],
            "kitchen_hall_passage"
        );
        assert_eq!(
            graph_edge(&graph, "kitchen_hall").unwrap()["directionality"],
            "one_way"
        );
        assert!(graph_edge(&graph, "kitchen_hall").unwrap()["placeholder"].is_null());
        assert_eq!(
            graph_edge(&graph, "kitchen_yard").unwrap(),
            &json!({
                "id": "kitchen_yard",
                "from": "kitchen",
                "to": null,
                "label": "Выход в задний двор",
                "description": "задний двор",
                "kind": null,
                "passable": false,
                "blocked_by": "ржавая цепь",
                "time_cost_minutes": null,
                "placeholder": {
                    "id": "exit:kitchen_yard",
                    "name": "Выход в задний двор",
                    "hint": "задний двор",
                },
            })
        );
        assert_eq!(
            graph_edge(&graph, "hall_unknown").unwrap()["placeholder"]["hint"],
            "нижний этаж"
        );
        let serialized = graph.to_string();
        assert!(!serialized.contains("Тайный двор с контрабандистами"));
        assert!(!serialized.contains("Скрытый тоннель"));
        assert!(!serialized.contains("Секретный лаз"));
        assert!(!serialized.contains("Спрятанный ключ"));
        assert!(!serialized.contains("gm-only"));
        assert!(!serialized.contains("засада контрабандистов"));
    }

    #[test]
    fn graph_adds_only_safe_persisted_images_to_full_nodes() {
        let mut canon = WorldCanon::default();
        canon.insert_place(place("kitchen", "Кухня", true, Provenance::seed()));
        canon.insert_place(place("hall", "Коридор", true, Provenance::seed()));
        canon.player_place_id = "hall".to_string();

        let mut assets = DialogVisualAssets::default();
        assets.locations.insert(
            "kitchen".to_string(),
            DialogVisualAsset {
                url: "/image-files/run/kitchen.png".to_string(),
                provider: "test".to_string(),
                model: String::new(),
            },
        );
        assets.locations.insert(
            "hall".to_string(),
            DialogVisualAsset {
                url: "https://example.invalid/private.png".to_string(),
                provider: "test".to_string(),
                model: String::new(),
            },
        );

        let current_scene = json!({
            "scene_id": "hall",
            "location_id": "hall",
            "title": "Коридор",
            "description": "Описание: Коридор",
            "image_url": "https://example.invalid/live-private.png",
            "exits": [],
            "items": [],
        });
        let graph = player_location_graph(&canon, &current_scene, &assets);

        assert_eq!(
            graph_node(&graph, "kitchen")["image_url"],
            "/image-files/run/kitchen.png"
        );
        assert_eq!(
            graph_node(&graph, "kitchen")["scene"]["image_url"],
            "/image-files/run/kitchen.png"
        );
        assert!(graph_node(&graph, "hall")["image_url"].is_null());
        assert!(graph_node(&graph, "hall")["scene"]["image_url"].is_null());
    }

    #[test]
    fn graph_hides_an_asymmetric_route_profile_until_it_is_reauthored() {
        let mut canon = WorldCanon::default();
        canon.insert_place(place("alley", "Переулок", true, Provenance::seed()));
        canon.insert_place(place("shop", "Лавка", true, Provenance::seed()));
        canon.player_place_id = "alley".to_string();

        let mut forward = transition("alley_to_shop", "alley", "shop", "В лавку", "лавка", true);
        forward.passage_id = "alley_shop_passage".to_string();
        forward.directionality = PassageDirectionality::Bidirectional;
        forward.kind = "path".to_string();
        forward.time_cost = 4;
        forward.risk = "medium".to_string();
        canon.insert_transition(forward);

        let mut reverse = transition(
            "shop_to_alley",
            "shop",
            "alley",
            "В переулок",
            "переулок",
            true,
        );
        reverse.passage_id = "alley_shop_passage".to_string();
        reverse.directionality = PassageDirectionality::Bidirectional;
        reverse.kind = "door".to_string();
        reverse.time_cost = 1;
        reverse.risk = "none".to_string();
        canon.insert_transition(reverse);

        let graph = player_location_graph(&canon, &json!({}), &DialogVisualAssets::default());
        for transition_id in ["alley_to_shop", "shop_to_alley"] {
            let edge = graph_edge(&graph, transition_id).expect("route edge");
            assert!(edge["kind"].is_null());
            assert!(edge["time_cost_minutes"].is_null());
            assert!(edge["risk"].is_null());
        }
    }

    #[test]
    fn graph_preserves_a_closed_bidirectional_passage_and_its_blocker() {
        let mut canon = WorldCanon::default();
        canon.insert_place(place("cave", "Пещера", true, Provenance::seed()));
        canon.insert_place(place("ledge", "Уступ", true, Provenance::seed()));
        canon.player_place_id = "cave".to_string();

        let mut down = transition(
            "cave_to_ledge",
            "cave",
            "ledge",
            "По верёвке вниз",
            "уступ",
            true,
        );
        down.passage_id = "cave_rope".to_string();
        down.directionality = PassageDirectionality::Bidirectional;
        down.passable = false;
        down.blocked_by = "верёвку убрали".to_string();
        canon.insert_transition(down);

        let mut up = transition(
            "ledge_to_cave",
            "ledge",
            "cave",
            "По верёвке наверх",
            "пещера",
            true,
        );
        up.passage_id = "cave_rope".to_string();
        up.directionality = PassageDirectionality::Bidirectional;
        // Harden the player projection against an inconsistent legacy save:
        // a recorded blocker always makes the passage unavailable.
        up.passable = true;
        up.blocked_by = "верёвку убрали".to_string();
        canon.insert_transition(up);

        let closed = player_location_graph(&canon, &json!({}), &DialogVisualAssets::default());
        assert_eq!(closed["edges"].as_array().expect("edges").len(), 2);
        for transition_id in ["cave_to_ledge", "ledge_to_cave"] {
            let edge = graph_edge(&closed, transition_id).expect("closed passage edge");
            assert_eq!(edge["passage_id"], "cave_rope");
            assert_eq!(edge["directionality"], "bidirectional");
            assert_eq!(edge["passable"], false);
            assert_eq!(edge["blocked_by"], "верёвку убрали");
        }

        for transition_id in ["cave_to_ledge", "ledge_to_cave"] {
            let transition = canon
                .transitions
                .get_mut(transition_id)
                .expect("passage direction");
            transition.passable = true;
            transition.blocked_by.clear();
        }
        let reopened = player_location_graph(&canon, &json!({}), &DialogVisualAssets::default());
        assert_eq!(reopened["edges"].as_array().expect("edges").len(), 2);
        for transition_id in ["cave_to_ledge", "ledge_to_cave"] {
            let edge = graph_edge(&reopened, transition_id).expect("reopened passage edge");
            assert_eq!(edge["passable"], true);
            assert_eq!(edge["blocked_by"], "");
        }
    }
}
