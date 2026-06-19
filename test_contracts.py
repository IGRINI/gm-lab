"""Contract tests for prompts/tools crossing the model boundary."""
import json
import os

os.environ.setdefault("GM_BACKEND", "mock")

import agents
import stories
import world as world_mod
from llm_client import make_client
from llm_client import _proper_nouns_line
from orchestrator import (
    Session,
    _normalize_tool_calls,
    _run_tool,
    _tool_full_text,
    _tool_model_text,
    context_usage,
    run_turn,
)


def tool_by_name(tools, name):
    return next(t["function"] for t in tools if t["function"]["name"] == name)


w = world_mod.World()
story_list = stories.list_stories()
assert stories.DEFAULT_STORY_ID == "turnvale-murder"
assert len(story_list) == 3
assert {story["id"] for story in story_list} == {"turnvale-murder", "frozen-harbor", "glass-garden"}
assert all({"id", "title", "description"} <= set(story) for story in story_list)
assert w.story_id == stories.DEFAULT_STORY_ID
assert w.story_title == "Убийство в Тёрнвейле"
assert "Борин" in w.proper_nouns()
assert "«Серый грифон»" in w.proper_nouns()
assert "borin" in w.scene.present_npcs
assert "mareth" not in w.scene.present_npcs
assert w.npc_can_react("borin")
assert not w.npc_can_react("mareth")
assert w.npc("mareth").pronouns == "F"
mareth_where = w.npc_whereabouts_export("mareth")
assert mareth_where["status"] == "likely"
assert "страж" in mareth_where["location_name"]
assert "Present named NPCs" in w.scene_context()
assert "Known offscreen NPC whereabouts" in w.scene_context()
assert "Капитан Марет" in w.scene_context()
assert "Visible exits" in w.npc_scene_slice("borin")
assert "Борин" in _proper_nouns_line(w.proper_nouns())
assert "Borin" not in _proper_nouns_line(w.proper_nouns())

import dialog_store
w_rev = world_mod.World()
assert w_rev.npc("borin").card_revision == 0
assert w_rev.update_npc("borin", {"persona": "полностью новая личность"})
assert w_rev.npc("borin").card_revision == 1
assert w_rev.update_npc("borin", {"color": "#123456"})
assert w_rev.npc("borin").card_revision == 1
assert w_rev.update_npc("borin", {"persona": "полностью новая личность"})
assert w_rev.npc("borin").card_revision == 1
assert w_rev.update_npc("borin", {"goals": "новая цель"})
assert w_rev.npc("borin").card_revision == 2
minimal_npc = dialog_store._npc_from_payload({"npc_id": "x", "name": "X", "persona": "p"})
assert minimal_npc.card_revision == 0
assert world_mod.NPC(npc_id="y", name="Y", persona="", voice="", goals="", knowledge="", secret="").card_revision == 0
assert dialog_store._npc_to_payload(w_rev.npc("borin"))["card_revision"] == 2

frozen_world = world_mod.World.from_story("frozen-harbor")
glass_world = world_mod.World.from_story("glass-garden")
assert frozen_world.story_id == "frozen-harbor"
assert glass_world.story_id == "glass-garden"
assert "iva" in frozen_world.npcs and "ella" in glass_world.npcs
assert "borin" not in frozen_world.npcs and "borin" not in glass_world.npcs
try:
    world_mod.World.from_story("missing-story")
    raise AssertionError("unknown story id must fail")
except KeyError:
    pass

gm_system = agents._gm_system(w, "")
assert "Do not require action ids" in gm_system
assert "Tool argument values are in RUSSIAN" in gm_system
assert "Streamed thinking / internal notes" in gm_system
assert "not upgrade it to shouting" in gm_system
assert "Quiet/private speech is private by default" in gm_system
assert "room heard the private content" in gm_system
assert "credible intimidation" in gm_system
assert "roll_dice before ask_npc" in gm_system
assert "Time and initiative must keep moving" in gm_system
assert "advance the world to the next meaningful change" in gm_system
assert "Treat that as permission to advance time" in gm_system
assert "paying it\n  off on a later beat" in gm_system
assert "Before asserting, summarizing, or acting on any non-visible world fact" in gm_system
assert "If get_world_fact returns unknown" in gm_system
assert "D&D 5E ROLL DISCIPLINE" in gm_system
assert "Actively call roll_dice for player-initiated attention" in gm_system
assert "2d20kh1" in gm_system
assert "Do not adjust the target after seeing the roll" in gm_system
assert "roll_dice private notes compact and English" in gm_system
assert "do not block core clues behind one bad roll" in gm_system
assert "CORE GM PRIORITY" in gm_system
assert "not to print a sparse event log" in gm_system
assert "PRE-TOOL NARRATION" in gm_system
assert "This prelude is shown before the tool result" in gm_system
assert "Make pre-tool narration as long as the scene needs" in gm_system
assert "Do not resolve uncertain outcomes in pre-tool narration" in gm_system
assert "Never mention tools" in gm_system
assert "there are no named-NPC words or personal actions" in gm_system
assert "Do not invent hidden facts" in gm_system
assert "Retrieved memory is source material, not automatic truth" in gm_system
assert "call move_npc before final" in gm_system
assert "set_scene before final narration" in gm_system
assert "Known offscreen NPC whereabouts" in gm_system
assert "set_npc_whereabouts" in gm_system
assert "After ask_npc, still write like a real GM" in gm_system
assert "A normal NPC exchange should usually have two parts" in gm_system
assert "breathing after the NPC card" in gm_system
assert "Avoid bland static openers" in gm_system
assert "You may briefly consolidate investigation progress" in gm_system
assert "Do not call a single NPC's statement a proven fact" in gm_system
assert "It is allowed to summarize the current case state" in gm_system
assert "Use Markdown actively" in gm_system
assert "Russian, immersive, sensory" in gm_system
assert "terse status update" in gm_system
assert "immediate visible result" in gm_system
assert "Atmosphere must be concrete" in gm_system
assert "Emojis are allowed when they improve scanning" in gm_system
assert "Борин, Лиза, Капитан Марет" not in gm_system

