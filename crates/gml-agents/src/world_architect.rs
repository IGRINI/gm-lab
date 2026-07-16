//! Dedicated world-architect agent.
//!
//! This is not the in-game GM and not the location generator. It is a separate
//! planning chat that helps the player author a reusable world bible. The only
//! mutating surface it has is a draft tool call; saving the draft creates a
//! standalone world, not a running campaign.

use serde_json::{json, Map, Value};

use gml_llm::{Backend, BackendError};

use crate::architect_runner::{
    architect_messages_with_user, architect_turn, ArchitectConfig, ArchitectOutput, ToolApplication,
};
// Re-exported through this module's public surface (unchanged for callers).
pub use crate::architect_runner::{ArchitectStream, NullArchitectStream};

const WORLD_ARCHITECT_SYSTEM: &str = r#"You are the GM-Lab world architect. You help the user build a reusable world
bible — the world-level canon (reality laws, peoples, powers, faiths, history,
geography, economy, secrets, location-generation rules) that later constrains the
in-game GM and the location generator. Write canon text in Russian; keep it
concrete. Fields ending in `_en`, when available, are English image-generation
prompts and must be written in English.

You author the world, not a playthrough: define canon only. Don't create a live
scene, player role, starting quest, or starting location — those belong to a
later story step.

Build the world with draft_world_bible. Make the first draft rich and specific:
fill every field the idea can reasonably support with several concrete entries,
inferring plausible, coherent detail rather than leaving sections empty or filled
with one vague line. The tool's field descriptions define what each section means
and what belongs in public vs GM-only fields — follow them. Keep public_premise
safe for the player; put GM-only truth in hidden_premise and hidden_secrets. The
summary fields world_size, population and public_premise read best as 1-3 full
sentences, not a couple of words.

Once a bible exists, make changes with edit_world_bible — patch only what differs
(set a field, add/remove/replace entries in a section). Do NOT resend the whole
bible with draft_world_bible for a small change; reserve draft_world_bible for the
first build or a deliberate full rebuild.

The bible itself lives on the server; user messages carry ONLY the user's text.
The single source of the current state is the read_world_bible tool. When the
conversation is empty and the user asks to create a world, build it straight
away with draft_world_bible. In every other case, before editing existing
content, before removing/replacing specific entries, and before making claims
about what the bible already says — call read_world_bible for the relevant
sections (or the whole bible) and act on what it returns. The state may have
changed between turns (the user edits fields by hand in the form). Never invent
or guess current content, and never ask the user to paste it.

Ask the user a question only when something important is genuinely missing or
unclear, and ask it in your chat reply, not in a tool field. Otherwise just note
briefly what you built or changed; questions are not required every turn.

How you work, like an agent: think about what the world needs, then update the
bible with a tool (draft_world_bible to build, edit_world_bible to change), then
finish the turn with a short chat reply about what you built or changed. You may
call tools more than once per turn. Each tool result comes back to you, so you can
keep going or wrap up — but always end the turn with a reply, never on a bare tool
call.

A section filled to the expected depth looks like this:
"world_laws": [
  "магия требует имени, цены или признанного права",
  "клятва, данная вслух при свидетеле-духе, связывает сильнее закона",
  "дальняя дорога меняет слухи и баланс сил между домами"
]"#;

#[derive(Clone, Copy, Debug, Default)]
pub struct WorldArchitectOptions {
    pub image_prompts: bool,
}

/// The world architect's turn output. The generic [`ArchitectOutput`] IS the
/// world output — this alias keeps the historical public type name so callers
/// (`gml-server`) compile unchanged.
pub type WorldArchitectOutput = ArchitectOutput;

/// The world-architect [`ArchitectConfig`]: prompt + tools + world-shaped
/// draft-folding. This is the whole domain surface the generic loop needs.
struct WorldArchitectConfig {
    options: WorldArchitectOptions,
}

impl ArchitectConfig for WorldArchitectConfig {
    fn system_prompt(&self) -> &str {
        WORLD_ARCHITECT_SYSTEM
    }

    fn tools(&self) -> Vec<Value> {
        world_architect_tools_with_options(self.options)
    }

    fn normalize_draft(&self, draft: &Value) -> Value {
        normalize_input_draft(draft)
    }

