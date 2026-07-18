//! Validation suite for gml-persistence.
//!
//! 1. Byte-identical golden round-trip of the on-disk payload.
//! 2. DialogStore CRUD over a temp sqlite file.
//! 3. `card_revision` defaults to 0 on an old snapshot missing the field.
//! 4. RNG state round-trips (getstate == setstate) through the world payload.

use std::sync::Arc;

use serde_json::{json, Map, Value};

use gml_config::Config;
use gml_llm::Backend;
use gml_mock::MockClient;
use gml_orchestrator::{ClientFactory, NpcClientState, Session};
use gml_persistence::{
    DialogRuntime, DialogStore, DialogVisualAsset, HistoryTurnKind, HistoryTurnReceiptKind,
    PreparedHistoryTurn, StoreError, TurnCheckpoint, WorldStore, MAX_REWIND_TURNS, SCHEMA_VERSION,
};
use gml_types::ContentLocale;
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
        visual_assets: Default::default(),
        rewindable_turns: Vec::new(),
    }
}

#[test]
fn visual_assets_roundtrip_with_dialog_history() {
    let (store, _dir) = temp_store();
    let chat_id = store
        .create_chat("g", None, None, 0, None, None, true)
        .expect("create chat");
    let mut runtime = store.load_chat("g", &chat_id).expect("load chat");
    runtime.visual_assets.characters.insert(
        "npc-1".to_string(),
        DialogVisualAsset {
            url: "/image-files/run/portrait.jpg".to_string(),
            provider: "grok".to_string(),
            model: "grok-imagine-image".to_string(),
        },
    );
    store.save(&mut runtime).expect("save visuals");

    let loaded = store.load_chat("g", &chat_id).expect("reload chat");
    assert_eq!(loaded.visual_assets, runtime.visual_assets);
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
    use gml_stories::StoryStore;
    use gml_world::World;

    // Deterministic construction (fixed dice seed -> deterministic rng_state),
    // matching the canon_payload tests so the golden is reproducible. The seed
    // comes from a HERMETIC tempdir store (no global store), so regenerating the
    // fixture never writes the builtins into the real user library.
    let dir = tempfile::tempdir().expect("tempdir");
    let store = StoryStore::new(dir.path()).expect("open store");
    let world = World::from_seed_with_dice_seed(&store.default_seed(), 20260622);
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
// 1. legacy golden migration + stable canonical round-trip
// =========================================================================

#[test]
fn golden_payload_migrates_binding_then_roundtrips_byte_identically() {
    let input = read_compact();
    let parsed: Value = serde_json::from_str(&input).expect("parse compact fixture");

    let runtime = runtime_from_payload_value(&parsed);
    let output = runtime.payload_json();

    let migrated: Value = serde_json::from_str(&output).expect("parse migrated payload");
    assert_eq!(
        migrated.pointer("/session/model_binding"),
        Some(&json!({"connector_id": "codex", "model_id": "mock"}))
    );

    let reloaded = runtime_from_payload_value(&migrated);
    assert_eq!(reloaded.payload_json(), output);
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
fn empty_store_default_chat_uses_requested_content_locale() {
    let (store, _dir) = temp_store();
    let guest = "english-default-chat";
    let chat_id = store
        .get_active_for_locale(guest, ContentLocale::English)
        .expect("create localized active chat");
    let locale = store
        .with_runtime(guest, &chat_id, |runtime| {
            runtime.session.world.world_canon.content_locale
        })
        .expect("read localized active chat")
        .expect("localized active chat exists");

    assert_eq!(locale, ContentLocale::English);
}

fn commit_test_turn(store: &DialogStore, guest: &str, chat_id: &str, turn: i64) -> String {
    let mut runtime = store.load_chat(guest, chat_id).expect("load before turn");
    let pre_turn_payload = runtime.payload_json();
    let text = format!("player action {turn}");
    let checkpoint =
        TurnCheckpoint::capture(&runtime, turn, format!("request-{turn}"), text.clone())
            .expect("capture checkpoint");

    runtime.turn_count = turn;
    runtime.session.turn = turn;
    runtime.session.last_player_action = text.clone();
    runtime.session.world.story_title = format!("world after turn {turn}");
    if turn == 1 {
        runtime.session.ensure_initial_tools(false);
    } else if turn == 2 {
        runtime.session.mark_tool_loaded("move_npc");
    } else if turn == 3 {
        // Membership without either staleness signal is intentional: an exact
        // checkpoint must not try to derive this set from the two maps.
        runtime
            .session
            .loaded_gm_tools
            .insert("world_debug".to_string());
    }
    runtime
        .session
        .gm_messages
        .push(json!({"role": "user", "content": text}));
    runtime.transcript.push(json!({
        "turn": turn,
        "request_id": format!("request-{turn}"),
        "event": {"kind": "player", "agent": "Игрок", "data": format!("player action {turn}")}
    }));
    runtime.transcript.push(json!({
        "turn": turn,
        "event": {"kind": "gm", "agent": "ГМ", "data": format!("answer {turn}")}
    }));
    store
        .save_owned_with_checkpoint(runtime, checkpoint)
        .expect("commit turn and checkpoint");
    pre_turn_payload
}

fn finish_prepared_history_turn(
    store: &DialogStore,
    prepared: PreparedHistoryTurn,
    turn: i64,
    request_id: &str,
    text: &str,
) -> String {
    let mut runtime = prepared.runtime;
    let destination_chat_id = runtime.chat_id.clone();
    let checkpoint = TurnCheckpoint::capture(&runtime, turn, request_id, text)
        .expect("capture staged checkpoint");
    runtime.turn_count = turn;
    runtime.session.turn = turn;
    runtime.session.last_player_action = text.to_string();
    runtime.session.world.story_title = format!("staged world: {text}");
    runtime
        .session
        .gm_messages
        .push(json!({"role": "user", "content": text}));
    runtime.transcript.push(json!({
        "turn": turn,
        "request_id": request_id,
        "event": {"kind": "player", "agent": "Игрок", "data": text}
    }));
    runtime.transcript.push(json!({
        "turn": turn,
        "event": {"kind": "gm", "agent": "ГМ", "data": "staged answer"}
    }));
    store
        .commit_prepared_history_turn(runtime, checkpoint, prepared.commit)
        .expect("commit staged history turn");
    destination_chat_id
}

fn without_provider_identities(payload: &str) -> Value {
    let mut payload: Value = serde_json::from_str(payload).expect("valid runtime payload");
    let Some(session) = payload.get_mut("session").and_then(Value::as_object_mut) else {
        return payload;
    };
    session.remove("client_session_id");
    session.remove("client_thread_id");
    if let Some(states) = session
        .get_mut("npc_client_state")
        .and_then(Value::as_object_mut)
    {
        for state in states.values_mut().filter_map(Value::as_object_mut) {
            state.remove("session_id");
            state.remove("thread_id");
        }
    }
    for key in [
        "location_generator_client_state",
        "character_generator_client_state",
    ] {
        if let Some(state) = session.get_mut(key).and_then(Value::as_object_mut) {
            state.remove("session_id");
            state.remove("thread_id");
        }
    }
    payload
}

#[test]
fn rewind_and_branch_restore_exact_state_and_keep_only_ten_turns() {
    let (store, _dir) = temp_store();
    let guest = "rewind-guest";
    let chat_id = store
        .create_chat(guest, None, None, 0, Some("Original"), None, true)
        .expect("create chat");

    let mut source_seed = store.load_chat(guest, &chat_id).expect("load source seed");
    source_seed.session.client_session_id = "source-conversation".to_string();
    source_seed.session.client_thread_id = "source-cache-scope".to_string();
    source_seed.session.npc_client_state.insert(
        "borin".to_string(),
        NpcClientState {
            model: "mock".to_string(),
            session_id: "source-npc-conversation".to_string(),
            thread_id: "source-npc-cache-scope".to_string(),
        },
    );
    source_seed.session.location_generator_client_state = NpcClientState {
        model: "mock".to_string(),
        session_id: "source-location-conversation".to_string(),
        thread_id: "source-location-cache-scope".to_string(),
    };
    source_seed.session.character_generator_client_state = NpcClientState {
        model: "mock".to_string(),
        session_id: "source-character-conversation".to_string(),
        thread_id: "source-character-cache-scope".to_string(),
    };
    store.save_owned(source_seed).expect("seed provider ids");

    let mut pre_turn_payloads = Vec::new();
    for turn in 1..=12 {
        pre_turn_payloads.push(commit_test_turn(&store, guest, &chat_id, turn));
    }

    let loaded = store
        .load_chat(guest, &chat_id)
        .expect("load committed chat");
    assert_eq!(loaded.rewindable_turns.len(), MAX_REWIND_TURNS);
    assert_eq!(loaded.rewindable_turns, (3..=12).collect::<Vec<_>>());
    assert!(matches!(
        store.rewind_chat_to_turn(guest, &chat_id, 2),
        Err(StoreError::TurnNotRewindable { turn: 2, .. })
    ));

    store
        .rewind_chat_to_turn(guest, &chat_id, 5)
        .expect("rewind to pre-turn 5");
    let rewound = store.load_chat(guest, &chat_id).expect("load rewound chat");
    assert_eq!(rewound.payload_json(), pre_turn_payloads[4]);
    assert_eq!(rewound.turn_count, 4);
    assert_eq!(rewound.rewindable_turns, vec![3, 4]);
    assert!(rewound.session.loaded_gm_tools.contains("world_debug"));

    let branch_id = store
        .branch_chat_from_turn(guest, &chat_id, 4, None)
        .expect("branch from pre-turn 4");
    assert_ne!(branch_id, chat_id);
    assert_eq!(
        store.active_chat_id(guest).unwrap().as_deref(),
        Some(branch_id.as_str())
    );
    let branch = store.load_chat(guest, &branch_id).expect("load branch");
    assert_eq!(
        without_provider_identities(&branch.payload_json()),
        without_provider_identities(&pre_turn_payloads[3])
    );
    assert_eq!(branch.turn_count, 3);
    assert_eq!(branch.rewindable_turns, vec![3]);
    assert_eq!(branch.title, "Original — ветка");
    assert_ne!(branch.session.client_session_id, "source-conversation");
    assert_ne!(branch.session.client_thread_id, "source-cache-scope");
    assert_ne!(
        branch.session.npc_client_state["borin"].session_id,
        "source-npc-conversation"
    );
    assert_ne!(
        branch.session.location_generator_client_state.thread_id,
        "source-location-cache-scope"
    );
    assert_ne!(
        branch.session.character_generator_client_state.thread_id,
        "source-character-cache-scope"
    );

    // Inherited checkpoints are rewritten too: rewinding the branch must not
    // restore any provider identity owned by the source history.
    store
        .rewind_chat_to_turn(guest, &branch_id, 3)
        .expect("rewind branch checkpoint");
    let branch_rewound = store
        .load_chat(guest, &branch_id)
        .expect("load rewound branch");
    assert_eq!(
        without_provider_identities(&branch_rewound.payload_json()),
        without_provider_identities(&pre_turn_payloads[2])
    );
    assert_ne!(
        branch_rewound.session.client_session_id,
        "source-conversation"
    );
    assert_ne!(
        branch_rewound.session.npc_client_state["borin"].thread_id,
        "source-npc-cache-scope"
    );

    // Branching and later edits never mutate the source chat.
    let source = store.load_chat(guest, &chat_id).expect("source remains");
    assert_eq!(source.payload_json(), pre_turn_payloads[4]);
    assert_eq!(source.rewindable_turns, vec![3, 4]);
}

#[test]
fn staged_history_is_invisible_until_success_and_commits_atomically() {
    let (store, _dir) = temp_store();
    let guest = "staged-history-guest";
    let source_chat_id = store
        .create_chat(guest, None, None, 0, Some("Source"), None, true)
        .expect("create source");
    for turn in 1..=3 {
        commit_test_turn(&store, guest, &source_chat_id, turn);
    }

    let source_before = store.load_chat(guest, &source_chat_id).unwrap();
    let source_bytes_before = source_before.payload_json();
    let source_metadata_before = (
        source_before.title.clone(),
        source_before.preview.clone(),
        source_before.created_at.clone(),
        source_before.updated_at.clone(),
    );
    let chats_before = store.list_chats(guest).unwrap();

    // Preparing and dropping an edit models any model error or cancellation.
    let cancelled_edit = store
        .prepare_history_turn(guest, &source_chat_id, 2, HistoryTurnKind::Edit)
        .expect("prepare edit");
    drop(cancelled_edit);
    let source_after_cancel = store.load_chat(guest, &source_chat_id).unwrap();
    assert_eq!(source_after_cancel.payload_json(), source_bytes_before);
    assert_eq!(
        (
            source_after_cancel.title,
            source_after_cancel.preview,
            source_after_cancel.created_at,
            source_after_cancel.updated_at,
        ),
        source_metadata_before
    );
    assert_eq!(store.list_chats(guest).unwrap(), chats_before);
    assert!(store
        .history_turn_receipt(guest, &source_chat_id, "edit-replacement")
        .unwrap()
        .is_none());

    let prepared_edit = store
        .prepare_history_turn(guest, &source_chat_id, 2, HistoryTurnKind::Edit)
        .expect("prepare successful edit");
    let edited_chat_id = finish_prepared_history_turn(
        &store,
        prepared_edit,
        2,
        "edit-replacement",
        "replacement action",
    );
    assert_eq!(edited_chat_id, source_chat_id);
    let edited = store.load_chat(guest, &source_chat_id).unwrap();
    assert_eq!(edited.turn_count, 2);
    assert_eq!(edited.rewindable_turns, vec![1, 2]);
    assert_eq!(
        edited.session.world.story_title,
        "staged world: replacement action"
    );
    let edit_receipt = store
        .history_turn_receipt(guest, &source_chat_id, "edit-replacement")
        .unwrap()
        .expect("edit receipt");
    assert_eq!(edit_receipt.kind, HistoryTurnReceiptKind::Edit);
    assert_eq!(edit_receipt.source_turn, 2);
    assert_eq!(edit_receipt.destination_chat_id, source_chat_id);
    assert_eq!(edit_receipt.player_text, "replacement action");

    let source_before_branch = edited.payload_json();
    let active_before_branch = store.active_chat_id(guest).unwrap();
    let cancelled_branch = store
        .prepare_history_turn(
            guest,
            &source_chat_id,
            1,
            HistoryTurnKind::Branch {
                title: Some("Cancelled branch".to_string()),
            },
        )
        .expect("prepare cancelled branch");
    let absent_branch_id = cancelled_branch.runtime.chat_id.clone();
    drop(cancelled_branch);
    assert!(!store.chat_exists(guest, &absent_branch_id).unwrap());
    assert_eq!(store.active_chat_id(guest).unwrap(), active_before_branch);
    assert_eq!(
        store
            .load_chat(guest, &source_chat_id)
            .unwrap()
            .payload_json(),
        source_before_branch
    );

    let prepared_branch = store
        .prepare_history_turn(
            guest,
            &source_chat_id,
            1,
            HistoryTurnKind::Branch {
                title: Some("Committed branch".to_string()),
            },
        )
        .expect("prepare successful branch");
    let branch_id = finish_prepared_history_turn(
        &store,
        prepared_branch,
        1,
        "branch-first-turn",
        "branch action",
    );
    assert_ne!(branch_id, source_chat_id);
    assert!(store.chat_exists(guest, &branch_id).unwrap());
    assert_eq!(
        store.active_chat_id(guest).unwrap().as_deref(),
        Some(branch_id.as_str())
    );
    assert_eq!(
        store
            .load_chat(guest, &source_chat_id)
            .unwrap()
            .payload_json(),
        source_before_branch
    );
    let branch = store.load_chat(guest, &branch_id).unwrap();
    assert_eq!(branch.turn_count, 1);
    assert_eq!(branch.title, "Committed branch");
    assert_eq!(branch.rewindable_turns, vec![1]);
    let branch_receipt = store
        .history_turn_receipt(guest, &source_chat_id, "branch-first-turn")
        .unwrap()
        .expect("branch receipt");
    assert_eq!(branch_receipt.kind, HistoryTurnReceiptKind::Branch);
    assert_eq!(branch_receipt.source_turn, 1);
    assert_eq!(branch_receipt.destination_chat_id, branch_id);
    assert_eq!(branch_receipt.player_text, "branch action");
}

#[test]
fn stale_staged_branch_fails_without_creating_or_overwriting_any_chat() {
    let (store, _dir) = temp_store();
    let guest = "stale-staged-history-guest";
    let source_chat_id = store
        .create_chat(guest, None, None, 0, Some("Source"), None, true)
        .expect("create source");
    commit_test_turn(&store, guest, &source_chat_id, 1);
    commit_test_turn(&store, guest, &source_chat_id, 2);

    let prepared = store
        .prepare_history_turn(
            guest,
            &source_chat_id,
            1,
            HistoryTurnKind::Branch { title: None },
        )
        .expect("prepare branch");
    let destination_chat_id = prepared.runtime.chat_id.clone();

    // Simulate a competing writer after model execution began. The optimistic
    // source-byte check must reject the staged commit before its first write.
    commit_test_turn(&store, guest, &source_chat_id, 3);
    let canonical_source = store
        .load_chat(guest, &source_chat_id)
        .unwrap()
        .payload_json();

    let mut runtime = prepared.runtime;
    let checkpoint = TurnCheckpoint::capture(&runtime, 1, "stale-branch", "stale action").unwrap();
    runtime.turn_count = 1;
    runtime.session.turn = 1;
    runtime.session.world.story_title = "must never commit".to_string();
    let error = store
        .commit_prepared_history_turn(runtime, checkpoint, prepared.commit)
        .expect_err("stale source must reject commit");
    assert!(matches!(error, StoreError::HistoryChanged { .. }));
    assert!(!store.chat_exists(guest, &destination_chat_id).unwrap());
    assert_eq!(
        store
            .load_chat(guest, &source_chat_id)
            .unwrap()
            .payload_json(),
        canonical_source
    );
    assert!(store
        .history_turn_receipt(guest, &source_chat_id, "stale-branch")
        .unwrap()
        .is_none());
}

#[test]
fn concurrent_first_access_creates_exactly_one_active_chat() {
    let (store, _dir) = temp_store();
    let store = Arc::new(store);
    let workers = 16;
    let start = Arc::new(std::sync::Barrier::new(workers));
    let mut handles = Vec::with_capacity(workers);

    for _ in 0..workers {
        let store = store.clone();
        let start = start.clone();
        handles.push(std::thread::spawn(move || {
            start.wait();
            store
                .get_active("parallel-first-access")
                .expect("get active")
        }));
    }

    let ids = handles
        .into_iter()
        .map(|handle| handle.join().expect("worker did not panic"))
        .collect::<Vec<_>>();
    assert!(ids.iter().all(|id| id == &ids[0]));
    assert_eq!(
        store
            .list_chats("parallel-first-access")
            .expect("list chats")
            .len(),
        1
    );
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

fn temp_world_store() -> (WorldStore, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = WorldStore::new(dir.path().join("library")).expect("create world store");
    (store, dir)
}

#[test]
fn world_store_create_update_list_get_delete_roundtrip() {
    let (store, _dir) = temp_world_store();

    assert!(store.list_worlds().expect("list worlds").is_empty());

    let world = store
        .create_world(json!({
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
        }))
        .expect("create world");
    let world_id = world["id"].as_str().expect("world id").to_string();
    assert_eq!(world["kind"], json!("world"));
    assert_eq!(world["title"], json!("Порог Второго Неба"));
    assert_eq!(world["genre"], json!("fantasy isekai"));
    assert!(world["preview"].as_str().unwrap_or("").contains("Клятвы"));

    // world.json is the source of truth on disk.
    let world_file = _dir
        .path()
        .join("library")
        .join("worlds")
        .join(&world_id)
        .join("world.json");
    assert!(world_file.is_file(), "world.json written to package dir");

    let worlds = store.list_worlds().expect("list worlds after create");
    assert_eq!(worlds.len(), 1);
    assert_eq!(worlds[0]["id"], json!(world_id));

    // get_world returns the same flattened shape.
    let got = store.get_world(&world_id).expect("get world");
    assert_eq!(got["id"], json!(world_id));
    assert_eq!(got["title"], json!("Порог Второго Неба"));

    // Shallow merge: a patch overwrites only the supplied keys.
    let updated = store
        .update_world(&world_id, json!({"genre": "dark fantasy"}))
        .expect("update world");
    assert_eq!(updated["genre"], json!("dark fantasy"));
    assert_eq!(
        updated["world_size"],
        json!("Континент с несколькими королевствами")
    );

    let deleted = store.delete_world(&world_id).expect("delete world");
    assert_eq!(deleted["deleted"], json!(true));
    assert!(store
        .list_worlds()
        .expect("list worlds after delete")
        .is_empty());

    // Deleting/getting a missing world.
    let missing = store.delete_world("nope").expect("delete missing");
    assert_eq!(missing["deleted"], json!(false));
    assert!(store.get_world("nope").is_err());
}

#[test]
fn world_store_architect_state_splits_from_content() {
    let (store, _dir) = temp_world_store();

    // A LEGACY package: the architect chat still rides inside the payload.
    let world = store
        .create_world(json!({
            "status": "draft",
            "genre": "fantasy",
            "architect_messages": [
                {"role": "assistant", "content": "Опиши мир."},
                {"role": "user", "content": "Хочу мир клятв."}
            ],
            "architect_model_history": [
                {"role": "user", "content": "## Current Draft JSON\n{}"},
                {"role": "assistant", "content": "Собираю основу."}
            ],
            "architect_cache_session_id": "world-architect:test-session",
            "architect_cache_thread_id": "world-architect:test-thread"
        }))
        .expect("create draft world");
    let world_id = world["id"].as_str().expect("world id").to_string();

    // The chat NEVER appears in world responses (content/chat split)...
    assert!(world.get("architect_messages").is_none());
    // ...but the legacy in-payload state is readable through the split API.
    let legacy = store
        .get_architect_state(&world_id)
        .expect("read architect state")
        .expect("legacy architect state present");
    assert_eq!(legacy["messages"][1]["content"], "Хочу мир клятв.");
    assert_eq!(legacy["model_history"][1]["content"], "Собираю основу.");
    assert_eq!(legacy["cache_session_id"], "world-architect:test-session");

    // A content update (ready save) keeps the legacy chat readable and hidden.
    let updated = store
        .update_world(
            &world_id,
            json!({
                "status": "ready",
                "title": "Порог Второго Неба",
                "world_lore": {
                    "name": "Порог Второго Неба",
                    "world_laws": ["магия требует имени, цены или признанного права"]
                }
            }),
        )
        .expect("update world");
    assert_eq!(updated["status"], json!("ready"));
    assert_eq!(updated["title"], json!("Порог Второго Неба"));
    assert!(updated.get("architect_messages").is_none());
    assert!(store
        .get_architect_state(&world_id)
        .expect("read after update")
        .is_some());
    let version_before = store.world_version(&world_id).expect("version");

    // After the conversation moves to the dialogs DB, the package artifacts are
    // purged: a stray architect.json is deleted, the legacy payload keys are
    // stripped, and the content version is NOT bumped.
    std::fs::write(
        store.world_dir(&world_id).join("architect.json"),
        b"{\"messages\": []}",
    )
    .expect("plant stray architect.json");
    store
        .purge_architect_artifacts(&world_id)
        .expect("purge artifacts");
    assert_eq!(
        store.world_version(&world_id).expect("version after purge"),
        version_before,
        "architect purge never bumps the content version"
    );
    assert!(!store.world_dir(&world_id).join("architect.json").is_file());
    let raw = std::fs::read_to_string(store.world_dir(&world_id).join("world.json"))
        .expect("read world.json");
    assert!(!raw.contains("architect_messages"));
    // With every artifact gone the fallback reader has nothing left.
    assert!(store
        .get_architect_state(&world_id)
        .expect("read after purge")
        .is_none());
}

#[test]
fn dialog_store_architect_chats_round_trip() {
    let (store, _dir) = temp_store();
    // Absent → None.
    assert!(store
        .get_architect_chat("world", "w1")
        .expect("get empty")
        .is_none());
    // Upsert + read back.
    store
        .set_architect_chat(
            "world",
            "w1",
            &json!({"messages": [{"role": "user", "content": "Хочу мир клятв."}]}),
        )
        .expect("set");
    store
        .set_architect_chat(
            "world",
            "w1",
            &json!({"messages": [], "cache_session_id": "s2"}),
        )
        .expect("upsert");
    let got = store
        .get_architect_chat("world", "w1")
        .expect("get")
        .expect("present");
    assert_eq!(got["cache_session_id"], "s2");
    // Kinds are independent keys.
    assert!(store
        .get_architect_chat("story", "w1")
        .expect("other kind")
        .is_none());
    // Delete is a no-op-safe cleanup.
    store.delete_architect_chat("world", "w1").expect("delete");
    assert!(store
        .get_architect_chat("world", "w1")
        .expect("get after delete")
        .is_none());
}

#[test]
fn world_store_migrates_legacy_sqlite_rows() {
    use rusqlite::Connection;

    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("dialogs.sqlite3");

    // Seed an old-style SQLite `worlds` row (the legacy schema).
    {
        let con = Connection::open(&db_path).expect("open legacy db");
        con.execute_batch(
            r#"
            CREATE TABLE worlds (
                guest_id TEXT NOT NULL,
                world_id TEXT NOT NULL,
                title TEXT NOT NULL,
                preview TEXT NOT NULL,
                payload TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (guest_id, world_id)
            );
            "#,
        )
        .expect("create legacy worlds table");
        let payload = json!({
            "title": "Старый Мир",
            "genre": "fantasy",
            "public_premise": "Наследие былых эпох.",
            "world_lore": {"name": "Старый Мир"}
        })
        .to_string();
        con.execute(
            "INSERT INTO worlds (guest_id, world_id, title, preview, payload, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                "shared",
                "legacy-world-1",
                "Старый Мир",
                "Наследие былых эпох.",
                payload,
                "2026-06-01 10:00:00",
                "2026-06-02 11:00:00"
            ],
        )
        .expect("insert legacy world");
    }

    let store = WorldStore::new(dir.path().join("library")).expect("create world store");
    let imported = store
        .migrate_from_sqlite(db_path.to_string_lossy().as_ref())
        .expect("migrate");
    assert_eq!(imported, 1, "one legacy world imported");

    // The package now exists and round-trips the payload + timestamps.
    let worlds = store.list_worlds().expect("list after migrate");
    assert_eq!(worlds.len(), 1);
    assert_eq!(worlds[0]["id"], json!("legacy-world-1"));
    assert_eq!(worlds[0]["title"], json!("Старый Мир"));
    assert_eq!(worlds[0]["genre"], json!("fantasy"));
    assert_eq!(worlds[0]["created_at"], json!("2026-06-01 10:00:00"));
    assert_eq!(worlds[0]["updated_at"], json!("2026-06-02 11:00:00"));

    // Idempotent: a second migration is a no-op (packages already present).
    let again = store
        .migrate_from_sqlite(db_path.to_string_lossy().as_ref())
        .expect("migrate again");
    assert_eq!(again, 0, "re-migration is a no-op");
    assert_eq!(store.list_worlds().expect("list").len(), 1);
}

#[test]
fn world_store_write_read_assets_roundtrip() {
    let (store, dir) = temp_world_store();
    let world = store
        .create_world(json!({"title": "Мир с картинкой"}))
        .expect("create world");
    let world_id = world["id"].as_str().expect("world id").to_string();

    assert!(!store.has_asset(&world_id, "world_image.png"));
    assert!(store
        .read_asset(&world_id, "world_image.png")
        .expect("read missing asset")
        .is_none());

    let png = b"\x89PNG\r\n\x1a\n-fake-bytes";
    store
        .write_asset(&world_id, "world_image.png", png)
        .expect("write asset");

    // The asset lands inside the package's assets/ directory.
    let on_disk = dir
        .path()
        .join("library")
        .join("worlds")
        .join(&world_id)
        .join("assets")
        .join("world_image.png");
    assert!(on_disk.is_file(), "asset written under assets/");

    assert!(store.has_asset(&world_id, "world_image.png"));
    assert_eq!(
        store
            .read_asset(&world_id, "world_image.png")
            .expect("read asset"),
        Some(png.to_vec())
    );
    assert_eq!(
        store.asset_path(&world_id, "world_image.png"),
        on_disk,
        "asset_path resolves to the on-disk location"
    );

    // Overwrite is atomic and replaces the bytes.
    let png2 = b"\x89PNG\r\n\x1a\n-other";
    store
        .write_asset(&world_id, "world_image.png", png2)
        .expect("overwrite asset");
    assert_eq!(
        store
            .read_asset(&world_id, "world_image.png")
            .expect("read asset 2"),
        Some(png2.to_vec())
    );
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
