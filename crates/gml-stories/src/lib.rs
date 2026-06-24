//! gml-stories — faithful port of `gm-lab/stories.py`, the story/scenario
//! catalog for GM-Lab.
//!
//! The Python module is pure DATA plus six trivial accessor functions; there is
//! no game logic. All concrete canon, NPC cards, public facts, and starting
//! scene data live in [`STORY_DEFINITIONS`], embedded here as JSON via
//! `include_str!` (exported from `stories.py` byte-for-byte, key order
//! preserved). serde_json is configured workspace-wide with the
//! `preserve_order` feature, so parsing keeps each dict's insertion order — the
//! World seed loader (`gml-world`) consumes the same [`serde_json::Value`] shape
//! a Python seed `dict` would.
//!
//! Public surface matches `stories.py`:
//! - [`DEFAULT_STORY_ID`]
//! - [`story_ids`]
//! - [`story_metadata`]
//! - [`list_stories`]
//! - [`story_seed`]
//! - [`default_story_seed`]
//!
//! Each call to [`story_seed`] / [`default_story_seed`] returns an OWNED, deep
//! [`Value`] clone (Python `copy.deepcopy`), so every session gets an
//! independent world with no shared mutable state across sessions.

use std::collections::BTreeSet;

use once_cell::sync::Lazy;
use serde_json::{Map, Value};

/// `DEFAULT_STORY_ID = "turnvale-murder"`.
pub const DEFAULT_STORY_ID: &str = "turnvale-murder";

/// Embedded catalog (exported verbatim from `stories.py` `STORY_DEFINITIONS`).
/// Each element is an object `{id, title, description, seed}`.
const CATALOG_JSON: &str = include_str!("catalog.json");

/// Parsed `STORY_DEFINITIONS`. Key order is preserved (serde_json
/// `preserve_order`), so seeds round-trip into byte-identical structures.
static STORY_DEFINITIONS: Lazy<Vec<Value>> = Lazy::new(|| {
    let parsed: Value =
        serde_json::from_str(CATALOG_JSON).expect("embedded catalog.json must be valid JSON");
    match parsed {
        Value::Array(items) => items,
        _ => panic!("embedded catalog.json must be a JSON array of story definitions"),
    }
});

/// Raised when a story id is not present in the catalog (Python `KeyError`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownStory(pub String);

impl std::fmt::Display for UnknownStory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown story: {}", self.0)
    }
}

impl std::error::Error for UnknownStory {}

