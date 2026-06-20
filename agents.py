"""Model roles: the GM with tools and the NPC sub-agent with JSON output."""
from __future__ import annotations

import json
import re

import config
import prompts
import tool_guidance
import world as world_mod

# --- GM tools (Ollama/OpenAI native function format) -----------------------
# Tool schemas stay static for prompt-cache reuse: no live roster, names, or
# dynamic id enums. The live world arrives late in current context and runtime
# execution validates ids.
# Tool names and descriptions are model-facing English; most argument values are requested
# in Russian so the lab/debug UI stays readable. roll_dice is the exception: it uses
# concise English mechanical notes because the code returns English outcome grades.
_SITUATION_DESC = (
    "Russian neutral third-person brief of what is happening RIGHT NOW and what this NPC "
    "perceives. Include the player's action and exact addressed words; quote player phrases "
    "unchanged when precision matters. Preserve declared delivery exactly: whisper, quiet "
    "voice, clenched-teeth mutter, silent gesture, or public speech. Do not upgrade a "
    "whisper/threat to shouting. If secrecy is risky because the room is crowded, describe "
    "that as risk of body language/proximity being noticed, not as other people hearing the "
    "content. State the intended listener/audience. If the player speaks quietly to this NPC, "
    "say that the content is meant for this NPC only unless someone explicitly overheard. Do "
    "include immediate leverage and danger that the NPC can perceive: weapons, distance, "
    "escape routes, witnesses, whether guards are nearby, whether the NPC is cornered, and "
    "any intimidation/check result already rolled by the GM. "
    "not write 'you'. Do not describe the NPC's feelings, motives, choices, or hidden "
    "thoughts. Keep proper nouns exactly as they are written in the current world."
)


def _constraints_text(constraints) -> str:
    return "\n".join(f"- {c}" for c in constraints or [])

_ROLL_DICE_TOOL = {"type": "function", "function": {
    "name": "roll_dice",
    "description": (
        "Roll dice for an uncertain D&D-style mechanical result. Before rolling, lock in "
        "the roll kind, target number, and compact stakes so the post-roll narration cannot "
        "move the goalposts. Call for ability checks "
        "(Perception, Investigation, Insight, Stealth, Persuasion, Deception, "
        "Intimidation, Athletics, Sleight of Hand, lore checks, etc.), contested checks, "
        "saving throws, attacks, damage, random chance, intimidation/coercion, or other "
        "social pressure where success and failure both matter. Do not call for pure "
        "conversation, visible scene "
        "description, trivial/impossible actions, or obvious consequences. Supports "
        "standard notation like 1d20+3 or 2d6, plus 2d20kh1/2d20kl1 for "
        "advantage/disadvantage. Put any known modifier directly in notation; do not "
        "invent unknown character-sheet bonuses."
    ),
    "parameters": {"type": "object", "properties": {
        "roll_kind": {"type": "string",
                      "enum": ["check", "save", "attack", "damage", "chance", "contest"],
                      "description": "Mechanical category. Use check/save/attack/contest when comparing to a target; damage/chance are ungraded unless a target truly matters."},
        "notation": {"type": "string",
                     "description": "Dice notation, e.g. '1d20+3', '2d6', '2d20kh1+2', or '2d20kl1'."},
        "target_number": {"type": "integer",
                          "description": "Pre-roll DC/AC/opposed total for check/save/attack/contest, e.g. 10, 15, 20. Omit for damage or open random chance."},
        "target_kind": {"type": "string",
                        "enum": ["DC", "AC", "opposed_total"],
                        "description": "What target_number means: DC for checks/saves, AC for attacks, opposed_total for contests. Omit when ungraded."},
        "check_name": {"type": "string",
                       "description": "Short English label, e.g. 'Wisdom (Perception)', 'Dexterity (Stealth)', 'Attack', or 'Damage'."},
        "reason": {"type": "string",
                   "description": "Very short English reason, one phrase only. Example: 'Scan the tavern hall.' Do not explain success/failure here."},
        "difficulty_label": {"type": "string",
                             "enum": ["trivial", "easy", "moderate", "hard", "very_hard", "nearly_impossible", "custom"],
                             "description": "Human label for target_number. Prefer easy=10, moderate=15, hard=20 when improvising."},
        "modifier_note": {"type": "string",
                          "description": "Only include when notation itself contains +N/-N, kh1, or kl1 from a real known modifier or advantage/disadvantage source, e.g. '+3 known Perception' or 'advantage from help'. For plain unmodified rolls like 1d20, omit this field entirely. Do not use for leverage, stakes, difficulty, or placeholder text."},
        "stakes": {"type": "object", "properties": {
            "intent": {"type": "string",
                       "description": "Short English pre-roll goal the player is trying to achieve."},
            "success": {"type": "string",
                        "description": "Short English pre-roll promise for what success unlocks."},
            "failure": {"type": "string",
                        "description": "Short English pre-roll consequence or lack of progress on failure."},
            "complication": {"type": "string",
                             "description": "Short English cost to use for near misses or weak failures."},
        }, "additionalProperties": False},
    }, "required": ["roll_kind", "notation", "check_name", "reason"], "additionalProperties": False},
}}

_GET_FACT_TOOL = {"type": "function", "function": {
    "name": "get_world_fact",
    "description": (
        "Retrieve actor-safe world memory when you need a fact, lead, testimony, known NPC "
        "whereabouts, public lore, or prior statement that is not already in CURRENT SCENE "
        "STATE, the public intro, or the conversation. Use before asserting or summarizing "
        "non-visible suspects, leads, clue meanings, timelines, ownership, relationships, "
        "factions, prior testimony, or offscreen NPC locations. Results include "
        "source/provenance and may contain unconfirmed testimony. Do not call for facts "
        "that are visible right now. If the result status is unknown or a source is "
        "unconfirmed, preserve uncertainty instead of inventing an answer."
    ),
    "parameters": {"type": "object", "properties": {
        "query": {"type": "string",
                  "description": "What you want to know, in Russian. Keep proper nouns exactly as written."},
    }, "required": ["query"], "additionalProperties": False},
}}

_TOOL_SEARCH_TOOL = {"type": "function", "function": {
    "name": "tool_search",
    "description": tool_guidance.TOOL_SEARCH_DESCRIPTION,
    "parameters": {"type": "object", "properties": {
        "query": {"type": "string",
                  "description": "Search query in Russian or English, or select:tool_name for exact loading."},
        "max_results": {"type": "integer",
                        "description": "Maximum number of tools to load. Default 5."},
    }, "required": ["query"], "additionalProperties": False},
}}

