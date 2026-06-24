//! Validation suite for gml-persistence.
//!
//! 1. Byte-identical golden round-trip of the on-disk payload.
//! 2. DialogStore CRUD over a temp sqlite file.
//! 3. `card_revision` defaults to 0 on an old snapshot missing the field.
//! 4. RNG state round-trips (getstate == setstate) through the world payload.

use std::sync::Arc;

use serde_json::{json, Map, Value};

use gml_config::Config;
use gml_llm::{Backend, MockClient};
use gml_orchestrator::{ClientFactory, Session};
use gml_persistence::{DialogRuntime, DialogStore, SCHEMA_VERSION};
use gml_world::World;

fn factory() -> ClientFactory {
    Arc::new(|| Arc::new(MockClient::new()) as Arc<dyn Backend>)
}

fn client() -> Arc<dyn Backend> {
    Arc::new(MockClient::new())
}

fn fixture_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("reference")
        .join("persistence")
}

fn read_compact() -> String {
    let path = fixture_dir().join("chat_payload.compact.json");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Build a DialogRuntime from a parsed full-payload Value.
fn runtime_from_payload_value(payload: &Value) -> DialogRuntime {
    let session = Session::from_payload(
        payload.get("session").unwrap_or(&Value::Null),
        client(),
        factory(),
    )
    .expect("session from payload");
    let transcript = match payload.get("transcript") {
        Some(Value::Array(a)) => a.clone(),
        _ => Vec::new(),
    };
    let turn_count = payload
        .get("turn_count")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    DialogRuntime {
        guest_id: "g".to_string(),
        chat_id: "c".to_string(),
        session,
        transcript,
        turn_count,
        title: String::new(),
        preview: String::new(),
        created_at: String::new(),
        updated_at: String::new(),
    }
}

/// Regenerate the canon-bearing persistence golden from a seeded Rust session.
///
/// Run explicitly when the payload shape changes:
///   cargo test -p gml-persistence regen_chat_payload_fixture -- --ignored --nocapture
///
/// The fixture is now a SELF-CONSISTENT RUST SNAPSHOT (not a Python capture):
/// it is the exact compact bytes `DialogStore.save()` writes for a freshly
/// seeded, canon-authoritative session. Locked decision #7 dropped Python
/// byte-compat, so the golden is whatever the Rust serializer emits — and it
/// now carries the living-world `world_canon` (locked decision #5: canon is
/// part of every save).
#[test]
#[ignore]
fn regen_chat_payload_fixture() {
    use gml_stories::default_story_seed;
    use gml_world::World;

    // Deterministic construction (fixed dice seed -> deterministic rng_state),
    // matching the canon_payload tests so the golden is reproducible.
    let world = World::from_seed_with_dice_seed(&default_story_seed(), 20260622);
    let session = Session::with_world(client(), world, factory());

    let payload = json!({
        "schema_version": SCHEMA_VERSION,
        "turn_count": 1,
        "session": session.to_payload(),
        "transcript": Value::Array(vec![]),
    });

    let compact = serde_json::to_string(&payload).expect("serialize compact");
    let pretty = serde_json::to_string_pretty(&payload).expect("serialize pretty");

    let dir = fixture_dir();
    std::fs::write(dir.join("chat_payload.compact.json"), &compact).expect("write compact");
    std::fs::write(dir.join("chat_payload.json"), pretty).expect("write pretty");

    // Sanity: the regenerated golden must carry a canon.
    let parsed: Value = serde_json::from_str(&compact).unwrap();
    assert!(
        parsed["session"]["world"]
            .as_object()
            .map(|w| w.contains_key("world_canon"))
            .unwrap_or(false),
        "regenerated golden must carry world_canon"
    );
    eprintln!(
        "wrote chat_payload fixtures ({} bytes compact)",
        compact.len()
    );
}

// =========================================================================
// 1. byte-identical golden round-trip
// =========================================================================

#[test]
fn golden_payload_roundtrip_is_byte_identical() {
    let input = read_compact();
    let parsed: Value = serde_json::from_str(&input).expect("parse compact fixture");

    let runtime = runtime_from_payload_value(&parsed);
    let output = runtime.payload_json();

    if output != input {
        // Find first divergence for a useful failure message.
        let a = input.as_bytes();
        let b = output.as_bytes();
        let mut i = 0;
        while i < a.len() && i < b.len() && a[i] == b[i] {
            i += 1;
        }
        let lo = i.saturating_sub(80);
        let in_ctx = &input[lo..(i + 80).min(input.len())];
        let out_ctx = &output[lo..(i + 80).min(output.len())];
        panic!(
            "payload not byte-identical at byte {i}\n--- expected (input) ---\n{in_ctx}\n--- got (output) ---\n{out_ctx}\n(input len {}, output len {})",
            input.len(),
            output.len()
        );
    }
    assert_eq!(output, input);
}

// =========================================================================
// 2. DialogStore CRUD over a temp sqlite file
// =========================================================================

fn temp_store() -> (DialogStore, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("dialogs.sqlite3");
    let mut cfg = Config::from_env();
    cfg.rag_enabled = false; // keep delete's embeddings purge a no-op in tests
    let store = DialogStore::new(db.to_string_lossy().into_owned(), factory(), Arc::new(cfg))
        .expect("create store");
    (store, dir)
}

#[test]
fn crud_create_save_load_list_delete() {
    let (store, _dir) = temp_store();
    let guest = "guest1";

    // create
    let chat_id = store
        .create_chat(guest, None, None, 0, Some("Моя игра"), None, true)
        .expect("create_chat");
    assert!(!chat_id.is_empty());

    // active pointer points at the new chat
    assert_eq!(
        store.active_chat_id(guest).unwrap().as_deref(),
        Some(chat_id.as_str())
    );

    // list shows it, marked active
    let chats = store.list_chats(guest).expect("list");
    assert_eq!(chats.len(), 1);
    assert_eq!(chats[0]["id"], json!(chat_id));
    assert_eq!(chats[0]["title"], json!("Моя игра"));
    assert_eq!(chats[0]["active"], json!(true));
    assert_eq!(chats[0]["story_id"], json!("procedural"));
    assert_eq!(chats[0]["kind"], json!("world"));

    // mutate via with_runtime + save
    store
        .with_runtime(guest, &chat_id, |rt| {
            rt.turn_count = 5;
            rt.session.last_player_action = "сделал ход".to_string();
        })
        .expect("with_runtime")
        .expect("runtime present");
    // persist the change
    store.with_runtime(guest, &chat_id, |_rt| {}).unwrap();
    // Re-save explicitly through load+save to exercise the DB write path.
    let mut loaded = store.load_chat(guest, &chat_id).expect("load_chat");
    loaded.turn_count = 7;
    store.save(&mut loaded).expect("save");
    assert!(!loaded.created_at.is_empty());
    assert!(!loaded.updated_at.is_empty());

    // load reflects the saved turn_count
    let reloaded = store.load_chat(guest, &chat_id).expect("reload");
    assert_eq!(reloaded.turn_count, 7);

    // create a second chat
    let mut static_world = World::from_seed_with_dice_seed(&json!({}), 123);
    static_world.story_id = "frozen-harbor".to_string();
    static_world.story_title = "Ледяная гавань".to_string();
    let static_session = Session::with_world(client(), static_world, factory());
    let chat2 = store
        .create_chat(
            guest,
            Some(static_session),
            None,
            0,
            Some("Вторая"),
            None,
            true,
        )
        .expect("create_chat 2");
    let chats = store.list_chats(guest).expect("list2");
    assert_eq!(chats.len(), 2);
    // List order is (updated_at DESC, created_at DESC, chat_id DESC), ported
    // verbatim from dialog_store.py. In this fast test both rows share the same
    // 1-second `datetime('now')` timestamp, so the deterministic part of the order
    // ties and the chat_id-DESC tiebreak (random tokens) decides — asserting a
    // strict newest-first index here would flake. Assert set-membership instead,
    // without weakening the production ORDER BY.
    let ids: std::collections::HashSet<&str> =
        chats.iter().filter_map(|c| c["id"].as_str()).collect();
    assert!(ids.contains(chat_id.as_str()), "first chat present in list");
    assert!(ids.contains(chat2.as_str()), "second chat present in list");
    let static_row = chats
        .iter()
        .find(|chat| chat["id"] == json!(chat2))
        .expect("static chat row");
    assert_eq!(static_row["story_id"], json!("frozen-harbor"));
    assert_eq!(static_row["story_title"], json!("Ледяная гавань"));
    assert_eq!(static_row["kind"], json!("chat"));
    // creating with activate=true makes chat2 the active chat
    assert_eq!(
        store.active_chat_id(guest).unwrap().as_deref(),
        Some(chat2.as_str())
    );

    // activate the first again
    assert!(store.activate_chat(guest, &chat_id).expect("activate"));
    assert_eq!(
        store.active_chat_id(guest).unwrap().as_deref(),
        Some(chat_id.as_str())
    );

    // delete the active chat -> active pointer self-heals to the remaining one
    let res = store.delete_chat(guest, &chat_id).expect("delete");
    assert_eq!(res["deleted"], json!(true));
    assert_eq!(res["active_chat_id"], json!(chat2));
    let chats = store.list_chats(guest).expect("list3");
    assert_eq!(chats.len(), 1);
    assert_eq!(chats[0]["id"], json!(chat2));

    // delete a missing chat
    let res = store.delete_chat(guest, "nope").expect("delete missing");
    assert_eq!(res["deleted"], json!(false));
}

#[test]
fn worlds_are_stored_separately_from_chats() {
    let (store, _dir) = temp_store();
    let guest = "worlds-guest";

    assert!(store.list_worlds(guest).expect("list worlds").is_empty());
    assert_eq!(
        store.active_chat_id(guest).expect("active before worlds"),
        None,
        "listing worlds must not create an active chat"
    );

    let world = store
        .create_world(
            guest,
            json!({
                "title": "Порог Второго Неба",
                "genre": "fantasy isekai",
                "tone": "tense hopeful",
                "world_size": "Континент с несколькими королевствами",
                "population": "Десятки миллионов жителей",
                "public_premise": "Клятвы и долги имеют силу закона и магии.",
                "world_lore": {
                    "name": "Порог Второго Неба",
                    "public_premise": "Клятвы и долги имеют силу закона и магии.",
                    "world_laws": ["магия требует имени, цены или признанного права"]
                }
            }),
        )
        .expect("create world");
    let world_id = world["id"].as_str().expect("world id").to_string();
    assert_eq!(world["kind"], json!("world"));
    assert_eq!(world["title"], json!("Порог Второго Неба"));
    assert_eq!(world["genre"], json!("fantasy isekai"));
    assert!(world["preview"].as_str().unwrap_or("").contains("Клятвы"));
    assert_eq!(
        store
            .active_chat_id(guest)
            .expect("active after create world"),
        None,
        "creating a world must not create or activate a chat"
    );

    let worlds = store.list_worlds(guest).expect("list worlds after create");
    assert_eq!(worlds.len(), 1);
    assert_eq!(worlds[0]["id"], json!(world_id));

    let deleted = store.delete_world(guest, &world_id).expect("delete world");
    assert_eq!(deleted["deleted"], json!(true));
    assert!(store
        .list_worlds(guest)
        .expect("list worlds after delete")
        .is_empty());
}

#[test]
fn get_active_creates_when_empty() {
    let (store, _dir) = temp_store();
    let guest = "fresh";
    // no chats yet -> get_active creates one and activates it
    let active = store.get_active(guest).expect("get_active");
    assert!(!active.is_empty());
    let chats = store.list_chats(guest).expect("list");
    assert_eq!(chats.len(), 1);
    assert_eq!(chats[0]["id"], json!(active));
    assert_eq!(chats[0]["active"], json!(true));
}

#[test]
fn save_owned_replaces_stale_cached_runtime() {
    let (store, _dir) = temp_store();
    let guest = "guest-cache";
    let chat_id = store
        .create_chat(guest, None, None, 0, Some("Кэш"), None, true)
        .expect("create_chat");

    store
        .with_runtime(guest, &chat_id, |rt| {
            assert_eq!(rt.turn_count, 0);
            rt.session.last_player_action = "old cached action".to_string();
        })
        .expect("with_runtime")
        .expect("runtime present");

    let mut owned = store.load_chat(guest, &chat_id).expect("load owned");
    owned.turn_count = 3;
    owned.session.last_player_action = "fresh streamed turn".to_string();
    store.save_owned(owned).expect("save_owned");

    let seen = store
        .with_runtime(guest, &chat_id, |rt| {
            (rt.turn_count, rt.session.last_player_action.clone())
        })
        .expect("with_runtime")
        .expect("runtime present");
    assert_eq!(seen.0, 3);
    assert_eq!(seen.1, "fresh streamed turn");
}

// =========================================================================
// 3. card_revision defaults to 0 on an old snapshot missing the field
// =========================================================================

#[test]
fn card_revision_defaults_zero_when_missing() {
    // Take the golden payload and strip card_revision from the player_character
    // and every npc, then ensure load defaults them to 0 and re-save succeeds.
    let input = read_compact();
    let mut parsed: Value = serde_json::from_str(&input).expect("parse");

    if let Some(world) = parsed.get_mut("session").and_then(|s| s.get_mut("world")) {
        if let Some(Value::Object(pc)) = world.get_mut("player_character") {
            pc.remove("card_revision");
        }
        if let Some(Value::Object(npcs)) = world.get_mut("npcs") {
            for (_id, npc) in npcs.iter_mut() {
                if let Value::Object(m) = npc {
                    m.remove("card_revision");
                }
            }
        }
    }

    let session = Session::from_payload(
        parsed.get("session").unwrap_or(&Value::Null),
        client(),
        factory(),
    )
    .expect("session from old snapshot");

    assert_eq!(session.world.player_character.card_revision, 0);
    for npc in session.world.npcs.values() {
        assert_eq!(
            npc.card_revision, 0,
            "npc {} card_revision should default 0",
            npc.npc_id
        );
    }
}

// =========================================================================
// 4. RNG state round-trips (getstate == setstate)
// =========================================================================

#[test]
fn rng_state_round_trips_through_world_payload() {
    let input = read_compact();
    let parsed: Value = serde_json::from_str(&input).expect("parse");

    // The RNG state from the fixture.
    let expected_state = parsed["session"]["world"]["rng_state"].clone();

    let session = Session::from_payload(
        parsed.get("session").unwrap_or(&Value::Null),
        client(),
        factory(),
    )
    .expect("session");

    // Re-serialize and confirm the rng_state survives unchanged.
    let out = session.to_payload();
    let got_state = out["world"]["rng_state"].clone();
    assert_eq!(
        got_state, expected_state,
        "rng_state must round-trip exactly"
    );

    // And the internal vector is the full 625-int CPython layout (624 + index).
    let internal_len = got_state["internal"]
        .as_array()
        .map(|a| a.len())
        .unwrap_or(0);
    assert_eq!(
        internal_len, 625,
        "internal must have 624 state words + index"
    );
}

// =========================================================================
// 5. schema version is hard-checked on load
// =========================================================================

// =========================================================================
// 6. canon is core: a present-but-malformed world_canon errors on load
//    (locked decision #5 — no silent default = no data loss)
// =========================================================================

#[test]
fn present_but_malformed_canon_errors_on_load() {
    let (store, _dir) = temp_store();
    let guest = "g";
    let chat_id = store
        .create_chat(guest, None, None, 0, None, None, true)
        .expect("create");

    // Take the canon-bearing golden, corrupt its world_canon to a non-object so
    // serde can't deserialize it, and write it straight into the DB.
    let input = read_compact();
    let mut parsed: Value = serde_json::from_str(&input).expect("parse");
    let world = parsed
        .get_mut("session")
        .and_then(|s| s.get_mut("world"))
        .and_then(Value::as_object_mut)
        .expect("world object");
    assert!(
        world.contains_key("world_canon"),
        "golden must carry world_canon for this test to be meaningful"
    );
    // A string is present but cannot deserialize into a WorldCanon struct.
    world.insert("world_canon".to_string(), json!("not a canon"));

    let con = rusqlite::Connection::open(store.db_path()).expect("open");
    con.execute(
        "UPDATE dialog_chats SET payload = ?1 WHERE guest_id = ?2 AND chat_id = ?3",
        rusqlite::params![parsed.to_string(), guest, chat_id],
    )
    .expect("update");
    drop(con);

    match store.load_chat(guest, &chat_id) {
        Ok(_) => panic!("expected a malformed-canon load error, got Ok"),
        Err(err) => {
            let msg = format!("{err}");
            assert!(
                msg.contains("world_canon"),
                "error should name world_canon, got: {msg}"
            );
        }
    }
}

#[test]
fn unsupported_schema_version_is_rejected() {
    let (store, _dir) = temp_store();
    let guest = "g";
    let chat_id = store
        .create_chat(guest, None, None, 0, None, None, true)
        .expect("create");

    // Hand-write a payload with schema_version=2 directly into the DB.
    let con = rusqlite::Connection::open(store.db_path()).expect("open");
    let bad = json!({"schema_version": 2, "turn_count": 0, "session": {}, "transcript": []});
    con.execute(
        "UPDATE dialog_chats SET payload = ?1 WHERE guest_id = ?2 AND chat_id = ?3",
        rusqlite::params![bad.to_string(), guest, chat_id],
    )
    .expect("update");
    drop(con);

    match store.load_chat(guest, &chat_id) {
        Ok(_) => panic!("expected schema-version error"),
        Err(err) => assert!(
            format!("{err}").contains("schema version"),
            "expected schema-version error, got: {err}"
        ),
    }
    assert_eq!(SCHEMA_VERSION, 1);
    let _: Map<String, Value> = Map::new();
}
