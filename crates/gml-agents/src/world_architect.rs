//! Dedicated world-architect agent.
//!
//! This is not the in-game GM and not the location generator. It is a separate
//! planning chat that helps the player author a reusable world bible. The only
//! mutating surface it has is a draft tool call; saving the draft creates a
//! standalone world, not a running campaign.

use serde_json::{json, Map, Value};

use gml_llm::{Backend, BackendError, DeltaSink};
use gml_types::Role;

/// Safety cap on the architect agent loop (think → draft → reply, possibly
/// refined across several `draft_world_bible` calls). Mirrors the GM turn's
/// `max_tool_hops`: a real model normally ends in 2-3 hops, but a degenerate
/// model that keeps calling the tool must still terminate.
const MAX_ARCHITECT_HOPS: usize = 6;

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

/// Streaming sink for the architect agent loop. The server implements this to
/// forward segments to the SSE client; tests can use a no-op. Each segment is
/// tagged with a `sid` (one per agent hop) so the UI groups deltas into separate
/// reasoning spoilers and reply bubbles, exactly like the main GM turn.
pub trait ArchitectStream {
    /// A content/thinking delta for the current hop. `channel` is
    /// [`gml_llm::channel::THINKING`] or [`gml_llm::channel::CONTENT`].
    fn delta(&mut self, channel: &str, text: &str, sid: &str);
    /// A tool call the model just made, surfaced inline as it happens.
    fn tool(&mut self, call: &Value, sid: &str);
}

/// A no-op [`ArchitectStream`] for callers that don't need streaming.
pub struct NullArchitectStream;
impl ArchitectStream for NullArchitectStream {
    fn delta(&mut self, _channel: &str, _text: &str, _sid: &str) {}
    fn tool(&mut self, _call: &Value, _sid: &str) {}
}

#[derive(Clone, Copy, Debug, Default)]
pub struct WorldArchitectOptions {
    pub image_prompts: bool,
}

/// Per-hop adapter so `chat_stream` (which wants a [`DeltaSink`]) forwards its
/// deltas to the architect stream tagged with this hop's `sid`.
struct HopSink<'a> {
    sid: String,
    inner: &'a mut (dyn ArchitectStream + Send),
}

impl DeltaSink for HopSink<'_> {
    fn emit(&mut self, ch: &str, text: &str) {
        if text.is_empty() {
            return;
        }
        self.inner.delta(ch, text, &self.sid);
    }
}

pub struct WorldArchitectOutput {
    /// The model's final chat reply (the last text segment of the turn).
    pub reply: String,
    pub draft: Option<Value>,
    pub user_msg: Value,
    pub assistant_history_msg: Value,
    pub assistant_msg: Value,
    pub calls: Vec<Value>,
    /// Ordered visible segments produced THIS turn — `think`, `assistant` and
    /// `tool` entries in production order — to append to the persisted chat and
    /// restore the interleaved view on reload.
    pub visible_segments: Vec<Value>,
    /// All reasoning text from the turn, joined (for the debug view).
    pub thinking: String,
    /// Summed `_meta` stats across every hop (cached_tokens, prompt_eval_count,
    /// eval_count) — drives the architect token-usage readout.
    pub stats: Map<String, Value>,
    /// The first-hop request messages array sent to the model (for the debug view).
    pub request_messages: Value,
}

pub fn world_architect_messages(history: &[Value], draft: &Value, user_text: &str) -> Vec<Value> {
    let user_msg = world_architect_user_message(draft, user_text);
    world_architect_messages_with_user(history, user_msg)
}

pub fn world_architect_user_message(draft: &Value, user_text: &str) -> Value {
    // Show the model the canonical draft shape (snake_case + nested world_lore) so
    // it can reference exact field/section names and existing entries when editing.
    let normalized = normalize_input_draft(draft);
    let draft_json = serde_json::to_string(&normalized).unwrap_or_else(|_| "null".to_string());
    json!({
        "role": "user",
        "content": format!(
            "## Current Draft JSON\n{draft_json}\n\n## User Message\n{}\n\nAnswer now. If the bible is empty, build it with draft_world_bible, filling every relevant section in detail (several concrete entries each). If it already has content, apply your changes with edit_world_bible (set/add/remove/replace) instead of resending the whole draft. Ask questions only if something important is genuinely missing.",
            user_text.trim()
        )
    })
}

