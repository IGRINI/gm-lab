//! GM tool catalog — faithful port of the STATIC tool schemas in `agents.py`.
//!
//! Tool definitions are STATIC: they describe tool behavior only and never
//! enumerate the current world (no NPC roster, no dynamic id enums), so the
//! tool payload stays a stable cache prefix across world/NPC edits. The only
//! enums present are closed engine types (roll kinds, whereabouts status,
//! scopes, profile presets/fields), reproduced verbatim.
//!
//! Key insertion order matches the Python dict construction exactly; with
//! serde_json's `preserve_order` feature the emitted JSON is byte-identical to
//! the golden `gm_tools.json` / `gm_tools.compact.json` fixtures.

use std::collections::BTreeSet;

use regex::Regex;
use serde_json::{json, Map, Value};

use crate::tool_guidance;

/// `_SITUATION_DESC` — verbatim.
const SITUATION_DESC: &str = "Russian neutral third-person NPC-perception brief of what is happening RIGHT NOW: \
only what this NPC can see, hear, already know, or plausibly infer from visible \
pressure. Include the player's action and exact addressed words; quote player phrases \
unchanged when precision matters. Preserve declared delivery exactly: whisper, quiet \
voice, clenched-teeth mutter, silent gesture, or public speech. Do not upgrade a \
whisper/threat to shouting. If secrecy is risky because the room is crowded, describe \
that as risk of body language/proximity being noticed, not as other people hearing the \
content. State the intended listener/audience. If the player speaks quietly to this NPC, \
say that the content is meant for this NPC only unless someone explicitly overheard. \
Include immediate leverage and danger that the NPC can perceive: weapons, distance, \
escape routes, witnesses, whether guards are nearby, whether the NPC is cornered, and \
any intimidation/check result already rolled by the GM. Roll/check outcomes sent in \
the situation are authoritative for how strongly the visible attempt lands on this \
NPC: follow the grade, margin, and stakes as pressure, credibility, confidence, \
hesitation, or apparent danger. A roll/check result is not secret truth about hidden \
facts. Do not include GM-only certainty, player-sheet validation, hidden facts, or \
conclusions about whether the player is bluffing/lying, lacks proof, lacks a spell/\
item/weapon, or whether a threatened effect is truly impossible unless this NPC can \
directly observe that fact or already knows it. For unsupported declared effects after \
a player-facing correction, describe only the visible gesture/threat and how convincing \
or dangerous it appears. Do not write 'you'. Do not describe the NPC's \
feelings, motives, choices, or hidden thoughts. Keep proper nouns exactly as they are \
written in the current world.";

/// `world_mod.NPC_PROFILE_FIELDS` — the sorted union of all profile-preset
/// fields. Static engine data; embedded here so `get_npc_profile.fields.items`
/// can enumerate it. (gml-world keeps this private; the list is closed.)
const NPC_PROFILE_FIELDS: [&str; 25] = [
    "abilities",
    "ac",
    "age",
    "boundaries",
    "condition",
    "distinctive_features",
    "habits",
    "hp",
    "languages",
    "life_status",
    "life_status_note",
    "name",
    "passive_perception",
    "persona",
    "personality",
    "physical_type",
    "pressure_response",
    "public_label",
    "role",
    "saving_throws",
    "senses",
    "skills",
    "speed",
    "values",
    "voice",
];

/// `_INITIAL_GM_TOOL_NAMES` — the eight always-loaded GM tools.
pub(crate) const INITIAL_GM_TOOL_NAMES: [&str; 8] = [
    "ask_npc",
    "roll_dice",
    "get_world_fact",
    "query_world_state",
    "update_world_state",
    "update_player_character",
    "advance_time",
    "tool_search",
];

/// `_PLAYER_OPTIONS_TOOL_NAME`.
pub(crate) const PLAYER_OPTIONS_TOOL_NAME: &str = "ask_player";

/// `_TOOL_SEARCH_HINTS` — name -> hint keywords, in Python insertion order.
pub(crate) const TOOL_SEARCH_HINTS: [(&str, &str); 12] = [
    (
        "ask_npc",
        "npc нпс персонаж поговорить спросить допросить ответить реакция речь \
угроза угрожать убедить обмануть торг приказать",
    ),
    (
        "move_npc",
        "npc нпс персонаж входит выходит появился ушел переместить присутствует \
сцена слышит видит visibility presence пришел ушел",
    ),
    (
        "set_npc_whereabouts",
        "npc нпс местонахождение где искать куда ушел где находится известное \
вероятное слух whereabouts absent offscreen",
    ),
    (
        "set_scene",
        "сцена локация перейти войти выйти добраться место комната улица здание \
travel location scene exits items present_npcs",
    ),
    (
        "get_npc_profile",
        "npc profile карточка персонаж статы механика abilities skills saves passive perception \
ac hp speed senses languages personality habits voice внешний вид приметы состояние",
    ),
    (
        "advance_time",
        "time время часы календарь прошло минут ожидать подождать спустя пауза день ночь \
advance clock elapsed minutes",
    ),
    (
        "roll_dice",
        "куб кубик бросок проверка d20 dice roll внимание расследование insight \
perception investigation stealth persuasion deception intimidation attack save damage",
    ),
    (
        "get_world_fact",
        "факт память мир lore зацепка улика слух показание где кто что известно \
fact memory rag testimony rumor lead source provenance player-safe answer public",
    ),
    (
        "update_world_state",
        "batch пакет обновить записать удалить состояние мир факт слух память npc relationship \
отношение цель goal goals npc_memory facts rumors world state compact scope id known_name \
локация место город регион scene location aliases алиасы",
    ),
    (
        "update_player_character",
        "player character персонаж игрока лист персонажа карточка игрок hp ac abilities skills \
inventory equipment feature condition status update damage heal предмет инвентарь",
    ),
    (
        "ask_player",
        "варианты действия реплики кнопки быстрый ответ player options quick replies \
suggest choices задать вопрос что делать дальше",
    ),
    // NOTE: query_world_state hint appears last in the Python dict literal,
    // after ask_player. Insertion order preserved.
    (
        "query_world_state",
        "query scoped state record id hash expected_hash update delete область gm npc player \
состояние память секрет цели отношения relationship goal npc_memory target private",
    ),
];

