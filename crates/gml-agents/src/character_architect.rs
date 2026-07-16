//! Dedicated CHARACTER architect agent (mirrors [`crate::story_architect`]).
//!
//! This is the character-package sibling of the world/story architects: a
//! planning chat that helps the user author a reusable PLAYER CHARACTER (`.gmchar`
//! package payload). Like the other two it is a THIN CONFIG over the shared
//! [`crate::architect_runner`] loop — same agent discipline (think → tool →
//! reply), same streaming, same stats — it only swaps the prompt, the tools, and
//! the draft-folding for the character-sheet shape.
//!
//! A character MAY be based on a world and/or a story (or stand alone): the
//! caller passes optional read-only context blocks (built by
//! [`character_architect_world_block`] / [`character_architect_story_block`])
//! that ride as extra system messages, exactly like the story architect's bound
//! world bible. The blocks are PUBLIC-ONLY: the character architect talks to the
//! PLAYER, so GM secrets (`hidden_premise`/`hidden_secrets`/`hidden_truth`) are
//! stripped — basing a hero on a story must not spoil it. One system template
//! renders two variants, picked by whether blocks are present:
//! a standalone conversation spends no tokens on base-block instructions, and a
//! based one doesn't carry the contradictory "standalone" rule. The binding is
//! fixed at creation, so the pick is stable across a conversation's turns.
//!
//! The draft is the `.gmchar` payload's `player_character` object — a FLAT sheet
//! (`gml-world/src/model.rs::PlayerCharacter`): name/pronouns/class_role/level/
//! background/age/physical_type + ability & skill maps + hp/ac + inventory/
//! equipment/features string lists + spells (SpellEntry objects) + flat
//! spell-slot maps. The merge is a shallow top-level merge with a special-case
//! deep merge for the nested stat objects (abilities, skills, saving_throws, hp,
//! spell_slots, spell_slots_max) so a partial re-draft refines instead of nuking.

use std::sync::LazyLock;

use serde_json::{json, Map, Value};

use gml_llm::{Backend, BackendError};
use gml_prompts::{render_prompt, PromptId};

use crate::architect_runner::{
    architect_messages_with_system_blocks, architect_turn, ArchitectConfig, ArchitectOutput,
    ArchitectStream, ToolApplication,
};

/// The character architect turn output — the generic [`ArchitectOutput`] under
/// the domain name (mirrors `StoryArchitectOutput`).
pub type CharacterArchitectOutput = ArchitectOutput;

/// Standalone character-architect prompt retained as a public constant for
/// compatibility. Runtime assembly uses [`character_architect_system`].
pub const CHARACTER_ARCHITECT_SYSTEM: &str = gml_prompts::CHARACTER_ARCHITECT_SYSTEM;

/// Based character-architect prompt retained as a public constant for
/// compatibility. Runtime assembly uses [`character_architect_system`].
pub const CHARACTER_ARCHITECT_SYSTEM_BASED: &str = gml_prompts::CHARACTER_ARCHITECT_SYSTEM_BASED;

static CHARACTER_ARCHITECT_SYSTEM_STANDALONE_RENDERED: LazyLock<String> = LazyLock::new(|| {
    render_prompt(PromptId::CharacterArchitectSystem, json!({"based": false}))
        .expect("embedded standalone character architect system prompt must render")
});

static CHARACTER_ARCHITECT_SYSTEM_BASED_RENDERED: LazyLock<String> = LazyLock::new(|| {
    render_prompt(PromptId::CharacterArchitectSystem, json!({"based": true}))
        .expect("embedded based character architect system prompt must render")
});

/// Rendered character-architect system rules. The mode paragraph is the only
/// difference; each variant is rendered once so its cache prefix stays stable.
pub fn character_architect_system(based: bool) -> &'static str {
    if based {
        CHARACTER_ARCHITECT_SYSTEM_BASED_RENDERED.as_str()
    } else {
        CHARACTER_ARCHITECT_SYSTEM_STANDALONE_RENDERED.as_str()
    }
}

// The character sheet field families the tool schema targets (documented in each
// tool description rather than looked up here — the ops route by key, not by a
// static membership set):
//   scalars: name, pronouns, class_role, level, background, age, physical_type,
//     distinctive_features, personality, values, gm_notes, ac, speed, senses,
//     languages, passive_perception, concentration, life_status
//   objects: abilities, skills, saving_throws, hp, spell_slots, spell_slots_max
//   lists: inventory, equipment, features, spells

/// The nested stat objects that deep-merge key-by-key on a partial re-draft (a
/// `{abilities: {STR: 14}}` refines rather than replacing the whole map).
const PC_OBJECT_FIELDS: [&str; 6] = [
    "abilities",
    "skills",
    "saving_throws",
    "hp",
    "spell_slots",
    "spell_slots_max",
];

// =========================================================================
// public message / tool builders (mirror the story architect surface)
// =========================================================================

/// The system-prompt variant for a conversation: BASED only when at least one
/// real (non-blank) context block rides along, else the standalone prompt that
/// never mentions base blocks (no tokens for an unused feature).
///
/// The standalone paragraph is an ACTIVE anti-tie instruction, so a character
/// whose refs exist but whose base material is unavailable (packages deleted /
/// bible empty) must NOT fall through to it — the sheet is already tied. The
/// server keeps such a conversation BASED by substituting
/// [`character_architect_base_unavailable_block`] for the missing material.
fn character_architect_prompt(context_blocks: &[String]) -> &'static str {
    character_architect_system(context_blocks.iter().any(|b| !b.trim().is_empty()))
}

/// The fallback base block for a character whose `world_ref`/`story_ref` are
/// recorded but whose base material is UNAVAILABLE (packages deleted, or
/// nothing public survives the whitelists). Static text (byte-stable → the
/// cache prefix holds) that keeps the conversation on the BASED prompt and
/// tells the model to PRESERVE the sheet's existing ties instead of the
/// standalone prompt's "do NOT tie" — which would contradict the sheet.
pub fn character_architect_base_unavailable_block() -> String {
    render_prompt(PromptId::CharacterArchitectBaseUnavailable, json!({}))
        .expect("embedded character architect unavailable-base prompt must render")
}

/// Assemble the character-architect request messages: system (variant picked by
/// the blocks) + the optional read-only base world/story blocks + filtered
/// history + the user message. The blocks (possibly empty — a standalone hero)
/// ride as STABLE system messages so the cache prefix holds across turns,
/// mirroring the story architect's bound world bible.
///
/// CACHE INVARIANT: the tail user message is the RAW user text — byte-equal to
/// the history entry the server stores for this turn. State never rides in
/// messages; the model reads it via read_player_character.
pub fn character_architect_messages(
    history: &[Value],
    context_blocks: &[String],
    user_text: &str,
) -> Vec<Value> {
    architect_messages_with_system_blocks(
        character_architect_prompt(context_blocks),
        context_blocks,
        history,
        json!({"role": "user", "content": user_text.trim()}),
    )
}

