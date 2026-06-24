//! World-seed building + scene-delta extraction — faithful port of the
//! LLM-driven helpers in `agents.py` (`build_world_seed`, `extract_scene_delta`,
//! and all `_seed_*` / `_brief_*` / `_apply_brief_display_names` helpers).
//!
//! The LLM call goes through a [`Backend`]; request/response shaping and the
//! repair logic are ported faithfully.

use std::collections::BTreeSet;

use regex::Regex;
use serde_json::{json, Map, Value};

use gml_llm::{Backend, BackendError};
use gml_types::Role;
use gml_world::World;

use crate::coerce::{as_list, text};

/// `SCENE_DELTA_SCHEMA`.
pub fn scene_delta_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "moves": {"type": "array", "items": {"type": "object", "properties": {
                "npc_id": {"type": "string"},
                "present": {"type": "boolean"},
                "location": {"type": "string"},
                "visible": {"type": "boolean"},
                "can_hear": {"type": "boolean"},
                "activity": {"type": "string"},
                "attitude": {"type": "string"},
                "reason": {"type": "string"},
            }, "required": ["npc_id", "present", "reason"]}},
        },
        "required": ["moves"],
    })
}

/// `WORLD_SEED_SCHEMA`.
pub fn world_seed_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "public_intro": {"type": "string"},
            "hidden_truth": {"type": "string"},
            "proper_nouns": {"type": "array", "items": {"type": "string"}},
            "public_facts": {"type": "array", "items": {"type": "string"}},
            "npcs": {"type": "array", "items": {"type": "object", "properties": {
                "id": {"type": "string"},
                "name": {"type": "string"},
                "role": {"type": "string"},
                "gender": {
                    "type": "string",
                    "description": "Russian grammatical gender marker: M, F, N, PL, OTHER, or a short custom Russian note.",
                },
                "persona": {"type": "string"},
                "voice": {"type": "string"},
                "goals": {"type": "string"},
                "knowledge": {"type": "string"},
                "secret": {"type": "string"},
            }, "required": ["id", "name", "role", "persona", "voice", "goals", "knowledge", "secret"]}},
            "scene": {"type": "object", "properties": {
                "id": {"type": "string"},
                "location_id": {"type": "string"},
                "title": {"type": "string"},
                "description": {"type": "string"},
                "present_npcs": {"type": "array", "items": {"type": "string"}},
                "items": {"type": "array", "items": {"type": "object", "properties": {
                    "id": {"type": "string"},
                    "name": {"type": "string"},
                    "location": {"type": "string"},
                    "visible": {"type": "boolean"},
                    "portable": {"type": "boolean"},
                    "owner": {"type": "string"},
                    "details": {"type": "string"},
                }, "required": ["id", "name", "location", "visible", "portable"]}},
                "exits": {"type": "array", "items": {"type": "object", "properties": {
                    "id": {"type": "string"},
                    "name": {"type": "string"},
                    "destination": {"type": "string"},
                    "visible": {"type": "boolean"},
                    "blocked_by": {"type": "string"},
                }, "required": ["id", "name", "destination", "visible"]}},
                "constraints": {"type": "array", "items": {"type": "string"}},
                "tension": {"type": "string"},
            }, "required": ["id", "location_id", "title", "description", "present_npcs",
                             "items", "exits", "constraints", "tension"]},
        },
        "required": ["public_intro", "hidden_truth", "proper_nouns", "public_facts", "npcs", "scene"],
    })
}

fn obj(v: &Value) -> Option<&Map<String, Value>> {
    v.as_object()
}

/// `_seed_present_ids(seed)`.
fn seed_present_ids(seed: &Value) -> Vec<String> {
    let o = match obj(seed) {
        Some(o) => o,
        None => return Vec::new(),
    };
    let scene = o.get("scene").filter(|v| v.is_object());
    let raw = scene
        .and_then(|s| s.get("present_npcs"))
        .filter(|v| !v.is_null())
        .or_else(|| o.get("present_npcs"))
        .cloned()
        .unwrap_or(Value::Null);
    match raw {
        Value::Array(a) => a.iter().map(text).filter(|s| !s.is_empty()).collect(),
        _ => Vec::new(),
    }
}

