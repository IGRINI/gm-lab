//! GM and NPC tool catalogs. GM schemas stay static/cacheable; NPC schemas are
//! also static and never enumerate live world ids.
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

use gml_prompts::{render_prompt, PromptId};

use crate::tool_guidance;

/// `_SITUATION_DESC` — verbatim.
const SITUATION_DESC: &str =
    "Russian neutral third-person NPC-perception brief of what is happening RIGHT NOW: \
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

/// `_INITIAL_GM_TOOL_NAMES` — the always-loaded GM tools. `move_player` is a
/// PRIMARY living-world tool (LOCKED DECISION #2): travel goes through the canon,
/// so the model always has it without a `tool_search`. `generate_npc` is NOT
/// initial: significant-NPC creation is a narrow, engine-authoritative trigger,
/// so the generator is DEFERRED like the other canon tools and the GM reaches it
/// through `tool_search`. Loaded tools persist for the session, so that search is
/// a one-time cost, not a per-turn hop.
pub(crate) const INITIAL_GM_TOOL_NAMES: [&str; 11] = [
    "ask_npc",
    "roll_dice",
    "get_world_fact",
    "get_memory",
    "note_memory",
    "update_player_character",
    "advance_time",
    "move_player",
    "load_tool_schema",
    "invoke_loaded_tool",
    "tool_search",
];

/// `_PLAYER_OPTIONS_TOOL_NAME`.
pub(crate) const PLAYER_OPTIONS_TOOL_NAME: &str = "ask_player";
pub(crate) const TOOL_SEARCH_TOOL_NAME: &str = "tool_search";
pub(crate) const LOAD_TOOL_SCHEMA_TOOL_NAME: &str = "load_tool_schema";
pub(crate) const INVOKE_LOADED_TOOL_NAME: &str = "invoke_loaded_tool";

/// `_TOOL_SEARCH_HINTS` — name -> hint keywords. Includes the living-world canon
/// tools (`move_player`, `world_debug`).
pub(crate) const TOOL_SEARCH_HINTS: [(&str, &str); 22] = [
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
travel location scene exits items present_npcs канон canon place",
    ),
    (
        "move_player",
        "идти перейти выход проход дверь покинуть уйти добраться двигаться канон \
move player transition exit travel walk leave traversal place",
    ),
    (
        "world_debug",
        "debug отладка канон граф мест переходов актёров причинный лог replay \
world debug causal log canon dump snapshot",
    ),
    (
        "generate_location",
        "генератор локация сцена помещение комната город деревня данж дорожная ситуация \
location scene room city village dungeon travel situation encounter anti-repeat",
    ),
    (
        "take_item",
        "взять поднять забрать подобрать предмет вещь из сцены в инвентарь \
take pick up grab loot item scene inventory карман",
    ),
    (
        "drop_item",
        "выложить бросить оставить положить предмет вещь из инвентаря в сцену \
drop put down leave item inventory scene на стол на землю",
    ),
    (
        "cast_spell",
        "заклинание спелл каст сотворить прочитать колдовать магия слот концентрация \
заговор апкаст cast spell slot cantrip concentration magic ритуал",
    ),
    (
        "generate_npc",
        "нпс npc персонаж житель встреча значимый именной новый создать сгенерировать \
герой сюжета character generate significant named recurring encounter roster",
    ),
    (
        "read_state",
        "состояние время сцена лист ростер факты текущее проверить посмотреть узнать \
current state time scene sheet roster facts check inspect look up",
    ),
    (
        "long_rest",
        "долгий отдых передышка выспаться ночёвка спать сон восстановить слоты хп \
концентрация полный отдых night sleep long rest recover restore slots hp full",
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
        "get_memory",
        "память воспоминание знание кто знает что помнит секрет слух город таверна \
memory scoped recall player npc actor place faction public private crystal source",
    ),
    (
        "note_memory",
        "записать память воспоминание слух секрет событие игрок npc город место фракция \
write memory note scoped owner visibility summary details source event",
    ),
    (
        "consolidate_memory",
        "кристалл память сжать суммаризировать raw episode arc durable consumed cold \
consolidate crystal memory sources summarize archive",
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
            "Player-safe answer lookup for compact world facts: leads, testimony, known NPC \
    whereabouts, public lore, rumors, or prior statements that are not already in \
    CURRENT SCENE STATE, the public intro, or the conversation. Use get_memory \
    when you need scoped living memory for a player, NPC, place, faction, route, \
    or GM-private lens. Use this before asserting or summarizing \
    non-visible suspects, leads, clue meanings, timelines, ownership, relationships, \
    factions, prior testimony, or offscreen NPC locations. The result is compact \
    structured text with status, text, and compact source lines; it may contain \
    unconfirmed testimony. Do not call for facts \
    that are visible right now. If the result status is unknown or a source is \
    unconfirmed, preserve uncertainty instead of inventing an answer. Do not use this \
    when you need detailed scoped memory cards; call get_memory explicitly for that. Within the \
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
        "name": TOOL_SEARCH_TOOL_NAME,
        "description": tool_guidance::TOOL_SEARCH_DESCRIPTION,
        "parameters": {"type": "object", "properties": {
            "query": {"type": "string",
                      "description": "Search query in Russian or English, or select:tool_name for exact catalog lookup."},
            "max_results": {"type": "integer",
                            "description": "Maximum number of catalog cards to return. Default 5."},
        }, "required": ["query"], "additionalProperties": false},
    }})
}

fn load_tool_schema_tool() -> Value {
    json!({"type": "function", "function": {
        "name": LOAD_TOOL_SCHEMA_TOOL_NAME,
        "description": tool_guidance::LOAD_TOOL_SCHEMA_DESCRIPTION,
        "parameters": {"type": "object", "properties": {
            "name": {"type": "string",
                     "description": "Exact canonical GM tool name returned by tool_search."},
        }, "required": ["name"], "additionalProperties": false},
    }})
}

