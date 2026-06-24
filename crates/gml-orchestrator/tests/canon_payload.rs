//! Phase-1 canon persistence wiring (session_payload.rs).
//!
//! Proves the additive serialization contract:
//!   - a freshly seeded session emits a `world_canon` key inside `world`;
//!   - the canon survives a `to_payload` -> `from_payload` round-trip intact;
//!   - a payload WITHOUT `world_canon` (a pre-canon save) loads with an empty
//!     canon and does not invent one (so the byte-identity gate is safe).

use std::sync::Arc;

use serde_json::{json, Value};

use gml_llm::{Backend, MockClient};
use gml_orchestrator::{ClientFactory, Session};
use gml_stories::default_story_seed;
use gml_world::{MemoryUnit, World};

fn factory() -> ClientFactory {
    Arc::new(|| Arc::new(MockClient::new()) as Arc<dyn Backend>)
}

fn client() -> Arc<dyn Backend> {
    Arc::new(MockClient::new())
}

fn seeded_session() -> Session {
    let world = World::from_seed_with_dice_seed(&default_story_seed(), 20260622);
    Session::with_world(client(), world, factory())
}

#[test]
fn seeded_session_payload_carries_canon() {
    let session = seeded_session();
    let payload = session.to_payload();
    let world = payload
        .get("world")
        .and_then(Value::as_object)
        .expect("world object");
    let canon = world
        .get("world_canon")
        .and_then(Value::as_object)
        .expect("world_canon present for a seeded world");
    assert!(
        canon
            .get("places")
            .and_then(Value::as_object)
            .map(|p| !p.is_empty())
            .unwrap_or(false),
        "canon must carry at least the starting place"
    );
}

#[test]
fn canon_survives_payload_round_trip() {
    let session = seeded_session();
    let original = session.world.world_canon.clone();
    assert!(!original.is_empty());

    let payload = session.to_payload();
    let restored = Session::from_payload(&payload, client(), factory()).expect("from_payload");

    assert_eq!(
        restored.world.world_canon, original,
        "canon must survive a full session payload round-trip"
    );
}

#[test]
fn canon_memory_survives_payload_round_trip() {
    let mut session = seeded_session();
    let id = session.world.add_memory_unit(MemoryUnit {
        memory_id: "payload_memory".to_string(),
        owner_scope: "player".to_string(),
        summary: "PAYLOAD_MEMORY_SENTINEL игрок помнит знак на воротах.".to_string(),
        details: "Детальная карточка тоже должна пережить round-trip.".to_string(),
        topic_tags: vec!["payload".to_string()],
        ..Default::default()
    });
    assert_eq!(id, "payload_memory");

    let payload = session.to_payload();
    assert_eq!(
        payload["world"]["world_canon"]["memory"]["units"]["payload_memory"]["summary"],
        json!("PAYLOAD_MEMORY_SENTINEL игрок помнит знак на воротах.")
    );

    let restored = Session::from_payload(&payload, client(), factory()).expect("from_payload");
    let restored_unit = restored
        .world
        .world_canon
        .memory
        .get("payload_memory")
        .expect("memory unit survives");
    assert_eq!(
        restored_unit.details,
        "Детальная карточка тоже должна пережить round-trip."
    );
}

#[test]
fn pre_canon_payload_loads_empty_canon() {
    // Take a real seeded payload and strip the canon key to emulate a save made
    // before the living-world layer existed.
    let session = seeded_session();
    let mut payload = session.to_payload();
    payload
        .get_mut("world")
        .and_then(Value::as_object_mut)
        .expect("world object")
        .remove("world_canon");

    let restored = Session::from_payload(&payload, client(), factory()).expect("from_payload");
    assert!(
        restored.world.world_canon.is_empty(),
        "a pre-canon save must load with an empty canon (no lazy rebuild)"
    );
}

#[test]
fn pre_canon_payload_does_not_gain_canon_on_reserialize() {
    // Locks the byte-identity contract at the payload level: loading a save
    // that lacks `world_canon` and re-serializing must NOT introduce the key
    // (guards against a future "helpfully rebuild canon on load" regression).
    let session = seeded_session();
    let mut payload = session.to_payload();
    payload
        .get_mut("world")
        .and_then(Value::as_object_mut)
        .expect("world object")
        .remove("world_canon");

    let restored = Session::from_payload(&payload, client(), factory()).expect("from_payload");
    let reserialized = restored.to_payload();
    let world = reserialized
        .get("world")
        .and_then(Value::as_object)
        .expect("world object");
    assert!(
        !world.contains_key("world_canon"),
        "a pre-canon save must not gain a world_canon key on re-serialize"
    );
}

#[test]
fn procedural_worldgen_session_round_trips_through_payload() {
    // A canon-authoritative procedural campaign (World::from_worldgen) must
    // survive a full persistence round-trip. Its legacy fact_records are empty
    // (facts live in the canon), so this also locks the relaxed contract: an
    // EMPTY fact_records list is valid (locked decision #7 dropped the old
    // non-empty Python-byte-compat invariant), and the generated canon, npc
    // cards, and start scene all reload intact.
    let world = World::from_worldgen(&gml_world::WorldSpec::from_seed("12345"));
    assert!(
        !world.world_canon.is_empty(),
        "procedural world must carry a generated canon"
    );
    assert!(!world.npcs.is_empty(), "procedural world derives npc cards");
    assert!(
        !world.scene.title.is_empty(),
        "procedural world has a canon-derived start scene"
    );
    let canon_before = world.world_canon.clone();

    let session = Session::with_world(client(), world, factory());
    let payload = session.to_payload();
    let restored = Session::from_payload(&payload, client(), factory())
        .expect("procedural session round-trip");
    assert_eq!(
        restored.world.world_canon, canon_before,
        "the generated canon must survive persistence intact"
    );
}

#[test]
fn present_but_malformed_canon_in_payload_errors() {
    // Locked decision #5: when `world_canon` is PRESENT but malformed,
    // from_payload must ERROR (no silent default = no data loss).
    let session = seeded_session();
    let mut payload = session.to_payload();
    payload
        .get_mut("world")
        .and_then(Value::as_object_mut)
        .expect("world object")
        .insert("world_canon".to_string(), Value::String("garbage".into()));

    match Session::from_payload(&payload, client(), factory()) {
        Ok(_) => panic!("expected a malformed-canon error, got Ok"),
        Err(e) => assert!(
            e.contains("world_canon"),
            "error should name world_canon, got: {e}"
        ),
    }
}