pub fn character_architect_tools() -> Vec<Value> {
    vec![
        draft_player_character_schema(),
        edit_player_character_schema(),
        read_player_character_schema(),
    ]
}

/// The PUBLIC `world_lore` fields allowed into the BASE WORLD block — a
/// WHITELIST (mirrors the story block below): unlike the GM-trusted story
/// architect, the character architect converses with the PLAYER, so basing a
/// hero on a world must not spoil its mysteries. The canonical GM-only fields
/// (`hidden_premise`/`hidden_secrets`), the image prompt/URL fields (token
/// bloat), and ANY ad-hoc key the world architect may have folded into the
/// bible (its draft/edit tools accept arbitrary keys — e.g. a user-requested
/// `dm_notes`) stay private until deliberately added here.
const CHARACTER_WORLD_PUBLIC_FIELDS: [&str; 26] = [
    "name",
    "genre",
    "tone",
    "scale",
    "public_premise",
    "dogmas",
    "world_laws",
    "inhabitants",
    "creatures",
    "power_sources",
    "technologies",
    "taboos",
    "conflicts",
    "inspirations",
    "regions",
    "power_centers",
    "religions",
    "gods",
    "cultures",
    "history",
    "economy",
    "daily_life",
    "story_hooks",
    "location_rules",
    "prohibited_elements",
    "open_questions",
];

/// Build the read-only BASE WORLD context block from a world's `world_lore`
/// object — the PUBLIC bible the hero is grounded in. Only
/// [`CHARACTER_WORLD_PUBLIC_FIELDS`] pass (whitelist; empty values dropped).
/// When NOTHING public survives (blank/non-object lore) the block is an EMPTY
/// STRING, not a heading over `{}` — the runner drops blank blocks, so no
/// tokens are spent on instructions with no material (the server substitutes
/// [`character_architect_base_unavailable_block`] when refs exist, keeping the
/// conversation on the BASED prompt).
pub fn character_architect_world_block(world_lore: &Value) -> String {
    let source = match world_lore {
        Value::Object(map) => map.clone(),
        _ => Map::new(),
    };
    let mut lore = Map::new();
    for key in CHARACTER_WORLD_PUBLIC_FIELDS {
        if let Some(v) = source.get(key) {
            let keep = match v {
                Value::String(s) => !s.trim().is_empty(),
                Value::Array(a) => !a.is_empty(),
                Value::Null => false,
                _ => true,
            };
            if keep {
                lore.insert(key.to_string(), v.clone());
            }
        }
    }
    if lore.is_empty() {
        return String::new();
    }
    let json =
        serde_json::to_string_pretty(&Value::Object(lore)).unwrap_or_else(|_| "{}".to_string());
    render_prompt(
        PromptId::CharacterArchitectWorldReference,
        json!({"json": json}),
    )
    .expect("embedded character architect world-reference prompt must render")
}

/// The PUBLIC story fields allowed into the BASE STORY block — a WHITELIST, so a
/// new seed field is private until deliberately added here. `hidden_truth`,
/// NPCs (they carry secrets) and state records never pass.
const CHARACTER_STORY_PUBLIC_FIELDS: [&str; 4] =
    ["title", "description", "story_brief", "public_intro"];

/// Build the read-only BASE STORY context block from a story envelope's public
/// fields (`title`/`description` from the envelope, `story_brief`/`public_intro`
/// from its seed — the same fields the player-facing catalog exposes). The
/// caller passes a combined object; only [`CHARACTER_STORY_PUBLIC_FIELDS`] pass
/// through. When nothing public survives the block is an EMPTY STRING (dropped
/// by the runner) — never a heading over `{}`.
pub fn character_architect_story_block(story_public: &Value) -> String {
    let source = match story_public {
        Value::Object(map) => map.clone(),
        _ => Map::new(),
    };
    let mut public = Map::new();
    for key in CHARACTER_STORY_PUBLIC_FIELDS {
        if let Some(v) = source.get(key) {
            let keep = match v {
                Value::String(s) => !s.trim().is_empty(),
                Value::Null => false,
                _ => true,
            };
            if keep {
                public.insert(key.to_string(), v.clone());
            }
        }
    }
    if public.is_empty() {
        return String::new();
    }
    let json =
        serde_json::to_string_pretty(&Value::Object(public)).unwrap_or_else(|_| "{}".to_string());
    render_prompt(
        PromptId::CharacterArchitectStoryReference,
        json!({"json": json}),
    )
    .expect("embedded character architect story-reference prompt must render")
}

/// The `read_player_character` tool: return the FULL current text of the named
/// sheet sections.
fn read_player_character_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "read_player_character",
            "description": "The ONLY source of the current character state. Pass section names (e.g. [\"abilities\", \"inventory\", \"spells\"]) for their complete content, or omit/empty for the whole sheet. ALWAYS read before edit_player_character remove/replace of specific entries, before rewriting a field, and before reasoning about existing content — the state may have changed between turns.",
            "parameters": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "sections": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Section names to read (top-level keys); empty or omitted = the whole sheet."
                    }
                }
            }
        }
    })
}