fn invoke_loaded_tool() -> Value {
    json!({"type": "function", "function": {
        "name": INVOKE_LOADED_TOOL_NAME,
        "description": tool_guidance::INVOKE_LOADED_TOOL_DESCRIPTION,
        "strict": false,
        "parameters": {"type": "object", "properties": {
            "name": {"type": "string",
                     "description": "Exact canonical GM tool name returned by load_tool_schema."},
            "arguments": {"type": "object",
                          "description": "Arguments matching the loaded schema.",
                          "additionalProperties": true},
        }, "required": ["name", "arguments"], "additionalProperties": false},
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
            "\
    Compatibility/debug fallback for applying a fully-authored current scene patch. \
    DO NOT use this as the normal way to invent a new room, side chamber, street, \
    building, point of interest, dungeon point, or road situation. For living-world \
    play, prefer move_player whenever a listed exit already leads where the player is \
    going, and call generate_location FIRST when the player discovers, opens, enters, \
    or needs a not-yet-authored place/situation. Use set_scene only after \
    generate_location is unavailable or rejected, or when an external tool/user debug \
    patch already supplies the complete visible scene. The destination is upserted as \
    a canonical Place (stable id from location_id or the title), a transition from the \
    player's current place is ensured, and the player's canonical place is set to it; \
    the live scene is then rebuilt FROM the canon, so it is authoritative — not a \
    wholesale scene replacement. DO NOT CALL for movement inside the same place, plans \
    to go somewhere, failed travel, vague searching without arrival, or when a visible \
    exit already leads there (use move_player). Include only visible/public state. The \
    title must name the exact current area, e.g. \
    'У входа в караульную' if they are still outside. Do not invent hidden facts or \
    conclusions. List in present_npcs only the npc_ids (from the current roster in \
    CURRENT TURN CONTEXT) of NPCs actually in the new place; unknown ids are ignored \
    and reported back so you can correct them. The result is compact structured text \
    with the new canonical place id, title, items, exits, and dropped NPC ids.",
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
            "Advance the hidden world clock by elapsed in-world minutes for waiting, \
    sleeping, work, study, recovery, rituals, or other time passing that is NOT \
    ordinary travel through a listed exit. Do NOT call this after move_player: \
    move_player already spends the transition's travel time and may trigger road \
    situations. NPC speech or a social exchange usually consumes at least a short \
    amount of time. The result is compact structured text with elapsed minutes and \
    current time.",
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
                              "description": "Full current inventory only when inventory changed. Each entry is a string; an optional description may follow the item name after ' — ' (e.g. 'кинжал — 1d4, скрыт в сапоге'). The name before ' — ' is the head used for all matching. For picking up an item already in the scene use take_item (it carries the same object with its details); for dropping one into the scene use drop_item."},
                "equipment": {"type": "array", "items": {"type": "string"},
                              "description": "Full current equipment only when equipment changed. Same ' — ' name/description convention as inventory; matching is by the head before ' — '."},
                "features": {"type": "array", "items": {"type": "string"},
                             "description": "Full current features only when features changed."},
                "spells": {"type": "array", "items": {"type": "object", "properties": {
                    "name": {"type": "string"},
                    "level": {"type": "integer", "description": "Spell level; 0 for a cantrip (заговор)."},
                    "concentration": {"type": "boolean", "description": "True if the spell requires concentration."},
                    "ritual": {"type": "boolean", "description": "True if the spell can be cast as a ritual (display only in v1)."},
                    "effect": {"type": "string", "description": "Prose effect. Put school, casting time, range, duration, and upcast text HERE — the engine reads only name/level/concentration/ritual."},
                }, "additionalProperties": false},
                           "description": "Full known-spell list only when it changed; each entry is an object with name/level/concentration/ritual/effect. To CAST a known spell (spend a slot, set concentration) call cast_spell instead; use this only to learn, forget, or edit spells."},
                "spell_slots": {"type": "object",
                                "description": "FLAT map «level → remaining slots», e.g. {\"1\": 3, \"2\": 1}. Send the full map when it changes (e.g. a long rest restores slots); the engine decrements it on cast_spell. An unlisted level means no slots at that level. Do NOT nest {current, max}."},
                "spell_slots_max": {"type": "object",
                                    "description": "FLAT map «level → max slots» the character has when fully rested. Author from the 5e slot table for the class/level (e.g. a level-1 full caster has {\"1\": 2}; level-3 has {\"1\": 4, \"2\": 2})."},
                "concentration": {"type": "string",
                                  "description": "Name of the spell the character is currently concentrating on; send \"\" to drop concentration without a new cast. Normally set by cast_spell."},
                "inventory_add": {"type": "array", "items": {"type": "string"},
                                  "description": "Inventory entries to append. Use INSTEAD of the full inventory array for small pickups. If you also send the full inventory it is applied first and these deltas run on top; do not mix them in the same call. Duplicates already present are skipped."},
                "inventory_remove": {"type": "array", "items": {"type": "string"},
                                     "description": "Inventory entries to drop, matched trimmed and exact; every occurrence is removed. Applied before inventory_add; do not send with a full inventory array."},
                "equipment_add": {"type": "array", "items": {"type": "string"},
                                  "description": "Equipment entries to append. Use INSTEAD of the full equipment array for small changes. If you also send the full equipment array it is applied first and these deltas run on top; do not mix them in the same call. Duplicates already present are skipped."},
                "equipment_remove": {"type": "array", "items": {"type": "string"},
                                     "description": "Equipment entries to drop, matched trimmed and exact; every occurrence is removed. Applied before equipment_add; do not send with a full equipment array."},
            }, "additionalProperties": false,
                "description": "Only changed player-character fields. Omit unchanged or empty fields; never resend the full card."},
            "reason": {"type": "string",
                       "description": "Very short Russian reason for the sheet update."},
        }, "required": ["fields", "reason"], "additionalProperties": false},
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
        roll_dice_tool(),
        get_fact_tool(),
        tool_search_tool(),
    ]
}

// --- canon / living-world tools (ADDITIVE, appended after the static catalog) -
//
// These map onto `gml_world::canon::Action`s and are routed through the
// validator-gated engine in the orchestrator (TZ §8). They are NOT part of the
// byte-identical `build_gm_tools()` fixture: a separate builder leaves
// `gm_tools.json` untouched while still making them dispatchable.
// `gm_tool_catalog()` appends `build_canon_gm_tools()` AFTER `build_gm_tools()`
// so the stable cache prefix is preserved (new tools only ever go at the end).

/// `_CANON_GM_TOOL_NAMES` — the additive living-world tools, in append order.
/// `take_item`/`drop_item` are the Phase-И scene↔inventory item movers, and
/// `cast_spell` is the Phase-С spell-slot/concentration mover
/// (`docs/ITEMS_AND_SPELLS_TZ.md` §И3/§С2), all appended at the END: they mutate
/// the player card and are NOT part of the byte-gated static catalog (no
/// re-bless), matching the move_player precedent. `long_rest` is the Phase-О
/// deferred full-rest mover, appended at the END after the core set: like the
/// other card movers it is NOT part of the byte-gated static catalog.
pub const CANON_GM_TOOL_NAMES: [&str; 9] = [
    "move_player",
    "world_debug",
    "generate_location",
    "take_item",
    "drop_item",
    "cast_spell",
    "generate_npc",
    "read_state",
    "long_rest",
];

