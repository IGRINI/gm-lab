"""SQLite dialog persistence smoke tests."""
import os
import sqlite3
import tempfile

os.environ.setdefault("GM_BACKEND", "mock")

from dialog_store import (  # noqa: E402
    DEFAULT_CHAT_TITLE,
    DialogStore,
)
from llm_client import make_client  # noqa: E402
from orchestrator import Session  # noqa: E402
import world as world_mod  # noqa: E402


def test_runtime_round_trip(tmp: str) -> None:
    db_path = os.path.join(tmp, "round_trip.sqlite3")
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
    dialog.session.world.advance_time(17, "проверка сохранения времени")
    dialog.session.world.update_player_character({
        "name": "Рин",
        "class_role": "следопыт",
        "condition": "ранен",
        "gm_notes": "PLAYER_STORE_SENTINEL",
        "abilities": {"DEX": 14, "WIS": 15},
        "skills": {"Perception": 5},
        "hp": {"current": 6, "max": 10},
        "inventory": ["фонарь", "верёвка"],
    }, "проверка сохранения карточки игрока")
    dialog.session.world.update_npc("borin", {
        "age": "Фактически 54 года; выглядит на 50-55.",
        "physical_type": "крупный трактирщик",
        "distinctive_features": "медное кольцо",
        "abilities": {"STR": 13, "WIS": 14},
        "passive_perception": 14,
        "hp": {"current": 10, "max": 11},
    })
    added_state = dialog.session.world.add_state_records([{
        "kind": "rumor",
        "text": "ENTITY_ROUNDTRIP_SENTINEL Борин сказал игроку о Лизе.",
        "scope": "shared",
        "owner": "borin",
        "subject": "player",
        "entity_id": "lysa",
        "source_npc": "borin",
        "participants": ["mareth"],
        "location_id": "turnvale_square",
        "location_name": "Площадь Тёрнвейля",
        "region_id": "turnvale",
        "region_name": "Тёрнвейль",
        "scene_id": "turnvale_square_gate",
        "importance": "clue",
        "aliases": ["Тёрнвейл", "Тёрнвейле", "Turnvale"],
        "metadata": {"known_name": "Лиза"},
    }])
    assert added_state
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
    assert reloaded.chat_id == dialog.chat_id
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
    assert reloaded.session.world.time_export()["absolute_minutes"] == 17
    assert reloaded.session.world.time_export()["last_advance_minutes"] == 17
    assert reloaded.session.world.time_export()["last_advance_reason"] == "проверка сохранения времени"
    assert "Previous player turn elapsed: 17 minutes" in reloaded.session.world.time_context()
    assert reloaded.session.world.player_character.name == "Рин"
    assert reloaded.session.world.player_character.class_role == "следопыт"
    assert reloaded.session.world.player_character.condition == "ранен"
    assert reloaded.session.world.player_character.gm_notes == "PLAYER_STORE_SENTINEL"
    assert reloaded.session.world.player_character.skills["Perception"] == 5
    assert reloaded.session.world.player_character.hp["current"] == 6
    assert reloaded.session.world.player_character.inventory == ["фонарь", "верёвка"]
    assert reloaded.session.world.npc("borin").age.startswith("Фактически 54")
    assert reloaded.session.world.npc("borin").physical_type == "крупный трактирщик"
    assert reloaded.session.world.npc("borin").abilities["WIS"] == 14
    assert reloaded.session.world.npc("borin").passive_perception == 14
    assert reloaded.session.world.npc("borin").hp["current"] == 10
    entity_rows = reloaded.session.world.state_records_for(
        "player",
        kinds=("rumor",),
        entity_id="lysa",
        source_npc="borin",
    )
    assert len(entity_rows) == 1
    assert entity_rows[0].subject == "player"
    assert entity_rows[0].participants == ("mareth",)
    assert reloaded.session.world.state_records_for(
        "mareth",
        kinds=("rumor",),
        entity_id="lysa",
        source_npc="borin",
    )
    assert entity_rows[0].location_id == "turnvale_square"
    assert entity_rows[0].location_name == "Площадь Тёрнвейля"
    assert entity_rows[0].region_id == "turnvale"
    assert entity_rows[0].region_name == "Тёрнвейль"
    assert entity_rows[0].scene_id == "turnvale_square_gate"
    assert entity_rows[0].importance == "clue"
    assert "Тёрнвейле" in entity_rows[0].aliases
    assert entity_rows[0].metadata["known_name"] == "Лиза"
    assert reloaded.session.world.npc_player_label("lysa") == "Лиза"
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