/// `_seed_named_npcs(seed)`.
fn seed_named_npcs(seed: &Value) -> BTreeSet<String> {
    let o = match obj(seed) {
        Some(o) => o,
        None => return BTreeSet::new(),
    };
    let mut named: BTreeSet<String> = BTreeSet::new();
    match o.get("npcs") {
        Some(Value::Array(npcs)) => {
            for raw in npcs {
                if let Some(m) = raw.as_object() {
                    let id = text(m.get("id").unwrap_or(&Value::Null));
                    let name = text(m.get("name").unwrap_or(&Value::Null));
                    if !id.is_empty() && !name.is_empty() {
                        named.insert(id);
                    }
                }
            }
        }
        Some(Value::Object(npcs)) => {
            for (npc_id, raw) in npcs {
                if let Some(m) = raw.as_object() {
                    if !text(m.get("name").unwrap_or(&Value::Null)).is_empty() {
                        named.insert(text(&Value::String(npc_id.clone())));
                    }
                }
            }
        }
        _ => {}
    }
    if let Some(Value::Object(details)) = o.get("npc_details") {
        for (npc_id, raw) in details {
            if let Some(m) = raw.as_object() {
                if !text(m.get("name").unwrap_or(&Value::Null)).is_empty() {
                    named.insert(text(&Value::String(npc_id.clone())));
                }
            }
        }
    }
    named
}

/// `_seed_needs_npc_repair(seed)`.
fn seed_needs_npc_repair(seed: &Value) -> bool {
    let present: BTreeSet<String> = seed_present_ids(seed).into_iter().collect();
    if present.is_empty() {
        return true;
    }
    let named = seed_named_npcs(seed);
    !present.is_subset(&named)
}

/// `_has_cyrillic(text)`.
fn has_cyrillic(s: &str) -> bool {
    s.chars()
        .any(|c| ('А'..='я').contains(&c) || c == 'Ё' || c == 'ё')
}

/// `_seed_player_facing_text(seed)`.
fn seed_player_facing_text(seed: &Value) -> String {
    let o = match obj(seed) {
        Some(o) => o,
        None => return String::new(),
    };
    let scene_owned = o.get("scene").filter(|v| v.is_object()).cloned();
    let scene = scene_owned.as_ref().and_then(|v| v.as_object());
    let g = |key: &str| -> Value { o.get(key).cloned().unwrap_or(Value::Null) };
    let sg = |key: &str| -> Value {
        scene
            .and_then(|m| m.get(key))
            .cloned()
            .unwrap_or(Value::Null)
    };
    let mut parts: Vec<String> = vec![
        text(&g("story_brief")),
        text(&g("public_intro")),
        text(&g("location_name")),
        text(&g("description")),
        text(&sg("title")),
        text(&sg("name")),
        text(&sg("description")),
    ];
    // public_facts
    for item in as_list(&g("public_facts"))
        .iter()
        .chain(as_list(&sg("public_facts")).iter())
    {
        parts.push(text(item));
    }
    for key in [
        "visible_objects",
        "objects",
        "items",
        "visible_exits",
        "exits",
    ] {
        for item in as_list(&g(key)).iter().chain(as_list(&sg(key)).iter()) {
            if let Some(m) = item.as_object() {
                parts.push(text(m.get("name").unwrap_or(&Value::Null)));
                parts.push(text(m.get("display_name").unwrap_or(&Value::Null)));
                parts.push(text(m.get("description").unwrap_or(&Value::Null)));
            } else {
                parts.push(text(item));
            }
        }
    }
    parts
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// `_seed_needs_text_repair(seed, brief)`.
fn seed_needs_text_repair(seed: &Value, brief: &str) -> bool {
    has_cyrillic(brief) && !has_cyrillic(&seed_player_facing_text(seed))
}

fn cyr_to_lat(ch: char) -> Option<&'static str> {
    Some(match ch {
        'а' => "a",
        'б' => "b",
        'в' => "v",
        'г' => "g",
        'д' => "d",
        'е' => "e",
        'ё' => "e",
        'ж' => "zh",
        'з' => "z",
        'и' => "i",
        'й' => "y",
        'к' => "k",
        'л' => "l",
        'м' => "m",
        'н' => "n",
        'о' => "o",
        'п' => "p",
        'р' => "r",
        'с' => "s",
        'т' => "t",
        'у' => "u",
        'ф' => "f",
        'х' => "h",
        'ц' => "ts",
        'ч' => "ch",
        'ш' => "sh",
        'щ' => "sch",
        'ъ' => "",
        'ы' => "y",
        'ь' => "",
        'э' => "e",
        'ю' => "yu",
        'я' => "ya",
        _ => return None,
    })
}