/// The additive living-world GM tools that commit through the canon engine.
pub fn build_canon_gm_tools() -> Vec<Value> {
    let move_player = json!({"type": "function", "function": {
        "name": "move_player",
        "description":
            "Move the player along a known, visible, passable transition out of their CURRENT \
    canon place, instead of replacing the scene wholesale. WHEN TO CALL: the player \
    takes a listed exit / path / doorway and actually leaves for the place it leads \
    to. The transition spends its canon travel time automatically; do NOT also call \
    advance_time for the same travel. Long risky routes may stop the player at a \
    temporary road situation before the destination, with continue/back exits and \
    remaining travel time. The transition is committed through the world canon and gated by the \
    validator: an unknown transition, one that does not start at the player's current \
    place, a hidden exit, or a blocked way is REJECTED and changes nothing, so the \
    player can never reach a contradictory location. If the destination has not been \
    authored yet it is lazily generated on first entry and then becomes canon, with a \
    guaranteed return path so the player can always go back. DO NOT CALL to invent an \
    exit that does not exist, to teleport, or for movement inside the same place. Use \
    the transition_id from the player's visible exits. The result is compact \
    structured text with the new current place and the committed canon events, or a \
    rejection reason to repair.",
        "parameters": {"type": "object", "properties": {
            "transition_id": {"type": "string",
                              "description": "Id of a visible, passable transition leaving the player's current place."},
            "reason": {"type": "string",
                       "description": "Very short Russian reason for the move."},
        }, "required": ["transition_id"], "additionalProperties": false},
    }});
    let world_debug = json!({"type": "function", "function": {
        "name": "world_debug",
        "description":
            "GM/developer replay tool: return a debug dump of the current living-world canon — \
    the full place/transition/actor graph plus the causal event log explaining WHY \
    the world reached its current state (TZ §12). Read-only; it commits nothing and \
    changes no state. GM-internal only: never reveal raw canon, hidden events, or \
    GM-private scopes to the player. The result is compact structured text with the \
    canon snapshot and the ordered causal log.",
        "parameters": {"type": "object", "properties": {
            "causal_log_only": {"type": "boolean",
                                "description": "When true, return only the causal event log, not the full canon dump."},
        }, "required": [], "additionalProperties": false},
    }});
    let generate_location = json!({"type": "function", "function": {
        "name": "generate_location",
        "description":
            "Ask the dedicated location/situation generator sub-agent to draft and optionally \
    commit a bounded canon location, room, dungeon point, city/village point of interest, \
    or road situation. FIRST CHOICE for living-world play when the player discovers, \
    opens, enters, searches out, or otherwise needs a new place/situation that is not \
    already represented by a visible transition. Use this instead of set_scene for new \
    side rooms, hidden chambers, street/building interiors, points of interest, dungeon \
    points, and road encounters. If the player enters the generated place immediately, \
    set enter_after_commit=true so the tool creates the place, links it, and moves the \
    player there atomically. If the player only sees/learns the generated place without \
    entering it, set player_observed=true. The generator has its own context/thread and receives \
    recent anti-repeat keys, so it avoids repeating nearby motifs. It returns short \
    player-visible description, hidden GM-only notes, concrete features/choices/\
    consequences, optional exits, and an anti_repeat_key. Do not use it for NPC speech \
    or for teleporting the player; use move_player for travel along existing exits.",
        "parameters": {"type": "object", "properties": {
            "purpose": {"type": "string",
                        "enum": ["place", "local_place", "room", "travel_situation", "city_point", "village_point", "dungeon_point"],
                        "description": "What kind of thing to generate."},
            "request": {"type": "string",
                        "description": "Short Russian brief: why this is needed, what should roughly be present, and what must be avoided."},
            "target_place_id": {"type": "string",
                                "description": "Existing canon place to flesh out. Defaults to current place when commit=true."},
            "parent_place_id": {"type": "string",
                                "description": "Parent/current place when a new place should be created."},
            "route_transition_id": {"type": "string",
                                    "description": "Transition id for a road/travel situation."},
            "commit": {"type": "boolean",
                       "description": "When true, apply generated place details to canon. Default true."},
            "player_observed": {"type": "boolean",
                                "description": "True when the player can already see or directly learn this generated place/situation. This reveals only player-safe generated fields."},
            "enter_after_commit": {"type": "boolean",
                                   "description": "True when the latest player action enters the generated place now. The tool commits the place and moves the player through the new/current transition atomically."},
            "elapsed_minutes": {"type": "integer",
                                "description": "For travel situations: minutes already travelled before the interruption."},
            "remaining_minutes": {"type": "integer",
                                  "description": "For travel situations: minutes left to destination after this stop."},
            "route_time_minutes": {"type": "integer",
                                   "description": "For travel situations: total planned transition time before interruption modifiers."},
            "situation_type": {"type": "string",
                               "enum": ["good", "bad", "neutral", "mixed"],
                               "description": "Road-situation tone/type if already rolled."},
            "rarity": {"type": "string",
                       "enum": ["common", "uncommon", "rare", "legendary"],
                       "description": "Road-situation rarity if already rolled."},
        }, "required": ["purpose", "request"], "additionalProperties": false},
    }});
    let take_item = json!({"type": "function", "function": {
        "name": "take_item",
        "description":
            "Pick up an item that is PRESENT in the CURRENT SCENE STATE and move that SAME \
    object — with its details — into the player's inventory. WHEN TO CALL: the player takes, \
    grabs, pockets, loots, or picks up an item the scene already lists. Identify it by \
    item_id when you have it (the ONLY way to take an item that is not visible — a \
    GM-trusted path), otherwise by name, matched case-insensitively against the VISIBLE \
    scene items. If no visible item matches, the tool returns item_not_here with the list \
    of visible items — do NOT retry with inventory_add; instead narrate that the thing is \
    not here. If more than one visible item shares the name, the tool returns \
    ambiguous_item with candidates — pick one by item_id, never silently grab the first. \
    If the matched item is not portable (bolted down, too heavy, fixed scenery), the tool \
    returns not_portable: that rejection is FICTION, not a retry — narrate why it cannot be \
    taken and wait for the player, exactly like a rejected move. On success the scene item \
    is removed and appended to inventory as \"name — details\". DO NOT CALL for an item the \
    scene does not list (a gift, purchase, crafted or narratively-found object) — use \
    update_player_character with inventory_add for those. The result is compact structured \
    text with the taken item and the updated card, or a rejection reason to narrate.",
        "parameters": {"type": "object", "properties": {
            "item_id": {"type": "string",
                        "description": "Exact id of a current scene item; required to take an item that is not visible."},
            "name": {"type": "string",
                     "description": "Item name to match against VISIBLE scene items when no item_id is given."},
            "reason": {"type": "string",
                       "description": "Very short Russian reason for taking the item."},
        }, "required": [], "additionalProperties": false},
    }});
    let drop_item = json!({"type": "function", "function": {
        "name": "drop_item",
        "description":
            "Put an item DOWN from the player's inventory into the CURRENT scene, so the same \
    object with its details becomes a scene item others can see and pick up. WHEN TO CALL: \
    the player drops, sets down, leaves, plants, or hands off an item onto the current \
    place. Match the inventory entry by its NAME (the part before \" — \"), matched \
    case-insensitively; the details after \" — \" travel with it. If no inventory entry \
    matches, the tool returns unknown_item with the current inventory — do not invent an \
    item the player does not carry. On success the entry is removed from the card and a \
    visible, portable scene item is inserted at the given location (or \"рядом\" by \
    default). DO NOT CALL when an item is destroyed, consumed, or used up without leaving \
    anything in the scene — use update_player_character with inventory_remove for those. \
    The result is compact structured text with the dropped item and the updated card.",
        "parameters": {"type": "object", "properties": {
            "name": {"type": "string",
                     "description": "Name (head before \" — \") of the inventory entry to drop."},
            "location": {"type": "string",
                         "description": "Where in the scene the item ends up, e.g. 'на столе'. Defaults to 'рядом'."},
            "reason": {"type": "string",
                       "description": "Very short Russian reason for dropping the item."},
        }, "required": ["name"], "additionalProperties": false},
    }});
    let cast_spell = json!({"type": "function", "function": {
        "name": "cast_spell",
        "description":
            "Cast a spell the player character KNOWS, spending the spell slot and setting \
    concentration in the engine. WHEN TO CALL: the player casts one of the spells listed \
    on their card, once the fiction commits to the cast. Match by name (case-insensitive). \
    The engine is authoritative for slots and concentration: a cantrip (level 0) spends no \
    slot; a leveled spell spends one slot of its own level, or of slot_level when you cast \
    it at a higher level (an upcast never drops below the spell's own level). If no slot of \
    the needed level is free, the tool returns no_slots — that rejection is FICTION, not a \
    retry: narrate the fizzled or aborted cast (the words falter, the gesture fails) and \
    wait for the player, exactly like a rejected move. If the character does not know the \
    spell, the tool returns unknown_spell with the known-spell list — do NOT invent a spell \
    the card lacks; if it should be learnable, add it first via update_player_character. A \
    concentration spell replaces any concentration already held; the tool reports the \
    dropped one as concentration_ended so you can narrate the earlier effect lapsing. This \
    tool does NO dice, attack, save, or damage math — resolve those with roll_dice using \
    the spell's notation, and describe upcast/higher-level effects from the spell's effect \
    prose. To drop concentration WITHOUT a new cast, clear the concentration field via \
    update_player_character. The result is compact structured text with the spent slot \
    level, remaining slots, and concentration changes, or a rejection reason to narrate.",
        "parameters": {"type": "object", "properties": {
            "name": {"type": "string",
                     "description": "Name of a spell on the player's card, matched case-insensitively."},
            "slot_level": {"type": "integer",
                           "description": "Optional higher slot level to upcast into; ignored for cantrips and clamped up to the spell's own level. Omit to spend a slot of the spell's base level."},
            "reason": {"type": "string",
                       "description": "Very short Russian reason for casting the spell."},
        }, "required": ["name"], "additionalProperties": false},
    }});
    let generate_npc = json!({"type": "function", "function": {
        "name": "generate_npc",
        "description":
            "Create ONE new significant NPC when the story needs a named, recurring, or \
    consequential character the roster does not already provide. The dedicated character \
    generator drafts a full canon card — persona, goals, agenda, voice, and calibrated \
    mechanics — from your qualitative brief, then commits it and places the NPC in the \
    scene. WHEN TO CALL: the player engages a person who matters (a suspect, patron, \
    rival, informant, captain) and no present or rostered NPC fits — call this FIRST, \
    then move_npc / ask_npc to voice them. Do NOT generate background extras, one-line \
    vendors, or unnamed passersby — describe them inline in narration, and reuse a roster \
    NPC (move_npc + ask_npc) when one already fits. Pass a QUALITATIVE brief only: who \
    this is, why the story needs them, what they can do, and how their power compares to \
    the player. NEVER pass numeric stats — the generator sets every number itself from the \
    player sheet, roster, and power_tier. If the brief closely matches existing NPCs the \
    tool returns duplicate_candidates instead of generating: reuse one of them, or resend \
    the SAME call with retry=true and add to request why those candidates do not fit; \
    retry is honored ONLY directly after a duplicate_candidates result and is ignored \
    otherwise. A status=created result means the NPC already exists and is present — go \
    straight to ask_npc with the returned npc_id; never call generate_npc twice for one \
    person. Reflect the result into Russian prose; never name this tool to the player.",
        "parameters": {"type": "object", "properties": {
            "request": {"type": "string",
                        "description": "Russian qualitative brief: who this NPC is, why the story needs them, what they can do, and their power relative to the player. No numbers. On retry, also say why the suggested existing NPCs do not fit."},
            "role": {"type": "string",
                     "description": "Short Russian role or profession, e.g. 'бармен', 'капитан стражи'."},
            "name": {"type": "string",
                     "description": "Optional Russian name to fix; omit to let the generator name them."},
            "appearance": {"type": "string",
                           "description": "Optional Russian appearance to fix; omit to let the generator decide."},
            "power_tier": {"type": "string",
                           "enum": ["much_weaker", "weaker", "comparable", "stronger", "much_stronger"],
                           "description": "Qualitative power versus the player; the generator calibrates numbers to it."},
            "place_id": {"type": "string",
                         "description": "Canon place where the NPC lives or appears. Defaults to the player's current place."},
            "present": {"type": "boolean",
                        "description": "When true, place the NPC into the current scene now. Default true."},
            "retry": {"type": "boolean",
                      "description": "Force generation, valid ONLY as the immediate resend of a duplicate_candidates result; ignored on fresh requests. Default false."},
        }, "required": ["request", "role"], "additionalProperties": false},
    }});
    let read_state = json!({"type": "function", "function": {
        "name": "read_state",
        "description":
            "Read the CURRENT authoritative engine state on demand. State is delivered once \
    as the WORLD SNAPSHOT and thereafter only as tool-result deltas; when you are unsure of \
    the current time, scene, player sheet, roster, or public facts — because history is \
    stale or a snapshot scrolled off — call this and check BEFORE narrating, never guess. \
    Returns exactly the requested sections rendered from live world state; pure read, it \
    changes nothing. Sections: time (world clock/date), scene (current place, present NPCs, \
    items, exits), player (the player character sheet incl. GM notes), roster (the FULL NPC \
    roster — the snapshot lists only nearby/relevant NPCs), facts (current public facts). \
    Never reveal this tool or raw ids to the player.",
        "parameters": {"type": "object", "properties": {
            "sections": {
                "type": "array",
                "items": {"type": "string",
                          "enum": ["time", "scene", "player", "roster", "facts"]},
                "description": "Which state sections to read. At least one required."},
        }, "required": ["sections"], "additionalProperties": false},
    }});
    let long_rest = json!({"type": "function", "function": {
        "name": "long_rest",
        "description":
            "Take a full LONG REST, restoring the player and passing eight hours in the world. \
    WHEN TO CALL: the fiction commits to the player sleeping the night, taking a full \
    day's rest, or otherwise completing a long rest — a change the engine owns that has \
    no visible tool. This restores the player to full through the engine: all spell slots \
    back to their maximum, current HP up to maximum, and any concentration dropped; then \
    the world clock advances by 8 hours (480 minutes) through the same mechanics as \
    advance_time (canon clock and schedules), so do NOT also call advance_time for the \
    same rest. This is a LONG rest only — a SHORT rest is NOT this tool: for a short rest \
    call advance_time for the elapsed time and adjudicate any partial recovery yourself; \
    do NOT call long_rest for it. The result is compact structured text with the new \
    in-world time, the restored slots and HP, and whether concentration was dropped.",
        "parameters": {"type": "object", "properties": {
            "reason": {"type": "string",
                       "description": "Very short Russian reason for the long rest, e.g. 'ночёвка на постоялом дворе'."},
        }, "required": [], "additionalProperties": false},
    }});
    vec![
        move_player,
        world_debug,
        generate_location,
        take_item,
        drop_item,
        cast_spell,
        generate_npc,
        read_state,
        long_rest,
    ]
}