fn hint_for(name: &str) -> &'static str {
    TOOL_SEARCH_HINTS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, h)| *h)
        .unwrap_or("")
}

// --- module-level static tools (Python module constants) -------------------

fn roll_dice_tool() -> Value {
    json!({"type": "function", "function": {
        "name": "roll_dice",
        "description":
            "Roll dice for an uncertain D&D-style mechanical result. Before rolling, lock in \
the roll kind, target number, and compact stakes so the post-roll narration cannot \
move the goalposts. Roll only after the action is possible with the current \
PLAYER CHARACTER CARD, inventory/equipment/features, and scene objects. Do not \
roll to conjure missing items, spells, authority, master training, tools, or \
materials into existence. If the latest action first needs a player-facing \
reality correction for a missing item/spell/feature/training/authority/body access/\
scene object/material/effect, do not call this tool; answer the correction and wait \
for the player. If a damage roll is made, the damaging effect is \
established as framed. Success means the locked intent works within the \
established fiction; critical success means the best plausible version of that \
success. Do not later treat a successful roll as a misfire, failed detonation, \
or no-effect outcome unless that condition was explicitly locked before rolling. \
Call for ability checks \
(Perception, Investigation, Insight, Stealth, Persuasion, Deception, \
Intimidation, Athletics, Sleight of Hand, lore checks, etc.), contested checks, \
saving throws, attacks, damage, random chance, intimidation/coercion, or other \
social pressure where success and failure both matter. Do not call for pure \
conversation, visible scene \
description, trivial/impossible actions, unsupported missing resources, or \
obvious consequences. Supports \
standard notation like 1d20+3 or 2d6, plus 2d20kh1/2d20kl1 for \
advantage/disadvantage. For player-side rolls, use PLAYER CHARACTER CARD \
modifiers/advantages when available. Put any known modifier directly in notation; do not \
invent unknown character-sheet bonuses. Skill/save modifiers must be exact card \
keys; never borrow a nearby skill or call an unlisted skill known. If the exact \
skill/save is missing, derive the ability modifier from the named ability score \
or roll plain 1d20 when that is also unknown. For actions opposed by a named NPC \
(stealing from them, sneaking past them, lying to them, attacking them, or testing \
whether they notice), get selected mechanics through get_npc_profile first when not \
already known; use their passive_perception, AC, save, skill, or ability data instead \
of a generic DC. The result is compact structured text containing only the \
new roll outcome: total, grade, margin, and natural roll.",
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
                              "description": "Player-facing SHORT RUSSIAN phrase naming only the SOURCE of the modifier or advantage/disadvantage, WITHOUT the number (the UI prints the number itself). Only include when notation itself contains a real +N/-N, kh1, or kl1 from a known source, e.g. 'навык Внимательности', 'помощь союзника', 'выгодная позиция', 'преимущество от засады', 'помеха из-за темноты'. For plain unmodified rolls like 1d20, omit this field entirely. Do not use for leverage, stakes, difficulty, or placeholder text."},
            "stakes": {"type": "object", "properties": {
                "intent": {"type": "string",
                           "description": "Short English pre-roll goal the player is trying to achieve."},
                "success": {"type": "string",
                            "description": "Short English pre-roll promise for what success unlocks."},
                "failure": {"type": "string",
                            "description": "Short English pre-roll consequence or lack of progress on failure."},
                "complication": {"type": "string",
                                 "description": "Short English cost to use for near misses or weak failures."},
            }, "additionalProperties": false},
        }, "required": ["roll_kind", "notation", "check_name", "reason"], "additionalProperties": false},
    }})
}