_INITIAL_GM_TOOL_NAMES = frozenset({
    "ask_npc",
    "roll_dice",
    "get_world_fact",
    "query_world_state",
    "update_world_state",
    "tool_search",
})

_TOOL_SEARCH_HINTS = {
    "ask_npc": (
        "npc нпс персонаж поговорить спросить допросить ответить реакция речь "
        "угроза угрожать убедить обмануть торг приказать борин лиза марет"
    ),
    "move_npc": (
        "npc нпс персонаж входит выходит появился ушел переместить присутствует "
        "сцена слышит видит visibility presence марет пришла борин ушел"
    ),
    "set_npc_whereabouts": (
        "npc нпс местонахождение где искать куда ушел где находится известное "
        "вероятное слух whereabouts absent offscreen марет стража караульная"
    ),
    "set_scene": (
        "сцена локация перейти войти выйти добраться место комната улица здание "
        "travel location scene exits items present_npcs"
    ),
    "roll_dice": (
        "куб кубик бросок проверка d20 dice roll внимание расследование insight "
        "perception investigation stealth persuasion deception intimidation attack save damage"
    ),
    "get_world_fact": (
        "факт память мир lore зацепка улика слух показание где кто что известно "
        "fact memory rag testimony rumor lead source provenance"
    ),
    "update_world_state": (
        "batch пакет обновить записать удалить состояние мир факт слух память npc relationship "
        "отношение цель goal goals npc_memory facts rumors world state compact scope id"
    ),
    "query_world_state": (
        "query scoped scope область player gm npc спросить проверить состояние память "
        "факт секрет цели отношения relationship goal npc_memory id target private public leak безопасный поиск"
    ),
}


def _tool_name(tool: dict) -> str:
    return str(((tool or {}).get("function") or {}).get("name") or "")


def _tool_description(tool: dict) -> str:
    return str(((tool or {}).get("function") or {}).get("description") or "")


def _short_tool_description(tool: dict, limit: int = 220) -> str:
    text = re.sub(r"\s+", " ", _tool_description(tool)).strip()
    if len(text) <= limit:
        return text
    return text[:limit].rstrip() + "..."


def _tool_parameters_text(tool: dict) -> str:
    params = ((tool or {}).get("function") or {}).get("parameters") or {}
    parts: list[str] = []

    def visit(schema: object) -> None:
        if not isinstance(schema, dict):
            return
        desc = schema.get("description")
        if desc:
            parts.append(str(desc))
        props = schema.get("properties")
        if isinstance(props, dict):
            for key, value in props.items():
                parts.append(str(key))
                visit(value)
        items = schema.get("items")
        if items:
            visit(items)

    visit(params)
    return " ".join(parts)


def _tool_search_text(tool: dict) -> str:
    name = _tool_name(tool)
    return " ".join([
        name,
        name.replace("_", " "),
        _tool_description(tool),
        _tool_parameters_text(tool),
        _TOOL_SEARCH_HINTS.get(name, ""),
    ]).lower()


def _score_tool(query_terms: list[str], required_terms: list[str], tool: dict) -> int:
    name = _tool_name(tool).lower()
    text = _tool_search_text(tool)
    if required_terms and not all(term in text for term in required_terms):
        return 0
    score = 0
    for term in query_terms:
        if not term:
            continue
        if term == name:
            score += 100
        elif term in name.split("_"):
            score += 35
        elif term in name:
            score += 20
        elif term in _TOOL_SEARCH_HINTS.get(name, "").lower():
            score += 12
        elif term in text:
            score += 5
    return score


def initial_gm_tool_names() -> set[str]:
    return set(_INITIAL_GM_TOOL_NAMES)