/// Scoped GM living-memory tools. `get_memory` and `note_memory` are direct
/// primary tools; consolidation is discoverable via tool_search. NPC personal
/// recall is intentionally not exposed here: NPCs get their own `remember` tool.
pub fn build_memory_gm_tools() -> Vec<Value> {
    let get_memory = json!({"type": "function", "function": {
        "name": "get_memory",
        "description":
            "Retrieve short, scoped living-world memory summaries after applying the access \
    gate BEFORE search/ranking. Use this when the GM needs to know what the player, a \
    specific NPC, a place/community, a faction, or GM-private canon may remember about \
    a topic. Default results return short summaries only. Do not reveal GM/private or \
    other-actor memory to the player unless the fiction establishes how the player \
    learns it. Live ids are strings from current context; schemas never enumerate \
    current NPCs/places/factions.",
        "parameters": {"type": "object", "properties": {
            "query": {"type": "string",
                      "description": "Topic to recall, in Russian or exact ids from context."},
            "scope": {"type": "string",
                      "description": "Access lens: player, gm, actor, place, settlement, region, faction, route, group, public. Use npc_id for actor scope when available."},
            "npc_id": {"type": "string",
                       "description": "NPC/actor id for scope=actor or npc-specific recall."},
            "scope_id": {"type": "string",
                         "description": "Id for place/settlement/region/faction/route/group scopes."},
            "max_results": {"type": "integer",
                            "description": "1..12 short summaries; default 5."},
            "include_cold": {"type": "boolean",
                             "description": "Include consumed/archived source memories for explicit drill-down/debug; default false."},
            "include_details": {"type": "boolean",
                                "description": "Return detailed cards, not just summaries. Use sparingly and never for ordinary player narration."},
        }, "required": ["query"], "additionalProperties": false},
    }});
    let note_memory = json!({"type": "function", "function": {
        "name": "note_memory",
        "description":
            "Write one short scoped living-world memory card after the fiction establishes \
    it. Use for NPC private memories, player-learned understanding, place/community \
    rumours, faction knowledge, GM-private secrets, or traces tied to events. Write a \
    compact summary for ordinary retrieval; put optional longer detail in details only \
    when a later explicit tool drill-down should be possible. This does not replace \
    canon facts/events; it records who remembers or can know what.",
        "parameters": {"type": "object", "properties": {
            "summary": {"type": "string",
                        "description": "Short Russian memory summary. This is what normal recall returns."},
            "details": {"type": "string",
                        "description": "Optional detailed card for explicit include_details lookup only."},
            "owner_scope": {"type": "string",
                            "description": "Owner scope string: actor:<id>, player, place:<id>, settlement:<id>, region:<id>, faction:<id>, public, gm_private, true_canon."},
            "visibility_scopes": {"type": "array", "items": {"type": "string"},
                                  "description": "Additional scopes allowed to recall it. Empty means only owner scope plus GM/debug."},
            "tier": {"type": "string", "enum": ["raw", "episode", "arc", "durable"],
                     "description": "Memory tier; default raw."},
            "truth_status": {"type": "string", "enum": ["actual", "claim", "rumor", "belief", "lie", "unknown"],
                             "description": "Whether this is truth, claim, rumour, belief, lie or unknown."},
            "topic_tags": {"type": "array", "items": {"type": "string"}},
            "place_ids": {"type": "array", "items": {"type": "string"}},
            "actor_ids": {"type": "array", "items": {"type": "string"}},
            "faction_ids": {"type": "array", "items": {"type": "string"}},
            "source_event_ids": {"type": "array", "items": {"type": "string"}},
            "source_memory_ids": {"type": "array", "items": {"type": "string"}},
            "entity_id": {"type": "string",
                          "description": "Optional NPC id this memory identifies. Required when known_name is set."},
            "known_name": {"type": "string",
                           "description": "Optional player-known name/label for entity_id when the fiction establishes what the player may call a specific NPC."},
            "confidence": {"type": "integer", "description": "0..100 optional confidence."},
            "reason": {"type": "string", "description": "Short GM-internal reason for the note."},
        }, "required": ["summary", "owner_scope"], "additionalProperties": false},
    }});
    let consolidate_memory = json!({"type": "function", "function": {
        "name": "consolidate_memory",
        "description":
            "Create a higher-tier memory crystal from existing source memory ids. Source \
    memories are NOT deleted: they are marked cold/consumed_by the crystal and remain \
    available for explicit debug/drill-down. Use when several raw/episode memories \
    should stop being injected separately and become one short stable recollection.",
        "parameters": {"type": "object", "properties": {
            "source_memory_ids": {"type": "array", "items": {"type": "string"},
                                  "description": "Existing memories to consume into the crystal."},
            "summary": {"type": "string",
                        "description": "Short Russian crystal summary."},
            "details": {"type": "string",
                        "description": "Optional detailed derived card."},
            "owner_scope": {"type": "string",
                            "description": "Scope that owns the crystal."},
            "visibility_scopes": {"type": "array", "items": {"type": "string"}},
            "tier": {"type": "string", "enum": ["episode", "arc", "durable"],
                     "description": "Target tier; default episode."},
            "truth_status": {"type": "string", "enum": ["actual", "claim", "rumor", "belief", "lie", "unknown"]},
            "topic_tags": {"type": "array", "items": {"type": "string"}},
            "reason": {"type": "string"},
        }, "required": ["source_memory_ids", "summary", "owner_scope"], "additionalProperties": false},
    }});
    vec![get_memory, note_memory, consolidate_memory]
}