fn get_fact_tool() -> Value {
    json!({"type": "function", "function": {
        "name": "get_world_fact",
        "description":
            "Player-safe answer lookup for world memory: facts, leads, testimony, known NPC \
whereabouts, public lore, rumors, or prior statements that are not already in \
CURRENT SCENE STATE, the public intro, or the conversation. Use this, not \
query_world_state, for ordinary player-facing public/lore answers. Use before asserting or summarizing \
non-visible suspects, leads, clue meanings, timelines, ownership, relationships, \
factions, prior testimony, or offscreen NPC locations. The result is compact \
structured text with status, text, and compact source lines; it may contain \
unconfirmed testimony. Do not call for facts \
that are visible right now. If the result status is unknown or a source is \
unconfirmed, preserve uncertainty instead of inventing an answer. Do not use this \
when you need state-record id/hash for update/delete; use query_world_state then. Within the \
active, not-yet-compacted GM context, repeated lookups return only new matching \
sources; sources already delivered to the model are suppressed and reported as \
already_delivered. After GM history compaction, this delivery memory resets.",
        "parameters": {"type": "object", "properties": {
            "query": {"type": "string",
                      "description": "What you want to know, in Russian. Keep proper nouns exactly as written."},
        }, "required": ["query"], "additionalProperties": false},
    }})
}

fn tool_search_tool() -> Value {
    json!({"type": "function", "function": {
        "name": "tool_search",
        "description": tool_guidance::TOOL_SEARCH_DESCRIPTION,
        "parameters": {"type": "object", "properties": {
            "query": {"type": "string",
                      "description": "Search query in Russian or English, or select:tool_name for exact loading."},
            "max_results": {"type": "integer",
                            "description": "Maximum number of tools to load. Default 5."},
        }, "required": ["query"], "additionalProperties": false},
    }})
}