def test_multiple_chats_for_one_guest(tmp: str) -> None:
    db_path = os.path.join(tmp, "multi_chat.sqlite3")
    guest_id = "guest_multi_0123456789abcdef012345"
    store = DialogStore(db_path, make_client)

    first = store.create_chat(guest_id, title="First thread", activate=True)
    first.transcript.append({
        "turn": 1,
        "event": {"kind": "player", "agent": "Player", "data": "first transcript"},
    })
    first.session.world.constraints.append("first-only constraint")
    first.turn_count = 1
    store.save(first)

    second = store.create_chat(guest_id, title="Second thread", activate=True)
    second.transcript.append({
        "turn": 1,
        "event": {"kind": "player", "agent": "Player", "data": "second transcript"},
    })
    second.session.world.constraints.append("second-only constraint")
    second.turn_count = 1
    store.save(second)

    con = sqlite3.connect(db_path)
    try:
        con.execute(
            "UPDATE dialog_chats SET updated_at = ? WHERE guest_id = ? AND chat_id = ?",
            ("2026-06-18T00:00:00Z", guest_id, first.chat_id),
        )
        con.execute(
            "UPDATE dialog_chats SET updated_at = ? WHERE guest_id = ? AND chat_id = ?",
            ("2026-06-18T00:01:00Z", guest_id, second.chat_id),
        )
        con.commit()
    finally:
        con.close()

    assert first.chat_id != second.chat_id
    assert store.get_active(guest_id).chat_id == second.chat_id

    activated_first = store.activate_chat(guest_id, first.chat_id)
    assert activated_first is not None
    assert store.get_active(guest_id).chat_id == first.chat_id

    chats = store.list_chats(guest_id)
    assert [chat["id"] for chat in chats] == [second.chat_id, first.chat_id]
    assert {chat["id"]: chat["active"] for chat in chats} == {
        first.chat_id: True,
        second.chat_id: False,
    }
    assert {chat["id"]: chat["turn_count"] for chat in chats} == {
        first.chat_id: 1,
        second.chat_id: 1,
    }
    assert {chat["id"]: chat["title"] for chat in chats} == {
        first.chat_id: "First thread",
        second.chat_id: "Second thread",
    }

    activated_first.session.world.scene.title = "Updated scene title"
    activated_first.transcript.append({
        "turn": 2,
        "event": {"kind": "player", "agent": "Player", "data": "first follow-up"},
    })
    activated_first.turn_count = 2
    store.save(activated_first)

    reloaded_store = DialogStore(db_path, make_client)
    first_reloaded = reloaded_store.get(guest_id, first.chat_id)
    second_reloaded = reloaded_store.get(guest_id, second.chat_id)

    assert first_reloaded.turn_count == 2
    assert first_reloaded.title == "First thread"
    assert first_reloaded.transcript[-1]["event"]["data"] == "first follow-up"
    assert first_reloaded.session.world.constraints[-1] == "first-only constraint"
    assert second_reloaded.turn_count == 1
    assert second_reloaded.title == "Second thread"
    assert second_reloaded.transcript[-1]["event"]["data"] == "second transcript"
    assert second_reloaded.session.world.constraints[-1] == "second-only constraint"