fn def_str(def: &Value, key: &str) -> String {
    def.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn seed_str(def: &Value, key: &str) -> String {
    def.get("seed")
        .and_then(Value::as_object)
        .and_then(|seed| seed.get(key))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// `story_ids() -> set[str]` — the set of catalog story ids.
pub fn story_ids() -> BTreeSet<String> {
    STORY_DEFINITIONS
        .iter()
        .map(|story| def_str(story, "id"))
        .collect()
}

/// `story_metadata(story_id) -> {id, title, description, story_brief}`.
///
/// Returns [`UnknownStory`] for an unknown id (Python raises `KeyError`).
/// The returned map preserves the public catalog key order: `id`, `title`,
/// `description`, `story_brief`.
pub fn story_metadata(story_id: &str) -> Result<Map<String, Value>, UnknownStory> {
    for story in STORY_DEFINITIONS.iter() {
        if def_str(story, "id") == story_id {
            let mut meta = Map::new();
            meta.insert(
                "id".to_string(),
                story.get("id").cloned().unwrap_or(Value::Null),
            );
            meta.insert(
                "title".to_string(),
                story.get("title").cloned().unwrap_or(Value::Null),
            );
            meta.insert(
                "description".to_string(),
                story.get("description").cloned().unwrap_or(Value::Null),
            );
            let story_brief = {
                let brief = seed_str(story, "story_brief");
                if !brief.is_empty() {
                    brief
                } else {
                    seed_str(story, "public_intro")
                }
            };
            meta.insert("story_brief".to_string(), Value::String(story_brief));
            return Ok(meta);
        }
    }
    Err(UnknownStory(story_id.to_string()))
}

/// `list_stories() -> list[{id, title, description}]` — catalog order.
pub fn list_stories() -> Vec<Map<String, Value>> {
    STORY_DEFINITIONS
        .iter()
        .map(|story| {
            // Mirrors Python: `story_metadata(story["id"])` for each entry.
            story_metadata(&def_str(story, "id")).expect("catalog id must resolve")
        })
        .collect()
}

/// `story_seed(story_id) -> dict`.
///
/// Deep-clones the story's `seed`, then overwrites `seed["id"]` / `seed["title"]`
/// from the outer story entry. The clone makes every session's world
/// independent — no shared mutable state. Returns [`UnknownStory`] for an
/// unknown id (Python raises `KeyError`).
pub fn story_seed(story_id: &str) -> Result<Value, UnknownStory> {
    for story in STORY_DEFINITIONS.iter() {
        if def_str(story, "id") == story_id {
            // copy.deepcopy(story["seed"]) — serde_json::Value::clone is deep.
            let mut seed = story.get("seed").cloned().unwrap_or(Value::Null);
            if let Value::Object(map) = &mut seed {
                // Re-inserting an existing key keeps its position (IndexMap),
                // matching Python's in-place `seed["id"] = ...` on a dict whose
                // first keys are already `id` then `title`.
                map.insert(
                    "id".to_string(),
                    story.get("id").cloned().unwrap_or(Value::Null),
                );
                map.insert(
                    "title".to_string(),
                    story.get("title").cloned().unwrap_or(Value::Null),
                );
            }
            return Ok(seed);
        }
    }
    Err(UnknownStory(story_id.to_string()))
}

/// `default_story_seed() -> dict` — `story_seed(DEFAULT_STORY_ID)`.
pub fn default_story_seed() -> Value {
    story_seed(DEFAULT_STORY_ID).expect("DEFAULT_STORY_ID must be present in the catalog")
}

#[cfg(test)]
mod tests {
    use super::*;
    use gml_world::World;

    #[test]
    fn default_story_id_present() {
        assert!(story_ids().contains(DEFAULT_STORY_ID));
        // Metadata resolves for the default id.
        let meta = story_metadata(DEFAULT_STORY_ID).expect("default metadata");
        assert_eq!(
            meta.get("id").and_then(Value::as_str),
            Some(DEFAULT_STORY_ID)
        );
    }

    #[test]
    fn expected_number_of_stories() {
        // stories.py ships exactly three scenarios.
        assert_eq!(STORY_DEFINITIONS.len(), 3);
        assert_eq!(story_ids().len(), 3);
        assert_eq!(list_stories().len(), 3);
        let expected: BTreeSet<String> = ["frozen-harbor", "glass-garden", "turnvale-murder"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(story_ids(), expected);
    }

    #[test]
    fn metadata_shape_and_order() {
        let meta = story_metadata("turnvale-murder").expect("metadata");
        let keys: Vec<&String> = meta.keys().collect();
        assert_eq!(keys, vec!["id", "title", "description", "story_brief"]);
        assert_eq!(
            meta.get("title").and_then(Value::as_str),
            Some("Убийство в Тёрнвейле")
        );
        assert!(meta
            .get("story_brief")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .contains("Дарра"));
    }

    #[test]
    fn unknown_story_errors() {
        assert_eq!(
            story_metadata("nope").unwrap_err(),
            UnknownStory("nope".to_string())
        );
        assert_eq!(
            story_seed("nope").unwrap_err(),
            UnknownStory("nope".to_string())
        );
    }

    #[test]
    fn seed_overwrites_id_and_title() {
        let seed = story_seed("frozen-harbor").expect("seed");
        assert_eq!(
            seed.get("id").and_then(Value::as_str),
            Some("frozen-harbor")
        );
        assert_eq!(
            seed.get("title").and_then(Value::as_str),
            Some("Ледяной порт Нордхольм")
        );
        // id and title remain the first two keys (preserve_order).
        let keys: Vec<&String> = seed.as_object().unwrap().keys().take(2).collect();
        assert_eq!(keys, vec!["id", "title"]);
    }

    #[test]
    fn default_seed_round_trips_into_world() {
        let seed = default_story_seed();
        let world = World::from_seed_with_dice_seed(&seed, 424242);
        // The seed builds a real World: story identity and roster populate.
        assert_eq!(world.story_id, DEFAULT_STORY_ID);
        assert_eq!(world.story_title, "Убийство в Тёрнвейле");
        assert!(world.story_brief.contains("Дарра"));
        assert_eq!(world.time.absolute_minutes, 480);
        assert_eq!(world.time.current_date_label, "Утро после убийства");
        assert_eq!(world.time_export()["time_of_day"], "08:00");
        // Both starting NPCs from the turnvale scene are loaded into the roster.
        assert!(world.npcs.contains_key("borin"));
        assert!(world.npcs.contains_key("lysa"));
        assert!(world.scene.present_npcs.contains("borin"));
        // The seed object carries the expected sections.
        let seed_obj = seed.as_object().unwrap();
        assert!(seed_obj.contains_key("npcs"));
        assert!(seed_obj.contains_key("scene"));
    }

    #[test]
    fn every_story_seed_builds_a_world() {
        for id in story_ids() {
            let seed = story_seed(&id).expect("seed");
            // Must not panic; builds a World deterministically.
            let _world = World::from_seed_with_dice_seed(&seed, 1);
        }
    }

    #[test]
    fn every_story_has_a_non_midnight_start_time() {
        for id in story_ids() {
            let seed = story_seed(&id).expect("seed");
            let world = World::from_seed_with_dice_seed(&seed, 1);
            assert!(
                world.time.absolute_minutes > 0,
                "{id} must define a story-specific start time"
            );
            assert_ne!(
                world.time_export()["time_of_day"],
                "00:00",
                "{id} must not start at default midnight"
            );
            assert_eq!(
                world.world_canon.clock_minutes, world.time.absolute_minutes,
                "{id} canon clock must match displayed world time"
            );
        }
    }

    #[test]
    fn seed_compact_byte_lengths_match_python() {
        // Captured from `json.dumps(story_seed(id), ensure_ascii=False,
        // separators=(',',':'))` on the Python source — proves the embedded
        // catalog preserves every byte of content and key order.
        let expected: &[(&str, usize)] = &[
            ("frozen-harbor", 28901),
            ("glass-garden", 29465),
            ("turnvale-murder", 25727),
        ];
        for (id, py_len) in expected {
            let seed = story_seed(id).expect("seed");
            // serde_json compact (preserve_order) == Python separators=(',',':').
            let compact = serde_json::to_string(&seed).expect("compact");
            assert_eq!(
                compact.len(),
                *py_len,
                "compact byte length mismatch for story {id}"
            );
        }
    }

    #[test]
    fn deep_clone_isolation_across_sessions() {
        // Two independent sessions from the same story.
        let seed_a = story_seed(DEFAULT_STORY_ID).expect("seed a");
        let seed_b = story_seed(DEFAULT_STORY_ID).expect("seed b");

        let mut world_a = World::from_seed_with_dice_seed(&seed_a, 1);
        let world_b = World::from_seed_with_dice_seed(&seed_b, 1);

        // Mutating world A must not affect world B (no shared state).
        let title_b_before = world_b.scene.title.clone();
        world_a.scene.title = "MUTATED SCENE A".to_string();
        world_a.npcs.remove("borin");
        assert_eq!(world_a.scene.title, "MUTATED SCENE A");
        assert_eq!(world_b.scene.title, title_b_before);
        assert_ne!(world_a.scene.title, world_b.scene.title);
        // world_b's roster is untouched by world_a's removal.
        assert!(world_b.npcs.contains_key("borin"));

        // The catalog seed Value itself is untouched by either session.
        let fresh = story_seed(DEFAULT_STORY_ID).expect("fresh seed");
        assert_eq!(fresh, seed_b);
    }
}