gm_context = agents._gm_turn_context(w, "Спрашиваю Борина о слухах.")
assert "CURRENT SCENE STATE" in gm_context
assert "SCENE CONSTRAINTS" in gm_context
assert w.constraints[0] in gm_context
assert "PLAYER ACTION" in gm_context
assert "Спрашиваю Борина" in gm_context
assert "CURRENT NAMED NPC ROSTER" in gm_context
assert w.npc("borin").name in gm_context
_public_fact_texts = [r.text for r in w.fact_records if r.kind == "public"]
assert "CURRENT PUBLIC FACTS" in gm_context
assert _public_fact_texts[0] in gm_context
assert w.npc("borin").secret not in gm_context

gm_request = agents._gm_request_messages(w, [agents.gm_user_message(w, "Тест")], "")
assert gm_request[0]["role"] == "system"
assert gm_request[0]["content"] == gm_system
gm_setup = gm_request[1]["content"]
assert "WORLD SETUP" in gm_setup
assert "PUBLIC INTRO" in gm_setup
assert w.public[:40] in gm_setup
assert "NAMED NPC ROSTER" not in gm_setup
assert "PUBLIC FACTS" not in gm_setup
assert gm_request[-1]["role"] == "user"

npc_msgs = agents._npc_messages(w.npc("borin"), "A player asks a question.", "", "", None)
npc_system = npc_msgs[0]["content"]
npc_card_turn = npc_msgs[-1]["content"]
assert npc_msgs[0]["role"] == "system"
assert "all generated\n  JSON values are in RUSSIAN" in npc_system
assert "`reasoning` and `claims` are also in RUSSIAN" in npc_system
assert "Do not become a GM" in npc_system
assert "do not call it shouting" in npc_system
assert "assume the spoken content is between you and the\n  player" in npc_system
assert "does NOT make you unbreakable" in npc_system
assert "believable ladder" in npc_system
assert "`speech` is only the exact words" in npc_system
assert "`speech` may use lightweight Markdown" in npc_system
assert "Values inside JSON may use lightweight Markdown" in npc_system
assert "`claims` are true internal facts" in npc_system
assert "Do not put hidden motives" in npc_system
assert "follow\n  the CURRENT NPC CARD" in npc_system
assert "`M`: refer to yourself/this character with masculine Russian forms" in npc_system
# Spec #1: static system carries NO concrete character DATA and no formatted gender marker.
# The literal label "CURRENT NPC CARD" DOES appear in the static instructions by design,
# so assert on data values, not the label.
assert w.npc("borin").persona not in npc_system
assert w.npc("borin").knowledge not in npc_system
assert w.npc("borin").secret not in npc_system
assert "Russian grammatical gender marker: M" not in npc_system
# Spec #2/#3: card is in the LAST (user) turn, after history.
assert npc_msgs[-1]["role"] == "user"
assert "CURRENT NPC CARD" in npc_card_turn
assert "Gender: M" in npc_card_turn
assert w.npc("borin").persona in npc_card_turn
assert "This card overrides older memory" in npc_card_turn
npc_with_constraints = agents._npc_messages(
    w.npc("lysa"), "The player asks about last night.", "", "", None, w.constraints
)[-1]["content"]
assert "VISIBLE SCENE LIMITS" in npc_with_constraints
assert w.constraints[0] in npc_with_constraints
npc_ordered = agents._npc_messages(
    w.npc("borin"), "A player asks a question.", "", "", None,
    history=[{"role": "user", "content": "HISTORY MARKER A"},
             {"role": "assistant", "content": "HISTORY MARKER B"}],
    summary="SUMMARY MARKER",
)
assert npc_ordered[0]["role"] == "system"
assert "SUMMARY MARKER" in npc_ordered[1]["content"]
assert npc_ordered[2]["content"] == "HISTORY MARKER A"
assert npc_ordered[3]["content"] == "HISTORY MARKER B"
assert "CURRENT NPC CARD" in npc_ordered[-1]["content"]
assert npc_ordered[-1]["role"] == "user"
assert all("CURRENT NPC CARD" not in m["content"] for m in npc_ordered[2:4])

tools = agents.build_gm_tools(w)
tool_names = {tool["function"]["name"] for tool in tools}
assert {"ask_npc", "roll_dice", "get_world_fact", "tool_search"} <= tool_names
initial_tools = agents.build_gm_tools_for_model(w, agents.initial_gm_tool_names())
initial_tool_names = {tool["function"]["name"] for tool in initial_tools}
assert initial_tool_names == {"ask_npc", "roll_dice", "get_world_fact", "tool_search"}
assert "set_scene" not in initial_tool_names
searched_scene = agents.search_gm_tools(w, "перейти новая сцена локация", 3, initial_tool_names)
assert "set_scene" in searched_scene["loaded_tools"]
searched_select = agents.search_gm_tools(w, "select:move_npc,set_npc_whereabouts", 5, initial_tool_names)
assert searched_select["loaded_tools"] == ["move_npc", "set_npc_whereabouts"]
ask_npc = tool_by_name(tools, "ask_npc")
assert ask_npc["parameters"]["required"] == ["npc_id", "situation"]
assert ask_npc["parameters"]["additionalProperties"] is False
assert "Russian neutral third-person brief" in ask_npc["parameters"]["properties"]["situation"]["description"]
assert "intended listener/audience" in ask_npc["parameters"]["properties"]["situation"]["description"]
assert "immediate leverage and danger" in ask_npc["parameters"]["properties"]["situation"]["description"]
assert "in Russian" in ask_npc["parameters"]["properties"]["correction"]["description"]

move_npc = tool_by_name(tools, "move_npc")
assert move_npc["parameters"]["required"] == ["npc_id", "present", "reason"]
assert move_npc["parameters"]["additionalProperties"] is False
assert "in Russian" in move_npc["parameters"]["properties"]["reason"]["description"]

set_npc_whereabouts = tool_by_name(tools, "set_npc_whereabouts")
assert set_npc_whereabouts["parameters"]["additionalProperties"] is False
assert "offscreen whereabouts" in set_npc_whereabouts["description"]
assert set_npc_whereabouts["parameters"]["required"] == ["npc_id", "status"]

set_scene = tool_by_name(tools, "set_scene")
assert set_scene["parameters"]["required"] == ["title", "description", "reason"]
assert set_scene["parameters"]["additionalProperties"] is False
assert "different room" in set_scene["description"]