/// Static tools available only to an NPC sub-agent while it is producing its own
/// response. Runtime binds the current npc_id; the model cannot ask as another
/// actor.
pub fn build_npc_tools() -> Vec<Value> {
    let remember = json!({"type": "function", "function": {
        "name": "remember",
        "description":
            "Recall what YOU, this NPC, can personally know or plausibly access about a \
    topic before answering. Runtime binds the current NPC identity and filters by that \
    actor's private memory, local place/community rumors, faction memory, and public \
    memory before ranking. This cannot read another NPC's private thoughts or GM-private \
    truth. Use it when the current situation asks what you remember, know, heard, saw, \
    or believe about a topic. Results are short summaries only.",
        "parameters": {"type": "object", "properties": {
            "query": {"type": "string",
                      "description": "What you are trying to remember, in Russian or exact names from the scene."},
            "max_results": {"type": "integer",
                            "description": "1..8 short summaries; default 5."},
            "include_cold": {"type": "boolean",
                             "description": "Include consumed raw source memories only for explicit audit/debug; default false."},
        }, "required": ["query"], "additionalProperties": false},
    }});
    let npc_note_memory = json!({"type": "function", "function": {
        "name": "npc_note_memory",
        "description":
            "Privately record what YOU, this NPC, now remember from this interaction. \
    Runtime binds the current NPC identity; you cannot write memory for another actor \
    and this tool never makes a rumour globally public. Use short Russian text only. \
    Public rumour spread must be established by the GM/world simulation separately.",
        "parameters": {"type": "object", "properties": {
            "text": {"type": "string",
                     "description": "Short Russian memory from this NPC's point of view."},
            "kind": {"type": "string",
                     "enum": ["interaction", "observation", "belief", "goal", "relationship", "rumor", "secret", "other"],
                     "description": "Memory category. Default interaction."},
            "about": {"type": "string",
                      "description": "Short topic or target, e.g. player, an NPC id/name, a place, or a faction."},
            "privacy": {"type": "string",
                        "enum": ["private", "scene", "shared", "public"],
                        "description": "Requested privacy. Runtime keeps the note actor-private; non-private values are recorded as requested_privacy metadata only."},
            "anchors": {"type": "array", "items": {"type": "string"},
                        "description": "Optional anchors like place:<id>, actor:<id>, faction:<id>, route:<id>. They tag the memory but do not grant public visibility."},
        }, "required": ["text"], "additionalProperties": false},
    }});
    let npc_recall_relationship = json!({"type": "function", "function": {
        "name": "npc_recall_relationship",
        "description":
            "Recall your own attitude/history toward a target before answering. Runtime \
    binds this NPC and filters through the same actor-private memory gate as remember. \
    Use for the player or another named NPC when relationship history matters.",
        "parameters": {"type": "object", "properties": {
            "target": {"type": "string",
                       "description": "Target to recall relationship with. Use player, an NPC id, or an exact visible name."},
            "max_results": {"type": "integer",
                            "description": "1..8 short summaries; default 5."},
        }, "required": ["target"], "additionalProperties": false},
    }});
    vec![remember, npc_note_memory, npc_recall_relationship]
}