def build_gm_tools(world: world_mod.World) -> list:
    """Builds the GM tools. Tool definitions are STATIC: they describe tool behavior
    only and never enumerate the current world (no NPC roster, no dynamic id enums),
    so the tool payload stays a stable cache prefix across world/NPC edits. The live
    world (current NPC roster, scene, whereabouts) arrives late in CURRENT TURN
    CONTEXT; backend execution validates ids at call time."""
    ask_npc = {"type": "function", "function": {
        "name": "ask_npc",
        "description": (
            "Ask one present, able-to-hear named NPC for their own speech and visible action. "
            "WHEN TO CALL: the player addresses, questions, threatens, orders, bargains with, "
            "attacks, follows, or otherwise demands a personal reaction from that NPC; or the "
            "NPC must decide/speak/act/show emotion/move for themselves. If the player's latest "
            "message contains a present NPC's name and asks or accuses them, call this before "
            "final narration. DO NOT CALL for absent NPCs, generic "
            "crowd color, visible scene description, or facts the GM can state from CURRENT "
            "SCENE STATE. If the fiction first brings an NPC into the scene, call move_npc "
            "before ask_npc. The result is a draft; if the action is physically impossible, "
            "call ask_npc again with the same npc_id and a correction. Use the npc_id from the "
            "current roster in CURRENT TURN CONTEXT; if the id is unknown the tool returns an "
            "error so you can retry with a valid id."
        ),
        "parameters": {"type": "object", "properties": {
            "npc_id": {"type": "string",
                       "description": "Whom to wake: the npc_id of a present NPC from the current roster."},
            "situation": {"type": "string", "description": _SITUATION_DESC},
            "correction": {"type": "string",
                           "description": "Fill in ONLY when sending a draft back for a redo: "
                                          "what is wrong and what to fix, in Russian. Omit this "
                                          "field on the first ask_npc call for a fresh player "
                                          "action."},
        }, "required": ["npc_id", "situation"], "additionalProperties": False},
    }}
    move_npc = {"type": "function", "function": {
        "name": "move_npc",
        "description": (
            "Update current-scene presence for a named NPC. WHEN TO CALL: a named NPC enters, "
            "leaves, becomes visible/hidden, moves into hearing range, leaves hearing range, "
            "or an accepted NPC draft physically changes their presence. Call before final "
            "narration. DO NOT CALL for anonymous crowds, future plans, rumors, a player "
            "ordering an NPC to move, the player approaching an already-present NPC, or NPC "
            "speech/motives. This tool only changes state; it does not make the NPC speak, "
            "decide, or feel anything. Use the npc_id from the current roster in CURRENT TURN "
            "CONTEXT; an unknown id returns an error instead of changing state."
        ),
        "parameters": {"type": "object", "properties": {
            "npc_id": {"type": "string",
                       "description": "npc_id of the NPC to update, from the current roster."},
            "present": {"type": "boolean",
                        "description": "true if the NPC is now in the current scene; false if not."},
            "location": {"type": "string",
                         "description": "Where the NPC is now, or where they went if absent."},
            "visible": {"type": "boolean",
                        "description": "Whether the NPC is visible in the current scene."},
            "can_hear": {"type": "boolean",
                         "description": "Whether the NPC can hear the current scene."},
            "activity": {"type": "string",
                         "description": "Neutral current activity/position, not dialogue."},
            "attitude": {"type": "string",
                         "description": "Optional short mood/relationship note."},
            "reason": {"type": "string",
                       "description": "Why the scene roster changed, in Russian."},
        }, "required": ["npc_id", "present", "reason"], "additionalProperties": False},
    }}
    set_npc_whereabouts = {"type": "function", "function": {
        "name": "set_npc_whereabouts",
        "description": (
            "Update an absent named NPC's known, likely, rumored, or unknown offscreen "
            "whereabouts without adding them to the current scene. WHEN TO CALL: testimony, "
            "public facts, travel, or scene logic establishes where an absent NPC is, was "
            "last seen, or is likely to be found; or a previous guess is corrected. DO NOT "
            "CALL to make the NPC speak, react, enter, leave the current scene, or become "
            "visible. Use move_npc for current-scene presence and set_scene when the player "
            "actually reaches that place. Use the npc_id from the current roster in CURRENT "
            "TURN CONTEXT; an unknown id returns an error instead of recording whereabouts."
        ),
        "parameters": {"type": "object", "properties": {
            "npc_id": {"type": "string",
                       "description": "npc_id of the absent NPC, from the current roster."},
            "location_id": {"type": "string",
                            "description": "Optional lowercase ascii snake_case id for the offscreen location."},
            "location_name": {"type": "string",
                              "description": "Russian player/world-facing place name, if known."},
            "status": {"type": "string", "enum": ["known", "likely", "rumored", "unknown"],
                       "description": "How certain the whereabouts are."},
            "details": {"type": "string",
                        "description": "Short Russian note: why this is the right place or what is known."},
            "source": {"type": "string",
                       "description": "What established this, in Russian: witness, public lore, scene result, etc."},
        }, "required": ["npc_id", "status"], "additionalProperties": False},
    }}
    set_scene = {"type": "function", "function": {
        "name": "set_scene",
        "description": (
            "Replace CURRENT SCENE STATE when the player actually enters or arrives at a "
            "different room, building, street, site, or area. WHEN TO CALL: before final "
            "narration if you will say the player has arrived in a new current place, uses "
            "a visible exit, reaches a destination, or starts interacting with a different "
            "location. DO NOT CALL for movement inside the same scene, plans to go somewhere, "
            "failed travel, or vague searching without arrival. Include only visible/public "
            "state. If the player wants to enter/go to a reachable place and no obstacle is "
            "established, make the new scene the reached place; do not stop them at the doorway "
            "unless the doorway/blocker matters in play. The title must name the exact current "
            "area, e.g. 'У входа в караульную' if they are still outside. Do not invent hidden "
            "facts or conclusions. List in present_npcs only the npc_ids (from the current "
            "roster in CURRENT TURN CONTEXT) of NPCs actually in the new scene; unknown ids "
            "are ignored and reported back so you can correct them."
        ),
        "parameters": {"type": "object", "properties": {
            "title": {"type": "string",
                      "description": "Russian player-facing title of the new current scene."},
            "description": {"type": "string",
                            "description": "Russian visible description of the new current scene."},
            "location_id": {"type": "string",
                            "description": "Optional lowercase ascii snake_case id for the new location."},
            "present_npcs": {"type": "array",
                             "items": {"type": "string"},
                             "description": "Known named NPC ids visibly present in the new scene."},
            "items": {"type": "array", "items": {"type": "object", "properties": {
                "id": {"type": "string"},
                "name": {"type": "string"},
                "location": {"type": "string"},
                "visible": {"type": "boolean"},
                "portable": {"type": "boolean"},
                "owner": {"type": "string"},
                "details": {"type": "string"},
            }, "required": ["name"], "additionalProperties": False}},
            "exits": {"type": "array", "items": {"type": "object", "properties": {
                "id": {"type": "string"},
                "name": {"type": "string"},
                "destination": {"type": "string"},
                "visible": {"type": "boolean"},
                "blocked_by": {"type": "string"},
            }, "required": ["name"], "additionalProperties": False}},
            "constraints": {"type": "array", "items": {"type": "string"}},
            "tension": {"type": "string"},
            "reason": {"type": "string",
                       "description": "Why the current scene changed, in Russian."},
        }, "required": ["title", "description", "reason"], "additionalProperties": False},
    }}
    update_world_state = {"type": "function", "function": {
        "name": "update_world_state",
        "description": (
            "Apply a compact batch of GM-authored world-state updates after the fiction "
            "establishes them. Accepts items[] for fact, rumor, npc_memory, relationship, "
            "and goal records. One item is one atomic durable note; batch 1-5 important "
            "changes instead of making repeated tool calls. Use op=add to create, op=update "
            "to revise an existing id, and op=delete to remove an id from active memory/RAG. "
            "For update/delete, include expected_hash when you have a fresh hash from "
            "query_world_state or a just-returned update_world_state result. If you do not "
            "have a fresh id/hash and an active record may already exist for the same "
            "npc_id and target, call query_world_state first; then update/delete that id "
            "instead of adding a duplicate. Use add only when lookup is unknown or the note "
            "is genuinely new. "
            f"{tool_guidance.WORLD_STATE_TYPE_GUIDE} "
            f"{tool_guidance.WORLD_STATE_SCOPE_GUIDE} "
            f"{tool_guidance.WORLD_STATE_SPLIT_GUIDE} "
            f"{tool_guidance.WORLD_STATE_EXAMPLE_GUIDE} "
            "Keep text short and in Russian. Omit optional fields when empty; do not send "
            "empty strings, empty arrays, or nulls for optional fields. Private NPC "
            "testimony, clues, promises, or leads told only to the player must use shared, "
            "not public. Every shared item must include both "
            "npc_id and target or it will be rejected. "
            "Do not use for visible scene movement, current-scene presence, or NPC speech; "
            "use set_scene, move_npc, or ask_npc for those."
        ),
        "parameters": {"type": "object", "properties": {
            "items": {"type": "array", "items": {"type": "object", "properties": {
                "op": {"type": "string", "enum": ["add", "update", "delete"],
                       "description": "Operation. Omit for add."},
                "id": {"type": "string",
                       "description": "Existing record id for update/delete, usually from query_world_state or a just-returned update result. Omit for add."},
                "expected_hash": {"type": "string",
                                  "description": "Optional concurrency precondition for update/delete: pass the record hash from query_world_state or a just-returned update result. If it mismatches, the change is not applied."},
                "type": {"type": "string",
                         "enum": ["fact", "rumor", "npc_memory", "relationship", "goal"],
                         "description": (
                             "What namespace this item updates. Required for add. fact is "
                             "objective established truth/visible stable state; rumor is "
                             "unverified testimony/claim/suspicion/lead; npc_memory is what "
                             "one NPC remembers, saw, was told, promised, hid, or learned; "
                             "relationship is ongoing attitude/trust/debt/leverage/fear/"
                             "loyalty/hatred/love/suspicion toward target; goal is current "
                             "want/plan/intent. Do not store NPC testimony as fact just "
                             "because someone said it."
                         )},
                "text": {"type": "string",
                         "description": (
                             "Compact Russian durable meaning, not a transcript quote. "
                             "Required for add/update unless deleting. For relationship, keep "
                             "the full multi-layer attitude in one string and update that "
                             "record as it changes."
                         )},
                "npc_id": {"type": "string",
                           "description": "NPC id that owns/knows this npc_memory, relationship, or goal; for rumor, the speaker if known. Required for shared scope. For private NPC-player exchange use npc_id=<speaker>. Omit when empty."},
                "target": {"type": "string",
                           "description": "Relationship/shared target such as player, an npc_id, faction, or place. Required for relationship and shared scope. For private NPC-player exchange use target=player."},
                "scope": {"type": "string",
                          "enum": ["public", "gm", "npc", "shared"],
                          "description": "Who may know this state. public is not private player knowledge; shared means only npc_id and target know; npc means only npc_id knows/thinks/remembers; gm means hidden author truth. Use shared for a private NPC-player exchange. shared requires npc_id and target. Omit to use the type default."},
                "witnesses": {"type": "array", "items": {"type": "string"},
                               "description": "For public rumors only: ids who heard it, plus player if relevant. Omit when empty."},
                "mode": {"type": "string", "enum": ["replace"],
                         "description": "Only for goal when replacing existing active goals. Omit for normal add/update and for all non-goal items."},
            }, "required": [], "additionalProperties": False},
                "description": "Compact updates. Omit optional item fields when empty."},
        }, "required": ["items"], "additionalProperties": False},
    }}
    query_world_state = {"type": "function", "function": {
        "name": "query_world_state",
        "description": (
            "Scoped world-state lookup. Use before update_world_state update/delete, and "
            "before adding a relationship, goal, or npc_memory that may already exist. Results "
            "include record ids and hashes for update/delete expected_hash. Use player scope for player-known safe memory "
            "(public plus private notes shared with player); "
            "player scope must never reveal GM truth, hidden events, NPC secrets, private NPC "
            "memory, or private goals. Use npc scope with npc_id for what that NPC may know: "
            "public memory plus that NPC's own private card/memory only. Use gm scope for "
            "author-only truth, hidden events, all NPC private notes, and public memory. "
            "Results are compact and include only matching scoped state."
        ),
        "parameters": {"type": "object", "properties": {
            "scope": {"type": "string", "enum": ["player", "gm", "npc"],
                      "description": "Visibility namespace to query."},
            "query": {"type": "string",
                      "description": "What to look up, in Russian or English. Include kind and parties when useful, e.g. 'relationship borin player' or 'goal lysa'. Keep proper nouns exact."},
            "npc_id": {"type": "string",
                       "description": "Required for npc scope. Omit for player or gm scope."},
            "max_results": {"type": "integer",
                            "description": "Maximum matching rows to return. Omit for default 5."},
        }, "required": ["scope", "query"], "additionalProperties": False},
    }}
    return [
        ask_npc,
        move_npc,
        set_npc_whereabouts,
        set_scene,
        update_world_state,
        query_world_state,
        _ROLL_DICE_TOOL,
        _GET_FACT_TOOL,
        _TOOL_SEARCH_TOOL,
    ]