roll_dice = tool_by_name(tools, "roll_dice")
assert roll_dice["parameters"]["additionalProperties"] is False
assert "intimidation/coercion" in roll_dice["description"]
assert "Perception" in roll_dice["description"]
assert "2d20kh1" in roll_dice["description"]
assert "Put any known modifier directly in notation" in roll_dice["description"]
assert roll_dice["parameters"]["required"] == ["roll_kind", "notation", "check_name", "reason"]
assert roll_dice["parameters"]["properties"]["roll_kind"]["enum"] == [
    "check", "save", "attack", "damage", "chance", "contest"
]
assert roll_dice["parameters"]["properties"]["target_kind"]["enum"] == [
    "DC", "AC", "opposed_total"
]
assert "none" not in roll_dice["parameters"]["properties"]["difficulty_label"]["enum"]
assert "Very short English reason" in roll_dice["parameters"]["properties"]["reason"]["description"]
assert "modifier" in roll_dice["parameters"]["properties"]["modifier_note"]["description"]
assert "none/unknown" not in roll_dice["parameters"]["properties"]["modifier_note"]["description"]
assert roll_dice["parameters"]["properties"]["stakes"]["additionalProperties"] is False
normalized_roll = _normalize_tool_calls([{
    "id": "roll_1",
    "name": "roll_dice",
    "arguments": {
        "roll_kind": "check",
        "notation": "1d20",
        "target_number": 15,
        "target_kind": "DC",
        "check_name": "Wisdom (Perception)",
        "reason": "Scan the tavern hall.",
        "difficulty_label": "moderate",
        "modifier_note": None,
        "stakes": {
            "intent": "Notice details.",
            "success": "Spot something useful.",
            "failure": None,
            "complication": None,
        },
    },
}], w)[0]["arguments"]
assert "modifier_note" not in normalized_roll
assert normalized_roll["stakes"] == {
    "intent": "Notice details.",
    "success": "Spot something useful.",
}
normalized_ungraded = _normalize_tool_calls([{
    "id": "roll_2",
    "name": "roll_dice",
    "arguments": {
        "roll_kind": "damage",
        "notation": "2d6",
        "target_number": None,
        "target_kind": None,
        "check_name": "Damage",
        "reason": "Roll damage.",
        "difficulty_label": None,
        "modifier_note": None,
        "stakes": None,
    },
}], w)[0]["arguments"]
assert normalized_ungraded == {
    "roll_kind": "damage",
    "notation": "2d6",
    "check_name": "Damage",
    "reason": "Roll damage.",
}

get_world_fact = tool_by_name(tools, "get_world_fact")
assert get_world_fact["parameters"]["additionalProperties"] is False
assert "source/provenance" in get_world_fact["description"]
assert "before asserting or summarizing" in get_world_fact["description"]
assert "in Russian" in get_world_fact["parameters"]["properties"]["query"]["description"]

tool_search = tool_by_name(tools, "tool_search")
assert tool_search["parameters"]["additionalProperties"] is False
assert "select:tool_name" in tool_search["description"]

def _drive(gen):
    try:
        while True:
            next(gen)
    except StopIteration as e:
        return e.value
for _t in tools:
    _fn = _t["function"]; _desc = _fn.get("description", "")
    assert "Available NPCs" not in _desc, f"{_fn['name']} description leaks roster"
    assert "borin" not in _desc and "Борин" not in _desc, f"{_fn['name']} names an NPC"
    _props = _fn.get("parameters", {}).get("properties", {})
    if "npc_id" in _props:
        assert "enum" not in _props["npc_id"], f"{_fn['name']}.npc_id has a dynamic enum"
        assert _props["npc_id"]["type"] == "string"
    for _live in ("location_id", "location"):
        if _live in _props:
            assert "enum" not in _props[_live], f"{_fn['name']}.{_live} has a dynamic enum"
assert set_scene["parameters"]["properties"]["present_npcs"]["items"] == {"type": "string"}
for _coll in ("items", "exits"):
    _item_schema = set_scene["parameters"]["properties"][_coll]["items"]
    assert _item_schema["additionalProperties"] is False
    assert _item_schema["required"] == ["name"]
    _item_props = _item_schema.get("properties", {})
    for _k, _v in _item_props.items():
        assert "enum" not in _v, f"set_scene.{_coll}.{_k} has a dynamic enum"