    fn apply_tool(
        &self,
        name: &str,
        args: &Map<String, Value>,
        working_draft: &mut Value,
    ) -> ToolApplication {
        // draft_world_bible builds/rebuilds (FLAT args → nested world_lore);
        // edit_world_bible patches the existing draft in place. The card/event
        // shows what the model actually sent (nested for a build, raw for an edit).
        match name {
            "draft_world_bible" => {
                let nested = nest_draft_args(args);
                if let Value::Object(map) = &nested {
                    *working_draft = merge_draft(Some(working_draft.clone()), map);
                    ToolApplication {
                        args: nested,
                        changed: true,
                        result: "Черновик мира создан/обновлён и показан пользователю. Дальше правь точечно через edit_world_bible (не пересылай весь черновик), либо кратко ответь пользователю в чат.".to_string(),
                    }
                } else {
                    ToolApplication {
                        args: nested,
                        changed: false,
                        result: "Аргументы draft_world_bible не разобраны — черновик не изменён."
                            .to_string(),
                    }
                }
            }
            "edit_world_bible" => {
                let before = working_draft.clone();
                *working_draft = apply_world_bible_edit(working_draft, args);
                ToolApplication {
                    args: Value::Object(args.clone()),
                    changed: true,
                    result: world_edit_facts(args, &before, working_draft),
                }
            }
            "read_world_bible" => ToolApplication {
                args: Value::Object(args.clone()),
                changed: false,
                result: read_world_bible_result(args, working_draft),
            },
            _ => ToolApplication {
                args: Value::Object(args.clone()),
                changed: false,
                result: format!("Неизвестный инструмент архитектора: {name}."),
            },
        }
    }

    fn finalize_draft(&self, draft: Value) -> Value {
        finalize_draft(draft)
    }
}

/// FACTS about what an `edit_world_bible` call actually changed — never a blind
/// "ok". Replays the ops over the BEFORE draft in the same order the real apply
/// uses (set → replace → add → remove), so per-op counts stay correct even when
/// one call combines several ops on the same section. A remove that matched
/// nothing says so explicitly and tells the model to read the section first.
fn world_edit_facts(args: &Map<String, Value>, before: &Value, _after: &Value) -> String {
    // The staged working copy of each touched list section.
    let mut stage: Map<String, Value> = before
        .get("world_lore")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let list_len = |stage: &Map<String, Value>, key: &str| -> usize {
        stage
            .get(key)
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0)
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
            stage.insert(key.clone(), value.clone());
            lines.push(format!(
                "{key}: раздел заменён ({} записей).",
                list_len(&stage, key)
            ));
        }
    }
    if let Some(Value::Object(add)) = args.get("add") {
        for (key, value) in add {
            let Some(items) = value.as_array() else {
                continue;
            };
            let entry = stage
                .entry(key.clone())
                .or_insert_with(|| Value::Array(Vec::new()));
            let Value::Array(existing) = entry else {
                continue;
            };
            let mut added = 0usize;
            for item in items {
                if !existing.contains(item) {
                    existing.push(item.clone());
                    added += 1;
                }
            }
            let skipped = items.len().saturating_sub(added);
            let mut line = format!("{key}: добавлено {added} (теперь {})", existing.len());
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
            let existing = stage.get_mut(key).and_then(|v| v.as_array_mut());
            let mut removed = 0usize;
            let mut misses: Vec<String> = Vec::new();
            if let Some(existing) = existing {
                for target in &targets {
                    let before_len = existing.len();
                    existing.retain(|e| e != target);
                    if existing.len() < before_len {
                        removed += before_len - existing.len();
                    } else if let Value::String(s) = target {
                        misses.push(format!("«{s}»"));
                    }
                }
                if removed > 0 {
                    lines.push(format!(
                        "{key}: удалено {removed} (теперь {}).",
                        existing.len()
                    ));
                }
            } else {
                misses.extend(
                    targets
                        .iter()
                        .filter_map(|t| t.as_str().map(|s| format!("«{s}»"))),
                );
            }
            if !misses.is_empty() {
                lines.push(format!(
                    "{key}: НЕ найдено для удаления: {} — точного совпадения нет; прочитай раздел read_world_bible и повтори с точной строкой.",
                    misses.join(", ")
                ));
            }
        }
    }

    if lines.is_empty() {
        return "Правка НИЧЕГО не изменила (пустые операции). Прочитай нужный раздел read_world_bible и повтори с точными данными.".to_string();
    }
    lines.push("Продолжай точечные правки или кратко ответь в чат.".to_string());
    lines.join("\n")
}

/// Render the requested bible sections (or the whole bible) from the working
/// draft as MODEL-READY PLAIN TEXT — headed blocks with bullet lists, the same
/// style as the GM context builders, never raw JSON. Section names resolve
/// against BOTH the draft's top-level scalar fields and the nested `world_lore`
/// sections; unknown names are reported so the model can correct itself.
fn read_world_bible_result(args: &Map<String, Value>, working_draft: &Value) -> String {
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
    let draft = match working_draft.as_object() {
        Some(m) => m,
        None => return "Библия пуста.".to_string(),
    };
    let lore = draft.get("world_lore").and_then(Value::as_object);

    let mut blocks: Vec<String> = Vec::new();
    let mut unknown: Vec<String> = Vec::new();
    if sections.is_empty() {
        // The whole bible: scalars first, then every lore section, draft order.
        for field in DRAFT_SUMMARY_FIELDS {
            if let Some(text) = draft.get(field).and_then(Value::as_str) {
                if !text.trim().is_empty() {
                    blocks.push(format!("## {field}\n{}", text.trim()));
                }
            }
        }
        if let Some(lore) = lore {
            for (key, value) in lore {
                if let Some(block) = bible_section_block(key, value) {
                    blocks.push(block);
                }
            }
        }
    } else {
        for name in sections {
            let value = draft.get(&name).or_else(|| lore.and_then(|l| l.get(&name)));
            match value {
                Some(v) => {
                    if let Some(block) = bible_section_block(&name, v) {
                        blocks.push(block);
                    } else {
                        blocks.push(format!("## {name}\n(пусто)"));
                    }
                }
                None => unknown.push(name),
            }
        }
    }
    if blocks.is_empty() {
        blocks.push("(пусто)".to_string());
    }
    if !unknown.is_empty() {
        blocks.push(format!(
            "Нет таких разделов: {}. Доступны поля ({}) и разделы world_lore.",
            unknown.join(", "),
            DRAFT_SUMMARY_FIELDS.join(", ")
        ));
    }
    blocks.join("\n\n")
}

