//! Player-safe live-state projections shared by the HTTP state endpoint and
//! transient turn-stream synchronization.
//!
//! Keeping this projection in the world layer prevents the SSE path and
//! `GET /state` from drifting apart, and makes the privacy boundary explicit:
//! private player notes and undiscovered canonical locations never leave this
//! module.

use std::collections::BTreeSet;

use serde_json::{json, Map, Value};

use crate::canon::{travel::TravelRisk, Place, WorldCanon};
use crate::world::{public_gender_for_locale, public_role_for_locale};
use crate::World;

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

fn current_location_scene(
    place: &Place,
    current_scene: &Value,
    title: &str,
    description: &str,
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
    json!({
        "scene_id": scene_id,
        "location_id": place.place_id,
        "title": title,
        "description": description,
        "present_npcs": present_npcs,
        "npc_whereabouts": npc_whereabouts,
        "exits": visible_scene_rows(current_scene, "exits"),
        "items": visible_scene_rows(current_scene, "items"),
    })
}

fn historical_location_scene(
    canon: &WorldCanon,
    place: &Place,
    visible_place_ids: &BTreeSet<String>,
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
    json!({
        "scene_id": place.place_id,
        "location_id": place.place_id,
        "title": place.name,
        "description": place.default_description,
        "present_npcs": [],
        "npc_whereabouts": {},
        "exits": exits,
        // Canon stores item links, not a durable player-visible item snapshot.
        "items": [],
    })
}

fn location_graph_root(
    canon: &WorldCanon,
    visible_place_ids: &BTreeSet<String>,
    current: Option<&str>,
) -> Option<String> {
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

/// Player-safe projection of the persistent canonical place graph.
///
/// Only visited places (plus the current place) become full nodes. A visible
/// exit whose target is still unknown remains an edge-local placeholder.
pub fn player_location_graph(canon: &WorldCanon, current_scene: &Value) -> Value {
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
            let scene = if is_current {
                current_location_scene(place, current_scene, name, description)
            } else {
                historical_location_scene(canon, place, &visible_place_ids)
            };
            json!({
                "id": place.place_id,
                "name": name,
                "description": description,
                "kind": place.kind,
                "scene": scene,
            })
        })
        .collect::<Vec<_>>();

    let mut edges = Vec::new();
    for from_place_id in &visible_place_ids {
        for transition in canon.exits_from(from_place_id) {
            if !transition.visible {
                continue;
            }
            let parsed_risk = TravelRisk::parse(&transition.risk);
            let profile_is_valid = transition.has_explicit_passage_profile()
                && !transition.kind.trim().is_empty()
                && transition.time_cost > 0
                && parsed_risk.is_some()
                && !crate::canon::travel::has_asymmetric_reciprocal_profile(
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
                .filter(|risk| *risk != TravelRisk::Certain)
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

impl World {
    /// Exact player-visible state used by both the regular state endpoint and
    /// transient per-tool synchronization. This intentionally excludes model,
    /// settings and usage metadata, which cannot change inside a tool call.
    pub fn player_state_export(&mut self) -> Value {
        let content_locale = self.world_canon.content_locale;
        let scene = self.scene_export();
        let location_graph = player_location_graph(&self.world_canon, &scene);
        let entities = self.entity_refs();
        let player_character = self.player_character_export(true);
        let time = self.time_export();

        let npc_ids: Vec<String> = self.npcs.keys().cloned().collect();
        let mut npcs = Vec::with_capacity(npc_ids.len());
        for npc_id in npc_ids {
            let label = self.npc_player_label(&npc_id, "player");
            let known_name = self.npc_known_name(&npc_id, "player");
            let npc = &self.npcs[&npc_id];
            npcs.push(json!({
                "id": npc.npc_id,
                "name": label,
                "label": label,
                "known_name": known_name,
                "public_label": npc.public_label,
                "role": public_role_for_locale(&npc.role, content_locale),
                "pronouns": public_gender_for_locale(&npc.pronouns, content_locale),
                "color": npc.color,
                "physical_type": npc.physical_type,
                "distinctive_features": npc.distinctive_features,
                "current_appearance": npc.current_appearance,
                "condition": npc.condition,
                "life_status": npc.life_status,
            }));
        }

        json!({
            "scene": scene,
            "time": time,
            "player_character": player_character,
            "npcs": npcs,
            "entities": entities,
            "location_graph": location_graph,
        })
    }
}