assert set_npc_whereabouts["parameters"]["properties"]["status"]["enum"] == ["known", "likely", "rumored", "unknown"]
normalized_optional_args = {
    call["name"]: call["arguments"]
    for call in _normalize_tool_calls([
        {"name": "ask_npc", "arguments": {"npc_id": "borin", "situation": "тест", "correction": None}},
        {"name": "move_npc", "arguments": {
            "npc_id": "borin",
            "present": False,
            "location": None,
            "visible": None,
            "can_hear": None,
            "activity": None,
            "attitude": None,
            "reason": "тест",
        }},
        {"name": "set_npc_whereabouts", "arguments": {
            "npc_id": "mareth",
            "location_id": None,
            "location_name": None,
            "status": "unknown",
            "details": "unknown",
            "source": None,
        }},
        {"name": "set_scene", "arguments": {
            "title": "Тест",
            "description": "Тест",
            "location_id": None,
            "present_npcs": None,
            "items": [{"name": "фонарь", "visible": None, "portable": None, "owner": None, "details": None}],
            "exits": [{"name": "дверь", "destination": None, "visible": None, "blocked_by": None}],
            "constraints": None,
            "tension": None,
            "reason": "тест",
        }},
        {"name": "tool_search", "arguments": {"query": "select:set_scene", "max_results": None}},
    ], w)
}
assert normalized_optional_args["ask_npc"] == {"npc_id": "borin", "situation": "тест"}
assert normalized_optional_args["move_npc"] == {"npc_id": "borin", "present": False, "reason": "тест"}
assert normalized_optional_args["set_npc_whereabouts"] == {
    "npc_id": "mareth",
    "status": "unknown",
    "details": "unknown",
}
assert normalized_optional_args["set_scene"] == {
    "title": "Тест",
    "description": "Тест",
    "items": [{"name": "фонарь"}],
    "exits": [{"name": "дверь"}],
    "reason": "тест",
}
assert normalized_optional_args["tool_search"] == {"query": "select:set_scene"}
normalized_empty_scene_lists = _normalize_tool_calls([{"name": "set_scene", "arguments": {
    "title": "Пустой двор",
    "description": "Во дворе никого.",
    "present_npcs": [],
    "items": [],
    "exits": [],
    "constraints": [],
    "reason": "тест",
}}], w)[0]["arguments"]
assert normalized_empty_scene_lists == {
    "title": "Пустой двор",
    "description": "Во дворе никого.",
    "present_npcs": [],
    "items": [],
    "exits": [],
    "constraints": [],
    "reason": "тест",
}
generated_id_call = _normalize_tool_calls([{
    "name": "get_world_fact",
    "arguments": {"query": "Где Марет?"},
}], w, id_prefix="test")[0]
assert generated_id_call["id"] == "test_1"
_s = Session(None)
_s.world = w
assert "no such NPC" in _tool_full_text(_drive(_run_tool(_s, "ask_npc", {"npc_id": "no_such_npc", "situation": "x"}, [])))
assert "tool error" in _tool_full_text(_drive(_run_tool(_s, "move_npc", {"npc_id": "ghost", "present": True, "reason": "тест"}, [])))
assert "tool error" in _tool_full_text(_drive(_run_tool(_s, "set_npc_whereabouts", {"npc_id": "ghost", "status": "known", "source": "тест"}, [])))
_scene_ret = _drive(_run_tool(_s, "set_scene", {"title": "Тест", "description": "Тест", "reason": "тест", "present_npcs": ["ghost"]}, []))
_scene_payload = json.loads(_tool_full_text(_scene_ret))
assert "ghost" in _scene_payload.get("dropped_present_npcs", [])
assert "ghost" not in _scene_payload.get("present_npcs", [])
move_null_s = Session(None)
move_null_args = _normalize_tool_calls([{"name": "move_npc", "arguments": {
    "npc_id": "borin",
    "present": True,
    "visible": None,
    "can_hear": None,
    "reason": "strict nullable smoke",
}}], move_null_s.world)[0]["arguments"]
assert "visible" not in move_null_args and "can_hear" not in move_null_args
_drive(_run_tool(move_null_s, "move_npc", move_null_args, []))
assert move_null_s.world.scene.presence["borin"].visible is True
assert move_null_s.world.scene.presence["borin"].can_hear is True
assert move_null_s.world.npc_can_react("borin")
move_false_s = Session(None)
move_false_args = _normalize_tool_calls([{"name": "move_npc", "arguments": {
    "npc_id": "borin",
    "present": True,
    "visible": False,
    "can_hear": False,
    "reason": "explicit false smoke",
}}], move_false_s.world)[0]["arguments"]
assert move_false_args["visible"] is False and move_false_args["can_hear"] is False
_drive(_run_tool(move_false_s, "move_npc", move_false_args, []))
assert move_false_s.world.scene.presence["borin"].visible is False
assert move_false_s.world.scene.presence["borin"].can_hear is False
assert not move_false_s.world.npc_can_react("borin")
scene_null_s = Session(None)
scene_null_args = _normalize_tool_calls([{"name": "set_scene", "arguments": {
    "title": "Склад",
    "description": "Сырой склад у переулка.",
    "items": [{"name": "фонарь", "visible": None, "portable": None}],
    "exits": [{"name": "дверь во двор", "visible": None, "blocked_by": None}],
    "reason": "strict nullable smoke",
}}], scene_null_s.world)[0]["arguments"]
_drive(_run_tool(scene_null_s, "set_scene", scene_null_args, []))
assert scene_null_s.world.scene.items[0].visible is True
assert scene_null_s.world.scene.items[0].portable is False
assert scene_null_s.world.scene.exits[0].visible is True
scene_false_s = Session(None)
scene_false_args = _normalize_tool_calls([{"name": "set_scene", "arguments": {
    "title": "Склад",
    "description": "Сырой склад у переулка.",
    "items": [{"name": "фонарь", "visible": False, "portable": False}],
    "exits": [{"name": "дверь во двор", "visible": False}],
    "reason": "explicit false smoke",
}}], scene_false_s.world)[0]["arguments"]
_drive(_run_tool(scene_false_s, "set_scene", scene_false_args, []))
assert scene_false_s.world.scene.items[0].visible is False
assert scene_false_s.world.scene.items[0].portable is False
assert scene_false_s.world.scene.exits[0].visible is False
where_s = Session(None)
where_args = _normalize_tool_calls([{"name": "set_npc_whereabouts", "arguments": {
    "npc_id": "mareth",
    "status": "unknown",
    "source": None,
}}], where_s.world)[0]["arguments"]
_drive(_run_tool(where_s, "set_npc_whereabouts", where_args, []))
assert where_s.world.npc_whereabouts_export("mareth")["source"] == "gm"

unknown = w.fact("Who exactly forged the mayor's seal?").as_tool_payload()
assert unknown["status"] == "unknown"
assert unknown["text"]

known = w.fact("What is known about Алдрик?").as_tool_payload()
assert known["status"] == "known"
assert "Алдрик" in known["text"]

adv_total, adv_detail = w.roll("2d20kh1+3")
assert adv_total >= 4
assert "keep highest 1" in adv_detail
dis_total, dis_detail = w.roll("2d20kl1")
assert dis_total >= 1
assert "keep lowest 1" in dis_detail
w.forced_die_next = 9
forced_total, forced_detail = w.roll("1d20")
assert forced_total == 9
assert "[forced]" not in forced_detail
w.forced_die_next = 17
graded_total, graded_detail = w.roll_for_outcome("1d20+3", target_number=20, target_kind="DC", roll_kind="check")
assert graded_total == 20
assert "grade=success" in graded_detail
assert "margin=+0" in graded_detail
assert "natural=17" in graded_detail
assert "[forced]" not in graded_detail
w.forced_die_next = 12
weak_total, weak_detail = w.roll_for_outcome("1d20", target_number=15, target_kind="DC", roll_kind="check")
assert weak_total == 12
assert "grade=weak_failure" in weak_detail
assert "margin=-3" in weak_detail
w.forced_die_next = 20
crit_total, crit_detail = w.roll_for_outcome("1d20", target_number=35, target_kind="AC", roll_kind="attack")
assert crit_total == 20
assert "grade=critical_success" in crit_detail
assert "margin=-15" in crit_detail
w.forced_die_next = 1
miss_total, miss_detail = w.roll_for_outcome("1d20+20", target_number=10, target_kind="AC", roll_kind="attack")
assert miss_total == 21
assert "grade=critical_failure" in miss_detail
assert "margin=+11" in miss_detail
damage_total, damage_detail = w.roll_for_outcome("2d6", roll_kind="damage")
assert damage_total >= 2
assert "grade=ungraded" in damage_detail