/// One `## section` text block: string lists as `-` bullets (exact entries —
/// remove/replace match these verbatim), scalar strings as-is.
fn bible_section_block(name: &str, value: &Value) -> Option<String> {
    match value {
        Value::Array(items) => {
            let bullets: Vec<String> = items
                .iter()
                .map(|v| match v {
                    Value::String(s) => format!("- {s}"),
                    other => format!("- {}", serde_json::to_string(other).unwrap_or_default()),
                })
                .collect();
            if bullets.is_empty() {
                return None;
            }
            Some(format!(
                "## {name} ({} записей)\n{}",
                bullets.len(),
                bullets.join("\n")
            ))
        }
        Value::String(s) if !s.trim().is_empty() => Some(format!("## {name}\n{}", s.trim())),
        _ => None,
    }
}

pub fn world_architect_messages(history: &[Value], user_text: &str) -> Vec<Value> {
    // CACHE INVARIANT: the tail user message is the RAW user text — byte-equal
    // to the history entry the server stores for this turn, so the request
    // prefix stays stable across turns. State never rides in messages; the
    // model reads it via read_world_bible.
    architect_messages_with_user(
        WORLD_ARCHITECT_SYSTEM,
        history,
        json!({"role": "user", "content": user_text.trim()}),
    )
}

/// The list (string-array) sections of the world bible. Flat tool fields; folded
/// into the nested `world_lore` object downstream by [`nest_draft_args`]. Keep in
/// sync with the frontend's LORE_PREVIEW_FIELDS.
pub const LORE_LIST_FIELDS: [&str; 21] = [
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
    "hidden_secrets",
    "location_rules",
    "prohibited_elements",
];

const WORLD_IMAGE_PROMPT_FIELD: &str = "world_image_prompt_en";
const WORLD_MAP_PROMPT_FIELD: &str = "world_map_prompt_en";

/// Top-level summary string fields of the draft.
const DRAFT_SUMMARY_FIELDS: [&str; 6] = [
    "title",
    "genre",
    "tone",
    "world_size",
    "population",
    "public_premise",
];

pub fn world_architect_tools() -> Vec<Value> {
    world_architect_tools_with_options(WorldArchitectOptions::default())
}

pub fn world_architect_tools_with_options(options: WorldArchitectOptions) -> Vec<Value> {
    vec![
        world_architect_tool_schema(options),
        world_architect_edit_tool_schema(options),
        world_architect_read_tool_schema(),
    ]
}

/// The `read_world_bible` tool: return the FULL current text of the named
/// sections/fields (the digest in the user message truncates entries). The
/// model must read a section before removing/replacing specific entries —
/// `remove` matches exact strings.
fn world_architect_read_tool_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "read_world_bible",
            "description": "The ONLY source of the current bible state. Pass section/field names (e.g. [\"taboos\", \"public_premise\"]) for their complete text, or omit/empty for the whole bible. ALWAYS read before edit_world_bible remove/replace of specific entries (remove matches exact strings) and before reasoning about existing content — the state may have changed between turns.",
            "parameters": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "sections": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Section/field names to read; empty or omitted = the whole bible."
                    }
                }
            }
        }
    })
}

