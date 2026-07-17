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
use gml_prompts::{render_prompt, PromptId};
use gml_types::Role;
use gml_world::World;

use crate::coerce::{as_list, text};

fn render_world_seed_system(repair: bool) -> String {
    render_prompt(PromptId::WorldSeedSystem, json!({"repair": repair}))
        .expect("embedded world seed system prompt must render")
}

fn render_world_seed_repair_user(brief: &str, broken_seed: &str) -> String {
    render_prompt(
        PromptId::WorldSeedRepairUser,
        json!({"brief": brief, "broken_seed": broken_seed}),
    )
    .expect("embedded world seed repair user prompt must render")
}

fn render_scene_delta_system() -> String {
    render_prompt(PromptId::SceneDeltaSystem, json!({}))
        .expect("embedded scene delta system prompt must render")
}

fn render_scene_delta_user(roster: &str, scene_context: &str, narration: &str) -> String {
    render_prompt(
        PromptId::SceneDeltaUser,
        json!({
            "roster": roster,
            "scene_context": scene_context,
            "narration": narration,
        }),
    )
    .expect("embedded scene delta user prompt must render")
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

/// Player-facing text fragments used by the seed quality check.
fn seed_player_facing_parts(seed: &Value) -> Vec<String> {
    let o = match obj(seed) {
        Some(o) => o,
        None => return Vec::new(),
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
    parts.into_iter().filter(|p| !p.is_empty()).collect()
}

fn meta_text_key(value: &str) -> String {
    let mut out = String::new();
    let mut pending_space = false;
    for ch in value.chars().flat_map(char::to_lowercase) {
        if ch.is_alphanumeric() {
            if pending_space && !out.is_empty() {
                out.push(' ');
            }
            out.push(ch);
            pending_space = false;
        } else {
            pending_space = true;
        }
    }
    out
}

/// Detect empty output and prompt/schema placeholders without inferring a
/// language from the user's brief. The fixed phrases below are structural
/// examples from the seed schema, not locale-specific output variants.
fn is_meta_placeholder(value: &str) -> bool {
    let key = meta_text_key(value);
    if key.is_empty() {
        return true;
    }
    matches!(
        key.as_str(),
        "tbd"
            | "todo"
            | "placeholder"
            | "placeholder text"
            | "fill me"
            | "replace me"
            | "who the player is where they are what happened and what is expected of them"
            | "short player facing introduction"
            | "scene title"
            | "scene description"
    )
}

/// Repair text only when player-facing content is absent or dominated by
/// structural placeholders. Any real writing system is accepted.
fn seed_needs_text_repair(seed: &Value) -> bool {
    let parts = seed_player_facing_parts(seed);
    if parts.is_empty() {
        return true;
    }
    let visible_chars = parts
        .iter()
        .flat_map(|part| part.chars())
        .filter(|ch| ch.is_alphanumeric())
        .count();
    if visible_chars < 3 {
        return true;
    }
    let placeholders = parts
        .iter()
        .filter(|part| is_meta_placeholder(part))
        .count();
    placeholders > 0 && placeholders * 2 >= parts.len()
}

fn display_name_key(name: &str) -> String {
    let mut key = String::new();
    let mut separator = false;
    for ch in name.chars().flat_map(char::to_lowercase) {
        if ch.is_alphanumeric() {
            if separator && !key.is_empty() {
                key.push('_');
            }
            key.push(ch);
            separator = false;
        } else {
            separator = true;
        }
    }
    key
}

/// Preserve brief-provided display names independently from output-language
/// validation. Matching is case-insensitive within the same writing system and
/// never guesses a transliteration.
fn apply_brief_display_names(mut seed: Value, brief: &str) -> Value {
    if !seed.is_object() {
        return seed;
    }
    let name_re = Regex::new(r"\b\p{Lu}[\p{L}\p{M}'’\-]{0,48}\b").unwrap();
    let spans: Vec<(usize, usize)> = name_re
        .find_iter(brief)
        .map(|matched| (matched.start(), matched.end()))
        .collect();
    let mut by_key: indexmap::IndexMap<String, String> = indexmap::IndexMap::new();
    for start in 0..spans.len() {
        for end in start..usize::min(start + 4, spans.len()) {
            if end > start {
                let gap = &brief[spans[end - 1].1..spans[end].0];
                if !gap.chars().all(|ch| {
                    ch.is_whitespace() || matches!(ch, '-' | '‑' | '–' | '—' | '\'' | '’')
                }) {
                    break;
                }
            }
            let candidate = &brief[spans[start].0..spans[end].1];
            by_key.insert(display_name_key(candidate), candidate.to_string());
        }
    }

    let apply = |raw: &mut Value, npc_id: &str, by_key: &indexmap::IndexMap<String, String>| {
        let current_name = raw.get("name").and_then(Value::as_str).unwrap_or("");
        let wanted = by_key
            .get(&display_name_key(current_name))
            .or_else(|| by_key.get(&display_name_key(npc_id)));
        if let Some(wanted) = wanted {
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
                    apply(raw, &npc_id, &by_key);
                }
            }
        }
        Some(Value::Object(npcs)) => {
            for (npc_id, raw) in npcs.iter_mut() {
                let id = npc_id.clone();
                apply(raw, &id, &by_key);
            }
        }
        _ => {}
    }
    if let Some(Value::Object(details)) = o.get_mut("npc_details") {
        for (npc_id, raw) in details.iter_mut() {
            let id = npc_id.clone();
            apply(raw, &id, &by_key);
        }
    }
    seed
}