/// `build_gm_tools(world)` — the STATIC GM tool list. `world` is accepted for
/// call-site compatibility but the schemas never read it.
pub fn build_gm_tools() -> Vec<Value> {
    let ask_npc = json!({"type": "function", "function": {
        "name": "ask_npc",
        "description":
            "Ask one present, able-to-hear named NPC for their own speech and visible action. \
WHEN TO CALL: the player addresses, questions, threatens, orders, bargains with, \
attacks, follows, or otherwise demands a personal reaction from that NPC; or the \
NPC must decide/speak/act/show emotion/move for themselves. If the player's latest \
message contains a present NPC's name and asks or accuses them, call this before \
final narration. DO NOT CALL when the latest action first needs a player-facing \
reality correction for a missing item, spell, feature, training, authority, body \
access, scene object, material, or effect; give the correction and wait. If the \
player later deliberately continues with a physically possible remainder, call \
ask_npc for the actual visible words/actions only. DO NOT CALL for absent NPCs, generic \
crowd color, visible scene description, or facts the GM can state from CURRENT \
SCENE STATE. If the fiction first brings an NPC into the scene, call move_npc \
before ask_npc. The result is the NPC response; if the action is physically \
impossible, call ask_npc again with the same npc_id and a correction. Use the \
npc_id from the current roster in CURRENT TURN CONTEXT; if the id is unknown \
the tool returns an error so you can retry with a valid id. The result is \
compact structured text with NPC speech/action already emitted to the player.",
        "parameters": {"type": "object", "properties": {
            "npc_id": {"type": "string",
                       "description": "Whom to wake: the npc_id of a present NPC from the current roster."},
            "situation": {"type": "string", "description": SITUATION_DESC},
            "correction": {"type": "string",
                           "description": "Fill in ONLY when sending an NPC response back for a redo: \
what is wrong and what to fix, in Russian. Omit this \
field on the first ask_npc call for a fresh player \
action."},
        }, "required": ["npc_id", "situation"], "additionalProperties": false},
    }});
    let move_npc = json!({"type": "function", "function": {
        "name": "move_npc",
        "description":
            "Update current-scene presence for a named NPC. WHEN TO CALL: a named NPC enters, \
leaves, becomes visible/hidden, moves into hearing range, leaves hearing range, \
or an accepted NPC response physically changes their presence. Call before final \
narration. DO NOT CALL for anonymous crowds, future plans, rumors, a player \
ordering an NPC to move, the player approaching an already-present NPC, or NPC \
speech/motives. This tool only changes state; it does not make the NPC speak, \
decide, or feel anything. Use the npc_id from the current roster in CURRENT TURN \
CONTEXT; an unknown id returns an error instead of changing state. The result \
is compact structured text with presence status only.",
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
        }, "required": ["npc_id", "present", "reason"], "additionalProperties": false},
    }});
    let set_npc_whereabouts = json!({"type": "function", "function": {
        "name": "set_npc_whereabouts",
        "description":
            "Update an absent named NPC's known, likely, rumored, or unknown offscreen \
whereabouts without adding them to the current scene. WHEN TO CALL: testimony, \
public facts, travel, or scene logic establishes where an absent NPC is, was \
last seen, or is likely to be found; or a previous guess is corrected. DO NOT \
CALL to make the NPC speak, react, enter, leave the current scene, or become \
visible. Use move_npc for current-scene presence and set_scene when the player \
actually reaches that place. Use the npc_id from the current roster in CURRENT \
TURN CONTEXT; an unknown id returns an error instead of recording whereabouts. \
The result is compact structured text with whereabouts status.",
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
        }, "required": ["npc_id", "status"], "additionalProperties": false},
    }});
    let get_npc_profile = json!({"type": "function", "function": {
        "name": "get_npc_profile",
        "description":
            "Fetch selected safe NPC card/mechanics fields without returning the full private \
NPC card. Use when a roll, visible description, status check, or social read \
needs specific NPC data. GM-internal: do not reveal raw stats to the player. \
It includes no secrets, private knowledge, or goals. The result is compact \
structured text listing only selected fields.",
        "parameters": {"type": "object", "properties": {
            "npc_id": {"type": "string",
                       "description": "NPC id from CURRENT TURN CONTEXT."},
            "preset": {"type": "string",
                       "enum": ["visible", "social", "mechanics", "status", "identity"],
                       "description": "Common field group. Omit for visible."},
            "fields": {"type": "array",
                       "items": {"type": "string", "enum": NPC_PROFILE_FIELDS.to_vec()},
                       "description": "Optional exact fields to union with preset."},
        }, "required": ["npc_id"], "additionalProperties": false},
    }});
    let set_scene = json!({"type": "function", "function": {
        "name": "set_scene",
        "description":
            "Replace CURRENT SCENE STATE when the player actually enters or arrives at a \
different room, building, street, site, or area. WHEN TO CALL: before final \
narration if you will say the player has arrived in a new current place, uses \
a visible exit, reaches a destination, or starts interacting with a different \
location. DO NOT CALL for movement inside the same scene, plans to go somewhere, \
failed travel, or vague searching without arrival. Include only visible/public \
state. If the player wants to enter/go to a reachable place and no obstacle is \
established, make the new scene the reached place; do not stop them at the doorway \
unless the doorway/blocker matters in play. The title must name the exact current \
area, e.g. 'У входа в караульную' if they are still outside. Do not invent hidden \
facts or conclusions. List in present_npcs only the npc_ids (from the current \
roster in CURRENT TURN CONTEXT) of NPCs actually in the new scene; unknown ids \
are ignored and reported back so you can correct them. The result is compact \
structured text with saved scene title, ids, items, exits, \
and dropped NPC ids.",
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
            }, "required": ["name"], "additionalProperties": false}},
            "exits": {"type": "array", "items": {"type": "object", "properties": {
                "id": {"type": "string"},
                "name": {"type": "string"},
                "destination": {"type": "string"},
                "visible": {"type": "boolean"},
                "blocked_by": {"type": "string"},
            }, "required": ["name"], "additionalProperties": false}},
            "constraints": {"type": "array", "items": {"type": "string"}},
            "tension": {"type": "string"},
            "reason": {"type": "string",
                       "description": "Why the current scene changed, in Russian."},
        }, "required": ["title", "description", "reason"], "additionalProperties": false},
    }});
    let advance_time = json!({"type": "function", "function": {
        "name": "advance_time",
        "description":
            "Advance the hidden world clock by elapsed in-world minutes for this resolved \
player turn. Call once before final narration when time passes. NPC speech or \
a social exchange usually consumes at least a short amount of time. The result \
is compact structured text with elapsed minutes and current time.",
        "parameters": {"type": "object", "properties": {
            "minutes": {"type": "integer",
                        "description": "Elapsed in-world minutes, non-negative."},
            "reason": {"type": "string",
                       "description": "Very short Russian reason."},
        }, "required": ["minutes", "reason"], "additionalProperties": false},
    }});
    let ask_player = json!({"type": "function", "function": {
        "name": PLAYER_OPTIONS_TOOL_NAME,
        "description":
            "Show quick-reply buttons above the player's input when CURRENT TURN CONTEXT says \
PLAYER OPTION SUGGESTIONS are enabled. This is the last tool before final \
narration: after all other required tools are resolved, call it exactly once, \
then use its tool result to write the closing player-facing narration and stop. \
Do not finish with narration only, do not call ask_player after final narration, \
and do not continue with more tools after ask_player unless its arguments were \
invalid. The engine will not synthesize fallback buttons if you skip this call. \
Provide at least 4 current, concrete actions or dialogue lines that fit the \
situation without replacing free text input. Each option has a short Russian \
label displayed on the button and a fuller Russian message that will be sent as \
the player's next message if clicked. Do not use this tool for hidden facts, \
spoilers, GM-only reasoning, NPC stats, or commands to the player. The result is \
compact structured text confirming that the buttons were shown; after receiving \
it, write the final narration for this turn.",
        "parameters": {"type": "object", "properties": {
            "question": {"type": "string",
                         "description": "Short Russian prompt above the buttons, e.g. 'Что ты делаешь дальше?'."},
            "options": {"type": "array", "minItems": 4, "maxItems": 8,
                        "items": {"type": "object", "properties": {
                            "label": {"type": "string",
                                      "description": "Short Russian button label, ideally 1-4 words."},
                            "message": {"type": "string",
                                        "description": "Full Russian player message to send when clicked."},
                        }, "required": ["label", "message"], "additionalProperties": false},
                        "description": "Four to eight distinct playable options for the current situation."},
        }, "required": ["question", "options"], "additionalProperties": false},
    }});
    let update_player_character = json!({"type": "function", "function": {
        "name": "update_player_character",
        "strict": false,
        "description":
            "Update the player character sheet after the fiction establishes a real change \
to the player's character: name/class/background details, life status, condition, \
HP, AC, abilities, skills, saves, passive Perception, senses, languages, \
inventory, equipment, features, or GM-only notes. Use this for the player \
character only, not for NPC memories, relationships, world facts, scene state, \
or time. Use it when the resolved fiction changes the sheet, or when a player-\
declared character detail is compatible with the current card and grants no \
unsupported item, authority, feature, expertise, or advantage. Do not record a \
contradictory or power-granting self-declaration as truth. Batch all player-sheet field changes \
for the turn in one call, but send only fields that changed; never echo the \
whole current card back to the tool. Omit optional fields when they did not \
change; do not send empty placeholders. The result is compact structured text \
with changed field names and card revision only.",
        "parameters": {"type": "object", "properties": {
            "fields": {"type": "object", "properties": {
                "name": {"type": "string"},
                "pronouns": {"type": "string"},
                "class_role": {"type": "string"},
                "level": {"type": "integer"},
                "background": {"type": "string"},
                "age": {"type": "string"},
                "physical_type": {"type": "string"},
                "distinctive_features": {"type": "string"},
                "life_status": {"type": "string"},
                "life_status_note": {"type": "string"},
                "condition": {"type": "string"},
                "personality": {"type": "string"},
                "values": {"type": "string"},
                "gm_notes": {"type": "string",
                             "description": "GM-only notes about the player character; never narrate directly."},
                "abilities": {"type": "object",
                              "description": "D&D ability scores, e.g. STR/DEX/CON/INT/WIS/CHA as scores, not roll modifiers."},
                "skills": {"type": "object",
                           "description": "Exact skill-name final modifiers only, e.g. Perception: 5. Do not add unlisted skills here unless the sheet truly changes."},
                "saving_throws": {"type": "object",
                                  "description": "Exact saving throw final modifiers, e.g. DEX: 5."},
                "passive_perception": {"type": "integer"},
                "ac": {"type": "integer"},
                "hp": {"type": "object",
                       "description": "Current hit point state, usually {current, max, temp?}."},
                "speed": {"type": "string"},
                "senses": {"type": "string"},
                "languages": {"type": "string"},
                "inventory": {"type": "array", "items": {"type": "string"},
                              "description": "Full current inventory only when inventory changed."},
                "equipment": {"type": "array", "items": {"type": "string"},
                              "description": "Full current equipment only when equipment changed."},
                "features": {"type": "array", "items": {"type": "string"},
                             "description": "Full current features only when features changed."},
            }, "additionalProperties": false,
                "description": "Only changed player-character fields. Omit unchanged or empty fields; never resend the full card."},
            "reason": {"type": "string",
                       "description": "Very short Russian reason for the sheet update."},
        }, "required": ["fields", "reason"], "additionalProperties": false},
    }});
    let update_world_state = json!({"type": "function", "function": {
        "name": "update_world_state",
        "strict": false,
        "description": update_world_state_description(),
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
                         "description":
                             "What namespace this item updates. Required for add. fact is \
objective established truth/visible stable state; rumor is \
unverified testimony/claim/suspicion/lead; npc_memory is what \
one NPC remembers, saw, was told, promised, hid, or learned; \
relationship is ongoing attitude/trust/debt/leverage/fear/\
loyalty/hatred/love/suspicion toward target; goal is current \
want/plan/intent. Do not store NPC testimony as fact just \
because someone said it."},
                "text": {"type": "string",
                         "description":
                            "Compact Russian durable meaning, not a transcript quote. \
Do not include English access labels like private, \
privately, shared, or public; use scope for access control. \
Required for add/update unless deleting. For relationship, keep \
the full multi-layer attitude in one string and update that \
record as it changes."},
                "npc_id": {"type": "string",
                           "description": "NPC id that owns/knows this npc_memory, relationship, or goal; for rumor, the speaker if known. Required for shared scope. For private NPC-player exchange use npc_id=<speaker>. Omit when empty."},
                "target": {"type": "string",
                           "description": "Relationship target such as player, an npc_id, faction, or place. For shared scope, target must be player or a known npc_id for one primary listener; use participants for multiple listeners. Required for relationship. For private NPC-player exchange use target=player."},
                "entity_id": {"type": "string",
                              "description": "Optional entity this note is about, such as an npc_id or location id. Use when someone reveals or remembers facts about another entity."},
                "known_name": {"type": "string",
                               "description": "Optional player-known name/label for the NPC named by entity_id, e.g. after an introduction or another character identifies them. Requires entity_id to be an NPC id. Never use for the player, locations, factions, items, or ordinary facts. This is what the player may now call that NPC; it need not prove the NPC's true identity."},
                "source_npc": {"type": "string",
                               "description": "Optional npc_id whose testimony/revelation is the source. Omit when same as npc_id or not relevant."},
                "participants": {"type": "array", "items": {"type": "string"},
                                 "description": "Optional extra actor ids who know/heard/share this exact record, such as player or npc ids. Use one record with participants instead of duplicating the same fact/rumor/npc_memory for several listeners. Do not use for public knowledge."},
                "location_id": {"type": "string",
                                "description": "Optional stable place id this note happened at or is about. Use for exact lookup; pair with location_name or aliases when the id is English/transliterated."},
                "location_name": {"type": "string",
                                  "description": "Optional human/Russian place name for search, e.g. a tavern, street, room, ruin, or district. Use when location_id alone may not match future Russian queries."},
                "region_id": {"type": "string",
                              "description": "Optional broader area id such as city, village, district, dungeon, faction territory, or campaign region."},
                "region_name": {"type": "string",
                                "description": "Optional human/Russian broader area name for search, e.g. the town/city name the GM may ask about later."},
                "scene_id": {"type": "string",
                             "description": "Optional exact scene id when the note belongs to a specific current or past scene. Omit when the note is not scene-specific."},
                "importance": {"type": "string",
                               "description": "Optional short priority label like low, normal, high, pinned, or clue. Omit when not useful."},
                "aliases": {"type": "array", "items": {"type": "string"},
                            "description": "Optional search aliases/spellings for this note: Russian names, case forms, transliterations, old names, nicknames, or common variants. Use to bridge English ids and Russian queries from the GM."},
                "scope": {"type": "string",
                          "enum": ["public", "gm", "npc", "shared"],
                          "description": "Who may know this state. public is not private player knowledge; shared means only npc_id plus target and/or participants know; npc means only npc_id knows/thinks/remembers; gm means hidden author truth. Use shared for a private NPC-player exchange. shared requires npc_id and either target or participants. Omit to use the type default."},
                "witnesses": {"type": "array", "items": {"type": "string"},
                               "description": "For public rumors only: ids who heard it, plus player if relevant. Omit when empty."},
                "mode": {"type": "string", "enum": ["replace"],
                         "description": "Only for goal when replacing existing active goals. Omit for normal add/update and for all non-goal items."},
            }, "required": [], "additionalProperties": false},
                "description": "Compact updates. Omit optional item fields when empty."},
        }, "required": ["items"], "additionalProperties": false},
    }});
    let query_world_state = json!({"type": "function", "function": {
        "name": "query_world_state",
        "description":
            "Scoped state-record lookup for durable memory, ids, and hashes. Use before \
update_world_state update/delete, and before adding a relationship, goal, or \
npc_memory that may already exist. Do not use this for ordinary player-safe \
public/lore answer lookup; use get_world_fact for that. The \
result is compact structured text with matching rows and record \
ids/hashes for update/delete expected_hash. Use player scope only when you need \
stored player-known state records, ids, hashes, or expected_hash for a write; \
player scope must never reveal GM truth, hidden events, NPC secrets, private NPC \
memory, or private goals. Use npc scope with npc_id for what that NPC may know: \
public memory plus that NPC's own private card/memory only. Use gm scope for \
author-only truth, hidden events, all NPC private notes, and public memory. \
The result includes only matching scoped state. Search can match \
record text plus location, region, scene, and aliases when those anchors were stored. \
Within the active, not-yet-compacted GM context, repeated queries return only \
new matching rows; rows already delivered to the model are suppressed and \
reported as already_delivered. After GM history compaction, this delivery \
memory resets.",
        "parameters": {"type": "object", "properties": {
            "scope": {"type": "string", "enum": ["player", "gm", "npc"],
                      "description": "Visibility namespace to query."},
            "query": {"type": "string",
                      "description": "State-record lookup text, in Russian or English. Include kind, parties, place, region, scene, or alias when useful, e.g. 'relationship borin player', 'goal lysa', 'known_name Лиза', or 'state_fact Тёрнвейле тайник'. For ordinary player-safe public/lore answers, use get_world_fact instead. Keep proper nouns exact."},
            "npc_id": {"type": "string",
                       "description": "Required for npc scope. Omit for player or gm scope."},
            "max_results": {"type": "integer",
                            "description": "Maximum matching rows to return. Omit for default 5."},
        }, "required": ["scope", "query"], "additionalProperties": false},
    }});
    vec![
        ask_npc,
        move_npc,
        set_npc_whereabouts,
        get_npc_profile,
        set_scene,
        advance_time,
        ask_player,
        update_player_character,
        update_world_state,
        query_world_state,
        roll_dice_tool(),
        get_fact_tool(),
        tool_search_tool(),
    ]
}