def gm_tool_catalog(world: world_mod.World) -> dict[str, dict]:
    """Executable GM tool registry keyed by tool name."""
    return {_tool_name(tool): tool for tool in build_gm_tools(world)}


def build_gm_tools_for_model(world: world_mod.World, loaded_tool_names: set[str] | None = None) -> list:
    """Return only tools visible to the model right now.

    If loaded_tool_names is None, preserve the legacy behavior and expose every tool.
    Otherwise expose tool_search plus the previously discovered tools.
    """
    catalog = gm_tool_catalog(world)
    if loaded_tool_names is None:
        return list(catalog.values())
    visible = set(loaded_tool_names) | _INITIAL_GM_TOOL_NAMES
    return [tool for name, tool in catalog.items() if name in visible]


def search_gm_tools(
    world: world_mod.World,
    query: str,
    max_results: int = 5,
    already_loaded: set[str] | None = None,
) -> dict:
    catalog = gm_tool_catalog(world)
    already_loaded = set(already_loaded or set())
    searchable = {
        name: tool
        for name, tool in catalog.items()
        if name != "tool_search" and name not in already_loaded
    }
    raw_query = (query or "").strip()
    if not raw_query:
        return {
            "query": raw_query,
            "matches": [],
            "loaded_tools": [],
            "already_loaded": [],
            "total_searchable_tools": len(searchable),
            "message": "Запрос пустой. Используй keywords или select:tool_name.",
        }
    try:
        limit = max(1, min(int(max_results or 5), 10))
    except (TypeError, ValueError):
        limit = 5

    selected: list[str] = []
    missing: list[str] = []
    known_loaded: list[str] = []
    if raw_query.lower().startswith("select:"):
        requested = [
            item.strip()
            for item in raw_query.split(":", 1)[1].split(",")
            if item.strip()
        ]
        all_tool_names = {name.lower(): name for name in catalog if name != "tool_search"}
        for item in requested:
            name = all_tool_names.get(item.lower())
            if not name:
                missing.append(item)
            elif name in already_loaded:
                known_loaded.append(name)
            else:
                selected.append(name)
    else:
        terms = re.findall(r"[\wа-яА-ЯёЁ-]+", raw_query.lower())
        required = [term[1:] for term in terms if term.startswith("+") and len(term) > 1]
        scoring_terms = required + [term for term in terms if not term.startswith("+")]
        scored = []
        for name, tool in searchable.items():
            score = _score_tool(scoring_terms, required, tool)
            if score > 0:
                scored.append((score, name))
        selected = [name for _score, name in sorted(scored, reverse=True)[:limit]]

    matches = []
    for name in selected[:limit]:
        tool = searchable[name]
        matches.append({
            "name": name,
            "description": _short_tool_description(tool),
            "loaded": True,
            "already_loaded": name in already_loaded,
        })
    already_loaded_result = sorted(set(known_loaded))
    if matches:
        message = (
            "Найденные инструменты будут доступны в следующем шаге ГМ. "
            "Вызови нужный инструмент после этого результата."
        )
    elif already_loaded_result:
        message = "Запрошенные инструменты уже доступны в текущем шаге ГМ."
    else:
        message = "Подходящих инструментов не найдено. Попробуй select:tool_name или другие ключевые слова."

    return {
        "query": raw_query,
        "matches": matches,
        "loaded_tools": [row["name"] for row in matches],
        "already_loaded": already_loaded_result,
        "missing": missing,
        "total_searchable_tools": len(searchable),
        "message": message,
    }