/// The `draft_player_character` tool schema — a FLAT hero sheet faithful to the
/// `PlayerCharacter` runtime contract. `name` is required (the minimum a
/// launchable hero needs); everything else is optional.
fn draft_player_character_schema() -> Value {
    let str_arr = |description: &str| json!({"type": "array", "items": {"type": "string"}, "description": description});
    // A number map (ability scores / skill mods / slot counts). additionalProperties
    // is a number so arbitrary keys are allowed under the strict Responses
    // conversion path stays valid — draft is non-strict anyway (see below).
    let num_map = |description: &str| {
        json!({
            "type": "object",
            "additionalProperties": {"type": "number"},
            "description": description
        })
    };
    let spell_schema = json!({
        "type": "object",
        "additionalProperties": true,
        "description": "One known spell. The engine reads only name/level/concentration/ritual; school/range/etc. live as prose in effect.",
        "properties": {
            "name": {"type": "string", "description": "Spell name (Russian)."},
            "level": {"type": "integer", "description": "Spell level (0 = cantrip)."},
            "concentration": {"type": "boolean", "description": "true if the spell requires concentration."},
            "ritual": {"type": "boolean", "description": "true if the spell can be cast as a ritual."},
            "effect": {"type": "string", "description": "What the spell does — prose the GM narrates (Russian)."}
        },
        "required": ["name", "level"]
    });

    let mut properties = Map::new();
    properties.insert("name".into(), json!({"type": "string", "description": "Character name (a real name, not a placeholder; Russian)."}));
    properties.insert("pronouns".into(), json!({"type": "string", "description": "Pronouns / grammatical gender (e.g. М, Ж, OTHER)."}));
    properties.insert("class_role".into(), json!({"type": "string", "description": "Class/role/archetype (e.g. воин-наёмник, странствующий жрец)."}));
    properties.insert(
        "level".into(),
        json!({"type": "integer", "description": "Character level (1+)."}),
    );
    properties.insert(
        "background".into(),
        json!({"type": "string", "description": "One-to-two-line background (Russian)."}),
    );
    properties.insert(
        "age".into(),
        json!({"type": "string", "description": "Age description (Russian)."}),
    );
    properties.insert(
        "physical_type".into(),
        json!({"type": "string", "description": "Build/appearance (Russian)."}),
    );
    properties.insert(
        "distinctive_features".into(),
        json!({"type": "string", "description": "Memorable visual/behavioral markers (Russian)."}),
    );
    properties.insert(
        "personality".into(),
        json!({"type": "string", "description": "Personality traits (Russian)."}),
    );
    properties.insert(
        "values".into(),
        json!({"type": "string", "description": "What the hero values / their drives (Russian)."}),
    );
    properties.insert(
        "gm_notes".into(),
        json!({"type": "string", "description": "GM-only notes about the hero (Russian)."}),
    );
    properties.insert("abilities".into(), num_map("Ability scores as a map: STR, DEX, CON, INT, WIS, CHA → integer (typically 8–18 at level 1)."));
    properties.insert(
        "skills".into(),
        num_map("Trained skills as a map: skill name → modifier (integer)."),
    );
    properties.insert(
        "saving_throws".into(),
        num_map("Saving-throw proficiencies as a map: ability → modifier (integer)."),
    );
    properties.insert(
        "passive_perception".into(),
        json!({"type": "integer", "description": "Passive Perception score."}),
    );
    properties.insert(
        "ac".into(),
        json!({"type": "integer", "description": "Armor class."}),
    );
    properties.insert(
        "hp".into(),
        json!({
            "type": "object",
            "additionalProperties": {"type": "number"},
            "description": "Hit points as {current, max} (integers).",
            "properties": {
                "current": {"type": "integer", "description": "Current HP."},
                "max": {"type": "integer", "description": "Maximum HP."}
            }
        }),
    );
    properties.insert(
        "speed".into(),
        json!({"type": "string", "description": "Movement speed (e.g. '30 ft')."}),
    );
    properties.insert("senses".into(), json!({"type": "string", "description": "Special senses (Russian; e.g. 'тёмное зрение 18 м')."}));
    properties.insert(
        "languages".into(),
        json!({"type": "string", "description": "Known languages (Russian)."}),
    );
    properties.insert(
        "inventory".into(),
        str_arr("Carried items (Russian strings)."),
    );
    properties.insert(
        "equipment".into(),
        str_arr("Worn/wielded equipment (Russian strings)."),
    );
    properties.insert(
        "features".into(),
        str_arr("Class/race features and traits (Russian strings)."),
    );
    properties.insert("spells".into(), json!({"type": "array", "items": spell_schema, "description": "Known spells (objects). Leave empty for a non-caster."}));
    properties.insert("spell_slots".into(), num_map("Remaining spell slots as a FLAT map: spell level (as a string key) → count, e.g. {\"1\": 3, \"2\": 1}."));
    properties.insert(
        "spell_slots_max".into(),
        num_map("Maximum spell slots as a FLAT map: spell level (as a string key) → count."),
    );
    properties.insert("concentration".into(), json!({"type": "string", "description": "Name of the active concentration spell; empty = none."}));
    properties.insert(
        "life_status".into(),
        json!({"type": "string", "description": "Life status (usually 'alive')."}),
    );

    json!({
        "type": "function",
        "function": {
            "name": "draft_player_character",
            "description": "Create or update the character sheet. Author a complete, launchable hero: a real name, class_role, background, the six ability scores, a few skills, sensible HP/AC and a starting inventory. Write all text in Russian. Use edit_player_character for small later changes, not a full re-draft.",
            // Nested stat maps (abilities/hp/spell_slots) need free-form keys; the
            // strict Responses conversion would force them empty, so the draft
            // tool is non-strict (same reason edit_* is) and declares
            // additionalProperties:true so nested fields survive.
            "strict": false,
            "parameters": {
                "type": "object",
                "additionalProperties": true,
                "properties": properties,
                "required": ["name"]
            }
        }
    })
}

/// The `edit_player_character` tool schema — targeted patches to an existing
/// sheet. `set` overwrites scalars AND whole object sections (abilities/hp/…);
/// `add`/`remove`/`replace` operate on the list sections (`inventory`,
/// `equipment`, `features`, `spells`).
fn edit_player_character_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "edit_player_character",
            "description": "Patch the EXISTING character sheet — change only what differs, do NOT resend the whole sheet. Prefer this over draft_player_character once a sheet exists. set overwrites scalars (name, class_role, level, background, ac, speed…) and whole objects (abilities, skills, saving_throws, hp, spell_slots, spell_slots_max). add/remove/replace target list sections: inventory, equipment, features, spells. All text in Russian.",
            // Free-form section maps (properties-less objects) die under the
            // strict Responses conversion — it forces additionalProperties:false
            // + properties:{}, so the model could only ever send {}. Same reason
            // edit_story_plot / edit_world_bible are non-strict.
            "strict": false,
            "parameters": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "set": {
                        "type": "object",
                        "additionalProperties": true,
                        "description": "Overwrite scalars (name/class_role/level/background/age/ac/speed/…) and whole objects (abilities/skills/saving_throws/hp/spell_slots/spell_slots_max). Example: {\"ac\": 15} or {\"abilities\": {\"STR\": 16, \"DEX\": 14}}."
                    },
                    "add": {
                        "type": "object",
                        "additionalProperties": {"type": "array"},
                        "description": "Append entries to a list section (existing kept). Keys: inventory, equipment, features, spells. inventory/equipment/features take strings; spells take objects (name+level required). Example: {\"inventory\": [\"верёвка\"], \"spells\": [{\"name\": \"Огненный снаряд\", \"level\": 1}]}."
                    },
                    "remove": {
                        "type": "object",
                        "additionalProperties": {"type": "array"},
                        "description": "Remove entries from a list section. For spells pass the spell name as a string; for inventory/equipment/features pass the exact strings. Example: {\"inventory\": [\"верёвка\"], \"spells\": [\"Огненный снаряд\"]}."
                    },
                    "replace": {
                        "type": "object",
                        "additionalProperties": {"type": "array"},
                        "description": "Replace a whole list section. Example: {\"inventory\": [\"меч\", \"щит\"]}."
                    }
                }
            }
        }
    })
}

