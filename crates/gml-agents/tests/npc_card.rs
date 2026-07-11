//! NPC card snapshot-once (GM_CONTEXT_TZ §7): the card is persisted once at the
//! head of history and re-sent verbatim, NOT glued to a copy of the final turn
//! each call; a persisted card message is authoritative (never downgraded to a
//! HISTORICAL exchange).

use gml_agents::{
    historical_npc_message, is_npc_card_message, npc_card_message, npc_request_messages,
    npc_user_message, NPC_CARD_HEADER,
};
use gml_world::Npc;
use serde_json::{json, Value};

fn sample_npc() -> Npc {
    serde_json::from_value(json!({
        "npc_id": "borin",
        "name": "Борин",
        "persona": "суровый кузнец",
        "voice": "низкий голос",
        "goals": "защитить деревню",
        "knowledge": "тайный ход",
        "secret": "он дезертир",
        "role": "кузнец",
    }))
    .expect("npc from json")
}

fn content(m: &Value) -> String {
    m.get("content").and_then(Value::as_str).unwrap_or("").to_string()
}

#[test]
fn card_message_is_a_user_message_with_the_card_header() {
    let npc = sample_npc();
    let msg = npc_card_message(&npc);
    assert_eq!(msg.get("role").and_then(Value::as_str), Some("user"));
    assert!(content(&msg).starts_with(NPC_CARD_HEADER));
    assert!(is_npc_card_message(&msg));
}

#[test]
fn persisted_card_is_sent_verbatim_not_downgraded_to_historical() {
    let npc = sample_npc();
    let card = npc_card_message(&npc);
    // historical_npc_message must pass the card through unchanged.
    let historical = historical_npc_message(&card);
    assert_eq!(historical, card, "card is authoritative, not a historical exchange");
    assert!(!content(&historical).contains("HISTORICAL NPC EXCHANGE"));
}

#[test]
fn request_with_card_in_history_sends_a_bare_final_turn() {
    let npc = sample_npc();
    let card = npc_card_message(&npc);
    let earlier_user = npc_user_message("что ты знаешь?", "", "", None, &[], "");
    let earlier_assistant = json!({"role": "assistant", "content": "Немного."});
    let history = vec![card.clone(), earlier_user, earlier_assistant];
    let final_turn = npc_user_message("а теперь?", "", "", None, &[], "");

    let req = npc_request_messages(&npc, &history, "", &final_turn);

    // The final message is the bare turn — the card is NOT glued onto it again.
    let last = req.last().expect("non-empty request");
    assert_eq!(last, &final_turn, "final turn must be sent bare");
    assert!(!content(last).contains(NPC_CARD_HEADER), "no per-call card injection");

    // The card still appears once, verbatim, inside the history segment.
    let card_count = req.iter().filter(|m| is_npc_card_message(m)).count();
    assert_eq!(card_count, 1, "exactly one card message, from history");
}

#[test]
fn request_without_card_in_history_falls_back_to_gluing_the_card() {
    let npc = sample_npc();
    let final_turn = npc_user_message("привет", "", "", None, &[], "");
    // Legacy/defensive path: no card in history.
    let req = npc_request_messages(&npc, &[], "", &final_turn);
    let last = req.last().expect("non-empty request");
    // Byte-identical to the legacy assembly: the raw card block (which itself
    // opens with "CURRENT NPC CARD (revision N)") is glued to the final turn.
    assert!(
        content(last).contains("CURRENT NPC CARD"),
        "defensive fallback keeps the card visible when history lacks it"
    );
    assert!(content(last).contains("привет"), "final turn text is preserved");
}
