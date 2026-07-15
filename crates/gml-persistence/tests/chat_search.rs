use std::sync::Arc;

use gml_config::Config;
use gml_llm::{Backend, MockClient};
use gml_orchestrator::ClientFactory;
use gml_persistence::{ChatSearchQuery, ChatSearchScope, DialogStore};
use serde_json::json;

fn factory() -> ClientFactory {
    Arc::new(|| Arc::new(MockClient::new()) as Arc<dyn Backend>)
}

fn temp_store() -> (DialogStore, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = Config::from_env();
    config.rag_enabled = false;
    let store = DialogStore::new(
        dir.path().join("dialogs.sqlite3").to_string_lossy(),
        factory(),
        Arc::new(config),
    )
    .expect("store");
    (store, dir)
}

fn query(text: &str) -> ChatSearchQuery {
    ChatSearchQuery {
        text: text.to_string(),
        ..ChatSearchQuery::default()
    }
}

#[test]
fn save_update_delete_and_scope_merge_keep_search_index_consistent() {
    let (store, _dir) = temp_store();
    let chat_id = store
        .create_chat("guest-a", None, None, 0, Some("Ледяная гавань"), None, true)
        .expect("create chat");
    let mut runtime = store.load_chat("guest-a", &chat_id).expect("load chat");
    runtime.turn_count = 1;
    runtime.transcript.push(json!({
        "turn": 1,
        "event": {"kind": "gm_narration", "agent": "ГМ", "data": "Старый маяк погас", "sid": "gm-1"}
    }));
    store.save_owned(runtime).expect("save with transcript");

    let first = store
        .search_chats("guest-a", &query("маяк"))
        .expect("search saved chat");
    assert_eq!(first.total, 1);
    assert_eq!(first.hits[0].id, chat_id);
    assert_eq!(first.hits[0].matched_fields, ["messages"]);

    let mut runtime = store.load_chat("guest-a", &chat_id).expect("reload chat");
    runtime.transcript.clear();
    runtime.transcript.push(json!({
        "turn": 1,
        "event": {"kind": "gm_narration", "agent": "ГМ", "data": "На причале звонит колокол", "sid": "gm-2"}
    }));
    store.save_owned(runtime).expect("replace transcript");
    assert_eq!(
        store.search_chats("guest-a", &query("маяк")).unwrap().total,
        0
    );
    assert_eq!(
        store
            .search_chats(
                "guest-a",
                &ChatSearchQuery {
                    text: "колок".to_string(),
                    scope: ChatSearchScope::Messages,
                    ..ChatSearchQuery::default()
                }
            )
            .unwrap()
            .total,
        1
    );

    store.delete_chat("guest-a", &chat_id).expect("delete chat");
    assert_eq!(
        store
            .search_chats("guest-a", &query("колокол"))
            .unwrap()
            .total,
        0
    );

    let moved_id = store
        .create_chat(
            "guest-b",
            None,
            None,
            0,
            Some("Караван через перевал"),
            None,
            true,
        )
        .expect("create source chat");
    assert_eq!(
        store
            .search_chats("guest-b", &query("караван"))
            .unwrap()
            .hits[0]
            .id,
        moved_id
    );
    assert_eq!(
        store
            .merge_all_chats_into_scope("guest-a")
            .expect("merge scopes"),
        1
    );
    assert_eq!(
        store
            .search_chats("guest-b", &query("караван"))
            .unwrap()
            .total,
        0
    );
    assert_eq!(
        store
            .search_chats("guest-a", &query("караван"))
            .unwrap()
            .total,
        1
    );
}
