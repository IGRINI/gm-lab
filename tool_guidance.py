"""Shared model-facing tool capability guidance."""
from __future__ import annotations


VISIBLE_TOOL_CAPABILITIES: tuple[tuple[str, str], ...] = (
    ("ask_npc", "NPC speech/reactions"),
    ("roll_dice", "dice and D&D-style uncertain outcomes"),
    ("get_world_fact", "public/player-safe world fact lookup"),
    ("query_world_state", "scoped world/NPC memory lookup before memory writes"),
    ("update_world_state", "durable world/NPC memory writes"),
)

HIDDEN_TOOL_CAPABILITIES: tuple[tuple[str, str], ...] = (
    ("set_scene", "scene changes"),
    ("move_npc", "NPC presence/movement"),
    ("set_npc_whereabouts", "offscreen NPC whereabouts"),
)


def _capability_list(rows: tuple[tuple[str, str], ...]) -> str:
    return ", ".join(f"{label} (`{name}`)" for name, label in rows)


VISIBLE_TOOL_CAPABILITY_TEXT = _capability_list(VISIBLE_TOOL_CAPABILITIES)
HIDDEN_TOOL_CAPABILITY_TEXT = _capability_list(HIDDEN_TOOL_CAPABILITIES)

GM_TOOL_CAPABILITY_OVERVIEW = (
    f"Visible GM tool capabilities: {VISIBLE_TOOL_CAPABILITY_TEXT}. "
    f"Hidden GM capabilities can be loaded with `tool_search`: "
    f"{HIDDEN_TOOL_CAPABILITY_TEXT}."
)

WORLD_STATE_TYPE_GUIDE = (
    "Choose world-state type by meaning: fact = objective established truth or visible "
    "stable state; rumor = unverified claim, testimony, suspicion, accusation, or lead; "
    "npc_memory = a specific event or knowledge one NPC remembers, saw, was told, "
    "promised, hid, learned, or should later act on; relationship = ongoing attitude, "
    "trust, debt, leverage, fear, loyalty, hatred, love, suspicion, or obligation toward "
    "a target; goal = current want, plan, intent, agenda, or task."
)

WORLD_STATE_SCOPE_GUIDE = (
    "Choose scope by who may know it: public = anyone can know; shared = only npc_id "
    "and target know; npc = only npc_id knows/thinks/remembers; gm = hidden author truth "
    "not known by characters unless discovered."
)

WORLD_STATE_SPLIT_GUIDE = (
    "Do not store testimony as fact just because someone said it. If an NPC privately "
    "claims something to the player, store the claim as rumor with scope=shared and store "
    "that the NPC told/asked/was threatened as npc_memory with scope=shared or npc. If "
    "trust, debt, threat, leverage, affection, hatred, loyalty, fear, suspicion, a bargain, "
    "or blackmail changes, write or update relationship; if a plan or intent changes, "
    "write or update goal."
)

WORLD_STATE_EXAMPLE_GUIDE = (
    "Examples: private witness claim -> rumor shared npc_id=<speaker> target=player; "
    "the witness remembers telling the player -> npc_memory shared npc_id=<speaker> "
    "target=player; private resentment after a threat -> relationship npc npc_id=<npc> "
    "target=player; secret plan to mislead the player -> goal npc npc_id=<npc>."
)

TOOL_SEARCH_DESCRIPTION = (
    "Search and load hidden GM tools named in the system tool capability map. Use "
    "this when a needed hidden scene, NPC movement, or whereabouts capability is "
    "not visible. Query with keywords or exact selection such as "
    "select:tool_name or select:tool_a,tool_b; matching tools become available on "
    "the next GM step."
)