# JSON-схема NPC — только для надёжного фолбэка в chat_json (грамматика без думалки).
NPC_SCHEMA = {
    "type": "object",
    "properties": {
        "reasoning": {"type": "string"},
        "speech": {"type": "string"},
        "action": {"type": "string"},
        "claims": {"type": "array", "items": {"type": "string"}},
    },
    "required": ["reasoning", "speech", "action", "claims"],
}

SCENE_DELTA_SCHEMA = {
    "type": "object",
    "properties": {
        "moves": {"type": "array", "items": {"type": "object", "properties": {
            "npc_id": {"type": "string"},
            "present": {"type": "boolean"},
            "location": {"type": "string"},
            "visible": {"type": "boolean"},
            "can_hear": {"type": "boolean"},
            "activity": {"type": "string"},
            "attitude": {"type": "string"},
            "reason": {"type": "string"},
        }, "required": ["npc_id", "present", "reason"]}},
    },
    "required": ["moves"],
}

WORLD_SEED_SCHEMA = {
    "type": "object",
    "properties": {
        "public_intro": {"type": "string"},
        "hidden_truth": {"type": "string"},
        "proper_nouns": {"type": "array", "items": {"type": "string"}},
        "public_facts": {"type": "array", "items": {"type": "string"}},
        "npcs": {"type": "array", "items": {"type": "object", "properties": {
            "id": {"type": "string"},
            "name": {"type": "string"},
            "role": {"type": "string"},
            "gender": {
                "type": "string",
                "description": "Russian grammatical gender marker: M, F, N, PL, OTHER, or a short custom Russian note.",
            },
            "persona": {"type": "string"},
            "voice": {"type": "string"},
            "goals": {"type": "string"},
            "knowledge": {"type": "string"},
            "secret": {"type": "string"},
        }, "required": ["id", "name", "role", "persona", "voice", "goals", "knowledge", "secret"]}},
        "scene": {"type": "object", "properties": {
            "id": {"type": "string"},
            "location_id": {"type": "string"},
            "title": {"type": "string"},
            "description": {"type": "string"},
            "present_npcs": {"type": "array", "items": {"type": "string"}},
            "items": {"type": "array", "items": {"type": "object", "properties": {
                "id": {"type": "string"},
                "name": {"type": "string"},
                "location": {"type": "string"},
                "visible": {"type": "boolean"},
                "portable": {"type": "boolean"},
                "owner": {"type": "string"},
                "details": {"type": "string"},
            }, "required": ["id", "name", "location", "visible", "portable"]}},
            "exits": {"type": "array", "items": {"type": "object", "properties": {
                "id": {"type": "string"},
                "name": {"type": "string"},
                "destination": {"type": "string"},
                "visible": {"type": "boolean"},
                "blocked_by": {"type": "string"},
            }, "required": ["id", "name", "destination", "visible"]}},
            "constraints": {"type": "array", "items": {"type": "string"}},
            "tension": {"type": "string"},
        }, "required": ["id", "location_id", "title", "description", "present_npcs",
                         "items", "exits", "constraints", "tension"]},
    },
    "required": ["public_intro", "hidden_truth", "proper_nouns", "public_facts", "npcs", "scene"],
}


def _seed_present_ids(seed: dict) -> list[str]:
    if not isinstance(seed, dict):
        return []
    scene = seed.get("scene") if isinstance(seed.get("scene"), dict) else {}
    raw = scene.get("present_npcs") or seed.get("present_npcs") or []
    if not isinstance(raw, list):
        return []
    return [_text(item) for item in raw if _text(item)]


def _seed_named_npcs(seed: dict) -> set[str]:
    if not isinstance(seed, dict):
        return set()
    named: set[str] = set()
    npcs = seed.get("npcs")
    if isinstance(npcs, list):
        for raw in npcs:
            if isinstance(raw, dict) and _text(raw.get("id")) and _text(raw.get("name")):
                named.add(_text(raw.get("id")))
    elif isinstance(npcs, dict):
        for npc_id, raw in npcs.items():
            if isinstance(raw, dict) and _text(raw.get("name")):
                named.add(_text(npc_id))
    details = seed.get("npc_details")
    if isinstance(details, dict):
        for npc_id, raw in details.items():
            if isinstance(raw, dict) and _text(raw.get("name")):
                named.add(_text(npc_id))
    return named


def _seed_needs_npc_repair(seed: dict) -> bool:
    present = set(_seed_present_ids(seed))
    if not present:
        return True
    return not present.issubset(_seed_named_npcs(seed))


def _has_cyrillic(text: str) -> bool:
    return bool(re.search(r"[А-Яа-яЁё]", text or ""))