/// `build_world_seed(client, brief)` — ask the model for a new playable world
/// seed; World validates it afterwards. Repair logic preserved.
pub async fn build_world_seed(client: &dyn Backend, brief: &str) -> Result<Value, BackendError> {
    let system = render_world_seed_system(false);

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
    if !seed_needs_npc_repair(&seed) && !seed_needs_text_repair(&seed) {
        return Ok(seed);
    }
    let repair_system = render_world_seed_system(true);

    let broken = serde_json::to_string(&seed).unwrap_or_default();
    let repair_user = render_world_seed_repair_user(&brief_user, &broken);
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
                line.push_str(&format!(
                    ", gender: {}",
                    crate::public_gender(&npc.pronouns)
                ));
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

    let system = render_scene_delta_system();

    let scene_context = world.scene_context();
    let user = render_scene_delta_user(&roster, &scene_context, narration);
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
    fn text_repair_is_language_agnostic_and_rejects_empty_or_meta_output() {
        for seed in [
            json!({"story_brief": "Rain closes the harbor while the bell keeps ringing."}),
            json!({"story_brief": "La lluvia cierra el puerto mientras sigue sonando la campana."}),
            json!({"story_brief": "雨に閉ざされた港で、鐘だけが鳴り続けている。"}),
        ] {
            assert!(!seed_needs_text_repair(&seed), "valid seed: {seed}");
        }

        assert!(seed_needs_text_repair(&json!({})));
        assert!(seed_needs_text_repair(&json!({"story_brief": "..."})));
        assert!(seed_needs_text_repair(&json!({
            "story_brief": "Who the player is, where they are, what happened, and what is expected of them.",
            "public_intro": "Short player-facing introduction.",
            "scene": {
                "title": "Scene title",
                "description": "Scene description",
                "items": [{"name": "Item"}],
                "exits": [{"name": "Exit"}]
            }
        })));

        let real_scene_with_generic_exit = json!({
            "story_brief": "The flood has trapped everyone in the old station.",
            "scene": {
                "title": "The Last Platform",
                "description": "Cold water rises over the rails.",
                "exits": [{"name": "Exit"}]
            }
        });
        assert!(!seed_needs_text_repair(&real_scene_with_generic_exit));
    }

    #[test]
    fn brief_display_names_match_unicode_names_without_cross_script_guessing() {
        let seed = json!({
            "proper_nouns": ["Лиза", "Ada Lovelace", "Élodie", "李娜"],
            "npcs": [
                {"id": "liza", "name": "лиза"},
                {"id": "ada_lovelace", "name": "ada_lovelace"},
                {"id": "elodie", "name": "élodie"},
                {"id": "li_na", "name": "李娜"}
            ]
        });
        let restored =
            apply_brief_display_names(seed, "Лиза ждёт Ada Lovelace и Élodie; 李娜已经在门口。");
        assert_eq!(restored["npcs"][0]["name"], "Лиза");
        assert_eq!(restored["npcs"][1]["name"], "Ada Lovelace");
        assert_eq!(restored["npcs"][2]["name"], "Élodie");
        assert_eq!(restored["npcs"][3]["name"], "李娜");
    }

    #[test]
    fn brief_display_name_repair_does_not_guess_ambiguous_proper_nouns() {
        let seed = json!({
            "proper_nouns": ["Alice"],
            "npcs": [{"id": "guard", "name": "sentry"}]
        });
        let restored = apply_brief_display_names(seed, "Alice questions the guard.");
        assert_eq!(restored["npcs"][0]["name"], "sentry");
    }

    #[test]
    fn world_seed_output_example_is_valid_and_complete() {
        let initial = render_world_seed_system(false);
        let repair = render_world_seed_system(true);
        assert!(initial.starts_with("Create a compact tabletop RP starting scene"));
        assert!(!initial.contains("Repair this tabletop RP world seed"));
        assert!(repair.starts_with("Repair this tabletop RP world seed"));
        assert!(!repair.contains("Create a compact tabletop RP starting scene"));

        let marker =
            "\n\nReturn exactly one JSON object with this shape; repeat array entries as needed:\n";
        let initial_example = initial
            .split_once(marker)
            .map(|(_, example)| example)
            .expect("initial world seed example");
        let repair_example = repair
            .split_once(marker)
            .map(|(_, example)| example)
            .expect("repair world seed example");
        assert_eq!(initial_example, repair_example);
        let example: Value =
            serde_json::from_str(initial_example).expect("valid world seed example");
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
        let repair_user = render_world_seed_repair_user("Бриф", "{\"broken\":true}");
        assert_eq!(
            repair_user,
            "USER BRIEF:\nБриф\n\nBROKEN SEED:\n{\"broken\":true}"
        );
    }

    #[test]
    fn scene_delta_output_example_has_required_and_optional_fields() {
        let system = render_scene_delta_system();
        let example_json = system
            .split_once("Example: ")
            .and_then(|(_, tail)| tail.split_once(" If there is no explicit roster change"))
            .map(|(example, _)| example)
            .expect("scene delta example");
        let example: Value = serde_json::from_str(example_json).expect("valid scene delta example");
        let row = example["moves"][0].as_object().expect("move example");
        for key in ["npc_id", "present", "reason"] {
            assert!(row.contains_key(key), "missing required move field {key}");
        }
        for key in ["location", "visible", "can_hear", "activity", "attitude"] {
            assert!(row.contains_key(key), "missing optional move field {key}");
        }
        assert_eq!(
            render_scene_delta_user("npc", "scene", "narration"),
            "ROSTER:\nnpc\n\nCURRENT SCENE:\nscene\n\nGM NARRATION:\nnarration"
        );
    }
}
