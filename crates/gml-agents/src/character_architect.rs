//! Dedicated CHARACTER architect agent (mirrors [`crate::story_architect`]).
//!
//! This is the character-package sibling of the world/story architects: a
//! planning chat that helps the user author a reusable PLAYER CHARACTER (`.gmchar`
//! package payload). Like the other two it is a THIN CONFIG over the shared
//! [`crate::architect_runner`] loop — same agent discipline (think → tool →
//! reply), same streaming, same stats — it only swaps the prompt, the tools, and
//! the draft-folding for the character-sheet shape.
//!
//! Unlike the story architect a character is ORTHOGONAL/standalone: there is no
//! bound world, so this config injects NO extra system block (the default
//! [`ArchitectConfig::extra_system_blocks`]) and the turn fn takes no lore block.
//!
//! The draft is the `.gmchar` payload's `player_character` object — a FLAT sheet
//! (`gml-world/src/model.rs::PlayerCharacter`): name/pronouns/class_role/level/
//! background/age/physical_type + ability & skill maps + hp/ac + inventory/
//! equipment/features string lists + spells (SpellEntry objects) + flat
//! spell-slot maps. The merge is a shallow top-level merge with a special-case
//! deep merge for the nested stat objects (abilities, skills, saving_throws, hp,
//! spell_slots, spell_slots_max) so a partial re-draft refines instead of nuking.

use serde_json::{json, Map, Value};

use gml_llm::{Backend, BackendError};

use crate::architect_runner::{
    architect_messages_with_user, architect_turn, ArchitectConfig, ArchitectOutput,
    ArchitectStream, ToolApplication,
};

/// The character architect turn output — the generic [`ArchitectOutput`] under
/// the domain name (mirrors `StoryArchitectOutput`).
pub type CharacterArchitectOutput = ArchitectOutput;

/// Canon-authoring rules for the CHARACTER architect. Russian content like the
/// other architects; authors a standalone hero sheet; questions go in the chat
/// reply, not a tool field; same agent-loop discipline.
pub const CHARACTER_ARCHITECT_SYSTEM: &str = r#"You are the GM-Lab character architect. You help the user author a reusable
PLAYER CHARACTER — a portable hero card that can be launched into any story or
world. Write all character text in Russian; keep it concrete and playable.

You author ONE protagonist: name, pronouns, class/role, level, background, look
and personality, D&D 5e stats (ability scores, skills, saving throws, AC, HP),
speed/senses/languages, starting inventory, equipment, features, and — if the
concept is a caster — known spells and spell slots. The hero is standalone: do
NOT tie them to a specific world's secret canon or a single story's plot; write
them so they read sensibly dropped into different adventures.

Build the sheet with draft_player_character. Make the first draft a complete,
launchable hero: a real name (not a placeholder), a class_role and background
that fit, the six ability scores, a few trained skills, sensible HP/AC for the
level, and a starting inventory. The tool's field descriptions define each
field's shape — follow them. abilities/skills/saving_throws are objects
(name → number); hp is {current, max}; inventory/equipment/features are string
lists; spells are objects; spell_slots/spell_slots_max are FLAT maps of
level → count (e.g. {"1": 3}).

Once a sheet exists, make changes with edit_player_character — patch only what
differs (set a scalar or a whole object like abilities/hp; add/remove/replace
entries in the list sections inventory, equipment, features, spells). Do NOT
resend the whole sheet with draft_player_character for a small change; reserve
draft_player_character for the first build or a deliberate full rebuild.

The character lives on the server; user messages carry ONLY the user's text. The
single source of the current state is the read_player_character tool. When the
conversation is empty and the user asks for a new hero, build it straight away
with draft_player_character. In every other case, before editing existing fields,
before removing/replacing specific entries, and before making claims about what
the sheet already says — call read_player_character for the relevant sections (or
the whole sheet) and act on what it returns. The state may have changed between
turns (the user edits fields by hand in the form). Never invent or guess current
content, and never ask the user to paste it.

Ask the user a question only when something important is genuinely missing or
unclear, and ask it in your chat reply, not in a tool field. Otherwise just note
briefly what you built or changed; questions are not required every turn.

How you work, like an agent: think about what the hero needs, then update the
sheet with a tool (draft_player_character to build, edit_player_character to
change), then finish the turn with a short chat reply about what you built or
changed. You may call tools more than once per turn. Each tool result comes back
to you, so you can keep going or wrap up — but always end the turn with a reply,
never on a bare tool call."#;

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