def _seed_player_facing_text(seed: dict) -> str:
    if not isinstance(seed, dict):
        return ""
    scene = seed.get("scene") if isinstance(seed.get("scene"), dict) else {}
    parts = [
        seed.get("public_intro"), seed.get("location_name"), seed.get("description"),
        scene.get("title"), scene.get("name"), scene.get("description"),
    ]
    for key in ("public_facts",):
        for item in _as_list(seed.get(key)) + _as_list(scene.get(key)):
            parts.append(item)
    for key in ("visible_objects", "objects", "items", "visible_exits", "exits"):
        for item in _as_list(seed.get(key)) + _as_list(scene.get(key)):
            if isinstance(item, dict):
                parts.extend([item.get("name"), item.get("display_name"), item.get("description")])
            else:
                parts.append(item)
    return " ".join(_text(part) for part in parts if _text(part))


def _seed_needs_text_repair(seed: dict, brief: str) -> bool:
    return _has_cyrillic(brief) and not _has_cyrillic(_seed_player_facing_text(seed))


_CYR_TO_LAT = {
    "а": "a", "б": "b", "в": "v", "г": "g", "д": "d", "е": "e", "ё": "e",
    "ж": "zh", "з": "z", "и": "i", "й": "y", "к": "k", "л": "l", "м": "m",
    "н": "n", "о": "o", "п": "p", "р": "r", "с": "s", "т": "t", "у": "u",
    "ф": "f", "х": "h", "ц": "ts", "ч": "ch", "ш": "sh", "щ": "sch",
    "ъ": "", "ы": "y", "ь": "", "э": "e", "ю": "yu", "я": "ya",
}


def _brief_name_slug(name: str) -> str:
    raw = "".join(_CYR_TO_LAT.get(ch, ch) for ch in name.lower())
    return re.sub(r"[^a-z0-9_]+", "_", raw).strip("_")


def _apply_brief_display_names(seed: dict, brief: str) -> dict:
    if not isinstance(seed, dict):
        return seed
    candidates = re.findall(r"\b[А-ЯЁ][а-яё]{1,24}\b", brief or "")
    by_slug = {_brief_name_slug(name): name for name in candidates}

    def apply(raw: dict, npc_id: str):
        slug = _brief_name_slug(npc_id)
        wanted = by_slug.get(slug)
        if wanted and isinstance(raw, dict):
            raw["name"] = wanted

    npcs = seed.get("npcs")
    if isinstance(npcs, list):
        for raw in npcs:
            if isinstance(raw, dict):
                apply(raw, _text(raw.get("id")))
    elif isinstance(npcs, dict):
        for npc_id, raw in npcs.items():
            apply(raw, _text(npc_id))
    details = seed.get("npc_details")
    if isinstance(details, dict):
        for npc_id, raw in details.items():
            apply(raw, _text(npc_id))
    return seed


def _gm_system(world: world_mod.World | None = None, summary: str = "") -> str:
    """Static GM instructions.

    Keep this stable across turns. Prompt/KV caches only hit identical prefixes; mutable
    scene snapshots belong in append-only turn messages, not here.
    """
    return prompts.GM_SYSTEM


def _gm_world_setup(world: world_mod.World) -> str:
    """Stable public world premise placed near the front of the prompt for cache reuse.

    Only the public intro lives here: it changes solely on full world recreation
    (/new, snapshot load), so it stays in the early cacheable prefix alongside the GM
    rules. The mutable named-NPC roster and current public facts moved to
    ``_gm_turn_context`` so per-turn edits (rename/add/remove NPC, /debug/fact) only
    invalidate the late turn tail instead of the whole prefix.
    """
    parts = [
        "WORLD SETUP (stable public premise; cacheable):",
        "PUBLIC INTRO:\n" + world.public,
    ]
    return "\n\n".join(parts)


def _gm_turn_context(world: world_mod.World, player_text: str) -> str:
    """Latest mutable state plus the player's free-text action.

    Appended as the new user turn. Old turns stay byte-for-byte unchanged so prefix
    caches reuse the long history. The named-NPC roster and current public facts live
    here (not in the early _gm_world_setup) because they change between turns; keeping
    them in the late tail means an NPC rename/add/remove or a public-fact edit only
    recomputes this turn, not the cached prefix. Both sections are built from live world
    state (no hardcoded names) and stay public-only: the roster exposes just
    id/name/role/род via _public_gender, and facts are filtered to kind == "public"
    (truth/rumor records and hidden_events are never included).
    """
    roster = "\n".join(
        f"- {npc.npc_id}: {npc.name}, {npc.role}"
        + (f", род: {world_mod._public_gender(npc.pronouns)}" if npc.pronouns else "")
        for npc in world.npcs.values()
    )
    public_facts = [
        record.text for record in getattr(world, "fact_records", [])
        if getattr(record, "kind", "") == "public"
    ]
    system = "CURRENT TURN CONTEXT (latest engine state snapshot):\n"
    system += "\nCURRENT NAMED NPC ROSTER:\n" + (roster or "(none)")
    if public_facts:
        system += "\n\nCURRENT PUBLIC FACTS:\n" + "\n".join(
            f"- {fact}" for fact in public_facts[:12]
        )
    system += "\n\nCURRENT SCENE STATE:\n" + world.scene_context()
    system += "\n\nENTITY REFERENCE MARKUP:\n" + world.entity_reference_context()
    if world.constraints:
        system += "\n\nSCENE CONSTRAINTS (must enforce when reviewing NPC drafts):\n"
        system += "\n".join(f"- {c}" for c in world.constraints)
    system += "\n\nPLAYER ACTION (latest user input, free roleplay text):\n"
    system += player_text.strip()
    return system


def gm_user_message(world: world_mod.World, player_text: str) -> dict:
    return {"role": "user", "content": _gm_turn_context(world, player_text)}


def _gm_request_messages(world: world_mod.World, gm_messages: list, summary: str = "") -> list:
    messages = [
        {"role": "system", "content": _gm_system(world, summary)},
        {"role": "system", "content": _gm_world_setup(world)},
    ]
    if summary:
        messages.append({"role": "system", "content": "STORY SO FAR (compact): " + summary})
    messages.extend(gm_messages)
    return messages