/// The `edit_world_bible` tool: targeted patches to an existing bible so the
/// architect changes only what differs instead of resending the whole draft
/// (cheaper, far better prompt-cache hit). Applied by [`apply_world_bible_edit`].
fn world_architect_edit_tool_schema(options: WorldArchitectOptions) -> Value {
    let str_map = |description: &str| {
        json!({
            "type": "object",
            "additionalProperties": {"type": "string"},
            "description": description,
        })
    };
    let list_map = |description: &str| {
        json!({
            "type": "object",
            "additionalProperties": {"type": "array", "items": {"type": "string"}},
            "description": description,
        })
    };
    let scalar_keys = if options.image_prompts {
        "title, genre, tone, world_size, population, public_premise, hidden_premise, world_image_prompt_en, world_map_prompt_en"
    } else {
        "title, genre, tone, world_size, population, public_premise, hidden_premise"
    };
    let language_note = if options.image_prompts {
        "All canon text is Russian; world_image_prompt_en and world_map_prompt_en must be English image-generation prompts."
    } else {
        "All text in Russian."
    };
    let description = format!("Patch the EXISTING world bible — change only what differs, do NOT resend the whole draft. Prefer this over draft_world_bible once a draft exists. List-section keys: dogmas, world_laws, inhabitants, creatures, power_sources, technologies, taboos, conflicts, inspirations, regions, power_centers, religions, gods, cultures, history, economy, daily_life, story_hooks, hidden_secrets, location_rules, prohibited_elements. {language_note}");
    let set_description = format!(
        "Overwrite scalar fields. Keys: {scalar_keys}. Example: {{\"tone\": \"мрачный\"}}."
    );
    json!({
        "type": "function",
        "function": {
            "name": "edit_world_bible",
            "description": description,
            // Free-form section maps (properties-less objects) die under the
            // strict Responses conversion (forced additionalProperties:false +
            // properties:{} -> the model can only send {}); same reason
            // invoke_loaded_tool is non-strict.
            "strict": false,
            "parameters": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "set": str_map(&set_description),
                    "add": list_map("Append entries to list sections (existing entries kept). Example: {\"religions\": [\"культ безмолвных дорог\"]}."),
                    "remove": list_map("Remove exact entries from list sections. Example: {\"taboos\": [\"устаревшее табу\"]}."),
                    "replace": list_map("Replace whole list sections. Example: {\"gods\": [\"единственный молчаливый бог\"]}.")
                }
            }
        }
    })
}

/// The `draft_world_bible` tool schema.
///
/// FLAT schema: every bible section is a top-level field, not a nested object.
/// Flat tool arguments are more reliable for tool-calling models and drop the
/// duplicate name/size/population the nested shape carried. The backend folds
/// these back into the canonical nested `world_lore` for storage
/// ([`nest_draft_args`]).
///
/// Each list field is a string array; the model should fill every RELEVANT
/// section with several (≈3-6) concrete, specific entries in Russian — the
/// descriptions are the contract the model reads.
fn world_architect_tool_schema(options: WorldArchitectOptions) -> Value {
    // One field = one terse, front-loaded description. The "fill several concrete
    // entries" rule is stated ONCE in the function description below, not repeated
    // on all 21 list fields (that duplication is pure token bloat + drift).
    let lore_list = |description: &str| {
        json!({
            "type": "array",
            "items": {"type": "string"},
            "description": description,
        })
    };
    let mut properties = Map::new();
    properties.insert(
        "title".to_string(),
        json!({"type": "string", "description": "World title (a real name, not a placeholder)."}),
    );
    properties.insert(
        "genre".to_string(),
        json!({"type": "string", "description": "Short genre label, e.g. fantasy isekai or machine postapocalypse."}),
    );
    properties.insert(
        "tone".to_string(),
        json!({"type": "string", "description": "Short tone label, e.g. tense hopeful, bleak, mythic."}),
    );
    properties.insert(
        "world_size".to_string(),
        json!({"type": "string", "description": "Descriptive setting size and reach (a continent, a sector, a single mega-city). Describe the world, not a starting scope, in 1-3 sentences."}),
    );
    properties.insert(
        "population".to_string(),
        json!({"type": "string", "description": "Approximate population scale AND diversity: rough numbers plus the kinds of peoples, species and groups, in 1-3 sentences."}),
    );
    properties.insert(
        "public_premise".to_string(),
        json!({"type": "string", "description": "Player-safe premise of the world (no starting quest, no GM secrets): the core idea a player may know, in 1-3 sentences."}),
    );
    properties.insert(
        "hidden_premise".to_string(),
        json!({"type": "string", "description": "GM-only core truth behind the world that the player must NOT learn directly."}),
    );
    properties.insert(
        "dogmas".to_string(),
        lore_list("Core beliefs/axioms this world treats as truth (догматы)."),
    );
    properties.insert("world_laws".to_string(), lore_list("Hard rules of reality — magic, technology, divinity, death, travel — with their limits and costs."));
    properties.insert(
        "inhabitants".to_string(),
        lore_list("Peoples, species, classes and notable populations that live here."),
    );
    properties.insert(
        "creatures".to_string(),
        lore_list("Creatures, monsters and anomalies that may exist, and why they belong here."),
    );
    properties.insert("power_sources".to_string(), lore_list("Sources of power — magic systems, technologies, divine forces — and the price each demands."));
    properties.insert(
        "technologies".to_string(),
        lore_list("Material culture: tools, infrastructure, level and spread of technology."),
    );
    properties.insert(
        "taboos".to_string(),
        lore_list("Taboos, prohibitions and punishable acts."),
    );
    properties.insert(
        "conflicts".to_string(),
        lore_list("Standing tensions and conflicts that can fuel many future stories."),
    );
    properties.insert("inspirations".to_string(), lore_list("References this world draws on AND explicit anti-references (what it must NOT feel like)."));
    properties.insert(
        "regions".to_string(),
        lore_list("Macro regions, roads, borders, dangerous zones and climate pressures."),
    );
    properties.insert(
        "power_centers".to_string(),
        lore_list("Rulers, factions, institutions, guilds, armies and councils that hold power."),
    );
    properties.insert(
        "religions".to_string(),
        lore_list("Faiths, creeds, cults, heresies, rituals and afterlife beliefs."),
    );
    properties.insert(
        "gods".to_string(),
        lore_list("Gods, spirits or forces and the domains they govern."),
    );
    properties.insert(
        "cultures".to_string(),
        lore_list("Customs, languages, law, education, food and daily norms of peoples."),
    );
    properties.insert("history".to_string(), lore_list("Layered history — ancient origin, major breaks, recent causes. Avoid a one-cause history."));
    properties.insert(
        "economy".to_string(),
        lore_list("Scarcity, trade, resources, money, production, transport and debt."),
    );
    properties.insert(
        "daily_life".to_string(),
        lore_list("What common people know, fear, celebrate, punish and want."),
    );
    properties.insert(
        "story_hooks".to_string(),
        lore_list("Reusable tensions/hooks for future stories WITHOUT fixing a specific start."),
    );
    properties.insert(
        "hidden_secrets".to_string(),
        lore_list("GM-only secrets that must not leak to the player directly."),
    );
    properties.insert("location_rules".to_string(), lore_list("Rules every future generated location (city, village, room, dungeon, road) must respect."));
    properties.insert(
        "prohibited_elements".to_string(),
        lore_list("Things that must NOT appear in this world without a special reason."),
    );
    let language_note = if options.image_prompts {
        properties.insert(
            WORLD_IMAGE_PROMPT_FIELD.to_string(),
            json!({
                "type": "string",
                "description": "English image-generation prompt for a single atmospheric overview image showing what this world looks like. Describe visual style, terrain, settlements, peoples, technology/magic cues and mood. Not a map."
            }),
        );
        properties.insert(
            WORLD_MAP_PROMPT_FIELD.to_string(),
            json!({
                "type": "string",
                "description": "English image-generation prompt for a readable world map. Describe map style, geography, regions, borders, settlements, routes, labels/cartography and scale. This is a map, not a scene illustration."
            }),
        );
        " Canon fields are Russian; world_image_prompt_en and world_map_prompt_en must be English prompts."
    } else {
        " Write all values in Russian."
    };
    json!({
        "type": "function",
        "function": {
            "name": "draft_world_bible",
            "description": format!("Create or update the reusable world bible (world canon) as a FLAT draft: every field is top-level, nothing is nested. Fill every field the world can support — each list section gets several (about 3-6) concrete, specific entries, not one vague line. title/genre/tone/world_size/population/public_premise are the short player-facing summary; the list sections plus hidden_premise are the full canon. hidden_premise and hidden_secrets are GM-only and must stay out of the player-facing fields.{language_note}"),
            "parameters": {
                "type": "object",
                "additionalProperties": true,
                "properties": properties,
                "required": ["title", "genre", "tone", "world_size", "population", "public_premise"]
            }
        }
    })
}