pub fn build_loader_gm_tools() -> Vec<Value> {
    vec![load_tool_schema_tool(), invoke_loaded_tool()]
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

/// `gm_tool_catalog()` — the MODEL-FACING executable registry keyed by tool name.
/// The static catalog (`build_gm_tools`) stays as a stable cache prefix and the
/// living-world canon tools (`build_canon_gm_tools` — `move_player`,
/// `world_debug`) are APPENDED after it, so the canon tools are part of the real
/// catalog the GM model sees while the static prefix bytes stay untouched.
pub fn gm_tool_catalog() -> indexmap::IndexMap<String, Value> {
    let mut map = indexmap::IndexMap::new();
    for tool in build_gm_tools() {
        map.insert(tool_name(&tool), tool);
    }
    for tool in build_canon_gm_tools() {
        map.insert(tool_name(&tool), tool);
    }
    for tool in build_memory_gm_tools() {
        map.insert(tool_name(&tool), tool);
    }
    for tool in build_loader_gm_tools() {
        map.insert(tool_name(&tool), tool);
    }
    map
}

/// `initial_gm_tool_names(include_player_options_tool)`.
pub fn initial_gm_tool_names(include_player_options_tool: bool) -> BTreeSet<String> {
    let mut names: BTreeSet<String> = INITIAL_GM_TOOL_NAMES
        .iter()
        .map(|s| s.to_string())
        .collect();
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
    let catalog = catalog_entries(include_player_options_tool);
    match loaded_tool_names {
        None => catalog.into_iter().map(|(_, t)| t).collect(),
        Some(loaded) => {
            let visible = effective_loaded_tools(Some(loaded), include_player_options_tool);
            catalog
                .into_iter()
                .filter(|(name, _)| visible.contains(name))
                .map(|(_, t)| t)
                .collect()
        }
    }
}

pub fn build_gm_tools_for_native_tool_search(include_player_options_tool: bool) -> Vec<Value> {
    let catalog = catalog_entries(include_player_options_tool);
    let visible = initial_gm_tool_names(include_player_options_tool);
    let mut tools = Vec::new();
    let mut deferred = Vec::new();

    for (name, tool) in catalog {
        if is_loader_tool_name(&name) {
            continue;
        }
        if visible.contains(&name) {
            tools.push(tool);
        } else {
            deferred.push(mark_tool_deferred(tool));
        }
    }

    if !deferred.is_empty() {
        tools.push(json!({
            "type": "namespace",
            "name": "gm_deferred",
            "description": "Deferred GM tools for scene movement, NPC profiles, NPC whereabouts, canon debug, memory writing/consolidation, and other non-primary GM operations. Use tool_search to load one only when needed.",
            "tools": deferred,
        }));
        tools.push(json!({"type": "tool_search"}));
    }

    tools
}

fn mark_tool_deferred(mut tool: Value) -> Value {
    if let Value::Object(ref mut obj) = tool {
        obj.insert("defer_loading".into(), Value::Bool(true));
    }
    tool
}

fn catalog_entries(include_player_options_tool: bool) -> Vec<(String, Value)> {
    gm_tool_catalog()
        .into_iter()
        .filter(|(name, _)| include_player_options_tool || name != PLAYER_OPTIONS_TOOL_NAME)
        .collect()
}

fn effective_loaded_tools(
    _loaded_tool_names: Option<&BTreeSet<String>>,
    include_player_options_tool: bool,
) -> BTreeSet<String> {
    initial_gm_tool_names(include_player_options_tool)
}

fn is_loader_tool_name(name: &str) -> bool {
    name == TOOL_SEARCH_TOOL_NAME
        || name == LOAD_TOOL_SCHEMA_TOOL_NAME
        || name == INVOKE_LOADED_TOOL_NAME
}

struct ToolSearchMetadata {
    name: &'static str,
    title: &'static str,
    description: &'static str,
    keywords: &'static [&'static str],
    aliases: &'static [&'static str],
    capabilities: &'static [&'static str],
}