// =========================================================================
// draft folding (mirrors story_architect's merge/apply/finalize for the sheet)
// =========================================================================

/// Normalize the incoming character draft into the canonical shape the loop
/// mutates. The frontend sends the sheet object mostly as-is (snake_case, flat),
/// so this is a light pass: keep an object, drop nothing.
fn normalize_input_pc(draft: &Value) -> Value {
    match draft {
        Value::Object(map) => Value::Object(map.clone()),
        _ => Value::Object(Map::new()),
    }
}

/// Merge a `draft_player_character` call's arguments into the accumulating sheet
/// draft: top-level scalars/lists overwrite; the nested stat objects
/// ([`PC_OBJECT_FIELDS`]) are merged key-by-key so a partial re-draft refines an
/// object instead of nuking it.
fn merge_pc(prev: Value, args: &Map<String, Value>) -> Value {
    let mut base = match prev {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    for (key, value) in args {
        if PC_OBJECT_FIELDS.contains(&key.as_str()) {
            if let Value::Object(new_obj) = value {
                let entry = base
                    .entry(key.clone())
                    .or_insert_with(|| Value::Object(Map::new()));
                if let Value::Object(existing) = entry {
                    for (k, v) in new_obj {
                        existing.insert(k.clone(), v.clone());
                    }
                    continue;
                }
            }
        }
        base.insert(key.clone(), value.clone());
    }
    Value::Object(base)
}

/// Apply an `edit_player_character` patch (set / add / remove / replace) onto the
/// current sheet draft and return the new full draft.
fn apply_pc_edit(draft: &Value, args: &Map<String, Value>) -> Value {
    let mut top = match draft {
        Value::Object(map) => map.clone(),
        _ => Map::new(),
    };

    // set: overwrite a scalar or a whole object section (last-writer-wins).
    if let Some(Value::Object(set)) = args.get("set") {
        for (key, value) in set {
            top.insert(key.clone(), value.clone());
        }
    }
    // replace / add / remove: list-section operations on a bare top-level list.
    if let Some(Value::Object(replace)) = args.get("replace") {
        for (key, value) in replace {
            let Value::Array(items) = value else { continue };
            top.insert(key.clone(), Value::Array(items.clone()));
        }
    }
    if let Some(Value::Object(add)) = args.get("add") {
        for (key, value) in add {
            let Value::Array(items) = value else { continue };
            add_to_list_section(&mut top, key, items);
        }
    }
    if let Some(Value::Object(remove)) = args.get("remove") {
        for (key, value) in remove {
            let Value::Array(items) = value else { continue };
            remove_from_list_section(&mut top, key, items);
        }
    }

    Value::Object(top)
}

fn add_to_list_section(top: &mut Map<String, Value>, key: &str, items: &[Value]) {
    let slot = list_section_slot(top, key);
    for item in items {
        // Object entries (spells) dedup by name; string entries by exact value.
        let dup = match item {
            Value::Object(obj) => {
                if let Some(name) = obj.get("name").and_then(Value::as_str) {
                    slot.iter().any(|e| entry_name(e) == Some(name))
                } else {
                    false
                }
            }
            _ => slot.contains(item),
        };
        if !dup {
            slot.push(item.clone());
        }
    }
}

fn remove_from_list_section(top: &mut Map<String, Value>, key: &str, items: &[Value]) {
    let slot = list_section_slot(top, key);
    slot.retain(|entry| {
        !items.iter().any(|target| match target {
            // A string target removes the object entry whose name matches, or the
            // exact string entry.
            Value::String(s) => entry_name(entry) == Some(s.as_str()) || entry == target,
            _ => entry == target,
        })
    });
}

/// Borrow (creating if needed) the `Vec<Value>` slot for a top-level list key.
fn list_section_slot<'a>(top: &'a mut Map<String, Value>, key: &str) -> &'a mut Vec<Value> {
    let entry = top
        .entry(key.to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    if !entry.is_array() {
        *entry = Value::Array(Vec::new());
    }
    entry.as_array_mut().expect("list section is an array")
}

/// The `name` of a list entry object (spells are name-keyed); `None` for a
/// non-object or an object without a name.
fn entry_name(entry: &Value) -> Option<&str> {
    entry.as_object()?.get("name").and_then(Value::as_str)
}

// =========================================================================
// config + public turn entrypoint
// =========================================================================

/// The character-architect [`ArchitectConfig`]. `context_blocks` are the
/// optional read-only base world/story blocks (empty = a standalone hero); they
/// ride as extra STABLE system messages like the story architect's world bible.
struct CharacterArchitectConfig {
    context_blocks: Vec<String>,
}

impl ArchitectConfig for CharacterArchitectConfig {
    fn system_prompt(&self) -> &str {
        character_architect_prompt(&self.context_blocks)
    }

    fn extra_system_blocks(&self) -> Vec<String> {
        self.context_blocks
            .iter()
            .filter(|b| !b.trim().is_empty())
            .cloned()
            .collect()
    }

    fn tools(&self) -> Vec<Value> {
        character_architect_tools()
    }

    fn normalize_draft(&self, draft: &Value) -> Value {
        normalize_input_pc(draft)
    }

    fn apply_tool(
        &self,
        name: &str,
        args: &Map<String, Value>,
        working_draft: &mut Value,
    ) -> ToolApplication {
        match name {
            "draft_player_character" => {
                *working_draft = merge_pc(working_draft.clone(), args);
                ToolApplication {
                    args: Value::Object(args.clone()),
                    changed: true,
                    result: render_prompt(PromptId::CharacterArchitectDraftSuccess, json!({}))
                        .expect("embedded character architect draft-success prompt must render"),
                }
            }
            "edit_player_character" => {
                let before = working_draft.clone();
                *working_draft = apply_pc_edit(working_draft, args);
                ToolApplication {
                    args: Value::Object(args.clone()),
                    changed: true,
                    result: pc_edit_facts(args, &before),
                }
            }
            "read_player_character" => ToolApplication {
                args: Value::Object(args.clone()),
                changed: false,
                result: read_player_character_result(args, working_draft),
            },
            _ => ToolApplication {
                args: Value::Object(args.clone()),
                changed: false,
                result: format!("Неизвестный инструмент архитектора персонажа: {name}."),
            },
        }
    }