/// Fold FLAT `draft_world_bible` arguments into the canonical draft shape the
/// store and UI expect: summary fields stay at the top level, every bible section
/// goes inside `world_lore`, and name/public_premise/world_size/population are
/// mirrored into `world_lore`. Robust to a model that nests `world_lore` anyway
/// (those keys are absorbed) so both shapes work.
fn nest_draft_args(args: &Map<String, Value>) -> Value {
    let mut top = Map::new();
    let mut lore = match args.get("world_lore") {
        Some(Value::Object(map)) => map.clone(),
        _ => Map::new(),
    };
    for (key, value) in args {
        match key.as_str() {
            "world_lore" => {}
            k if DRAFT_SUMMARY_FIELDS.contains(&k) => {
                top.insert(key.clone(), value.clone());
            }
            // hidden_premise + the list sections + any extra key live in the bible.
            _ => {
                lore.insert(key.clone(), value.clone());
            }
        }
    }
    // Mirror the player-facing summary into world_lore (game/location-gen read it
    // from there); never overwrite a value the model put in world_lore directly.
    if !lore.contains_key("name") {
        if let Some(title) = top.get("title") {
            lore.insert("name".to_string(), title.clone());
        }
    }
    for key in ["public_premise", "world_size", "population"] {
        if !lore.contains_key(key) {
            if let Some(value) = top.get(key) {
                lore.insert(key.to_string(), value.clone());
            }
        }
    }
    top.insert("world_lore".to_string(), Value::Object(lore));
    Value::Object(top)
}