bad_npc = agents._norm_npc({
    "reasoning": None,
    "speech": "  да  ",
    "action": 12,
    "claims": "not-a-list",
})
assert bad_npc == {"reasoning": "", "speech": "да", "action": "12", "claims": []}

private_s = Session(None)
private_s.turn = 1
private_s.last_player_action = "Говорю Лизе тихо: не называй догадки."
private_s.record_player_for("lysa")
private_s.draft(
    "lysa",
    "Я не стану врать.",
    "тихо отходит к столу",
    [],
    witnesses=frozenset({"player", "lysa"}),
)
assert private_s.events[0].witnesses == frozenset({"player", "lysa"})
assert private_s.observations("borin") == ""
private_s.commit_turn()
assert private_s.events[-1].witnesses == frozenset({"player", "lysa"})
assert private_s.observations("borin") == ""

s = Session(None)
ctx = context_usage(s)
assert ctx["gm"]["active"] > 0
assert ctx["next_compact"]["scope"] == "gm"
assert len(ctx["npcs"]) == len(s.world.npcs)
assert {entry["id"] for entry in ctx["npcs"]} == set(s.world.npcs)
assert all("has_session" in entry for entry in ctx["npcs"])
gen = _run_tool(s, "ask_npc", {"npc_id": "borin"}, [])
events = []
try:
    while True:
        events.append(next(gen))
except StopIteration as stop:
    result = stop.value
assert any(e["kind"] == "error" and "situation" in e["data"] for e in events)
assert "tool error" in _tool_full_text(result)

gen = _run_tool(s, "get_world_fact", {"query": "unknown thing"}, [])
try:
    while True:
        next(gen)
except StopIteration as stop:
    result = stop.value
    payload = json.loads(_tool_full_text(result))
    model_payload = json.loads(_tool_model_text(result))
assert payload["status"] == "unknown"
assert model_payload["status"] == "unknown"
assert set(model_payload) <= {"status", "text", "sources"}

gen = _run_tool(s, "get_world_fact", {"query": "Где искать Капитана Марет?"}, [])
try:
    while True:
        next(gen)
except StopIteration as stop:
    result = stop.value
    known_fact_model = json.loads(_tool_model_text(result))
assert known_fact_model["status"] == "known"
assert known_fact_model.get("sources")
assert all(set(source) <= {"n", "kind", "status", "source"} for source in known_fact_model["sources"])
assert all("score" not in source for source in known_fact_model["sources"])

gen = _run_tool(s, "set_npc_whereabouts", {
    "npc_id": "mareth",
    "location_id": "turnvale_guardhouse",
    "location_name": "караульная Тёрнвейла",
    "status": "known",
    "details": "её там ждут по делу Алдрика",
    "source": "стражник сказал игроку",
}, [])
events = []
try:
    while True:
        events.append(next(gen))
except StopIteration as stop:
    result = stop.value
    payload = json.loads(_tool_full_text(result))
    model_payload = json.loads(_tool_model_text(result))
assert payload["whereabouts"]["location_name"] == "караульная Тёрнвейла"
assert payload["whereabouts"]["status"] == "known"
assert model_payload["whereabouts"]["location_name"] == "караульная Тёрнвейла"
assert "mareth" not in s.world.scene.present_npcs
assert any(e["kind"] == "npc_whereabouts" for e in events)

gen = _run_tool(s, "ask_npc", {
    "npc_id": "mareth",
    "situation": "The player tries to question Марет in the tavern.",
}, [])
events = []
try:
    while True:
        events.append(next(gen))
except StopIteration as stop:
    result = stop.value
assert "not present" in _tool_full_text(result)
assert "Known whereabouts" in _tool_full_text(result)
assert not s.pending

ask_success = Session(make_client())
gen = _run_tool(ask_success, "ask_npc", {
    "npc_id": "borin",
    "situation": "Игрок тихо спрашивает Борина, что он знает об Алдрике.",
}, [])
events = []
try:
    while True:
        events.append(next(gen))
except StopIteration as stop:
    result = stop.value
ask_full = json.loads(_tool_full_text(result))
ask_model = json.loads(_tool_model_text(result))
assert "gm_instruction" in ask_full
assert "gm_instruction" not in ask_model
assert ask_model["already_emitted"] is True
assert "paraphrase" in ask_model["final_narration_rule"]
assert "ask_npc" in ask_model["final_narration_rule"]
assert any(e["kind"] == "npc_speech" for e in events)

gen = _run_tool(s, "move_npc", {
    "npc_id": "mareth",
    "present": True,
    "location": "near the main door",
    "visible": True,
    "can_hear": True,
    "activity": "entering the inn on guard business",
    "reason": "Марет enters the inn looking for witnesses.",
}, [])
events = []
try:
    while True:
        events.append(next(gen))
except StopIteration as stop:
    result = stop.value
    payload = json.loads(_tool_full_text(result))
    model_payload = json.loads(_tool_model_text(result))
assert payload["present"] is True
assert payload["whereabouts"]["status"] == "present"
assert "present_npcs" not in model_payload
assert model_payload["whereabouts"]["status"] == "present"
assert s.world.npc_can_react("mareth")
assert any(e["kind"] == "scene_update" for e in events)