/// `_brief_name_slug(name)`.
fn brief_name_slug(name: &str) -> String {
    let lowered = name.to_lowercase();
    let mut raw = String::new();
    for ch in lowered.chars() {
        match cyr_to_lat(ch) {
            Some(s) => raw.push_str(s),
            None => raw.push(ch),
        }
    }
    let re = Regex::new(r"[^a-z0-9_]+").unwrap();
    re.replace_all(&raw, "_").trim_matches('_').to_string()
}

/// `_apply_brief_display_names(seed, brief)` — mutates the seed in place.
fn apply_brief_display_names(mut seed: Value, brief: &str) -> Value {
    if !seed.is_object() {
        return seed;
    }
    let name_re = Regex::new(r"\b[А-ЯЁ][а-яё]{1,24}\b").unwrap();
    let mut by_slug: indexmap::IndexMap<String, String> = indexmap::IndexMap::new();
    for m in name_re.find_iter(brief) {
        // Python dict: later duplicates overwrite earlier; insert overwrites.
        by_slug.insert(brief_name_slug(m.as_str()), m.as_str().to_string());
    }

    let apply = |raw: &mut Value, npc_id: &str, by_slug: &indexmap::IndexMap<String, String>| {
        let slug = brief_name_slug(npc_id);
        if let Some(wanted) = by_slug.get(&slug) {
            if let Some(m) = raw.as_object_mut() {
                m.insert("name".to_string(), Value::String(wanted.clone()));
            }
        }
    };

    let o = seed.as_object_mut().unwrap();
    match o.get_mut("npcs") {
        Some(Value::Array(npcs)) => {
            for raw in npcs.iter_mut() {
                if raw.is_object() {
                    let npc_id = text(raw.get("id").unwrap_or(&Value::Null));
                    apply(raw, &npc_id, &by_slug);
                }
            }
        }
        Some(Value::Object(npcs)) => {
            for (npc_id, raw) in npcs.iter_mut() {
                let id = npc_id.clone();
                apply(raw, &id, &by_slug);
            }
        }
        _ => {}
    }
    if let Some(Value::Object(details)) = o.get_mut("npc_details") {
        for (npc_id, raw) in details.iter_mut() {
            let id = npc_id.clone();
            apply(raw, &id, &by_slug);
        }
    }
    seed
}