/// Normalize the incoming draft (the frontend sends camelCase summary fields +
/// a nested `worldLore`) into the canonical nested shape the loop and tools
/// operate on. This is the base the agent merges/edits onto, and the state shown
/// to the model in the user message.
fn normalize_input_draft(draft: &Value) -> Value {
    let src = draft.as_object();
    let pick = |aliases: &[&str]| -> Option<Value> {
        let map = src?;
        aliases.iter().find_map(|key| map.get(*key)).cloned()
    };
    let mut top = Map::new();
    for (canon, aliases) in [
        ("title", &["title"][..]),
        ("genre", &["genre"][..]),
        ("tone", &["tone"][..]),
        ("world_size", &["world_size", "worldSize"][..]),
        ("population", &["population"][..]),
        ("public_premise", &["public_premise", "publicPremise"][..]),
    ] {
        if let Some(value) = pick(aliases) {
            if !value.is_null() {
                top.insert(canon.to_string(), value);
            }
        }
    }
    if let Some(Value::Object(lore)) = pick(&["world_lore", "worldLore"]) {
        top.insert("world_lore".to_string(), Value::Object(lore));
    }
    Value::Object(top)
}

/// Apply an `edit_world_bible` patch (set / add / remove / replace) onto the
/// current draft and return the new full draft. Lets the architect change a few
/// fields or grow a section without resending the whole bible.
fn apply_world_bible_edit(draft: &Value, args: &Map<String, Value>) -> Value {
    let mut top = match draft {
        Value::Object(map) => map.clone(),
        _ => Map::new(),
    };
    let mut lore = match top.get("world_lore") {
        Some(Value::Object(map)) => map.clone(),
        _ => Map::new(),
    };

    // set: overwrite scalar fields. Summary fields stay at the top level; every
    // other scalar (hidden_premise, …) belongs to the bible.
    if let Some(Value::Object(set)) = args.get("set") {
        for (key, value) in set {
            if DRAFT_SUMMARY_FIELDS.contains(&key.as_str()) {
                top.insert(key.clone(), value.clone());
            } else {
                lore.insert(key.clone(), value.clone());
            }
        }
    }
    // replace: swap a whole list section.
    if let Some(Value::Object(replace)) = args.get("replace") {
        for (key, value) in replace {
            lore.insert(key.clone(), value.clone());
        }
    }
    // add: append entries to a list section, skipping duplicates.
    if let Some(Value::Object(add)) = args.get("add") {
        for (key, value) in add {
            let Value::Array(items) = value else { continue };
            let entry = lore
                .entry(key.clone())
                .or_insert_with(|| Value::Array(Vec::new()));
            if let Value::Array(existing) = entry {
                for item in items {
                    if !existing.contains(item) {
                        existing.push(item.clone());
                    }
                }
            } else {
                *entry = Value::Array(items.clone());
            }
        }
    }
    // remove: drop matching entries from a list section.
    if let Some(Value::Object(remove)) = args.get("remove") {
        for (key, value) in remove {
            let Value::Array(items) = value else { continue };
            if let Some(Value::Array(existing)) = lore.get_mut(key) {
                existing.retain(|entry| !items.contains(entry));
            }
        }
    }

    top.insert("world_lore".to_string(), Value::Object(lore));
    Value::Object(top)
}

/// Mirror the player-facing summary into `world_lore` (the game + location
/// generator read name/premise/size/population from there) without clobbering
/// values the model set directly. Applied to the final draft each turn.
fn finalize_draft(draft: Value) -> Value {
    let mut top = match draft {
        Value::Object(map) => map,
        other => return other,
    };
    let mut lore = match top.get("world_lore") {
        Some(Value::Object(map)) => map.clone(),
        _ => Map::new(),
    };
    if !lore.contains_key("name") {
        if let Some(title) = top.get("title") {
            lore.insert("name".to_string(), title.clone());
        }
    }
    for key in ["public_premise", "world_size", "population"] {
        if !lore.contains_key(key) {
            if let Some(value) = top.get(key) {
                lore.insert(key.to_string(), value.clone());
            }
        }
    }
    top.insert("world_lore".to_string(), Value::Object(lore));
    Value::Object(top)
}

pub async fn world_architect_turn(
    client: &dyn Backend,
    history: &[Value],
    draft: &Value,
    user_text: &str,
    stream: &mut (dyn ArchitectStream + Send),
) -> Result<WorldArchitectOutput, BackendError> {
    world_architect_turn_with_options(
        client,
        history,
        draft,
        user_text,
        WorldArchitectOptions::default(),
        stream,
    )
    .await
}

pub async fn world_architect_turn_with_options(
    client: &dyn Backend,
    history: &[Value],
    draft: &Value,
    user_text: &str,
    options: WorldArchitectOptions,
    stream: &mut (dyn ArchitectStream + Send),
) -> Result<WorldArchitectOutput, BackendError> {
    // Thin config over the generic runner: prompt + tools + world-shaped
    // draft-folding. The loop body lives in `architect_runner` and is shared with
    // the story architect; this path stays byte-identical to the former inline loop.
    let config = WorldArchitectConfig { options };
    architect_turn(&config, client, history, draft, user_text, stream).await
}