def gm_turn(client, world: world_mod.World, gm_messages: list, summary: str = "",
            loaded_tool_names: set[str] | None = None):
    """Ход ГМ. Возвращает (thinking, content, calls, assistant_msg)."""
    messages = _gm_request_messages(world, gm_messages, summary)
    return client.chat(
        messages,
        tools=build_gm_tools_for_model(world, loaded_tool_names),
        think=True,
        reasoning_role=config.ROLE_GM,
    )


def gm_turn_stream(client, world: world_mod.World, gm_messages: list, summary: str = "",
                   loaded_tool_names: set[str] | None = None):
    """Стримящий ход ГМ. Возвращает генератор client.chat_stream
    (yield ('thinking'|'content', delta); return (thinking, content, calls, assistant_msg, stats))."""
    messages = _gm_request_messages(world, gm_messages, summary)
    return client.chat_stream(
        messages,
        tools=build_gm_tools_for_model(world, loaded_tool_names),
        think=True,
        reasoning_role=config.ROLE_GM,
    )


def gm_prelude_stream(client, world: world_mod.World, player_text: str, calls: list):
    """Player-facing setup shown before visible tool resolution."""
    call_brief = []
    for call in calls or []:
        if not isinstance(call, dict):
            continue
        args = call.get("arguments") if isinstance(call.get("arguments"), dict) else {}
        call_brief.append({
            "name": call.get("name", ""),
            "arguments": args,
        })
    system = """\
You are the Game Master writing visible scene narration BEFORE a pending tool resolution
in a tabletop D&D 5e roleplay scene.

Write in Russian only. Use the length the moment deserves: usually one vivid paragraph,
or two compact paragraphs when there is public attention, travel, threat, searching,
social pressure, or a tense pause.
Address the player character as "ты"; do not call them "игрок" in the visible text.
Describe only what is already visible or directly declared by the player: where they
stand, who they address, how loudly/quietly they speak, what the room can notice, and
what sensory details and unresolved tension matter.
Do not resolve the action. Do not make NPCs answer, obey, refuse, enter, leave, reveal
facts, or react personally. Do not mention tools, JSON, checks, prompts, or internal
mechanics. Keep proper nouns exactly as written.
When important people or places are mentioned and the id is listed in ENTITY REFERENCE
MARKUP, use refs such as [[npc:borin|Борин]] or [[loc:grey_griffon|Трактир]].
"""
    user = (
        "CURRENT SCENE STATE:\n"
        f"{world.scene_context()}\n\n"
        "ENTITY REFERENCE MARKUP:\n"
        f"{world.entity_reference_context()}\n\n"
        "PLAYER ACTION:\n"
        f"{player_text.strip()}\n\n"
        "PENDING RESOLUTION CONTEXT (do not mention this as mechanics):\n"
        f"{json.dumps(call_brief, ensure_ascii=False)[:config.PRELUDE_CALLBRIEF_CHARS]}\n\n"
        "Write the pre-tool narration now."
    )
    return client.chat_stream(
        [{"role": "system", "content": system}, {"role": "user", "content": user}],
        tools=None,
        think=False,
        reasoning_role=config.ROLE_GM,
    )


def npc_system_message(npc: world_mod.NPC | None = None) -> dict:
    # Fully static now: the concrete character is delivered late via npc_card_block().
    # `npc` is accepted but ignored for call-site compatibility.
    return {"role": "system", "content": prompts.NPC_SYSTEM_STATIC}


def npc_card_block(npc: world_mod.NPC) -> str:
    """Render the late CURRENT NPC CARD block (overrides older memory on conflict)."""
    return prompts.NPC_CARD_TEMPLATE.format(
        revision=int(getattr(npc, "card_revision", 0) or 0),
        name=npc.name,
        role=npc.role or "(не указана)",
        gender=npc.pronouns or "OTHER",
        persona=npc.persona,
        voice=npc.voice,
        goals=npc.goals,
        knowledge=npc.knowledge,
        secret=npc.secret,
    )


def npc_user_message(npc: world_mod.NPC, situation: str, observations: str,
                     commitments: str, feedback: str | None, constraints=None,
                     scene_slice: str = "") -> dict:
    # CURRENT SITUATION (GM brief — what's now) -> own memory -> what was seen earlier.
    parts = [f"CURRENT SITUATION (what's happening now, what you react to): {situation}"]
    if scene_slice:
        parts.append("YOUR CURRENT SCENE SLICE (what is actually around you):\n"
                     + scene_slice)
    if constraints:
        parts.append("VISIBLE SCENE LIMITS (physical facts you must obey):\n"
                     + _constraints_text(constraints))
    parts.append("YOUR MEMORY (what you've already said/done — stay consistent):\n"
                 + (commitments or "(nothing yet)"))
    parts.append("WHAT YOU SAW/HEARD EARLIER:\n"
                 + (observations or "(nothing)"))
    user = "\n\n".join(parts)
    if feedback:
        user += (f"\n\nGM NOTE — your previous action did not pass: {feedback}\n"
                 f"REDO: give a new reaction that takes the note into account.")
    return {"role": "user", "content": user}


def npc_request_messages(npc: world_mod.NPC, history: list | None, summary: str,
                         user_message: dict) -> list:
    messages = [npc_system_message()]
    if summary:
        messages.append({
            "role": "system",
            "content": "YOUR PRIVATE MEMORY SO FAR (compact):\n" + summary,
        })
    messages.extend(history or [])
    # Late dynamic block: the CURRENT NPC CARD leads the final user turn, placed AFTER
    # summary + history so a card edit only invalidates this tail. The card is prepended
    # to a COPY so the recorded history message (user_message) stays card-free.
    final_turn = dict(user_message)
    final_turn["content"] = npc_card_block(npc) + "\n\n" + final_turn.get("content", "")
    messages.append(final_turn)
    return messages


def _npc_messages(npc: world_mod.NPC, situation: str, observations: str,
                  commitments: str, feedback: str | None, constraints=None,
                  scene_slice: str = "", history: list | None = None,
                  summary: str = "") -> list:
    user_message = npc_user_message(
        npc, situation, observations, commitments, feedback, constraints, scene_slice)
    return npc_request_messages(npc, history, summary, user_message)


def _text(value) -> str:
    if value is None:
        return ""
    if isinstance(value, str):
        return value.strip()
    return str(value).strip()


def _as_list(value) -> list:
    if value is None:
        return []
    if isinstance(value, list):
        return value
    if isinstance(value, tuple):
        return list(value)
    return [value]


def _claims(value) -> list[str]:
    if not isinstance(value, list):
        return []
    return [claim for claim in (_text(item) for item in value) if claim]