fn world_architect_messages_with_user(history: &[Value], user_msg: Value) -> Vec<Value> {
    let mut messages = vec![json!({"role": "system", "content": WORLD_ARCHITECT_SYSTEM})];
    messages.extend(history.iter().filter_map(history_message));
    messages.push(user_msg);
    messages
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
    ]
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
    let user_msg = world_architect_user_message(draft, user_text);
    // The running model conversation: system + history + user, then assistant
    // turns and tool results appended as the loop drives the agent.
    let mut messages = world_architect_messages_with_user(history, user_msg.clone());
    let request_messages = Value::Array(messages.clone());
    let tools = Value::Array(world_architect_tools_with_options(options));

    let mut visible_segments: Vec<Value> = Vec::new();
    let mut all_calls: Vec<Value> = Vec::new();
    let mut thinking_parts: Vec<String> = Vec::new();
    // The full draft state the agent mutates this turn — seeded from the current
    // draft so edit_world_bible patches apply to the real bible, not a blank one.
    let mut working_draft = normalize_input_draft(draft);
    let mut draft_changed = false;
    let mut reply = String::new();
    let mut stats = Map::new();

    let mut hop = 0usize;
    loop {
        hop += 1;
        let sid = format!("arch-{hop}");

        // Stream this hop. Thinking/content deltas are tagged with `sid` so the
        // UI renders a separate reasoning spoiler and reply bubble per hop.
        let output = {
            let mut seg = HopSink {
                sid: sid.clone(),
                inner: &mut *stream,
            };
            client
                .chat_stream(
                    &Value::Array(messages.clone()),
                    Some(&tools),
                    Some(true),
                    Role::Gm.as_str(),
                    &mut seg,
                )
                .await?
        };

        accumulate_stats(&mut stats, &output.stats);

        let thinking = output.thinking.trim().to_string();
        if !thinking.is_empty() {
            visible_segments.push(json!({"role": "think", "content": thinking, "sid": sid}));
            thinking_parts.push(thinking);
        }

        let content = output.content.trim().to_string();

        // No tool calls → this hop's text is the final reply; end the turn.
        if output.calls.is_empty() {
            if !content.is_empty() {
                visible_segments
                    .push(json!({"role": "assistant", "content": content.clone(), "sid": sid}));
                reply = content;
            }
            messages.push(output.assistant_msg);
            break;
        }

        // Intermediate text alongside a tool call (a "response between tools").
        if !content.is_empty() {
            visible_segments
                .push(json!({"role": "assistant", "content": content.clone(), "sid": sid}));
        }

        let normalized = normalize_architect_calls(&output.calls, &sid);
        messages.push(assistant_with_tool_calls(output.assistant_msg, &normalized));

        for (name, args, id) in &normalized {
            // draft_world_bible builds/rebuilds (FLAT args → nested world_lore);
            // edit_world_bible patches the existing draft in place. The card/event
            // shows what the model actually sent.
            let tool_args = match name.as_str() {
                "draft_world_bible" => {
                    let nested = nest_draft_args(args);
                    if let Value::Object(map) = &nested {
                        working_draft = merge_draft(Some(working_draft), map);
                        draft_changed = true;
                    }
                    nested
                }
                "edit_world_bible" => {
                    working_draft = apply_world_bible_edit(&working_draft, args);
                    draft_changed = true;
                    Value::Object(args.clone())
                }
                _ => Value::Object(args.clone()),
            };
            let call_json = json!({"name": name, "arguments": tool_args, "id": id});
            all_calls.push(call_json.clone());
            visible_segments
                .push(json!({"role": "tool", "name": name, "args": tool_args, "sid": sid}));
            stream.tool(&call_json, &sid);
            // Feed the result back so the model can keep refining or finish with a
            // chat reply (this is what makes it an agent loop, not a one-shot).
            messages.push(json!({
                "role": "tool",
                "tool_call_id": id,
                "content": architect_tool_result(name),
            }));
        }

        if hop >= MAX_ARCHITECT_HOPS {
            break;
        }
    }

    let assistant_history_msg = json!({"role": "assistant", "content": reply});
    Ok(WorldArchitectOutput {
        reply,
        // The full draft after this turn's build/edits (None if nothing changed).
        draft: if draft_changed {
            Some(finalize_draft(working_draft))
        } else {
            None
        },
        user_msg,
        assistant_history_msg: assistant_history_msg.clone(),
        assistant_msg: assistant_history_msg,
        calls: all_calls,
        visible_segments,
        thinking: thinking_parts.join("\n\n"),
        stats,
        request_messages,
    })
}