const TOOL_SEARCH_METADATA: [ToolSearchMetadata; 22] = [
    ToolSearchMetadata {
        name: "ask_npc",
        title: "Ask NPC",
        description: "Get one present NPC's visible speech and action.",
        keywords: &["npc", "speech", "reaction", "dialogue", "допрос"],
        aliases: &["talk", "question", "угроза"],
        capabilities: &["npc_dialogue", "visible_reaction"],
    },
    ToolSearchMetadata {
        name: "roll_dice",
        title: "Roll Dice",
        description: "Resolve uncertain checks, saves, attacks, damage, or chance.",
        keywords: &["dice", "d20", "check", "roll", "проверка"],
        aliases: &["кубик", "бросок"],
        capabilities: &["mechanical_resolution", "stakes_locked_roll"],
    },
    ToolSearchMetadata {
        name: "get_world_fact",
        title: "World Fact",
        description: "Look up player-safe lore, leads, testimony, and known facts.",
        keywords: &["fact", "lore", "memory", "rumor", "улика"],
        aliases: &["rag", "lookup", "что известно"],
        capabilities: &["player_safe_lookup", "source_dedup"],
    },
    ToolSearchMetadata {
        name: "get_memory",
        title: "Get Memory",
        description: "Read short scoped living-world memory summaries.",
        keywords: &["memory", "scope", "recall", "secret", "rumor"],
        aliases: &["вспомнить", "что знает"],
        capabilities: &["scoped_memory", "access_filtered_recall"],
    },
    ToolSearchMetadata {
        name: "note_memory",
        title: "Note Memory",
        description: "Write one scoped living-world memory card.",
        keywords: &["write", "memory", "note", "rumor", "secret"],
        aliases: &["save memory", "записать память"],
        capabilities: &["memory_write", "scoped_visibility"],
    },
    ToolSearchMetadata {
        name: "consolidate_memory",
        title: "Consolidate Memory",
        description: "Create a higher-tier memory crystal and mark sources cold.",
        keywords: &["crystal", "consolidate", "summary", "episode", "archive"],
        aliases: &["сжать память", "memory crystal"],
        capabilities: &["hierarchical_memory", "source_retention"],
    },
    ToolSearchMetadata {
        name: "update_player_character",
        title: "Player Card",
        description: "Update the player character card after established fiction changes it.",
        keywords: &["player", "character", "hp", "inventory", "condition"],
        aliases: &["pc card", "лист персонажа"],
        capabilities: &["player_state_update"],
    },
    ToolSearchMetadata {
        name: "advance_time",
        title: "Advance Time",
        description: "Move the world clock by a known elapsed duration.",
        keywords: &["time", "clock", "minutes", "wait", "время"],
        aliases: &["подождать", "спустя"],
        capabilities: &["world_clock"],
    },
    ToolSearchMetadata {
        name: "ask_player",
        title: "Ask Player",
        description: "Show quick-reply choices or a focused question to the player.",
        keywords: &["options", "choices", "player", "buttons", "варианты"],
        aliases: &["quick replies", "что делать"],
        capabilities: &["player_prompt", "quick_replies"],
    },
    ToolSearchMetadata {
        name: "move_npc",
        title: "Move NPC",
        description: "Move an NPC into, out of, or within the current scene.",
        keywords: &["npc", "scene", "presence", "enters", "leaves"],
        aliases: &["set_npc_presence", "вошел", "ушел"],
        capabilities: &["scene_presence", "npc_movement"],
    },
    ToolSearchMetadata {
        name: "set_npc_whereabouts",
        title: "NPC Whereabouts",
        description: "Record an NPC's known or likely offscreen location.",
        keywords: &["npc", "whereabouts", "absent", "location", "где"],
        aliases: &["offscreen", "местонахождение"],
        capabilities: &["offscreen_location", "whereabouts_memory"],
    },
    ToolSearchMetadata {
        name: "get_npc_profile",
        title: "NPC Profile",
        description: "Read player-safe NPC mechanics, persona, voice, or profile fields.",
        keywords: &["npc", "profile", "stats", "mechanics", "persona"],
        aliases: &["card", "анкета", "статы"],
        capabilities: &["npc_mechanics", "profile_fields"],
    },
    ToolSearchMetadata {
        name: "set_scene",
        title: "Set Scene",
        description: "Compatibility/debug fallback for applying a fully authored scene patch; use generate_location for new living-world places.",
        keywords: &["scene", "debug", "fallback", "patch", "legacy"],
        aliases: &["manual scene patch"],
        capabilities: &["scene_patch_fallback"],
    },
    ToolSearchMetadata {
        name: "move_player",
        title: "Move Player",
        description: "Move the player through a visible canon transition.",
        keywords: &["player", "move", "transition", "exit", "travel"],
        aliases: &["идти", "выход", "дверь"],
        capabilities: &["canon_travel", "transition_validation"],
    },
    ToolSearchMetadata {
        name: "world_debug",
        title: "World Debug",
        description: "Read the living-world canon graph and causal log for debugging.",
        keywords: &["debug", "canon", "graph", "causal", "snapshot"],
        aliases: &["отладка", "replay"],
        capabilities: &["debug_dump", "causal_log"],
    },
    ToolSearchMetadata {
        name: "generate_location",
        title: "Generate Location",
        description: "First-choice living-world tool to draft and commit a new location, room, point of interest, or road situation through a dedicated generator agent.",
        keywords: &["location", "scene", "room", "travel", "encounter", "anti-repeat"],
        aliases: &["генератор", "ситуация", "локация"],
        capabilities: &["location_generation", "road_situation_generation", "anti_repeat_context"],
    },
    ToolSearchMetadata {
        name: "take_item",
        title: "Take Item",
        description: "Move a present scene item's body into the player's inventory.",
        keywords: &["take", "pick", "grab", "loot", "item"],
        aliases: &["взять", "подобрать", "предмет"],
        capabilities: &["scene_to_inventory", "item_transfer"],
    },
    ToolSearchMetadata {
        name: "drop_item",
        title: "Drop Item",
        description: "Put an inventory item down into the current scene.",
        keywords: &["drop", "put", "leave", "item", "inventory"],
        aliases: &["выложить", "бросить", "оставить"],
        capabilities: &["inventory_to_scene", "item_transfer"],
    },
    ToolSearchMetadata {
        name: "cast_spell",
        title: "Cast Spell",
        description: "Spend a spell slot and set concentration for a known spell.",
        keywords: &["spell", "cast", "slot", "concentration", "cantrip"],
        aliases: &["заклинание", "каст", "колдовать"],
        capabilities: &["spell_slot_spend", "concentration_tracking"],
    },
    ToolSearchMetadata {
        name: "generate_npc",
        title: "Generate NPC",
        description: "Draft and commit one new significant NPC through a dedicated character generator agent.",
        keywords: &["npc", "character", "significant", "named", "нпс", "персонаж"],
        aliases: &["создать нпс", "новый персонаж", "герой"],
        capabilities: &["npc_generation", "power_calibration", "duplicate_gate"],
    },
    ToolSearchMetadata {
        name: "read_state",
        title: "Read State",
        description: "Read current engine state on demand — time, scene, player sheet, full roster, or public facts.",
        keywords: &["state", "time", "scene", "roster", "facts", "состояние", "сцена"],
        aliases: &["проверить состояние", "текущее время", "полный ростер"],
        capabilities: &["state_read", "roster_full", "no_mutation"],
    },
    ToolSearchMetadata {
        name: "long_rest",
        title: "Long Rest",
        description: "Take a full long rest: restore spell slots and HP, drop concentration, advance 8 hours.",
        keywords: &["rest", "sleep", "recover", "restore", "отдых", "выспаться"],
        aliases: &["долгий отдых", "ночёвка", "night sleep"],
        capabilities: &["full_restore", "slot_hp_recovery", "world_clock"],
    },
];

fn metadata_for(name: &str) -> Option<&'static ToolSearchMetadata> {
    TOOL_SEARCH_METADATA.iter().find(|meta| meta.name == name)
}