/// `build_world_seed(client, brief)` — ask the model for a new playable world
/// seed; World validates it afterwards. Repair logic preserved.
pub async fn build_world_seed(client: &dyn Backend, brief: &str) -> Result<Value, BackendError> {
    let system =
        "Create a compact tabletop RP starting scene from the user's brief. Return JSON only. \
This is not prose for the player; it is a seed that code will validate. Keep it small: \
2-4 NPCs, 2-5 visible objects, 1-3 visible exits, 3-6 public facts. \
Include `story_brief`: 2-4 short Russian sentences shown to the player at the start: \
who they are, where they are, what just happened, and what is expected from them. \
Do not put hidden truth, GM-only causes, future spoilers, or mechanical meta-information in `story_brief`. \
NPC ids must be lowercase ascii snake_case. Put only NPC ids in scene.present_npcs. \
Every present NPC must also have a full object in `npcs` with id, exact display name, \
role, gender marker if known, persona, voice, goals, knowledge, and secret. Use `gender` \
as M, F, N, PL, OTHER, or a short custom Russian note: M=он/masculine, F=она/feminine, \
N=оно/neuter, PL=они/plural. If the \
user gives NPC names, preserve those names exactly in `name`; never return only ids \
like iva/run without display names. \
All player-facing seed text must be in Russian: story_brief, public_intro, scene title, scene \
description, item names, exit names, public facts, NPC display names, NPC roles, \
NPC persona/voice/goals summaries, gender custom notes, and scene positions/activities. Preserve \
Russian proper nouns from the brief exactly; do not translate them. \
The scene must contain enough concrete state to start play: where the player is, \
who is present, what is visible, what exits exist, and what physical limits matter. \
Do not create action ids or intent ids; characters will act in free text.";

    let brief_user = {
        let t = brief.trim();
        if t.is_empty() {
            "Create a small mystery scene.".to_string()
        } else {
            t.to_string()
        }
    };
    let messages = json!([
        {"role": "system", "content": system},
        {"role": "user", "content": brief_user},
    ]);
    let schema = world_seed_schema();
    let raw = client
        .chat_json(&messages, &schema, Some(false), Role::Gm.as_str())
        .await?;
    let seed = apply_brief_display_names(Value::Object(raw), brief);
    if !seed_needs_npc_repair(&seed) && !seed_needs_text_repair(&seed, brief) {
        return Ok(seed);
    }
    let repair_system =
        "Repair this tabletop RP world seed into the required strict JSON shape. Return JSON \
only. Keep the same scene idea, visible objects, exits, and public facts. Create a \
`npcs` array with one full NPC object for every id in scene.present_npcs or \
present_npcs. Preserve exact user-provided display names from the brief, especially \
Cyrillic names. NPC ids remain lowercase ascii snake_case; NPC `name` is the display \
name shown to the player. Include `story_brief`: 2-4 short Russian sentences for the \
player's start card with no hidden truth, future spoilers, or mechanics. All player-facing \
strings must be in Russian: story_brief, scene title, scene description, item names, exit names, public facts, NPC display names, NPC roles, \
NPC persona/voice/goals summaries, gender custom notes, and scene positions/activities. \
Use `gender` as M, F, N, PL, OTHER, or a short custom Russian note. Keep \
proper nouns from the brief exactly, for example do not translate Russian names of \
places, ships, people, factions, or objects. Do not add action ids or intent ids.";

    let broken = serde_json::to_string(&seed).unwrap_or_default();
    let repair_user = format!("USER BRIEF:\n{}\n\nBROKEN SEED:\n{}", brief_user, broken);
    let repair_messages = json!([
        {"role": "system", "content": repair_system},
        {"role": "user", "content": repair_user},
    ]);
    let repaired_raw = client
        .chat_json(&repair_messages, &schema, Some(false), Role::Gm.as_str())
        .await?;
    let repaired = apply_brief_display_names(Value::Object(repaired_raw), brief);
    // Python: `repaired if isinstance(repaired, dict) and repaired else seed`.
    match repaired.as_object() {
        Some(m) if !m.is_empty() => Ok(repaired),
        _ => Ok(seed),
    }
}

/// `extract_scene_delta(client, world, narration)` — extract explicit roster
/// changes from accepted final narration (state sync, not validation).
pub async fn extract_scene_delta(
    client: &dyn Backend,
    world: &mut World,
    narration: &str,
) -> Result<Map<String, Value>, BackendError> {
    let roster: String = world
        .npcs
        .values()
        .map(|npc| {
            let mut line = format!("- {}: {}, {}", npc.npc_id, npc.name, npc.role);
            if !npc.pronouns.is_empty() {
                line.push_str(&format!(", род: {}", crate::public_gender(&npc.pronouns)));
            }
            if world.scene.present_npcs.contains(&npc.npc_id) {
                line.push_str("; present");
            } else {
                line.push_str("; absent");
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n");

    let system = "Extract only explicit current-scene NPC roster changes from the GM narration. \
Use only npc_id values from the roster. Return JSON only. \
A move with present=true means the NPC explicitly entered/arrived/is now in the \
current scene or can hear it. A move with present=false means the NPC explicitly \
left/exited/went to another room/is no longer able to hear. \
Track the roster at the END of the narration for the CURRENT SCENE only. If the \
narration moves the player and an NPC outside the current scene, do not add that \
NPC to the old current scene. \
Do NOT infer from wishes, requests, plans, future possibilities, searches, rumors, \
or someone being mentioned as absent. If there is no explicit roster change, \
return {\"moves\":[]}.";

    let scene_context = world.scene_context();
    let user = format!(
        "ROSTER:\n{}\n\nCURRENT SCENE:\n{}\n\nGM NARRATION:\n{}",
        roster, scene_context, narration
    );
    let messages = json!([
        {"role": "system", "content": system},
        {"role": "user", "content": user},
    ]);
    client
        .chat_json(
            &messages,
            &scene_delta_schema(),
            Some(false),
            Role::Gm.as_str(),
        )
        .await
}