/// The model-facing result of a `draft_world_bible` call. The architect tool has
/// no real side effect beyond recording the draft, so the result just confirms
/// success and nudges the model to either refine further or finish with a reply.
fn architect_tool_result(name: &str) -> String {
    match name {
        "draft_world_bible" => json!({
            "ok": true,
            "status": "draft_updated",
            "note": "Черновик мира создан/обновлён и показан пользователю. Дальше правь точечно через edit_world_bible (не пересылай весь черновик), либо кратко ответь пользователю в чат."
        })
        .to_string(),
        "edit_world_bible" => json!({
            "ok": true,
            "status": "draft_edited",
            "note": "Правка применена к черновику и показана пользователю. Продолжай точечные правки или кратко ответь в чат."
        })
        .to_string(),
        _ => json!({"ok": false, "error": format!("unknown architect tool: {name}")}).to_string(),
    }
}

/// Assign a stable id to each call and return `(name, args, id)` tuples.
fn normalize_architect_calls(
    calls: &[gml_types::ParsedCall],
    sid: &str,
) -> Vec<(String, Map<String, Value>, String)> {
    calls
        .iter()
        .enumerate()
        .map(|(idx, call)| {
            let id = if call.id.trim().is_empty() {
                format!("{sid}_{}", idx + 1)
            } else {
                call.id.clone()
            };
            (call.name.clone(), call.arguments.clone(), id)
        })
        .collect()
}

/// Attach `tool_calls` to the assistant message so the next request is a valid
/// tool-call/tool-result pair (mirrors the orchestrator helper).
fn assistant_with_tool_calls(
    assistant_msg: Value,
    calls: &[(String, Map<String, Value>, String)],
) -> Value {
    let mut msg = match assistant_msg {
        Value::Object(m) => m,
        other => return other,
    };
    let raw_calls: Vec<Value> = calls
        .iter()
        .filter(|(name, _, _)| !name.trim().is_empty())
        .map(|(name, args, id)| {
            json!({
                "id": id,
                "type": "function",
                "function": {
                    "name": name,
                    "arguments": serde_json::to_string(&Value::Object(args.clone()))
                        .unwrap_or_else(|_| "{}".to_string()),
                },
            })
        })
        .collect();
    if !raw_calls.is_empty() {
        msg.insert("tool_calls".to_string(), Value::Array(raw_calls));
    }
    Value::Object(msg)
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

/// Sum integer `_meta` counters across hops (token counts add up like the main
/// chat's per-turn total); non-integer/last-wins for everything else.
fn accumulate_stats(acc: &mut Map<String, Value>, next: &Map<String, Value>) {
    for (key, value) in next {
        let summed = match (acc.get(key), value) {
            (Some(a), b) if a.is_i64() && b.is_i64() => Some(Value::from(
                a.as_i64().unwrap_or(0) + b.as_i64().unwrap_or(0),
            )),
            _ => None,
        };
        acc.insert(key.clone(), summed.unwrap_or_else(|| value.clone()));
    }
}

fn history_message(message: &Value) -> Option<Value> {
    let object = message.as_object()?;
    let role = object.get("role").and_then(Value::as_str)?;
    if !matches!(role, "user" | "assistant") {
        return None;
    }
    let content = object.get("content").and_then(Value::as_str)?.trim();
    if content.is_empty() {
        return None;
    }
    Some(json!({"role": role, "content": content}))
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
