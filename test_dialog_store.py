"""SQLite dialog persistence smoke tests."""
import os
import tempfile

os.environ.setdefault("GM_BACKEND", "mock")

from dialog_store import DialogStore
from llm_client import make_client


with tempfile.TemporaryDirectory() as tmp:
    db_path = os.path.join(tmp, "dialogs.sqlite3")
    guest_id = "guest_0123456789abcdef0123456789"

    store = DialogStore(db_path, make_client)
    dialog = store.get(guest_id)

    first_roll = dialog.session.world.roll("1d20")
    dialog.session.world.constraints.append("Only the persisted guest sees this.")
    dialog.session.world.set_npc_whereabouts(
        "mareth",
        location_id="turnvale_guardhouse",
        location_name="караульная Тёрнвейла",
        status="known",
        details="её там ищут по делу Алдрика",
        source="test",
    )
    dialog.session.gm_messages.append({"role": "user", "content": "hello"})
    dialog.session.gm_summary = "summary"
    dialog.session.npc_messages["borin"] = [
        {"role": "user", "content": "npc situation"},
        {"role": "assistant", "content": "{\"speech\":\"да\"}"},
    ]
    dialog.session.npc_summaries["borin"] = "Борин уже отвечал уклончиво."
    dialog.session.npc_client_state["borin"] = {
        "model": "mock-npc",
        "session_id": "npc-session",
        "thread_id": "npc-thread",
    }
    dialog.session.add_turn_usage({
        "calls": [{"label": "Борин", "in": 10, "out": 5, "cached": 7}],
        "in": 10,
        "out": 5,
        "cached": 7,
        "tokens": 15,
        "peak_context": 10,
        "secs": 1.25,
    })
    dialog.session.turn = 1
    dialog.session.record_public("player", "speech", speech="hello")
    dialog.transcript.append({
        "turn": 1,
        "event": {"kind": "player", "agent": "Player", "data": "hello", "sid": None},
    })
    dialog.turn_count = 1
    # A pending NPC draft must round-trip its LLM turn (user_message/assistant_message),
    # otherwise a save landing between draft() and commit_turn() drops the NPC's private
    # history while still letting the event reach the world.
    dialog.session.draft(
        "lysa", "тихо", "кивает", [],
        user_message={"role": "user", "content": "lysa situation"},
        assistant_message={"role": "assistant", "content": "{\"speech\":\"тихо\"}"},
        witnesses=frozenset({"player", "lysa"}),
    )
    store.save(dialog)

    expected_next_roll = dialog.session.world.roll("1d20")

    reloaded_store = DialogStore(db_path, make_client)
    reloaded = reloaded_store.get(guest_id)

    assert first_roll[1].startswith("1d20 ->")
    assert reloaded.turn_count == 1
    assert reloaded.transcript[0]["event"]["data"] == "hello"
    assert reloaded.session.gm_messages == [{"role": "user", "content": "hello"}]
    assert reloaded.session.gm_summary == "summary"
    assert reloaded.session.npc_messages["borin"][0]["content"] == "npc situation"
    assert reloaded.session.npc_summaries["borin"] == "Борин уже отвечал уклончиво."
    assert reloaded.session.npc_client_state["borin"]["thread_id"] == "npc-thread"
    assert reloaded.session.run_usage["tokens"] == 15
    assert reloaded.session.run_usage["cached"] == 7
    assert reloaded.session.run_usage["npc_tokens"] == 15
    assert reloaded.session.events[0].speech == "hello"
    assert reloaded.session.events[0].witnesses == frozenset({"borin", "lysa", "player"})
    assert reloaded.session.world.constraints[-1] == "Only the persisted guest sees this."
    assert (
        reloaded.session.world.npc_whereabouts_export("mareth")["location_name"]
        == "караульная Тёрнвейла"
    )
    assert reloaded.session.world.roll("1d20") == expected_next_roll

    # Pending LLM turn survived the round-trip...
    pending_lysa = reloaded.session.pending["lysa"]
    assert pending_lysa["user_message"] == {"role": "user", "content": "lysa situation"}
    assert pending_lysa["assistant_message"]["role"] == "assistant"
    # ...and commit_turn() can therefore still fold it into the NPC's private history.
    reloaded.session.commit_turn()
    assert reloaded.session.npc_messages["lysa"] == [
        {"role": "user", "content": "lysa situation"},
        {"role": "assistant", "content": "{\"speech\":\"тихо\"}"},
    ]

print("DIALOG STORE TEST PASSED")