gen = _run_tool(s, "set_scene", {
    "title": "Караульная Тёрнвейла",
    "description": "Тесная караульная у городских ворот.",
    "location_id": "turnvale_guardhouse",
    "present_npcs": ["mareth"],
    "items": [{"id": "desk", "name": "письменный стол", "location": "у окна",
               "visible": True, "portable": False}],
    "exits": [{"id": "street", "name": "дверь на улицу", "destination": "улица",
               "visible": True}],
    "constraints": ["На виду только служебные вещи стражи."],
    "reason": "Игрок дошёл до караульной.",
}, [])
events = []
try:
    while True:
        events.append(next(gen))
except StopIteration as stop:
    result = stop.value
    payload = json.loads(_tool_full_text(result))
    model_payload = json.loads(_tool_model_text(result))
assert payload["title"] == "Караульная Тёрнвейла"
assert payload["location_id"] == "turnvale_guardhouse"
assert "mareth" in s.world.scene.present_npcs
assert payload["npc_whereabouts"]["mareth"]["status"] == "present"
assert "description" not in model_payload
assert model_payload["title"] == "Караульная Тёрнвейла"
assert model_payload["present_npcs"] == ["mareth"]
assert s.world.npc_can_react("mareth")
assert any(e["kind"] == "scene_update" and e["data"].get("title") for e in events)

seed = agents.build_world_seed(make_client(), "ледяной порт, Ива и Рун, пропал корабль")
new_world = world_mod.World.from_seed(seed)
assert "borin" not in new_world.npcs
assert "Алдрик" not in new_world.proper_nouns()
assert new_world.scene.present_npcs
assert new_world.scene.visible_exits()

new_world.record_rumor(1, 1, next(iter(new_world.npcs)), "Рун сказал, что видел огонь на льду.",
                       frozenset({"player"}))
rumor_lookup = new_world.fact("огонь на льду").as_tool_payload()
assert rumor_lookup["status"] == "unknown"
assert "unconfirmed" in rumor_lookup["text"]
assert any(source["kind"] == "testimony" for source in rumor_lookup.get("sources", []))

loose_seed = {
    "name": "Таверна 'Ледяной Клык'",
    "description": "Внутри таверны пахнет хвоей.",
    "present_npcs": ["iva", "run"],
    "visible_objects": ["камин"],
    "exits": [{"id": "docks", "direction": "north", "description": "Дверь к причалу."}],
    "public_facts": ["Корабль 'Северная свеча' пропал без вести три дня назад."],
    "npcs": {
        "iva": {"name": "Ива", "role": "Хозяйка таверны", "location": "behind_bar"},
        "run": {"name": "Рун", "role": "Моряк", "location": "table_2"},
    },
}
loose_world = world_mod.World.from_seed(loose_seed)
assert loose_world.npc("iva").name == "Ива"
assert loose_world.scene.presence["run"].location == "table_2"
assert "камин" in loose_world.scene_context()

nested_loose_seed = {
    "scene": {
        "id": "frozen_hearth",
        "name": "Таверна «Костер»",
        "description": "Тесная таверна у ледяного порта.",
        "present_npcs": ["iva", "run"],
        "visible_objects": [{"id": "map", "display_name": "карта порта"}],
        "visible_exits": [{"id": "dock_door", "display_name": "дверь к причалам",
                           "direction": "south"}],
        "public_facts": ["Корабль «Северная свеча» пропал."],
    },
    "npcs": [
        {"id": "iva", "name": "Ива", "role": "хозяйка", "persona": "Сдержанная.",
         "voice": "Коротко.", "goals": "Беречь таверну.", "knowledge": "Слухи.",
         "secret": "Долг."},
        {"id": "run", "name": "Рун", "role": "моряк", "persona": "Нервный.",
         "voice": "Тихо.", "goals": "Не попасться.", "knowledge": "Видел сигнал.",
         "secret": "Спал на вахте."},
    ],
}
nested_world = world_mod.World.from_seed(nested_loose_seed)
assert nested_world.npc("iva").name == "Ива"
assert "карта порта" in nested_world.scene_context()
assert "дверь к причалам" in nested_world.scene_context()

alias_seed = {
    "public_intro": "Порт Нордхольм ждёт новостей.",
    "scene_title": "Таверна «Коралловый Улей»",
    "scene_description": "Внутри тепло, на стене висит карта.",
    "present_npcs": ["iva"],
    "npcs": {"iva": {"name": "Ива", "role": "хозяйка"}},
    "items": [{"id": "map", "name": "карта северных морей"}],
    "exits": [{"id": "dock", "name": "выход на причал"}],
}
alias_world = world_mod.World.from_seed(alias_seed)
assert alias_world.scene.title == "Таверна «Коралловый Улей»"
assert "карта северных морей" in alias_world.scene_context()


class PreludeToolClient:
    def __init__(self):
        self.calls = 0

    def chat_stream(self, messages, tools=None, think=False, reasoning_role="gm"):
        self.calls += 1
        if self.calls == 1:
            prelude = "Ты остаёшься у стойки и просишь ближайшего завсегдатая позвать капитана."
            yield ("content", prelude)
            raw_tool_calls = [{
                "id": "prelude_fact",
                "type": "function",
                "function": {
                    "name": "get_world_fact",
                    "arguments": json.dumps({"query": "Где искать Капитана Марет?"}, ensure_ascii=False),
                },
            }]
            return (
                "",
                prelude,
                [{"id": "prelude_fact", "name": "get_world_fact",
                  "arguments": {"query": "Где искать Капитана Марет?"}}],
                {"role": "assistant", "content": prelude, "tool_calls": raw_tool_calls},
                {},
            )
        final = "Ожидание затягивается, но теперь у тебя есть направление для поиска."
        yield ("content", final)
        return "", final, [], {"role": "assistant", "content": final}, {}


prelude_events = list(run_turn(Session(PreludeToolClient()), "Прошу кого-нибудь позвать Марет."))
prelude_idx = next(i for i, e in enumerate(prelude_events)
                   if e["kind"] == "gm_narration" and "остаёшься у стойки" in str(e["data"]))
tool_idx = next(i for i, e in enumerate(prelude_events)
                if e["kind"] == "gm_tool_call" and e["data"]["name"] == "get_world_fact")
assert prelude_idx < tool_idx


