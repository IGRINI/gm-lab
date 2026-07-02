//! gml-stories — faithful port of `gm-lab/stories.py`, the story/scenario
//! catalog for GM-Lab.
//!
//! Story data is the StoryStore/package model: each story is a filesystem
//! package read at runtime, not a compiled-in table. There is no game logic
//! here — concrete canon, NPC cards, public facts, and starting scene data all
//! live in each package's `story.json` `seed` object. serde_json is configured
//! workspace-wide with the `preserve_order` feature, so parsing keeps each
//! dict's insertion order — the World seed loader (`gml-world`) consumes the
//! resulting [`serde_json::Value`] shape directly.
//!
//! Public surface:
//! - [`DEFAULT_STORY_ID`]
//! - [`story_ids`]
//! - [`story_metadata`]
//! - [`list_stories`]
//! - [`story_seed`]
//! - [`default_story_seed`]
//!
//! Each call to [`story_seed`] / [`default_story_seed`] returns an OWNED, deep
//! [`Value`] clone, so every session gets an independent world with no shared
//! mutable state across sessions.
//!
//! ## Stories are filesystem packages (Phase 3 of `docs/MODS_PACKAGES_TZ.md`)
//!
//! Story data is read ONLY from runtime FILE PACKAGES under
//! `<root>/stories/<story_id>/story.json` (scanned by [`StoryStore`]). A user
//! can add a story by dropping a folder — no recompile. The three built-in
//! stories (`frozen-harbor`, `glass-garden`, `turnvale-murder`) ship as DEFAULT
//! packages materialized on first run.
//!
//! The embedded [`CATALOG_JSON`] exists ONLY as the SOURCE used to materialize
//! those three defaults — it is never a live read path. Prefer constructing a
//! [`StoryStore`] explicitly (the server threads one through `AppState`); the
//! free functions below delegate to a process-global default-rooted store for
//! callers that cannot receive an injected store (tests, library helpers).

use std::collections::BTreeSet;

use once_cell::sync::Lazy;
use serde_json::{Map, Value};

mod story_store;
pub use story_store::{StoryStore, StoryStoreError, StoryWorldRef, STORY_FORMAT};

/// `DEFAULT_STORY_ID = "turnvale-murder"`.
pub const DEFAULT_STORY_ID: &str = "turnvale-murder";

/// Embedded catalog of the three built-in default stories. Each element is an
/// object `{id, title, description, seed}`.
///
/// This is NOT a live read path: it is consumed solely by [`StoryStore`] to
/// materialize the three built-in default packages on first run. All live story
/// reads go through scanned packages.
pub(crate) const CATALOG_JSON: &str = include_str!("catalog.json");

/// Raised when a story id is not present in the library (Python `KeyError`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownStory(pub String);

impl std::fmt::Display for UnknownStory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown story: {}", self.0)
    }
}

impl std::error::Error for UnknownStory {}

/// Process-global story store rooted at [`StoryStore::default_root`]. Backs the
/// free functions for callers that cannot receive an explicitly-injected store.
/// This is the SAME `StoryStore` type the server injects — there is only one live
/// data source (the scanned packages on disk), never two.
static DEFAULT_STORE: Lazy<StoryStore> = Lazy::new(|| {
    StoryStore::new(StoryStore::default_root())
        .expect("default story library must materialize and scan")
});

/// `story_ids() -> set[str]` — the set of story ids in the default library.
pub fn story_ids() -> BTreeSet<String> {
    DEFAULT_STORE.story_ids()
}

/// `story_metadata(story_id) -> {id, title, description, story_brief}`.
///
/// Returns [`UnknownStory`] for an unknown id. The returned map preserves the
/// public catalog key order: `id`, `title`, `description`, `story_brief`.
pub fn story_metadata(story_id: &str) -> Result<Map<String, Value>, UnknownStory> {
    DEFAULT_STORE.story_metadata(story_id)
}

/// `list_stories() -> list[{id, title, description, story_brief}]` — discovery
/// order.
pub fn list_stories() -> Vec<Map<String, Value>> {
    DEFAULT_STORE.list_stories()
}