    fn finalize_draft(&self, draft: Value) -> Value {
        // The sheet has no summary/mirror fields; the draft is already canonical.
        draft
    }
}

/// FACTS about what an `edit_player_character` call actually changed — never a
/// blind "ok". Replays the list ops over the BEFORE sheet in the same order the
/// real apply uses (set → replace → add → remove), so per-op counts stay correct
/// even when one call combines several ops on the same section. A remove that
/// matched nothing says so explicitly (with the read-first nudge).
fn pc_edit_facts(args: &Map<String, Value>, before: &Value) -> String {
    let mut stage: Map<String, Value> = Map::new();
    let staged = |stage: &mut Map<String, Value>, key: &str| -> Value {
        if let Some(v) = stage.get(key) {
            return v.clone();
        }
        let value = before
            .get(key)
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new()));
        stage.insert(key.to_string(), value.clone());
        value
    };
    let matches_target = |entry: &Value, target: &Value| -> bool {
        match target {
            Value::String(s) => entry_name(entry) == Some(s.as_str()) || entry == target,
            _ => entry == target,
        }
    };
    let is_dup = |existing: &[Value], item: &Value| -> bool {
        match item {
            Value::Object(obj) => {
                if let Some(name) = obj.get("name").and_then(Value::as_str) {
                    existing.iter().any(|e| entry_name(e) == Some(name))
                } else {
                    false
                }
            }
            _ => existing.contains(item),
        }
    };
    let mut lines: Vec<String> = Vec::new();

    if let Some(Value::Object(set)) = args.get("set") {
        if !set.is_empty() {
            lines.push(format!(
                "Поля обновлены: {}.",
                set.keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    if let Some(Value::Object(replace)) = args.get("replace") {
        for (key, value) in replace {
            let n = value.as_array().map(Vec::len).unwrap_or(0);
            stage.insert(key.clone(), value.clone());
            lines.push(format!("{key}: раздел заменён ({n} записей)."));
        }
    }
    if let Some(Value::Object(add)) = args.get("add") {
        for (key, value) in add {
            let Some(items) = value.as_array() else {
                continue;
            };
            let mut current = staged(&mut stage, key);
            let Some(existing) = current.as_array_mut() else {
                continue;
            };
            let mut added = 0usize;
            for item in items {
                if !is_dup(existing, item) {
                    existing.push(item.clone());
                    added += 1;
                }
            }
            let now = existing.len();
            stage.insert(key.clone(), current);
            let skipped = items.len().saturating_sub(added);
            let mut line = format!("{key}: добавлено {added} (теперь {now})");
            if skipped > 0 {
                line.push_str(&format!(", {skipped} пропущено как дубли"));
            }
            line.push('.');
            lines.push(line);
        }
    }
    if let Some(Value::Object(remove)) = args.get("remove") {
        for (key, value) in remove {
            let targets: Vec<Value> = value.as_array().cloned().unwrap_or_default();
            let mut current = staged(&mut stage, key);
            let Some(existing) = current.as_array_mut() else {
                continue;
            };
            let mut removed = 0usize;
            let mut misses: Vec<String> = Vec::new();
            for target in &targets {
                let before_len = existing.len();
                existing.retain(|e| !matches_target(e, target));
                if existing.len() < before_len {
                    removed += before_len - existing.len();
                } else {
                    misses.push(match target {
                        Value::String(s) => format!("«{s}»"),
                        other => serde_json::to_string(other).unwrap_or_default(),
                    });
                }
            }
            let now = existing.len();
            stage.insert(key.clone(), current);
            if removed > 0 {
                lines.push(format!("{key}: удалено {removed} (теперь {now})."));
            }
            if !misses.is_empty() {
                lines.push(format!(
                    "{key}: НЕ найдено для удаления: {} — совпадения нет (имя/точная строка); прочитай раздел read_player_character и повтори.",
                    misses.join(", ")
                ));
            }
        }
    }

    if lines.is_empty() {
        return "Правка НИЧЕГО не изменила (пустые операции). Прочитай нужный раздел read_player_character и повтори с точными данными.".to_string();
    }
    lines.push(
        render_prompt(PromptId::ArchitectEditSuccess, json!({}))
            .expect("embedded architect edit-success prompt must render"),
    );
    lines.join("\n")
}

/// The scalar sheet fields rendered (in this order) for a whole-sheet read.
/// Order mirrors the `PlayerCharacter` struct so a whole-sheet read reads like
/// the model. Every scalar the sheet can carry is here — `gm_notes`,
/// `life_status`, `life_status_note` and `condition` included — so a whole-sheet
/// read never silently drops a populated, valid, readable-by-name section.
const PC_SCALAR_FIELDS: [&str; 17] = [
    "name",
    "pronouns",
    "class_role",
    "level",
    "background",
    "age",
    "physical_type",
    "distinctive_features",
    "life_status",
    "life_status_note",
    "condition",
    "personality",
    "values",
    "gm_notes",
    "speed",
    "senses",
    "languages",
];

/// Render the requested sheet sections (or the whole sheet) from the working
/// draft as MODEL-READY PLAIN TEXT — headed blocks, `key: value` lines, bullet
/// lists — never raw JSON. Unknown names are reported back.
fn read_player_character_result(args: &Map<String, Value>, working_draft: &Value) -> String {
    let sections: Vec<String> = args
        .get("sections")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let sheet = match working_draft.as_object() {
        Some(m) => m,
        None => return "Лист персонажа пуст.".to_string(),
    };

    let mut blocks: Vec<String> = Vec::new();
    let mut unknown: Vec<String> = Vec::new();
    if sections.is_empty() {
        for key in PC_SCALAR_FIELDS {
            if let Some(block) = sheet.get(key).and_then(|v| pc_section_block(key, v)) {
                blocks.push(block);
            }
        }
        for key in [
            "abilities",
            "skills",
            "saving_throws",
            "ac",
            "hp",
            "passive_perception",
            "inventory",
            "equipment",
            "features",
            "spells",
            "spell_slots",
            "spell_slots_max",
            "concentration",
        ] {
            if let Some(block) = sheet.get(key).and_then(|v| pc_section_block(key, v)) {
                blocks.push(block);
            }
        }
    } else {
        for name in sections {
            match sheet.get(&name) {
                Some(v) => match pc_section_block(&name, v) {
                    Some(block) => blocks.push(block),
                    None => blocks.push(format!("## {name}\n(пусто)")),
                },
                None => unknown.push(name),
            }
        }
    }
    if blocks.is_empty() {
        blocks.push("(пусто)".to_string());
    }
    if !unknown.is_empty() {
        blocks.push(format!(
            "Нет таких разделов: {}. Доступны: name, pronouns, class_role, level, background, \
             age, physical_type, distinctive_features, personality, values, gm_notes, abilities, \
             skills, saving_throws, ac, hp, passive_perception, speed, senses, languages, \
             inventory, equipment, features, spells, spell_slots, spell_slots_max, concentration, \
             life_status, life_status_note, condition.",
            unknown.join(", ")
        ));
    }
    blocks.join("\n\n")
}

/// One `## section` text block for a sheet value: strings/numbers as-is, objects
/// as `key: value` lines, arrays as `-` bullets (spells rendered specially).
fn pc_section_block(name: &str, value: &Value) -> Option<String> {
    match value {
        Value::String(s) if !s.trim().is_empty() => Some(format!("## {name}\n{}", s.trim())),
        Value::Number(n) => Some(format!("## {name}\n{n}")),
        Value::Bool(b) => Some(format!("## {name}\n{b}")),
        Value::Array(items) => {
            if items.is_empty() {
                return None;
            }
            let bullets: Vec<String> = items
                .iter()
                .map(|v| format!("- {}", pc_entry_text(v)))
                .collect();
            Some(format!(
                "## {name} ({} записей)\n{}",
                bullets.len(),
                bullets.join("\n")
            ))
        }
        Value::Object(map) => {
            if map.is_empty() {
                return None;
            }
            let mut lines: Vec<String> = Vec::new();
            for (key, v) in map {
                match v {
                    Value::String(s) if !s.trim().is_empty() => {
                        lines.push(format!("{key}: {}", s.trim()));
                    }
                    Value::Number(n) => lines.push(format!("{key}: {n}")),
                    Value::Bool(b) => lines.push(format!("{key}: {b}")),
                    _ => {}
                }
            }
            if lines.is_empty() {
                return None;
            }
            Some(format!("## {name}\n{}", lines.join("\n")))
        }
        _ => None,
    }
}

/// One line of a sheet list entry, model-readable: spells as «имя (ур. N) —
/// эффект», plain strings as-is.
fn pc_entry_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Object(o) => {
            let name = o.get("name").and_then(Value::as_str).unwrap_or("");
            if !name.is_empty() {
                let mut head = name.to_string();
                if let Some(level) = o.get("level").and_then(Value::as_i64) {
                    head.push_str(&format!(" (ур. {level})"));
                }
                let effect = o.get("effect").and_then(Value::as_str).unwrap_or("");
                let mut tags: Vec<&str> = Vec::new();
                if o.get("concentration").and_then(Value::as_bool) == Some(true) {
                    tags.push("концентрация");
                }
                if o.get("ritual").and_then(Value::as_bool) == Some(true) {
                    tags.push("ритуал");
                }
                let tail = if effect.trim().is_empty() {
                    String::new()
                } else {
                    format!(" — {}", effect.trim())
                };
                let tagstr = if tags.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", tags.join(", "))
                };
                return format!("{head}{tail}{tagstr}");
            }
            serde_json::to_string(value).unwrap_or_default()
        }
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// Run one character-architect turn. Mirrors [`crate::story_architect::story_architect_turn`];
/// `context_blocks` are the optional read-only base world/story blocks (pass an
/// empty slice for a standalone hero).
pub async fn character_architect_turn(
    client: &dyn Backend,
    history: &[Value],
    context_blocks: &[String],
    draft: &Value,
    user_text: &str,
    stream: &mut (dyn ArchitectStream + Send),
) -> Result<CharacterArchitectOutput, BackendError> {
    let config = CharacterArchitectConfig {
        context_blocks: context_blocks.to_vec(),
    };
    architect_turn(&config, client, history, draft, user_text, stream).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(base: &Value, args: Value) -> Value {
        apply_pc_edit(base, args.as_object().unwrap())
    }

    #[test]
    fn draft_tool_requires_name_and_is_flat() {
        let tools = character_architect_tools();
        let draft = tools
            .iter()
            .find(|t| t["function"]["name"] == "draft_player_character")
            .expect("draft_player_character");
        let required = draft["function"]["parameters"]["required"]
            .as_array()
            .expect("required array");
        let required: Vec<&str> = required.iter().filter_map(Value::as_str).collect();
        assert_eq!(required, vec!["name"]);
        let props = draft["function"]["parameters"]["properties"]
            .as_object()
            .expect("properties");
        // The sheet is FLAT: stats/lists are top-level fields.
        assert_eq!(props["abilities"]["type"], "object");
        assert_eq!(props["hp"]["type"], "object");
        assert_eq!(props["inventory"]["type"], "array");
        assert_eq!(props["spells"]["type"], "array");
        // No story/world fields leak in.
        for forbidden in ["scene", "npcs", "story_brief", "world_lore"] {
            assert!(
                !props.contains_key(forbidden),
                "{forbidden} must not appear"
            );
        }
    }

    #[test]
    fn edit_tool_has_the_four_ops() {
        let tools = character_architect_tools();
        let edit = tools
            .iter()
            .find(|t| t["function"]["name"] == "edit_player_character")
            .expect("edit_player_character");
        let props = edit["function"]["parameters"]["properties"]
            .as_object()
            .expect("properties");
        for op in ["set", "add", "remove", "replace"] {
            assert!(props.contains_key(op), "edit op {op} missing");
        }
    }

    #[test]
    fn edit_tool_is_non_strict_so_section_maps_survive_responses_conversion() {
        // The strict Responses conversion rewrites properties-less objects into
        // additionalProperties:false + properties:{} — the model could then only
        // send empty set/add/remove/replace. The edit tool must opt out.
        let tools = character_architect_tools();
        let edit = tools
            .iter()
            .find(|t| t["function"]["name"] == "edit_player_character")
            .expect("edit_player_character present");
        assert_eq!(edit["function"]["strict"], json!(false));
    }

    #[test]
    fn merge_pc_deep_merges_stat_objects() {
        let base = json!({
            "name": "Ариан",
            "abilities": {"STR": 10, "DEX": 12},
            "hp": {"current": 9, "max": 9}
        });
        let args = json!({
            "abilities": {"STR": 16, "CON": 14},
            "hp": {"max": 12}
        })
        .as_object()
        .unwrap()
        .clone();
        let merged = merge_pc(base, &args);
        // abilities: STR overwritten, DEX kept, CON added.
        assert_eq!(merged["abilities"]["STR"], 16);
        assert_eq!(merged["abilities"]["DEX"], 12);
        assert_eq!(merged["abilities"]["CON"], 14);
        // hp: max overwritten, current kept.
        assert_eq!(merged["hp"]["current"], 9);
        assert_eq!(merged["hp"]["max"], 12);
    }

    #[test]
    fn edit_set_overwrites_scalar_and_whole_object() {
        let base = json!({"ac": 12, "abilities": {"STR": 10}});
        let out = edit(
            &base,
            json!({"set": {"ac": 15, "abilities": {"STR": 16, "DEX": 14}}}),
        );
        assert_eq!(out["ac"], 15);
        // set on an object REPLACES it wholesale (last-writer-wins).
        assert_eq!(out["abilities"]["STR"], 16);
        assert_eq!(out["abilities"]["DEX"], 14);
    }

    #[test]
    fn edit_add_dedups_spells_by_name_and_strings_by_value() {
        let base = json!({
            "spells": [{"name": "Свет", "level": 0}],
            "inventory": ["верёвка"]
        });
        let out = edit(
            &base,
            json!({"add": {
                "spells": [{"name": "Свет", "level": 0}, {"name": "Огненный снаряд", "level": 1}],
                "inventory": ["верёвка", "факел"]
            }}),
        );
        let spells = out["spells"].as_array().unwrap();
        assert_eq!(spells.len(), 2);
        assert_eq!(spells[1]["name"], "Огненный снаряд");
        assert_eq!(out["inventory"], json!(["верёвка", "факел"]));
    }

    #[test]
    fn edit_remove_spell_by_name_and_replace_section() {
        let base = json!({
            "spells": [{"name": "Свет", "level": 0}, {"name": "Щит", "level": 1}],
            "inventory": ["меч"]
        });
        let removed = edit(&base, json!({"remove": {"spells": ["Свет"]}}));
        let spells = removed["spells"].as_array().unwrap();
        assert_eq!(spells.len(), 1);
        assert_eq!(spells[0]["name"], "Щит");
        let replaced = edit(&base, json!({"replace": {"inventory": ["лук", "стрелы"]}}));
        assert_eq!(replaced["inventory"], json!(["лук", "стрелы"]));
    }

    #[test]
    fn edit_facts_report_hits_and_misses() {
        let config = CharacterArchitectConfig {
            context_blocks: Vec::new(),
        };
        let mut working = json!({
            "spells": [{"name": "Свет", "level": 0}],
            "inventory": ["меч", "щит"]
        });
        let args = json!({
            "set": {"ac": 16},
            "add": {"spells": [{"name": "Свет", "level": 0}, {"name": "Щит", "level": 1}]},
            "remove": {"inventory": ["меч", "нет_такого"]}
        });
        let applied = config.apply_tool(
            "edit_player_character",
            args.as_object().unwrap(),
            &mut working,
        );
        assert!(applied.changed);
        assert!(applied.result.contains("Поля обновлены: ac."));
        assert!(
            applied.result.contains("spells: добавлено 1"),
            "{}",
            applied.result
        );
        assert!(applied.result.contains("пропущено как дубли"));
        assert!(applied.result.contains("inventory: удалено 1 (теперь 1)."));
        assert!(applied
            .result
            .contains("НЕ найдено для удаления: «нет_такого»"));
        assert!(applied.result.contains("read_player_character"));
    }

    #[test]
    fn user_tail_is_raw_text_matching_stored_history() {
        // CACHE INVARIANT: sent tail == stored history entry, byte for byte.
        let messages = character_architect_messages(&[], &[], "  Сделай мага.  ");
        let tail = messages.last().unwrap();
        assert_eq!(tail["role"], "user");
        assert_eq!(tail["content"], "Сделай мага.");
        // System prompt carries the recognition marker (drives the mock).
        assert_eq!(messages[0]["role"], "system");
        assert!(messages[0]["content"]
            .as_str()
            .unwrap()
            .contains("GM-Lab character architect"));
    }

    #[test]
    fn read_player_character_renders_plain_text_not_json() {
        let config = CharacterArchitectConfig {
            context_blocks: Vec::new(),
        };
        let mut working = json!({
            "name": "Ариан",
            "abilities": {"STR": 16, "DEX": 12},
            "inventory": ["меч", "щит"],
            "spells": [{"name": "Огненный снаряд", "level": 1, "effect": "бьёт огнём", "concentration": false}]
        });
        let mut args = Map::new();
        args.insert(
            "sections".into(),
            json!(["name", "abilities", "inventory", "spells", "nope"]),
        );
        let out = config
            .apply_tool("read_player_character", &args, &mut working)
            .result;
        assert!(out.contains("## name\nАриан"));
        assert!(out.contains("STR: 16"));
        assert!(out.contains("- меч"));
        assert!(out.contains("Огненный снаряд (ур. 1) — бьёт огнём"));
        assert!(out.contains("Нет таких разделов: nope"));
        assert!(!out.trim_start().starts_with('{'), "not JSON: {out}");
    }

    #[test]
    fn whole_sheet_read_includes_gm_notes_and_life_status() {
        // A whole-sheet read (no sections) must render every populated, valid
        // section — including gm_notes / life_status / life_status_note /
        // condition, which the model can author and read by name. Dropping them
        // here would let the architect contradict notes it cannot see.
        let config = CharacterArchitectConfig {
            context_blocks: Vec::new(),
        };
        let mut working = json!({
            "name": "Ариан",
            "gm_notes": "Тайно служит культу.",
            "life_status": "alive",
            "life_status_note": "Ранен в бок.",
            "condition": "истощение 1",
        });
        let args = Map::new(); // no "sections" key => whole sheet
        let out = config
            .apply_tool("read_player_character", &args, &mut working)
            .result;
        assert!(out.contains("## name\nАриан"));
        assert!(out.contains("## gm_notes\nТайно служит культу."), "{out}");
        assert!(out.contains("## life_status\nalive"), "{out}");
        assert!(out.contains("## life_status_note\nРанен в бок."), "{out}");
        assert!(out.contains("## condition\nистощение 1"), "{out}");
    }

    #[test]
    fn messages_have_no_extra_system_block_when_standalone() {
        // A standalone hero (no base world/story) keeps the old shape: exactly
        // ONE system message — and the STANDALONE prompt variant, which spends
        // zero tokens on the base-block feature it is not using.
        let msgs = character_architect_messages(&[], &[], "Собери героя.");
        let system_count = msgs.iter().filter(|m| m["role"] == "system").count();
        assert_eq!(system_count, 1);
        let prompt = msgs[0]["content"].as_str().unwrap();
        assert!(
            !prompt.contains("BASE WORLD"),
            "standalone prompt must not mention bases"
        );
        assert!(
            !prompt.contains("BASE STORY"),
            "standalone prompt must not mention bases"
        );
        assert!(prompt.contains("standalone"));
        assert_eq!(msgs.last().unwrap()["role"], "user");
        // All-blank blocks degrade to the same standalone prompt.
        let blank = character_architect_messages(&[], &["  ".to_string()], "Собери героя.");
        assert_eq!(blank[0]["content"], msgs[0]["content"]);
    }

    #[test]
    fn base_blocks_ride_as_second_and_third_system_messages() {
        // A based hero: the BASED prompt variant (grounding guidance instead of
        // the contradictory standalone rule), with the world/story blocks
        // spliced right after it (part of the stable cache prefix), before
        // history and the user tail. Blank blocks are dropped.
        let world_block =
            character_architect_world_block(&json!({"name": "Эмберфолл", "genre": "fantasy"}));
        let story_block = character_architect_story_block(&json!({"title": "Пепел у дороги"}));
        let msgs = character_architect_messages(
            &[],
            &[world_block.clone(), story_block.clone(), "  ".to_string()],
            "Собери героя.",
        );
        assert_eq!(msgs[0]["role"], "system");
        let prompt = msgs[0]["content"].as_str().unwrap();
        assert!(
            prompt.contains("base reference"),
            "based prompt variant expected"
        );
        assert!(
            !prompt.contains("do NOT tie them"),
            "the standalone rule would contradict the base blocks"
        );
        // The based prompt is GENERIC: all world-/story-specific guidance lives
        // in the blocks, so a world-only base never carries story instructions.
        assert!(
            !prompt.contains("public premise"),
            "story guidance belongs in the story block"
        );
        assert!(
            !prompt.contains("hidden answers"),
            "story guidance belongs in the story block"
        );
        assert!(
            !prompt.contains("proper nouns"),
            "world guidance belongs in the world block"
        );
        assert_eq!(msgs[1]["role"], "system");
        assert_eq!(msgs[1]["content"], json!(world_block));
        assert_eq!(msgs[2]["role"], "system");
        assert_eq!(msgs[2]["content"], json!(story_block));
        let system_count = msgs.iter().filter(|m| m["role"] == "system").count();
        assert_eq!(system_count, 3, "the blank block must be dropped");
        assert_eq!(msgs.last().unwrap()["role"], "user");
    }

    #[test]
    fn world_block_is_a_public_whitelist() {
        let lore = json!({
            "name": "Эмберфолл",
            "genre": "fantasy",
            "public_premise": "Долина у вулкана.",
            "dogmas": ["огонь свят"],
            "hidden_premise": "СЕКРЕТ-ПРЕМИСА",
            "hidden_secrets": ["СЕКРЕТ-1"],
            "world_image_prompt_en": "an image prompt",
            "world_image_url": "/world-assets/x.png",
            // An ad-hoc key the world architect can fold into the bible — the
            // whitelist must keep it private without knowing its name.
            "dm_notes": "АДХОК-СЕКРЕТ",
        });
        let block = character_architect_world_block(&lore);
        assert!(block.contains("## BASE WORLD"));
        assert!(block.contains("Эмберфолл"));
        assert!(block.contains("Долина у вулкана."));
        assert!(block.contains("огонь свят"));
        assert!(!block.contains("СЕКРЕТ-ПРЕМИСА"), "{block}");
        assert!(!block.contains("СЕКРЕТ-1"), "{block}");
        assert!(!block.contains("an image prompt"), "{block}");
        assert!(!block.contains("/world-assets/"), "{block}");
        assert!(!block.contains("АДХОК-СЕКРЕТ"), "{block}");
    }

    #[test]
    fn blocks_with_no_public_material_are_empty_strings() {
        // No heading over `{}`: an empty block is dropped by the runner and the
        // prompt picker falls back to the standalone variant.
        assert_eq!(character_architect_world_block(&json!({})), "");
        assert_eq!(character_architect_world_block(&Value::Null), "");
        assert_eq!(
            character_architect_world_block(&json!({"hidden_premise": "СЕКРЕТ", "name": "  "})),
            ""
        );
        assert_eq!(character_architect_story_block(&json!({})), "");
        assert_eq!(
            character_architect_story_block(&json!({"hidden_truth": "СЕКРЕТ", "title": ""})),
            ""
        );
        // And the message assembly degrades to the standalone shape.
        let msgs = character_architect_messages(
            &[],
            &[character_architect_world_block(&json!({}))],
            "Собери героя.",
        );
        assert_eq!(msgs.iter().filter(|m| m["role"] == "system").count(), 1);
        assert!(msgs[0]["content"].as_str().unwrap().contains("standalone"));
    }

    #[test]
    fn base_unavailable_block_keeps_the_based_prompt() {
        // A hero with recorded refs whose base material is gone must NOT fall
        // through to the standalone prompt (its "do NOT tie" would contradict
        // the already-grounded sheet) — the server substitutes this note.
        let note = character_architect_base_unavailable_block();
        assert!(note.contains("## BASE (reference unavailable)"));
        assert!(note.contains("Preserve the hero's existing ties"));
        let msgs = character_architect_messages(&[], std::slice::from_ref(&note), "Поправь лук.");
        assert_eq!(msgs.iter().filter(|m| m["role"] == "system").count(), 2);
        let prompt = msgs[0]["content"].as_str().unwrap();
        assert!(prompt.contains("base reference"), "based prompt expected");
        assert!(!prompt.contains("do NOT tie them"));
        assert_eq!(msgs[1]["content"], json!(note));
    }

    #[test]
    fn story_block_is_a_public_whitelist() {
        let story = json!({
            "title": "Пепел у дороги",
            "description": "Каравану нужен проводник.",
            "story_brief": "Довести караван живым.",
            "public_intro": "Вы стоите у ворот.",
            "hidden_truth": "СКРЫТАЯ-ПРАВДА",
            "npcs": [{"name": "Викар", "secret": "предатель"}],
            "player_character": {"name": "Авторский герой"},
        });
        let block = character_architect_story_block(&story);
        assert!(block.contains("## BASE STORY"));
        assert!(block.contains("Пепел у дороги"));
        assert!(block.contains("Довести караван живым."));
        assert!(block.contains("Вы стоите у ворот."));
        assert!(!block.contains("СКРЫТАЯ-ПРАВДА"), "{block}");
        assert!(!block.contains("предатель"), "{block}");
        assert!(!block.contains("Авторский герой"), "{block}");
    }
}