class FallbackPreludeClient:
    def __init__(self):
        self.tool_decision_done = False
        self.prelude_done = False

    def chat_stream(self, messages, tools=None, think=False, reasoning_role="gm"):
        if tools is None:
            self.prelude_done = True
            prelude = "Ты склоняешься к стойке и начинаешь искать следы, не отпуская зал из внимания."
            yield ("content", prelude)
            return "", prelude, [], {"role": "assistant", "content": prelude}, {}
        if not self.tool_decision_done:
            self.tool_decision_done = True
            raw_tool_calls = [{
                "id": "fallback_roll",
                "type": "function",
                "function": {
                    "name": "roll_dice",
                    "arguments": json.dumps({"notation": "1d20", "reason": "Проверка Мудрости (Внимание)."},
                                            ensure_ascii=False),
                },
            }]
            return (
                "",
                "",
                [{"id": "fallback_roll", "name": "roll_dice",
                  "arguments": {"notation": "1d20", "reason": "Проверка Мудрости (Внимание)."}}],
                {"role": "assistant", "content": "", "tool_calls": raw_tool_calls},
                {},
            )
        final = "По броску становится ясно, что заметить детали непросто."
        yield ("content", final)
        return "", final, [], {"role": "assistant", "content": final}, {}


fallback_client = FallbackPreludeClient()
fallback_events = list(run_turn(Session(fallback_client), "Осматриваю зал."))
fallback_prelude_idx = next(i for i, e in enumerate(fallback_events)
                            if e["kind"] == "gm_narration" and "склоняешься к стойке" in str(e["data"]))
fallback_tool_idx = next(i for i, e in enumerate(fallback_events)
                         if e["kind"] == "gm_tool_call" and e["data"]["name"] == "roll_dice")
assert fallback_client.prelude_done
assert fallback_prelude_idx < fallback_tool_idx


class CanonicalToolCallClient:
    def __init__(self):
        self.calls = 0

    def chat_stream(self, messages, tools=None, think=False, reasoning_role="gm"):
        self.calls += 1
        if self.calls == 1:
            raw_tool_calls = [{
                "id": "",
                "type": "function",
                "function": {
                    "name": "tool_search",
                    "arguments": json.dumps({
                        "query": "select:set_scene",
                        "max_results": None,
                    }, ensure_ascii=False),
                },
            }]
            return (
                "",
                "",
                [{"id": "", "name": "tool_search",
                  "arguments": {"query": "select:set_scene", "max_results": None}}],
                {"role": "assistant", "content": "", "tool_calls": raw_tool_calls},
                {},
            )
        final = "Путь для перехода подготовлен."
        yield ("content", final)
        return "", final, [], {"role": "assistant", "content": final}, {}


canonical_session = Session(CanonicalToolCallClient())
canonical_events = list(run_turn(canonical_session, "Проверь доступные инструменты сцены."))
canonical_call_event = next(e for e in canonical_events if e["kind"] == "gm_tool_call")
assert canonical_call_event["data"] == {
    "name": "tool_search",
    "arguments": {"query": "select:set_scene"},
}
canonical_assistant = next(m for m in canonical_session.gm_messages if m.get("tool_calls"))
canonical_tool_call = canonical_assistant["tool_calls"][0]
canonical_tool_result = next(m for m in canonical_session.gm_messages if m.get("role") == "tool")
canonical_tool_result_event = next(e for e in canonical_events if e["kind"] == "tool_result")
assert canonical_tool_call["id"]
assert canonical_tool_result["tool_call_id"] == canonical_tool_call["id"]
assert canonical_tool_call["function"]["arguments"] == '{"query":"select:set_scene"}'
assert canonical_tool_result["content"] == '{"loaded_tools":["set_scene"],"missing":[]}'
assert "matches" in json.loads(canonical_tool_result_event["data"])


class CompactSceneHistoryClient:
    def __init__(self):
        self.calls = 0

    def chat_stream(self, messages, tools=None, think=False, reasoning_role="gm"):
        self.calls += 1
        if self.calls == 1:
            args = {
                "title": "Склад у трактира",
                "description": "Темный склад с мешками у стены.",
                "location_id": "inn_storehouse",
                "present_npcs": [],
                "items": [{"name": "фонарь", "location": "на крюке", "visible": True}],
                "exits": [{"name": "дверь в зал", "destination": "общий зал", "visible": True}],
                "constraints": [],
                "reason": "Игрок вошёл на склад.",
            }
            raw_tool_calls = [{
                "id": "scene_call",
                "type": "function",
                "function": {
                    "name": "set_scene",
                    "arguments": json.dumps(args, ensure_ascii=False),
                },
            }]
            return (
                "",
                "",
                [{"id": "scene_call", "name": "set_scene", "arguments": args}],
                {"role": "assistant", "content": "", "tool_calls": raw_tool_calls},
                {},
            )
        final = "Склад остаётся тихим."
        yield ("content", final)
        return "", final, [], {"role": "assistant", "content": final}, {}


scene_history_session = Session(CompactSceneHistoryClient())
scene_history_events = list(run_turn(scene_history_session, "Захожу на склад."))
scene_history_tool_msg = next(m for m in scene_history_session.gm_messages if m.get("role") == "tool")
scene_history_event = next(e for e in scene_history_events if e["kind"] == "tool_result")
scene_history_model = json.loads(scene_history_tool_msg["content"])
scene_history_full = json.loads(scene_history_event["data"])
assert "description" not in scene_history_model
assert "npc_whereabouts" not in scene_history_model
assert scene_history_model["title"] == "Склад у трактира"
assert scene_history_model["items"] == [{"item_id": "item_1", "name": "фонарь", "visible": True, "portable": False}]
assert scene_history_full["description"] == "Темный склад с мешками у стены."

rs = Session(None)
for _id in ("borin", "lysa"):
    rs.npc_messages[_id] = [{"role": "user", "content": f"hi {_id}"}]
    rs.npc_summaries[_id] = f"summary-{_id}"
    rs.npc_client_state[_id] = {"thread_id": f"thread-{_id}"}
    rs.npc_clients[_id] = object()
    rs.commitments[_id] = [f"commit-{_id}"]
    rs.delivered[_id] = 5
    rs._shown[_id] = 3
    rs.pending[_id] = {"seq": 7}