fn title_from_name(name: &str) -> String {
    name.split('_')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn value_strings(values: &[&str]) -> Value {
    Value::Array(
        values
            .iter()
            .map(|value| Value::String((*value).to_string()))
            .collect(),
    )
}

fn metadata_text(name: &str) -> String {
    match metadata_for(name) {
        Some(meta) => [
            meta.name.to_string(),
            meta.title.to_string(),
            meta.description.to_string(),
            meta.keywords.join(" "),
            meta.aliases.join(" "),
            meta.capabilities.join(" "),
        ]
        .join(" "),
        None => title_from_name(name),
    }
}

fn tool_search_card(name: &str, tool: &Value, score: i64, loaded: bool) -> Value {
    let fallback_description = short_tool_description(tool, 180);
    let (title, description, keywords, aliases, capabilities) = match metadata_for(name) {
        Some(meta) => (
            meta.title.to_string(),
            meta.description.to_string(),
            value_strings(meta.keywords),
            value_strings(meta.aliases),
            value_strings(meta.capabilities),
        ),
        None => (
            title_from_name(name),
            fallback_description,
            Value::Array(Vec::new()),
            Value::Array(Vec::new()),
            Value::Array(Vec::new()),
        ),
    };
    json!({
        "name": name,
        "title": title,
        "description": description,
        "keywords": keywords,
        "aliases": aliases,
        "capabilities": capabilities,
        "score": score,
        "loaded": loaded,
        "load_tool": LOAD_TOOL_SCHEMA_TOOL_NAME,
        "invoke_tool": INVOKE_LOADED_TOOL_NAME,
        "load_schema": {
            "tool": LOAD_TOOL_SCHEMA_TOOL_NAME,
            "name": name,
            "hint": format!("Call {LOAD_TOOL_SCHEMA_TOOL_NAME} with name=\"{name}\" to load the full schema, then call {INVOKE_LOADED_TOOL_NAME} if the tool is not directly visible."),
        },
    })
}

fn short_tool_description(tool: &Value, limit: usize) -> String {
    let ws = Regex::new(r"\s+").unwrap();
    let text = ws
        .replace_all(&tool_description(tool), " ")
        .trim()
        .to_string();
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
        metadata_text(&name),
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

fn gm_tool_search_next(state: &str) -> String {
    render_prompt(PromptId::GmToolSearchNext, json!({ "state": state }))
        .expect("embedded GM tool-search next-step prompt must render")
}

fn gm_tool_schema_next(status: &str) -> String {
    render_prompt(PromptId::GmToolSchemaNext, json!({ "status": status }))
        .expect("embedded GM tool-schema next-step prompt must render")
}

/// `search_gm_tools(world, query, max_results, already_loaded, include_player_options_tool)`.
pub fn search_gm_tools(
    query: &str,
    max_results: i64,
    already_loaded: Option<&BTreeSet<String>>,
    include_player_options_tool: bool,
) -> Value {
    let catalog = catalog_entries(include_player_options_tool);
    let already_loaded = effective_loaded_tools(already_loaded, include_player_options_tool);
    let searchable: Vec<(String, Value)> = catalog
        .iter()
        .filter(|(name, _)| !is_loader_tool_name(name) && !already_loaded.contains(name))
        .cloned()
        .collect();

    let raw_query = query.trim().to_string();
    if raw_query.is_empty() {
        return json!({
            "query": raw_query,
            "matches": [],
            "already_loaded": [],
            "missing": [],
            "total_searchable_tools": searchable.len(),
            "load_tool": LOAD_TOOL_SCHEMA_TOOL_NAME,
            "invoke_tool": INVOKE_LOADED_TOOL_NAME,
            "next": gm_tool_search_next("empty"),
            "message": "Запрос пустой. Используй keywords или select:tool_name.",
        });
    }
    // limit = max(1, min(int(max_results or 5), 10))
    let limit_src = if max_results == 0 { 5 } else { max_results };
    let limit = limit_src.clamp(1, 10) as usize;

    let mut selected: Vec<(String, i64)> = Vec::new();
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
            if !is_loader_tool_name(name) {
                all_tool_names.insert(name.to_lowercase(), name.clone());
            }
        }
        for item in requested {
            match all_tool_names.get(&item.to_lowercase()) {
                None => missing.push(item),
                Some(name) => {
                    if already_loaded.contains(name) {
                        known_loaded.push(name.clone());
                    }
                    selected.push((name.clone(), 100));
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
        selected = scored
            .into_iter()
            .take(limit)
            .map(|(s, n)| (n, s))
            .collect();
    }

    let catalog_map: indexmap::IndexMap<String, Value> = catalog.into_iter().collect();
    let mut matches: Vec<Value> = Vec::new();
    for (name, score) in selected.iter().take(limit) {
        if let Some(tool) = catalog_map.get(name) {
            matches.push(tool_search_card(
                name,
                tool,
                *score,
                already_loaded.contains(name),
            ));
        }
    }
    let already_loaded_result: Vec<String> = {
        let s: BTreeSet<String> = known_loaded.into_iter().collect();
        s.into_iter().collect()
    };
    let message = if !matches.is_empty() {
        "Найдены компактные карточки инструментов. Вызови load_tool_schema с точным name, \
чтобы загрузить одну полную схему."
    } else if !already_loaded_result.is_empty() {
        "Запрошенные инструменты уже доступны в текущем шаге ГМ."
    } else {
        "Подходящих инструментов не найдено. Попробуй select:tool_name или другие ключевые слова."
    };

    json!({
        "query": raw_query,
        "matches": matches,
        "already_loaded": already_loaded_result,
        "missing": missing,
        "total_searchable_tools": searchable.len(),
        "load_tool": LOAD_TOOL_SCHEMA_TOOL_NAME,
        "invoke_tool": INVOKE_LOADED_TOOL_NAME,
        "next": gm_tool_search_next("results"),
        "message": message,
    })
}

pub fn load_gm_tool_schema(
    name: &str,
    already_loaded: Option<&BTreeSet<String>>,
    include_player_options_tool: bool,
) -> Value {
    let raw_name = name.trim().to_string();
    if raw_name.is_empty() {
        return json!({
            "status": "invalid",
            "name": raw_name,
            "loaded_schema": null,
            "invoke_tool": INVOKE_LOADED_TOOL_NAME,
            "already_loaded": [],
            "missing": [],
            "schema": null,
            "next": gm_tool_schema_next("invalid"),
            "message": "Имя инструмента пустое. Передай точный name из tool_search.",
        });
    }

    let already_loaded = effective_loaded_tools(already_loaded, include_player_options_tool);
    let catalog = catalog_entries(include_player_options_tool);
    let mut loadable: indexmap::IndexMap<String, (String, Value)> = indexmap::IndexMap::new();
    for (tool_name, schema) in catalog {
        if !is_loader_tool_name(&tool_name) {
            loadable.insert(tool_name.to_lowercase(), (tool_name, schema));
        }
    }

    let Some((canonical_name, schema)) = loadable.get(&raw_name.to_lowercase()).cloned() else {
        return json!({
            "status": "missing",
            "name": raw_name,
            "loaded_schema": null,
            "invoke_tool": INVOKE_LOADED_TOOL_NAME,
            "already_loaded": [],
            "missing": [raw_name],
            "schema": null,
            "next": gm_tool_schema_next("missing"),
            "message": "Инструмент не найден или не загружается через load_tool_schema.",
        });
    };

    if already_loaded.contains(&canonical_name) {
        return json!({
            "status": "already_loaded",
            "name": canonical_name,
            "loaded_schema": canonical_name,
            "invoke_tool": INVOKE_LOADED_TOOL_NAME,
            "already_loaded": [canonical_name],
            "missing": [],
            "schema": schema,
            "next": gm_tool_schema_next("already_loaded"),
            "message": "Инструмент уже доступен; схема возвращена для подтверждения.",
        });
    }

    json!({
        "status": "loaded_schema",
        "name": canonical_name,
        "loaded_schema": canonical_name,
        "invoke_tool": INVOKE_LOADED_TOOL_NAME,
        "already_loaded": [],
        "missing": [],
        "schema": schema,
        "next": gm_tool_schema_next("loaded_schema"),
        "message": "Схема инструмента загружена в конец контекста; список top-level tools не изменён.",
    })
}
