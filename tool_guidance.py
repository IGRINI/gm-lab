"""Shared model-facing tool capability guidance."""
from __future__ import annotations


VISIBLE_TOOL_CAPABILITIES: tuple[tuple[str, str], ...] = (
    ("ask_npc", "NPC speech/reactions"),
    ("roll_dice", "dice and D&D-style uncertain outcomes"),
    ("get_world_fact", "player-safe answer lookup for facts, lore, leads, and testimony"),
    ("query_world_state", "state-record/id-hash lookup for updates or private npc/gm scopes"),
    ("update_world_state", "durable world/NPC memory writes"),
    ("update_player_character", "player character sheet updates"),
    ("advance_time", "world clock advancement"),
)

HIDDEN_TOOL_CAPABILITIES: tuple[tuple[str, str], ...] = (
    ("set_scene", "scene changes"),
    ("move_npc", "NPC presence/movement"),
    ("set_npc_whereabouts", "offscreen NPC whereabouts"),
    ("get_npc_profile", "selected NPC card/mechanics fields"),
)


def _capability_list(rows: tuple[tuple[str, str], ...]) -> str:
    return ", ".join(f"{label} (`{name}`)" for name, label in rows)


VISIBLE_TOOL_CAPABILITY_TEXT = _capability_list(VISIBLE_TOOL_CAPABILITIES)
HIDDEN_TOOL_CAPABILITY_TEXT = _capability_list(HIDDEN_TOOL_CAPABILITIES)

MODEL_TOOL_RESULT_GUIDE = (
    "GM tool results are compact structured text. They usually omit arguments "
    "you already sent and include only new information: totals, found text, changed state, "
    "ids/hashes, status/error lines, and optional <system-reminder> blocks. "
    "For get_world_fact/query_world_state, already_delivered means matching sources/rows "
    "were already returned inside the active, not-yet-compacted GM context."
)

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
    "plus target and/or participants know; npc = only npc_id knows/thinks/remembers; "
    "gm = hidden author truth not known by characters unless discovered."
)

WORLD_STATE_SPLIT_GUIDE = (
    "Do not store testimony as fact just because someone said it. If an NPC privately "
    "claims something to the player, store the claim as rumor with scope=shared and store "
    "that the NPC told/asked/was threatened as npc_memory with scope=shared or npc. If "
    "trust, debt, threat, leverage, affection, hatred, loyalty, fear, suspicion, a bargain, "
    "or blackmail changes, write or update relationship; if a plan or intent changes, "
    "write or update goal."
)

WORLD_STATE_CONSOLIDATION_GUIDE = (
    "Consolidate memory by durable meaning: one event, clue cluster, testimony block, "
    "or orientation for the same audience should usually be one record, not several "
    "near-duplicates. Put names, descriptions, and search words in text/aliases; use "
    "participants for extra actors who know the same record. Split only when truth status, "
    "access scope, owner, relationship target, or future update lifecycle is genuinely "
    "different."
)

WORLD_STATE_EXAMPLE_GUIDE = (
    "Examples: private witness claim -> rumor shared npc_id=<speaker> target=player; "
    "the witness remembers telling the player -> npc_memory shared npc_id=<speaker> "
    "target=player; private resentment after a threat -> relationship npc npc_id=<npc> "
    "target=player; secret plan to mislead the player -> goal npc npc_id=<npc>; "
    "identity revealed -> fact with entity_id=<npc> known_name=<player-facing name>."
)

WORLD_STATE_SEARCH_ANCHOR_GUIDE = (
    "Search anchors: when a note is tied to a place, use location_id/location_name for "
    "the concrete site, region_id/region_name for the broader town/area, scene_id only "
    "when the exact scene matters, and aliases for Russian names, case forms, "
    "transliterations, old names, nicknames, or spelling variants. Machine ids help exact lookup, but names "
    "and aliases help Russian semantic/keyword search."
)

TOOL_SEARCH_DESCRIPTION = (
    "Search and load hidden GM tools named in the system tool capability map. Use "
    "this when a needed hidden scene, NPC movement, NPC profile, or whereabouts capability is "
    "not visible. Query with keywords or exact selection such as "
    "select:tool_name or select:tool_a,tool_b; matching tools become available on "
    "the next GM step. Result is compact structured text: loaded and missing tools."
)