assert "borin" in rs.npc_messages and "lysa" in rs.npc_messages
# #12 positive control: with NO reset call history is preserved (the /debug/npc handler
# only clears when data['reset_memory'] is truthy; absent the flag nothing is touched).
assert rs.npc_messages["borin"] == [{"role": "user", "content": "hi borin"}]
assert rs.reset_npc_memory("borin") is True
for _store in (rs.npc_messages, rs.npc_summaries, rs.npc_client_state, rs.npc_clients, rs.commitments, rs.pending):
    assert "borin" not in _store
# delivered/_shown are visibility boundaries into the shared event log, NOT private memory:
# reset PINS them to the current max seq instead of deleting them, so observations() does not
# fall back to 0 and resurface old events as "new" after a reset.
assert rs.delivered["borin"] == rs._seq and rs._shown["borin"] == rs._seq
assert rs.npc_messages["lysa"] == [{"role": "user", "content": "hi lysa"}]
assert rs.npc_summaries["lysa"] == "summary-lysa"
assert rs.npc_client_state["lysa"] == {"thread_id": "thread-lysa"}
assert "lysa" in rs.npc_clients
assert rs.commitments["lysa"] == ["commit-lysa"]
assert rs.delivered["lysa"] == 5 and rs._shown["lysa"] == 3 and rs.pending["lysa"] == {"seq": 7}
assert rs.reset_npc_memory("nonexistent") is False
# Unknown id is rejected BEFORE any mutation: it must not leak into delivered/_shown.
assert "nonexistent" not in rs.delivered and "nonexistent" not in rs._shown
assert rs.reset_npc_memory("") is False
# Contract: True for any real NPC (even with no prior memory), False only for unknown/empty id.
# A valid NPC with ONLY commitments/pending still mutates (cleared + pinned) -> must report True,
# never False-after-mutation.
assert Session(None).reset_npc_memory("mareth") is True
co_s = Session(None)
co_s.commitments["borin"] = ["block"]
co_s.pending["borin"] = {"seq": 1}
assert co_s.reset_npc_memory("borin") is True
assert "borin" not in co_s.commitments and "borin" not in co_s.pending

# Regression (reset must not resurface old observations): an NPC whose delivered boundary
# already covers the old log must STILL see nothing new after a memory reset.
obs_s = Session(None)
obs_s.turn = 1
obs_s.last_player_action = "old talk"
obs_s.record_player_for("borin")
obs_s.snapshot_shown("borin")
obs_s.draft("borin", "привет", "", [], witnesses=frozenset({"player", "borin"}))
obs_s.commit_turn()
obs_s.turn = 2
assert obs_s.observations("borin") == ""
obs_s.reset_npc_memory("borin")
assert obs_s.observations("borin") == ""

# Regression (#2 — debug card save must not flip presence/visibility): exercise the REAL
# handler logic via Session.apply_debug_edit (the exact call /debug/npc delegates to).
pres_s = Session(None)
pres_s.world.scene.presence["borin"].visible = False
pres_s.world.scene.presence["borin"].can_hear = False
# Card-only edit (frontend omits `present` entirely): presence/visibility untouched.
assert pres_s.apply_debug_edit("borin", {"fields": {"persona": "переписанное описание"}}) is True
assert pres_s.world.scene.presence["borin"].visible is False
assert pres_s.world.scene.presence["borin"].can_hear is False
assert not pres_s.world.npc_can_react("borin")
# Same-state present=True (checkbox left ON, NPC already present) is a guarded no-op for visibility.
assert pres_s.apply_debug_edit("borin", {"present": True}) is True
assert pres_s.world.scene.presence["borin"].visible is False
# A genuine present-state CHANGE still toggles presence.
assert pres_s.apply_debug_edit("borin", {"present": False}) is True
assert "borin" not in pres_s.world.scene.present_npcs
assert pres_s.apply_debug_edit("borin", {"present": True}) is True
assert "borin" in pres_s.world.scene.present_npcs
# Unknown id is rejected without mutation; reset_memory via the edit path clears the chosen NPC.
assert pres_s.apply_debug_edit("not_real", {"fields": {"persona": "x"}}) is False
pres_s.npc_messages["borin"] = [{"role": "user", "content": "hi"}]
assert pres_s.apply_debug_edit("borin", {"reset_memory": True}) is True
assert "borin" not in pres_s.npc_messages

import config as _cfg
_rag_saved = _cfg.RAG_ENABLED
_cfg.RAG_ENABLED = False
try:
    leak_world = world_mod.World()
    leak_world.hidden_events = ["LEAK_SENTINEL: гильдия перепрятала контрабанду этой ночью"]
    secret_world = world_mod.World()
    borin_secret = secret_world.npc("borin").secret
    assert "осведомитель Гильдии воров" in borin_secret
    for probe in ("что случилось этой ночью", "контрабанда", "гильдия", "unknown thing"):
        leak_payload = leak_world.fact(probe).as_tool_payload()
        assert "LEAK_SENTINEL" not in leak_payload["text"]
        assert "Recent events" not in leak_payload["text"]
    empty_lookup = leak_world.fact("совершенно несвязанный запрос про погоду").as_tool_payload()
    assert empty_lookup["status"] == "unknown"
    assert empty_lookup["text"] == "Nothing is reliably known about this in town."
    scene_ctx = secret_world.scene_context()
    assert "осведомитель Гильдии воров" not in scene_ctx
    gm_turn_ctx = agents._gm_turn_context(secret_world, "Спрашиваю Борина о слухах.")
    assert "осведомитель Гильдии воров" not in gm_turn_ctx
    gm_full_request = agents._gm_request_messages(secret_world, [agents.gm_user_message(secret_world, "Тест")], "")
    assert all("осведомитель Гильдии воров" not in str(m.get("content", "")) for m in gm_full_request)
    rag_corpus_text = " ".join(doc.text for doc in secret_world.retrieval_documents())
    assert "осведомитель Гильдии воров" not in rag_corpus_text
    assert secret_world.canon and secret_world.canon not in rag_corpus_text
    assert all(getattr(doc, "kind", "") != "truth" for doc in secret_world.retrieval_documents())
finally:
    _cfg.RAG_ENABLED = _rag_saved
print("ALL CONTRACT TESTS PASSED")