/// `story_seed(story_id) -> dict` — an OWNED deep clone of the story's seed with
/// `id`/`title` overwritten from the package envelope. Returns [`UnknownStory`]
/// for an unknown id.
pub fn story_seed(story_id: &str) -> Result<Value, UnknownStory> {
    DEFAULT_STORE.seed(story_id)
}

/// `default_story_seed() -> dict` — `story_seed(DEFAULT_STORY_ID)`.
pub fn default_story_seed() -> Value {
    DEFAULT_STORE.default_seed()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gml_world::World;

    /// Build a hermetic [`StoryStore`] over a fresh tempdir. These tests must
    /// NEVER touch the real user library, so they construct an explicit store
    /// rather than going through the [`DEFAULT_STORE`]-backed free functions
    /// (which materialize the builtins into the live library when
    /// `GM_PACKAGES_DIR` is unset). Mirrors `story_store::tests::temp_store`.
    fn temp_store() -> (tempfile::TempDir, StoryStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = StoryStore::new(dir.path()).expect("open store");
        (dir, store)
    }

    #[test]
    fn default_story_id_present() {
        let (_dir, store) = temp_store();
        assert!(store.story_ids().contains(DEFAULT_STORY_ID));
        // Metadata resolves for the default id.
        let meta = store.story_metadata(DEFAULT_STORY_ID).expect("default metadata");
        assert_eq!(
            meta.get("id").and_then(Value::as_str),
            Some(DEFAULT_STORY_ID)
        );
    }

    #[test]
    fn expected_number_of_stories() {
        let (_dir, store) = temp_store();
        // The library ships exactly three built-in scenarios.
        assert_eq!(store.story_ids().len(), 3);
        assert_eq!(store.list_stories().len(), 3);
        let expected: BTreeSet<String> = ["frozen-harbor", "glass-garden", "turnvale-murder"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(store.story_ids(), expected);
    }

    #[test]
    fn metadata_shape_and_order() {
        let (_dir, store) = temp_store();
        let meta = store.story_metadata("turnvale-murder").expect("metadata");
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
        let (_dir, store) = temp_store();
        assert_eq!(
            store.story_metadata("nope").unwrap_err(),
            UnknownStory("nope".to_string())
        );
        assert_eq!(
            store.seed("nope").unwrap_err(),
            UnknownStory("nope".to_string())
        );
    }

    #[test]
    fn seed_overwrites_id_and_title() {
        let (_dir, store) = temp_store();
        let seed = store.seed("frozen-harbor").expect("seed");
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
        let (_dir, store) = temp_store();
        let seed = store.default_seed();
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
        let (_dir, store) = temp_store();
        for id in store.story_ids() {
            let seed = store.seed(&id).expect("seed");
            // Must not panic; builds a World deterministically.
            let _world = World::from_seed_with_dice_seed(&seed, 1);
        }
    }

    #[test]
    fn every_story_has_a_non_midnight_start_time() {
        let (_dir, store) = temp_store();
        for id in store.story_ids() {
            let seed = store.seed(&id).expect("seed");
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
        // separators=(',',':'))` on the Python source — proves the materialized
        // package seeds preserve every byte of content and key order.
        let (_dir, store) = temp_store();
        let expected: &[(&str, usize)] = &[
            ("frozen-harbor", 28901),
            ("glass-garden", 29465),
            ("turnvale-murder", 25727),
        ];
        for (id, py_len) in expected {
            let seed = store.seed(id).expect("seed");
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
        let (_dir, store) = temp_store();
        // Two independent sessions from the same story.
        let seed_a = store.seed(DEFAULT_STORY_ID).expect("seed a");
        let seed_b = store.seed(DEFAULT_STORY_ID).expect("seed b");

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

        // The store seed Value itself is untouched by either session.
        let fresh = store.seed(DEFAULT_STORY_ID).expect("fresh seed");
        assert_eq!(fresh, seed_b);
    }
}