def _norm_npc(out: dict) -> dict:
    if not isinstance(out, dict):
        out = {}
    return {
        "reasoning": _text(out.get("reasoning")),
        "speech": _text(out.get("speech")),
        "action": _text(out.get("action")),
        "claims": _claims(out.get("claims")),
    }


def npc_turn(client, npc, situation, observations="", commitments="", feedback=None,
             constraints=None, scene_slice="", history=None, summary="") -> dict:
    """Реакция NPC-субагента (нестримящая). NPC реагирует на СИТУАЦИЮ (брифинг ГМ) +
    свою память; эмоции/мотивы — его собственные."""
    msgs = _npc_messages(npc, situation, observations, commitments, feedback, constraints,
                         scene_slice, history, summary)
    return _norm_npc(
        client.chat_json(
            msgs,
            NPC_SCHEMA,
            think=True,
            reasoning_role=config.ROLE_NPC,
        )
    )


def npc_turn_stream(client, npc, situation, observations="", commitments="", feedback=None,
                    constraints=None, scene_slice="", history=None, summary=""):
    """Стримящая реакция NPC. yield ('content', delta); return (normalized dict, stats)."""
    msgs = _npc_messages(npc, situation, observations, commitments, feedback, constraints,
                         scene_slice, history, summary)
    data, stats = yield from client.chat_json_stream(
        msgs,
        NPC_SCHEMA,
        think=True,
        reasoning_role=config.ROLE_NPC,
    )
    return _norm_npc(data), stats


def build_world_seed(client, brief: str) -> dict:
    """Ask the local model for a new playable world seed; World validates it afterwards."""
    system = (
        "Create a compact tabletop RP starting scene from the user's brief. Return JSON only. "
        "This is not prose for the player; it is a seed that code will validate. Keep it small: "
        "2-4 NPCs, 2-5 visible objects, 1-3 visible exits, 3-6 public facts. "
        "NPC ids must be lowercase ascii snake_case. Put only NPC ids in scene.present_npcs. "
        "Every present NPC must also have a full object in `npcs` with id, exact display name, "
        "role, gender marker if known, persona, voice, goals, knowledge, and secret. Use `gender` "
        "as M, F, N, PL, OTHER, or a short custom Russian note: M=он/masculine, F=она/feminine, "
        "N=оно/neuter, PL=они/plural. If the "
        "user gives NPC names, preserve those names exactly in `name`; never return only ids "
        "like iva/run without display names. "
        "All player-facing seed text must be in Russian: public_intro, scene title, scene "
        "description, item names, exit names, public facts, NPC display names, NPC roles, "
        "NPC persona/voice/goals summaries, gender custom notes, and scene positions/activities. Preserve "
        "Russian proper nouns from the brief exactly; do not translate them. "
        "The scene must contain enough concrete state to start play: where the player is, "
        "who is present, what is visible, what exits exist, and what physical limits matter. "
        "Do not create action ids or intent ids; characters will act in free text."
    )
    messages = [
        {"role": "system", "content": system},
        {"role": "user", "content": brief.strip() or "Create a small mystery scene."},
    ]
    seed = _apply_brief_display_names(
        client.chat_json(messages, WORLD_SEED_SCHEMA, think=False), brief)
    if not _seed_needs_npc_repair(seed) and not _seed_needs_text_repair(seed, brief):
        return seed
    repair_system = (
        "Repair this tabletop RP world seed into the required strict JSON shape. Return JSON "
        "only. Keep the same scene idea, visible objects, exits, and public facts. Create a "
        "`npcs` array with one full NPC object for every id in scene.present_npcs or "
        "present_npcs. Preserve exact user-provided display names from the brief, especially "
        "Cyrillic names. NPC ids remain lowercase ascii snake_case; NPC `name` is the display "
        "name shown to the player. All player-facing strings must be in Russian: scene title, "
        "scene description, item names, exit names, public facts, NPC display names, NPC roles, "
        "NPC persona/voice/goals summaries, gender custom notes, and scene positions/activities. "
        "Use `gender` as M, F, N, PL, OTHER, or a short custom Russian note. Keep "
        "proper nouns from the brief exactly, for example do not translate Russian names of "
        "places, ships, people, factions, or objects. Do not add action ids or intent ids."
    )
    repair_messages = [
        {"role": "system", "content": repair_system},
        {"role": "user", "content": "USER BRIEF:\n" + (brief.strip() or "Create a small mystery scene.")
         + "\n\nBROKEN SEED:\n" + json.dumps(seed, ensure_ascii=False)},
    ]
    repaired = _apply_brief_display_names(
        client.chat_json(repair_messages, WORLD_SEED_SCHEMA, think=False), brief)
    return repaired if isinstance(repaired, dict) and repaired else seed


def extract_scene_delta(client, world: world_mod.World, narration: str) -> dict:
    """Extract explicit roster changes from accepted final narration.

    This is state sync, not validation: the text is not rejected or rewritten.
    """
    roster = "\n".join(
        f"- {npc.npc_id}: {npc.name}, {npc.role}"
        + (f", род: {world_mod._public_gender(npc.pronouns)}" if npc.pronouns else "")
        + ("; present" if npc.npc_id in world.scene.present_npcs else "; absent")
        for npc in world.npcs.values()
    )
    system = (
        "Extract only explicit current-scene NPC roster changes from the GM narration. "
        "Use only npc_id values from the roster. Return JSON only. "
        "A move with present=true means the NPC explicitly entered/arrived/is now in the "
        "current scene or can hear it. A move with present=false means the NPC explicitly "
        "left/exited/went to another room/is no longer able to hear. "
        "Track the roster at the END of the narration for the CURRENT SCENE only. If the "
        "narration moves the player and an NPC outside the current scene, do not add that "
        "NPC to the old current scene. "
        "Do NOT infer from wishes, requests, plans, future possibilities, searches, rumors, "
        "or someone being mentioned as absent. If there is no explicit roster change, "
        "return {\"moves\":[]}."
    )
    messages = [
        {"role": "system", "content": system},
        {"role": "user", "content": "ROSTER:\n" + roster
         + "\n\nCURRENT SCENE:\n" + world.scene_context()
         + "\n\nGM NARRATION:\n" + narration},
    ]
    return client.chat_json(messages, SCENE_DELTA_SCHEMA, think=False)