/// The `update_world_state` description f-string, with the six tool_guidance
/// fragments spliced in exactly as Python concatenates them.
fn update_world_state_description() -> String {
    format!(
        "Apply a compact batch of GM-authored world-state updates after the fiction \
establishes them. Accepts items[] for fact, rumor, npc_memory, relationship, \
and goal records. One item is one atomic durable note; batch 1-5 important \
changes instead of making repeated tool calls. Use op=add to create, op=update \
to revise an existing id, and op=delete to remove an id from active memory/RAG. \
After ask_npc, use this before final narration when the NPC answer confirms, \
denies, hides, promises, threatens, refuses, or changes something that should \
matter later. \
When the player learns what to call an NPC, include known_name plus entity_id=<npc_id>; \
this is GM-authored identity state, never inferred automatically. known_name is \
only for NPC entity_id values, never for the player, locations, factions, items, \
or ordinary facts. \
For update/delete, include expected_hash when you have a fresh hash from \
query_world_state or a just-returned update_world_state result. If you do not \
have a fresh id/hash and an active record may already exist for the same \
npc_id, target, and participants, call query_world_state first; then update/delete that id \
instead of adding a duplicate. Use add only when lookup is unknown or the note \
is genuinely new. For op=add, never invent or send id, expected_hash, mode, or \
placeholder hash values; the engine assigns ids. \
{} {} {} {} {} {} \
Keep text short and in Russian. Do not put English access labels like \
private, privately, shared, or public into item text; access belongs in \
scope only. Omit optional fields when empty; do not send empty strings, \
empty arrays, or nulls for optional fields. Private NPC testimony, clues, \
promises, or leads told only to the player must use shared, not public. \
When one durable note is known by several specific actors, write one \
item and put the extra actor ids in participants instead of duplicating \
the same text for each actor. \
Every shared item must include npc_id and either target or participants \
or it will be rejected. \
Do not use for visible scene movement, current-scene presence, or NPC speech; \
use set_scene, move_npc, or ask_npc for those. The result is compact structured \
text: applied/not-stored rows with ids, hashes, status, \
and conflict/duplicate hints.",
        tool_guidance::WORLD_STATE_TYPE_GUIDE,
        tool_guidance::WORLD_STATE_SCOPE_GUIDE,
        tool_guidance::WORLD_STATE_SPLIT_GUIDE,
        tool_guidance::WORLD_STATE_CONSOLIDATION_GUIDE,
        tool_guidance::WORLD_STATE_EXAMPLE_GUIDE,
        tool_guidance::WORLD_STATE_SEARCH_ANCHOR_GUIDE,
    )
}