/// Assemble the character-architect request messages: system + filtered history
/// + the user message. No extra system block (a character has no bound world).
///
/// CACHE INVARIANT: the tail user message is the RAW user text — byte-equal to
/// the history entry the server stores for this turn. State never rides in
/// messages; the model reads it via read_player_character.
pub fn character_architect_messages(history: &[Value], user_text: &str) -> Vec<Value> {
    architect_messages_with_user(
        CHARACTER_ARCHITECT_SYSTEM,
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
    let str_arr = |description: &str| {
        json!({"type": "array", "items": {"type": "string"}, "description": description})
    };
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
    properties.insert("level".into(), json!({"type": "integer", "description": "Character level (1+)."}));
    properties.insert("background".into(), json!({"type": "string", "description": "One-to-two-line background (Russian)."}));
    properties.insert("age".into(), json!({"type": "string", "description": "Age description (Russian)."}));
    properties.insert("physical_type".into(), json!({"type": "string", "description": "Build/appearance (Russian)."}));
    properties.insert("distinctive_features".into(), json!({"type": "string", "description": "Memorable visual/behavioral markers (Russian)."}));
    properties.insert("personality".into(), json!({"type": "string", "description": "Personality traits (Russian)."}));
    properties.insert("values".into(), json!({"type": "string", "description": "What the hero values / their drives (Russian)."}));
    properties.insert("gm_notes".into(), json!({"type": "string", "description": "GM-only notes about the hero (Russian)."}));
    properties.insert("abilities".into(), num_map("Ability scores as a map: STR, DEX, CON, INT, WIS, CHA → integer (typically 8–18 at level 1)."));
    properties.insert("skills".into(), num_map("Trained skills as a map: skill name → modifier (integer)."));
    properties.insert("saving_throws".into(), num_map("Saving-throw proficiencies as a map: ability → modifier (integer)."));
    properties.insert("passive_perception".into(), json!({"type": "integer", "description": "Passive Perception score."}));
    properties.insert("ac".into(), json!({"type": "integer", "description": "Armor class."}));
    properties.insert("hp".into(), json!({
        "type": "object",
        "additionalProperties": {"type": "number"},
        "description": "Hit points as {current, max} (integers).",
        "properties": {
            "current": {"type": "integer", "description": "Current HP."},
            "max": {"type": "integer", "description": "Maximum HP."}
        }
    }));
    properties.insert("speed".into(), json!({"type": "string", "description": "Movement speed (e.g. '30 ft')."}));
    properties.insert("senses".into(), json!({"type": "string", "description": "Special senses (Russian; e.g. 'тёмное зрение 18 м')."}));
    properties.insert("languages".into(), json!({"type": "string", "description": "Known languages (Russian)."}));
    properties.insert("inventory".into(), str_arr("Carried items (Russian strings)."));
    properties.insert("equipment".into(), str_arr("Worn/wielded equipment (Russian strings)."));
    properties.insert("features".into(), str_arr("Class/race features and traits (Russian strings)."));
    properties.insert("spells".into(), json!({"type": "array", "items": spell_schema, "description": "Known spells (objects). Leave empty for a non-caster."}));
    properties.insert("spell_slots".into(), num_map("Remaining spell slots as a FLAT map: spell level (as a string key) → count, e.g. {\"1\": 3, \"2\": 1}."));
    properties.insert("spell_slots_max".into(), num_map("Maximum spell slots as a FLAT map: spell level (as a string key) → count."));
    properties.insert("concentration".into(), json!({"type": "string", "description": "Name of the active concentration spell; empty = none."}));
    properties.insert("life_status".into(), json!({"type": "string", "description": "Life status (usually 'alive')."}));

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

/// The character-architect [`ArchitectConfig`]. Standalone — no bound world, so
/// no extra system block.
struct CharacterArchitectConfig;

impl ArchitectConfig for CharacterArchitectConfig {
    fn system_prompt(&self) -> &str {
        CHARACTER_ARCHITECT_SYSTEM
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
                    result: "Черновик персонажа создан/обновлён и показан пользователю. Дальше правь точечно через edit_player_character (не пересылай весь лист), либо кратко ответь пользователю в чат.".to_string(),
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
                set.keys().map(String::as_str).collect::<Vec<_>>().join(", ")
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
            let Some(items) = value.as_array() else { continue };
            let mut current = staged(&mut stage, key);
            let Some(existing) = current.as_array_mut() else { continue };
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
            let Some(existing) = current.as_array_mut() else { continue };
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
    lines.push("Продолжай точечные правки или кратко ответь в чат.".to_string());
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
            let bullets: Vec<String> =
                items.iter().map(|v| format!("- {}", pc_entry_text(v))).collect();
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

/// Run one character-architect turn. Mirrors [`crate::story_architect::story_architect_turn`]
/// but takes no bound-world lore block — a character is standalone.
pub async fn character_architect_turn(
    client: &dyn Backend,
    history: &[Value],
    draft: &Value,
    user_text: &str,
    stream: &mut (dyn ArchitectStream + Send),
) -> Result<CharacterArchitectOutput, BackendError> {
    let config = CharacterArchitectConfig;
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
            assert!(!props.contains_key(forbidden), "{forbidden} must not appear");
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
        let config = CharacterArchitectConfig;
        let mut working = json!({
            "spells": [{"name": "Свет", "level": 0}],
            "inventory": ["меч", "щит"]
        });
        let args = json!({
            "set": {"ac": 16},
            "add": {"spells": [{"name": "Свет", "level": 0}, {"name": "Щит", "level": 1}]},
            "remove": {"inventory": ["меч", "нет_такого"]}
        });
        let applied =
            config.apply_tool("edit_player_character", args.as_object().unwrap(), &mut working);
        assert!(applied.changed);
        assert!(applied.result.contains("Поля обновлены: ac."));
        assert!(applied.result.contains("spells: добавлено 1"), "{}", applied.result);
        assert!(applied.result.contains("пропущено как дубли"));
        assert!(applied.result.contains("inventory: удалено 1 (теперь 1)."));
        assert!(applied.result.contains("НЕ найдено для удаления: «нет_такого»"));
        assert!(applied.result.contains("read_player_character"));
    }

    #[test]
    fn user_tail_is_raw_text_matching_stored_history() {
        // CACHE INVARIANT: sent tail == stored history entry, byte for byte.
        let messages = character_architect_messages(&[], "  Сделай мага.  ");
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
        let config = CharacterArchitectConfig;
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
        let config = CharacterArchitectConfig;
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
    fn messages_have_no_extra_system_block() {
        // A character is standalone — unlike the story architect there is no
        // bound-world lore block, so there is exactly ONE system message.
        let msgs = character_architect_messages(&[], "Собери героя.");
        let system_count = msgs
            .iter()
            .filter(|m| m["role"] == "system")
            .count();
        assert_eq!(system_count, 1);
        assert_eq!(msgs.last().unwrap()["role"], "user");
    }
}