/// Merge a `draft_world_bible` call's arguments into the accumulating draft:
/// top-level fields overwrite, `world_lore` is merged key-by-key so successive
/// refinements add sections instead of replacing the whole bible.
fn merge_draft(prev: Option<Value>, args: &Map<String, Value>) -> Value {
    let mut base = match prev {
        Some(Value::Object(m)) => m,
        _ => Map::new(),
    };
    for (key, value) in args {
        if key == "world_lore" {
            if let Value::Object(new_lore) = value {
                let entry = base
                    .entry("world_lore".to_string())
                    .or_insert_with(|| Value::Object(Map::new()));
                if let Value::Object(existing) = entry {
                    for (lk, lv) in new_lore {
                        existing.insert(lk.clone(), lv.clone());
                    }
                } else {
                    *entry = Value::Object(new_lore.clone());
                }
            }
        } else {
            base.insert(key.clone(), value.clone());
        }
    }
    Value::Object(base)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patch(base: &Value, args: Value) -> Value {
        apply_world_bible_edit(base, args.as_object().unwrap())
    }

    #[test]
    fn normalize_input_draft_canonicalizes_frontend_shape() {
        let draft = json!({
            "title": "Мир",
            "worldSize": "континент",
            "publicPremise": "клятвы",
            "worldLore": {"gods": ["Старший"], "dogmas": ["имя есть закон"]}
        });
        let n = normalize_input_draft(&draft);
        assert_eq!(n["title"], "Мир");
        assert_eq!(n["world_size"], "континент");
        assert_eq!(n["public_premise"], "клятвы");
        assert_eq!(n["world_lore"]["gods"][0], "Старший");
        assert!(n.get("worldSize").is_none());
    }

    #[test]
    fn edit_facts_report_hits_and_misses() {
        let config = WorldArchitectConfig {
            options: WorldArchitectOptions::default(),
        };
        let mut working = json!({
            "title": "Карагай",
            "world_lore": {"taboos": ["нельзя свистеть ночью", "нельзя сыпать соль"]}
        });
        let args = json!({
            "set": {"tone": "мрачный"},
            "add": {"taboos": ["нельзя сыпать соль", "нельзя лгать под знаменем"]},
            "remove": {"taboos": ["нельзя свистеть ночью", "такого табу нет"]}
        });
        let applied =
            config.apply_tool("edit_world_bible", args.as_object().unwrap(), &mut working);
        assert!(applied.changed);
        assert!(applied.result.contains("Поля обновлены: tone."));
        // One of the two adds was a duplicate.
        assert!(applied.result.contains("добавлено 1"), "{}", applied.result);
        assert!(applied.result.contains("пропущено как дубли"));
        // One remove hit, one missed — the miss is called out with the nudge.
        assert!(applied.result.contains("удалено 1"), "{}", applied.result);
        assert!(applied
            .result
            .contains("НЕ найдено для удаления: «такого табу нет»"));
        assert!(applied.result.contains("read_world_bible"));

        // A remove that matches nothing at all must NOT read as success.
        let miss_only = json!({"remove": {"taboos": ["мимо"]}});
        let applied = config.apply_tool(
            "edit_world_bible",
            miss_only.as_object().unwrap(),
            &mut working,
        );
        assert!(!applied.result.contains("удалено"));
        assert!(applied.result.contains("НЕ найдено для удаления"));
    }

    #[test]
    fn user_tail_is_raw_text_matching_stored_history() {
        // CACHE INVARIANT: sent tail == stored history entry, byte for byte.
        let messages = world_architect_messages(&[], "  Добавь религии.  ");
        let tail = messages.last().unwrap();
        assert_eq!(tail["role"], "user");
        assert_eq!(tail["content"], "Добавь религии.");
    }

    #[test]
    fn read_world_bible_renders_plain_text_not_json() {
        let config = WorldArchitectConfig {
            options: WorldArchitectOptions::default(),
        };
        let working = json!({
            "title": "Карагай",
            "world_lore": {
                "taboos": ["нельзя свистеть ночью", "нельзя сыпать соль"]
            }
        });
        let mut working = working;
        let mut args = Map::new();
        args.insert("sections".into(), json!(["taboos", "title", "nope"]));
        let out = config
            .apply_tool("read_world_bible", &args, &mut working)
            .result;
        assert!(out.contains("## taboos (2 записей)"));
        assert!(out.contains("- нельзя свистеть ночью"));
        assert!(out.contains("## title\nКарагай"));
        assert!(out.contains("Нет таких разделов: nope"));
        assert!(!out.trim_start().starts_with('{'), "not JSON: {out}");
    }

    #[test]
    fn edit_tool_is_non_strict_so_section_maps_survive_responses_conversion() {
        // Mirrors edit_story_plot: the strict Responses conversion would force
        // the free-form set/add/remove/replace maps to empty objects.
        let tools = world_architect_tools();
        let edit = tools
            .iter()
            .find(|t| t["function"]["name"] == "edit_world_bible")
            .expect("edit_world_bible present");
        assert_eq!(edit["function"]["strict"], json!(false));
    }

    #[test]
    fn image_prompt_fields_are_conditional_in_draft_tool() {
        let base_tools = world_architect_tools();
        let base_props = base_tools[0]["function"]["parameters"]["properties"]
            .as_object()
            .expect("base draft properties");
        assert!(!base_props.contains_key(WORLD_IMAGE_PROMPT_FIELD));
        assert!(!base_props.contains_key(WORLD_MAP_PROMPT_FIELD));

        let image_tools = world_architect_tools_with_options(WorldArchitectOptions {
            image_prompts: true,
        });
        let image_props = image_tools[0]["function"]["parameters"]["properties"]
            .as_object()
            .expect("image draft properties");
        assert_eq!(image_props[WORLD_IMAGE_PROMPT_FIELD]["type"], "string");
        assert_eq!(image_props[WORLD_MAP_PROMPT_FIELD]["type"], "string");
        assert!(image_props[WORLD_IMAGE_PROMPT_FIELD]["description"]
            .as_str()
            .unwrap_or_default()
            .contains("world looks"));

        let set_description = image_tools[1]["function"]["parameters"]["properties"]["set"]
            ["description"]
            .as_str()
            .unwrap_or_default();
        assert!(set_description.contains(WORLD_IMAGE_PROMPT_FIELD));
        assert!(set_description.contains(WORLD_MAP_PROMPT_FIELD));
    }

    #[test]
    fn image_prompt_fields_are_stored_in_world_lore() {
        let args = json!({
            "title": "Мир",
            "world_size": "континент",
            "world_image_prompt_en": "A sweeping fantasy world of oath-bound roads and moonlit shrines.",
            "world_map_prompt_en": "A parchment world map with seven kingdoms, spirit roads, shrines and labeled borders."
        });
        let nested = nest_draft_args(args.as_object().expect("object args"));
        assert_eq!(
            nested["world_lore"][WORLD_IMAGE_PROMPT_FIELD],
            "A sweeping fantasy world of oath-bound roads and moonlit shrines."
        );
        assert_eq!(
            nested["world_lore"][WORLD_MAP_PROMPT_FIELD],
            "A parchment world map with seven kingdoms, spirit roads, shrines and labeled borders."
        );
        assert!(nested.get(WORLD_IMAGE_PROMPT_FIELD).is_none());
        assert!(nested.get(WORLD_MAP_PROMPT_FIELD).is_none());
    }

    #[test]
    fn edit_set_routes_summary_to_top_and_scalars_to_lore() {
        let base = json!({"title": "Мир", "world_lore": {"gods": ["A"]}});
        let out = patch(
            &base,
            json!({"set": {
                "tone": "мрачный",
                "hidden_premise": "секрет",
                "world_image_prompt_en": "A bleak mythic world under a broken red moon.",
                "world_map_prompt_en": "A parchment map with ruined roads, spirit borders and labeled shrine-cities."
            }}),
        );
        assert_eq!(out["tone"], "мрачный"); // summary field → top level
        assert_eq!(out["world_lore"]["hidden_premise"], "секрет"); // other scalar → bible
        assert_eq!(
            out["world_lore"][WORLD_IMAGE_PROMPT_FIELD],
            "A bleak mythic world under a broken red moon."
        );
        assert_eq!(
            out["world_lore"][WORLD_MAP_PROMPT_FIELD],
            "A parchment map with ruined roads, spirit borders and labeled shrine-cities."
        );
        assert_eq!(out["world_lore"]["gods"][0], "A"); // untouched section kept
    }

    #[test]
    fn edit_add_is_idempotent_and_appends() {
        let base = json!({"world_lore": {"religions": ["культ дорог", "вера клятв"]}});
        let out = patch(
            &base,
            json!({"add": {"religions": ["вера клятв", "орден молчания"]}}),
        );
        assert_eq!(
            out["world_lore"]["religions"],
            json!(["культ дорог", "вера клятв", "орден молчания"])
        );
    }

    #[test]
    fn edit_remove_and_replace_sections() {
        let base =
            json!({"world_lore": {"religions": ["культ дорог", "вера клятв"], "gods": ["A"]}});
        let removed = patch(&base, json!({"remove": {"religions": ["культ дорог"]}}));
        assert_eq!(removed["world_lore"]["religions"], json!(["вера клятв"]));
        let replaced = patch(&base, json!({"replace": {"gods": ["единственный бог"]}}));
        assert_eq!(replaced["world_lore"]["gods"], json!(["единственный бог"]));
    }

    #[test]
    fn finalize_mirrors_summary_into_lore() {
        let drafted = finalize_draft(json!({
            "title": "Мир",
            "world_size": "континент",
            "world_lore": {"gods": ["A"]}
        }));
        assert_eq!(drafted["world_lore"]["name"], "Мир");
        assert_eq!(drafted["world_lore"]["world_size"], "континент");
        assert_eq!(drafted["world_lore"]["gods"][0], "A");
    }
}
