//! Phase-1 canon coverage over the three real catalog stories.
//!
//! `gml-world` cannot depend on `gml-stories` (it would be a dependency cycle),
//! so the cross-catalog assertions live here, where both the real story seeds
//! and `World` are available. These prove that for EVERY shipped story the
//! living-world canon (LIVING_WORLD_ARCHITECTURE_TZ.md §13 "Фаза 1"):
//!   - derives a starting place keyed by the scene location_id;
//!   - turns every seeded exit into a first-class transition (no collapse);
//!   - projects a byte-identical scene back out of the canon.

use gml_stories::{story_ids, story_seed};
use gml_world::World;

fn worlds() -> Vec<(String, World)> {
    let mut out = Vec::new();
    for id in story_ids() {
        let seed = story_seed(&id).expect("seed");
        let world = World::from_seed_with_dice_seed(&seed, 20260622);
        out.push((id, world));
    }
    out
}

#[test]
fn every_story_derives_a_canon() {
    for (id, world) in worlds() {
        let canon = &world.world_canon;
        assert!(
            !canon.is_empty(),
            "[{id}] canon must be derived at seed time"
        );

        // A starting place keyed by the scene's location_id exists.
        let place = canon
            .place(&world.scene.location_id)
            .unwrap_or_else(|| panic!("[{id}] place for scene.location_id"));
        assert_eq!(
            place.name, world.scene.title,
            "[{id}] place name == scene title"
        );
        assert!(place.is_visited(), "[{id}] starting place is visited");

        // Present NPCs are mirrored as canonical occupants.
        assert_eq!(
            place.occupant_ids, world.scene.present_npcs,
            "[{id}] occupants mirror present_npcs"
        );
    }
}

#[test]
fn every_seeded_exit_becomes_a_transition() {
    for (id, world) in worlds() {
        let canon = &world.world_canon;
        // No exit is dropped or collapsed (BTreeMap count == Vec count).
        assert_eq!(
            canon.transitions.len(),
            world.scene.exits.len(),
            "[{id}] one transition per exit"
        );
        for exit in &world.scene.exits {
            let t = canon
                .transition(&exit.exit_id)
                .unwrap_or_else(|| panic!("[{id}] transition for exit {}", exit.exit_id));
            assert_eq!(t.label, exit.name, "[{id}] transition label");
            assert_eq!(
                t.destination_hint, exit.destination,
                "[{id}] destination hint"
            );
            assert_eq!(t.from_place, world.scene.location_id, "[{id}] from_place");
        }
    }
}

#[test]
fn canon_projection_is_byte_identical_for_every_story() {
    for (id, mut world) in worlds() {
        let original = serde_json::to_string(&world.scene_export()).unwrap();

        let view = world.build_current_view();
        let saved = std::mem::replace(&mut world.scene, view);
        let rebuilt = serde_json::to_string(&world.scene_export()).unwrap();
        world.scene = saved;

        assert_eq!(
            original, rebuilt,
            "[{id}] scene projected from canon must be byte-identical to the live scene"
        );
    }
}
