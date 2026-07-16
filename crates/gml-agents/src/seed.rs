//! World-seed building + scene-delta extraction — faithful port of the
//! LLM-driven helpers in `agents.py` (`build_world_seed`, `extract_scene_delta`,
//! and all `_seed_*` / `_brief_*` / `_apply_brief_display_names` helpers).
//!
//! The LLM call goes through a [`Backend`]; request/response shaping and the
//! repair logic are ported faithfully.

use std::collections::BTreeSet;

use regex::Regex;
use serde_json::{json, Map, Value};

use gml_llm::{Backend, BackendError, NullSink};
use gml_types::Role;
use gml_world::World;

use crate::coerce::{as_list, text};

const WORLD_SEED_OUTPUT_EXAMPLE: &str = r#"{"story_brief":"Кто игрок, где он находится, что произошло и что от него ждут.","public_intro":"Короткое вступление для игрока.","hidden_truth":"Скрытая истина для ГМ.","proper_nouns":["Точное имя"],"public_facts":["Публичный факт"],"npcs":[{"id":"npc_id","name":"Имя","role":"Роль","gender":"M","persona":"Характер","voice":"Манера речи","goals":"Цели","knowledge":"Знания","secret":"Тайна"}],"scene":{"id":"scene_id","location_id":"location_id","title":"Название сцены","description":"Описание сцены","present_npcs":["npc_id"],"items":[{"id":"item_id","name":"Предмет","location":"Где находится","visible":true,"portable":true,"owner":"","details":"Детали"}],"exits":[{"id":"exit_id","name":"Выход","destination":"Куда ведёт","visible":true,"blocked_by":""}],"constraints":["Физическое ограничение"],"tension":"Текущее напряжение"}}"#;

const SCENE_DELTA_OUTPUT_EXAMPLE: &str = r#"{"moves":[{"npc_id":"npc_id","present":true,"reason":"явно вошёл в сцену","location":"текущее место","visible":true,"can_hear":true,"activity":"что делает","attitude":"текущее отношение"}]}"#;

fn with_world_seed_output_example(instructions: &str) -> String {
    format!(
        "{instructions}\n\nReturn exactly one JSON object with this shape; repeat array entries as needed:\n{WORLD_SEED_OUTPUT_EXAMPLE}"
    )
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
    let system = with_world_seed_output_example(
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
Do not create action ids or intent ids; characters will act in free text.",
    );

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
    let raw = client
        .chat_json(&messages, Some(false), Role::Gm.as_str())
        .await?;
    let seed = apply_brief_display_names(Value::Object(raw), brief);
    if !seed_needs_npc_repair(&seed) && !seed_needs_text_repair(&seed, brief) {
        return Ok(seed);
    }
    let repair_system = with_world_seed_output_example(
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
places, ships, people, factions, or objects. Do not add action ids or intent ids.",
    );

    let broken = serde_json::to_string(&seed).unwrap_or_default();
    let repair_user = format!("USER BRIEF:\n{}\n\nBROKEN SEED:\n{}", brief_user, broken);
    let repair_messages = json!([
        {"role": "system", "content": repair_system},
        {"role": "user", "content": repair_user},
    ]);
    let repaired_raw = client
        .chat_json(&repair_messages, Some(false), Role::Gm.as_str())
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
) -> Result<(Map<String, Value>, Map<String, Value>), BackendError> {
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

    let system = format!(
        "Extract only explicit current-scene NPC roster changes from the GM narration. \
Use only npc_id values from the roster. Return JSON only. \
A move with present=true means the NPC explicitly entered/arrived/is now in the \
current scene or can hear it. A move with present=false means the NPC explicitly \
left/exited/went to another room/is no longer able to hear. \
Track the roster at the END of the narration for the CURRENT SCENE only. If the \
narration moves the player and an NPC outside the current scene, do not add that \
NPC to the old current scene. \
Do NOT infer from wishes, requests, plans, future possibilities, searches, rumors, \
or someone being mentioned as absent. Every non-empty move MUST contain npc_id, \
present, and reason. Optional fields are location, visible, can_hear, activity, and \
attitude. Example: {SCENE_DELTA_OUTPUT_EXAMPLE} If there is no explicit roster \
change, return {{\"moves\":[]}}."
    );

    let scene_context = world.scene_context();
    let user = format!(
        "ROSTER:\n{}\n\nCURRENT SCENE:\n{}\n\nGM NARRATION:\n{}",
        roster, scene_context, narration
    );
    let messages = json!([
        {"role": "system", "content": system},
        {"role": "user", "content": user},
    ]);
    let mut sink = NullSink;
    let output = client
        .chat_json_stream(&messages, Some(false), Role::Gm.as_str(), &mut sink)
        .await?;
    Ok((output.data, output.stats))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn world_seed_output_example_is_valid_and_complete() {
        let example: Value =
            serde_json::from_str(WORLD_SEED_OUTPUT_EXAMPLE).expect("valid world seed example");
        for key in [
            "story_brief",
            "public_intro",
            "hidden_truth",
            "proper_nouns",
            "public_facts",
            "npcs",
            "scene",
        ] {
            assert!(example.get(key).is_some(), "missing root field {key}");
        }
        let scene = example.get("scene").expect("scene example");
        for key in [
            "id",
            "location_id",
            "title",
            "description",
            "present_npcs",
            "items",
            "exits",
            "constraints",
            "tension",
        ] {
            assert!(scene.get(key).is_some(), "missing scene field {key}");
        }
        assert!(with_world_seed_output_example("base").ends_with(WORLD_SEED_OUTPUT_EXAMPLE));
    }

    #[test]
    fn scene_delta_output_example_has_required_and_optional_fields() {
        let example: Value =
            serde_json::from_str(SCENE_DELTA_OUTPUT_EXAMPLE).expect("valid scene delta example");
        let row = example["moves"][0].as_object().expect("move example");
        for key in ["npc_id", "present", "reason"] {
            assert!(row.contains_key(key), "missing required move field {key}");
        }
        for key in ["location", "visible", "can_hear", "activity", "attitude"] {
            assert!(row.contains_key(key), "missing optional move field {key}");
        }
    }
}