// --- catalog / per-model / search ------------------------------------------

fn tool_name(tool: &Value) -> String {
    tool.get("function")
        .and_then(|f| f.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn tool_description(tool: &Value) -> String {
    tool.get("function")
        .and_then(|f| f.get("description"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// `gm_tool_catalog(world)` — executable registry keyed by tool name.
pub fn gm_tool_catalog() -> indexmap::IndexMap<String, Value> {
    let mut map = indexmap::IndexMap::new();
    for tool in build_gm_tools() {
        map.insert(tool_name(&tool), tool);
    }
    map
}

/// `initial_gm_tool_names(include_player_options_tool)`.
pub fn initial_gm_tool_names(include_player_options_tool: bool) -> BTreeSet<String> {
    let mut names: BTreeSet<String> =
        INITIAL_GM_TOOL_NAMES.iter().map(|s| s.to_string()).collect();
    if include_player_options_tool {
        names.insert(PLAYER_OPTIONS_TOOL_NAME.to_string());
    }
    names
}

/// `build_gm_tools_for_model(world, loaded_tool_names, include_player_options_tool)`.
pub fn build_gm_tools_for_model(
    loaded_tool_names: Option<&BTreeSet<String>>,
    include_player_options_tool: bool,
) -> Vec<Value> {
    let catalog = gm_tool_catalog();
    let catalog: Vec<(String, Value)> = catalog
        .into_iter()
        .filter(|(name, _)| include_player_options_tool || name != PLAYER_OPTIONS_TOOL_NAME)
        .collect();
    match loaded_tool_names {
        None => catalog.into_iter().map(|(_, t)| t).collect(),
        Some(loaded) => {
            let mut visible: BTreeSet<String> = loaded.clone();
            visible.extend(initial_gm_tool_names(include_player_options_tool));
            catalog
                .into_iter()
                .filter(|(name, _)| visible.contains(name))
                .map(|(_, t)| t)
                .collect()
        }
    }
}

fn short_tool_description(tool: &Value, limit: usize) -> String {
    let ws = Regex::new(r"\s+").unwrap();
    let text = ws.replace_all(&tool_description(tool), " ").trim().to_string();
    if text.chars().count() <= limit {
        return text;
    }
    // Python: text[:limit].rstrip() + "..."  (char-based slice).
    let truncated: String = text.chars().take(limit).collect();
    format!("{}...", truncated.trim_end())
}

fn tool_parameters_text(tool: &Value) -> String {
    let params = tool
        .get("function")
        .and_then(|f| f.get("parameters"))
        .cloned()
        .unwrap_or(Value::Object(Map::new()));
    let mut parts: Vec<String> = Vec::new();
    fn visit(schema: &Value, parts: &mut Vec<String>) {
        let obj = match schema.as_object() {
            Some(o) => o,
            None => return,
        };
        if let Some(desc) = obj.get("description") {
            if let Some(s) = desc.as_str() {
                parts.push(s.to_string());
            } else if !desc.is_null() {
                parts.push(desc.to_string());
            }
        }
        if let Some(Value::Object(props)) = obj.get("properties") {
            for (key, value) in props {
                parts.push(key.clone());
                visit(value, parts);
            }
        }
        if let Some(items) = obj.get("items") {
            if !items.is_null() {
                visit(items, parts);
            }
        }
    }
    visit(&params, &mut parts);
    parts.join(" ")
}

fn tool_search_text(tool: &Value) -> String {
    let name = tool_name(tool);
    [
        name.clone(),
        name.replace('_', " "),
        tool_description(tool),
        tool_parameters_text(tool),
        hint_for(&name).to_string(),
    ]
    .join(" ")
    .to_lowercase()
}

fn score_tool(query_terms: &[String], required_terms: &[String], tool: &Value) -> i64 {
    let name = tool_name(tool).to_lowercase();
    let text = tool_search_text(tool);
    if !required_terms.is_empty() && !required_terms.iter().all(|t| text.contains(t.as_str())) {
        return 0;
    }
    let name_parts: BTreeSet<&str> = name.split('_').collect();
    let hint_lower = hint_for(&name).to_lowercase();
    let mut score = 0;
    for term in query_terms {
        if term.is_empty() {
            continue;
        }
        if *term == name {
            score += 100;
        } else if name_parts.contains(term.as_str()) {
            score += 35;
        } else if name.contains(term.as_str()) {
            score += 20;
        } else if hint_lower.contains(term.as_str()) {
            score += 12;
        } else if text.contains(term.as_str()) {
            score += 5;
        }
    }
    score
}

/// `search_gm_tools(world, query, max_results, already_loaded, include_player_options_tool)`.
pub fn search_gm_tools(
    query: &str,
    max_results: i64,
    already_loaded: Option<&BTreeSet<String>>,
    include_player_options_tool: bool,
) -> Value {
    let catalog = gm_tool_catalog();
    let catalog: Vec<(String, Value)> = catalog
        .into_iter()
        .filter(|(name, _)| include_player_options_tool || name != PLAYER_OPTIONS_TOOL_NAME)
        .collect();
    let already_loaded: BTreeSet<String> = already_loaded.cloned().unwrap_or_default();
    let searchable: Vec<(String, Value)> = catalog
        .iter()
        .filter(|(name, _)| name != "tool_search" && !already_loaded.contains(name))
        .cloned()
        .collect();

    let raw_query = query.trim().to_string();
    if raw_query.is_empty() {
        return json!({
            "query": raw_query,
            "matches": [],
            "loaded_tools": [],
            "already_loaded": [],
            "total_searchable_tools": searchable.len(),
            "message": "Запрос пустой. Используй keywords или select:tool_name.",
        });
    }
    // limit = max(1, min(int(max_results or 5), 10))
    let limit_src = if max_results == 0 { 5 } else { max_results };
    let limit = limit_src.clamp(1, 10) as usize;

    let mut selected: Vec<String> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    let mut known_loaded: Vec<String> = Vec::new();

    if raw_query.to_lowercase().starts_with("select:") {
        let after = raw_query.split_once(':').map(|x| x.1).unwrap_or("");
        let requested: Vec<String> = after
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        // all_tool_names = {name.lower(): name for name in catalog if name != tool_search}
        let mut all_tool_names: indexmap::IndexMap<String, String> = indexmap::IndexMap::new();
        for (name, _) in &catalog {
            if name != "tool_search" {
                all_tool_names.insert(name.to_lowercase(), name.clone());
            }
        }
        for item in requested {
            match all_tool_names.get(&item.to_lowercase()) {
                None => missing.push(item),
                Some(name) => {
                    if already_loaded.contains(name) {
                        known_loaded.push(name.clone());
                    } else {
                        selected.push(name.clone());
                    }
                }
            }
        }
    } else {
        let term_re = Regex::new(r"[\wа-яА-ЯёЁ-]+").unwrap();
        let lower = raw_query.to_lowercase();
        let terms: Vec<String> = term_re
            .find_iter(&lower)
            .map(|m| m.as_str().to_string())
            .collect();
        let required: Vec<String> = terms
            .iter()
            .filter(|t| t.starts_with('+') && t.chars().count() > 1)
            .map(|t| t.chars().skip(1).collect())
            .collect();
        let mut scoring_terms: Vec<String> = required.clone();
        scoring_terms.extend(terms.iter().filter(|t| !t.starts_with('+')).cloned());
        let mut scored: Vec<(i64, String)> = Vec::new();
        for (name, tool) in &searchable {
            let score = score_tool(&scoring_terms, &required, tool);
            if score > 0 {
                scored.push((score, name.clone()));
            }
        }
        // sorted(scored, reverse=True): by (score desc, name desc).
        scored.sort_by(|a, b| b.cmp(a));
        selected = scored.into_iter().take(limit).map(|(_, n)| n).collect();
    }

    let searchable_map: indexmap::IndexMap<String, Value> = searchable.into_iter().collect();
    let mut matches: Vec<Value> = Vec::new();
    for name in selected.iter().take(limit) {
        if let Some(tool) = searchable_map.get(name) {
            matches.push(json!({
                "name": name,
                "description": short_tool_description(tool, 220),
                "loaded": true,
                "already_loaded": already_loaded.contains(name),
            }));
        }
    }
    let already_loaded_result: Vec<String> = {
        let s: BTreeSet<String> = known_loaded.into_iter().collect();
        s.into_iter().collect()
    };
    let message = if !matches.is_empty() {
        "Найденные инструменты будут доступны в следующем шаге ГМ. \
Вызови нужный инструмент после этого результата."
    } else if !already_loaded_result.is_empty() {
        "Запрошенные инструменты уже доступны в текущем шаге ГМ."
    } else {
        "Подходящих инструментов не найдено. Попробуй select:tool_name или другие ключевые слова."
    };

    let loaded_tools: Vec<String> = matches
        .iter()
        .map(|m| m["name"].as_str().unwrap_or("").to_string())
        .collect();

    json!({
        "query": raw_query,
        "matches": matches,
        "loaded_tools": loaded_tools,
        "already_loaded": already_loaded_result,
        "missing": missing,
        "total_searchable_tools": searchable_map.len(),
        "message": message,
    })
}