def test_merge_all_chats_into_shared_scope(tmp: str) -> None:
    db_path = os.path.join(tmp, "shared_scope.sqlite3")
    store = DialogStore(db_path, make_client)

    shared = store.create_chat("shared", title="Existing shared", activate=True)
    first = store.create_chat("guest_a", title="Guest A", activate=True)
    second = store.create_chat("guest_b", title="Guest B", activate=True)

    con = sqlite3.connect(db_path)
    try:
        con.execute(
            "UPDATE dialog_chats SET updated_at = ? WHERE guest_id = ? AND chat_id = ?",
            ("2026-06-18T00:00:00Z", "shared", shared.chat_id),
        )
        con.execute(
            "UPDATE dialog_chats SET updated_at = ? WHERE guest_id = ? AND chat_id = ?",
            ("2026-06-18T00:01:00Z", "guest_a", first.chat_id),
        )
        con.execute(
            "UPDATE dialog_chats SET updated_at = ? WHERE guest_id = ? AND chat_id = ?",
            ("2026-06-18T00:02:00Z", "guest_b", second.chat_id),
        )
        con.commit()
    finally:
        con.close()

    assert store.merge_all_chats_into_scope("shared") == 2

    chats = store.list_chats("shared")
    assert [chat["title"] for chat in chats] == ["Guest B", "Guest A", "Existing shared"]
    assert store.active_chat_id("shared") == second.chat_id
    assert store.list_chats("guest_a") == []
    assert store.list_chats("guest_b") == []

    assert store.merge_all_chats_into_scope("shared") == 0
    assert len(store.list_chats("shared")) == 3


def test_selected_story_round_trip(tmp: str) -> None:
    db_path = os.path.join(tmp, "story.sqlite3")
    guest_id = "guest_story_0123456789abcdef01234"
    store = DialogStore(db_path, make_client)

    dialog = store.create_chat(
        guest_id,
        session=Session(None, world_mod.World.from_story("frozen-harbor")),
        activate=True,
    )
    dialog.session.world.add_fact("Debug-only fact survives.", "truth")
    dialog.session.world.set_npc_whereabouts(
        "sana",
        location_id="secret_warehouse",
        location_name="тайный склад",
        status="known",
        details="перенесена через debug menu",
        source="debug",
    )
    store.save(dialog)

    reloaded = DialogStore(db_path, make_client).get_active(guest_id)

    assert reloaded.session.world.story_id == "frozen-harbor"
    assert reloaded.session.world.story_title == "Ледяной порт Нордхольм"
    assert "iva" in reloaded.session.world.npcs
    assert "borin" not in reloaded.session.world.npcs
    assert any(record.text == "Debug-only fact survives." for record in reloaded.session.world.fact_records)
    assert (
        reloaded.session.world.npc_whereabouts_export("sana")["location_name"]
        == "тайный склад"
    )


def test_new_schema_and_default_title(tmp: str) -> None:
    db_path = os.path.join(tmp, "schema.sqlite3")
    guest_id = "guest_schema_0123456789abcdef0123"
    store = DialogStore(db_path, make_client)

    dialog = store.get_active(guest_id)
    dialog.session.world.scene.title = "Scene title should not replace default"
    dialog.transcript.append({
        "turn": 1,
        "event": {"kind": "player", "agent": "Player", "data": "player text"},
    })
    dialog.turn_count = 1
    store.save(dialog)

    reloaded_store = DialogStore(db_path, make_client)
    reloaded = reloaded_store.get_active(guest_id)
    assert reloaded.title == DEFAULT_CHAT_TITLE
    assert reloaded.preview == "player text"

    tables = set()
    con = sqlite3.connect(db_path)
    try:
        rows = con.execute(
            "SELECT name FROM sqlite_master WHERE type = 'table'"
        ).fetchall()
        tables = {row[0] for row in rows}
    finally:
        con.close()

    assert "dialog_chats" in tables
    assert "guest_dialog_state" in tables


with tempfile.TemporaryDirectory() as tmp:
    test_runtime_round_trip(tmp)
    test_multiple_chats_for_one_guest(tmp)
    test_merge_all_chats_into_shared_scope(tmp)
    test_selected_story_round_trip(tmp)
    test_new_schema_and_default_title(tmp)

print("DIALOG STORE TEST PASSED")
