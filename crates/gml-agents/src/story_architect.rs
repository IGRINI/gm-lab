//! Dedicated STORY architect agent (`docs/CHARACTERS_AND_STORY_TZ.md` §С1.2).
//!
//! This is the story-level sibling of [`crate::world_architect`]: a planning chat
//! that helps the user author a reusable PLOT on top of an ALREADY-EXISTING world
//! bible. It is a THIN CONFIG over the shared [`crate::architect_runner`] loop —
//! same agent discipline (think → tool → reply), same streaming, same stats — it
//! only swaps the prompt, the tools, and the draft-folding for the plot shape.
//!
//! The tool schema targets EXACTLY the authored-plot runtime contract consumed by
//! `World::overlay_authored_plot` (`gml-world/src/world.rs`) and NOTHING else:
//! `title, description, story_brief, public_intro, hidden_truth,
//! player_character{...}, scene{title,description,location_id,present_npcs,exits,
//! items,constraints,tension}, npcs[], public_facts[], state_records[],
//! proper_nouns[], time`. There are deliberately NO acts / objectives / endings —
//! the runtime does not read them (`§С1.2`, future "plot progression engine").
//!
//! Unlike the world bible (whose lore is a FLAT list of sections), a plot is
//! genuinely NESTED (a scene object, a player-character object, arrays of NPC
//! cards), so `draft_story_plot` is a nested schema and the merge is a shallow
//! top-level merge with a special-case deep merge for `scene`.

use serde_json::{json, Map, Value};

use gml_llm::{Backend, BackendError};

use crate::architect_runner::{
    architect_messages_with_system_blocks, architect_turn, ArchitectConfig, ArchitectOutput,
    ArchitectStream, ToolApplication,
};

/// The story architect turn output — the generic [`ArchitectOutput`] under the
/// domain name (mirrors `WorldArchitectOutput`).
pub type StoryArchitectOutput = ArchitectOutput;

/// Canon-authoring rules for the STORY architect. Russian canon like the world
/// one; authors plots ONLY within the given world bible; questions go in the
/// chat reply, not a tool field; same agent-loop discipline.
pub const STORY_ARCHITECT_SYSTEM: &str = r#"You are the GM-Lab story architect. You help the user author a reusable STORY
(a plot) that runs ON TOP OF an already-built world bible. The bound world's
canon is given to you below as a read-only reference — you do NOT edit the world,
only write a story that lives inside it. Write all story text in Russian; keep it
concrete.

You author a playthrough START, not new world canon: define the opening situation
of ONE story — its premise, hidden truth, the suggested protagonist, the starting
scene, the people in it, the public facts and initial state. Everything you write
must be consistent with the bound world bible (its laws, powers, factions,
secrets); reuse its proper nouns and honor its location_rules and taboos. Do not
invent world-level canon that contradicts the bible.

Build the plot with draft_story_plot. Make the first draft rich and playable:
a clear story_brief (what the player is and what pulls them in), a player-safe
public_intro, a GM-only hidden_truth, a concrete starting scene with a couple of
present NPCs, a few public_facts, and a suggested player_character. The tool's
field descriptions define what each field means and what is player-facing vs
GM-only — follow them. hidden_truth and NPC secrets are GM-only and must not leak
into public_intro or public_facts.

Once a plot exists, make changes with edit_story_plot — patch only what differs
(set a scalar or a whole object like scene/player_character; add/remove/replace
entries in the list sections npcs, public_facts, state_records, proper_nouns, and
in the scene lists present_npcs, exits, items). Do NOT resend the whole plot with
draft_story_plot for a small change; reserve draft_story_plot for the first build
or a deliberate full rebuild.

The plot itself lives on the server; user messages carry ONLY the user's text.
The single source of the current state is the read_story_plot tool. When the
conversation is empty and the user asks for a new story, build it straight away
with draft_story_plot. In every other case, before editing existing content,
before removing/replacing specific entries, and before making claims about what
the plot already says — call read_story_plot for the relevant sections (or the
whole plot) and act on what it returns. The state may have changed between
turns (the user edits fields by hand in the form). Never invent or guess
current content, and never ask the user to paste it.

The player_character you author is only a SUGGESTED protagonist — the player may
pick a different hero at launch, so write the story so its facts and NPCs still
read sensibly around a different protagonist where possible.

Ask the user a question only when something important is genuinely missing or
unclear, and ask it in your chat reply, not in a tool field. Otherwise just note
briefly what you built or changed; questions are not required every turn.

How you work, like an agent: think about what the plot needs, then update it with
a tool (draft_story_plot to build, edit_story_plot to change), then finish the
turn with a short chat reply about what you built or changed. You may call tools
more than once per turn. Each tool result comes back to you, so you can keep going
or wrap up — but always end the turn with a reply, never on a bare tool call.

Do NOT author acts, objectives, chapters or endings — this engine does not track
them yet. Author only the opening state listed above."#;

// The plot field families the tool schema targets (documented in each tool
// description rather than looked up here — the ops route by key prefix, not by a
// static membership set):
//   scalars: title, description, story_brief, public_intro, hidden_truth
//   objects: player_character, scene
//   top lists: npcs, public_facts, state_records, proper_nouns
//   scene lists (via `scene.<name>`): present_npcs, exits, items

// =========================================================================
// public message / tool builders (mirror the world architect surface)
// =========================================================================

/// Assemble the story-architect request messages: system + read-only world lore
/// block + filtered history + the user message. `world_lore_block` is the
/// pre-serialized, image-field-stripped bound-world context (`§С1.2`), placed as
/// a STABLE system block so the cache prefix holds across turns.
///
/// CACHE INVARIANT: the tail user message is the RAW user text — byte-equal to
/// the history entry the server stores for this turn. State never rides in
/// messages; the model reads it via read_story_plot.
pub fn story_architect_messages(
    history: &[Value],
    world_lore_block: &str,
    user_text: &str,
) -> Vec<Value> {
    architect_messages_with_system_blocks(
        STORY_ARCHITECT_SYSTEM,
        &[world_lore_block.to_string()],
        history,
        json!({"role": "user", "content": user_text.trim()}),
    )
}

pub fn story_architect_tools() -> Vec<Value> {
    vec![
        draft_story_plot_schema(),
        edit_story_plot_schema(),
        read_story_plot_schema(),
    ]
}

/// The `read_story_plot` tool: return the FULL current text of the named plot
/// sections. Supports the same `scene.<name>` addressing as `edit_story_plot`.
fn read_story_plot_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "read_story_plot",
            "description": "The ONLY source of the current plot state. Pass section names (e.g. [\"hidden_truth\", \"npcs\", \"scene.items\"]) for their complete content, or omit/empty for the whole plot. ALWAYS read before edit_story_plot remove/replace of specific entries, before rewriting a field, and before reasoning about existing content — the state may have changed between turns.",
            "parameters": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "sections": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Section names to read (top-level keys or scene.<list>); empty or omitted = the whole plot."
                    }
                }
            }
        }
    })
}

/// Image/URL fields of the world lore that MUST NOT be injected into the story
/// architect context (`§С1.2`): they are English image-gen prompts / servable
/// URLs, irrelevant to plotting and pure token bloat.
const WORLD_LORE_IMAGE_FIELDS: [&str; 4] = [
    "world_image_prompt_en",
    "world_map_prompt_en",
    "world_image_url",
    "world_map_url",
];

/// Build the read-only bound-world context block from a world's `world_lore`
/// object (`§С1.2`). Image/URL fields are stripped; the FULL internal lore
/// (including `hidden_premise` / `hidden_secrets` — the story architect is
/// GM-trusted) is kept and serialized as a stable, human-readable JSON block
/// under a heading. `world_lore` is the raw object from the saved world package;
/// a non-object yields a minimal "no bible" note (the caller should have
/// validated, but this never panics).
pub fn story_architect_world_lore_block(world_lore: &Value) -> String {
    let mut lore = match world_lore {
        Value::Object(map) => map.clone(),
        _ => Map::new(),
    };
    for key in WORLD_LORE_IMAGE_FIELDS {
        lore.remove(key);
    }
    let json =
        serde_json::to_string_pretty(&Value::Object(lore)).unwrap_or_else(|_| "{}".to_string());
    format!(
        "## BOUND WORLD BIBLE (read-only reference)\n\
        This is the canon of the world your story runs in. Do NOT edit it — write a plot that fits it. \
        The hidden_premise/hidden_secrets are GM-only truths; you may use them to author hidden_truth and NPC secrets, but they must not leak into player-facing fields.\n\n\
        {json}"
    )
}

/// The `draft_story_plot` tool schema — a NESTED plot draft faithful to the
/// runtime contract. `title`, `story_brief`, `public_intro` are required (the
/// minimum a launchable story needs); everything else is optional.
fn draft_story_plot_schema() -> Value {
    let str_arr = |description: &str| json!({"type": "array", "items": {"type": "string"}, "description": description});
    // Scene items/exits are OBJECTS — the same contract the world seed and the
    // GM set_scene tool use (a plain string coerces with portable:false, which
    // makes an authored prop untakeable, so the schema demands the full shape).
    let item_arr = json!({
        "type": "array",
        "description": "Notable objects visible/available in the scene (full objects, not strings).",
        "items": {"type": "object", "properties": {
            "id": {"type": "string", "description": "Stable ascii snake_case id (a-z, 0-9, _ only)."},
            "name": {"type": "string", "description": "Item name (Russian)."},
            "details": {"type": "string", "description": "Short prose detail the player can learn (Russian)."},
            "portable": {"type": "boolean", "description": "true = the player can pick it up (notes, keys, tools); false = scenery/furniture."},
            "visible": {"type": "boolean", "description": "Visible to the player at scene start (default true)."},
            "owner": {"type": "string", "description": "npc id who owns/holds it (empty = unowned)."},
            "location": {"type": "string", "description": "Where in the scene it sits (Russian, e.g. 'на стойке')."}
        }, "required": ["name", "portable"], "additionalProperties": false}
    });
    let exit_arr = json!({
        "type": "array",
        "description": "Ways out of the scene (full objects, not strings).",
        "items": {"type": "object", "properties": {
            "id": {"type": "string", "description": "Stable ascii snake_case id (a-z, 0-9, _ only)."},
            "name": {"type": "string", "description": "Player-facing exit label (Russian, e.g. 'двор к пирсу')."},
            "destination": {"type": "string", "description": "Where it leads: an ascii snake_case location_id (a-z, 0-9, _ only) or a short Russian phrase."},
            "visible": {"type": "boolean", "description": "Visible at scene start (default true)."},
            "blocked_by": {"type": "string", "description": "What blocks it (empty = passable)."}
        }, "required": ["name", "destination"], "additionalProperties": false}
    });
    let scene_schema = json!({
        "type": "object",
        "additionalProperties": true,
        "description": "The starting scene the player opens in.",
        "properties": {
            "title": {"type": "string", "description": "Short scene/location name (Russian)."},
            "description": {"type": "string", "description": "What the player sees on arrival — concrete, sensory (Russian)."},
            "location_id": {"type": "string", "description": "Stable ascii snake_case id (a-z, 0-9, _ only) for this place (honored verbatim in canon)."},
            "present_npcs": str_arr("Ids of NPCs present in the scene at the start (must match npcs[].id)."),
            "exits": exit_arr,
            "items": item_arr,
            "constraints": str_arr("Hard limits on the scene (what is impossible or forbidden here)."),
            "tension": {"type": "string", "description": "The immediate pressure that makes this a scene, not a lobby."}
        }
    });
    let pc_schema = json!({
        "type": "object",
        "additionalProperties": true,
        "description": "The SUGGESTED protagonist (the player may override at launch). Author name/pronouns/class_role/background and any card fields that fit; stats may be left to defaults.",
        "properties": {
            "name": {"type": "string", "description": "Protagonist name (Russian)."},
            "pronouns": {"type": "string", "description": "Pronouns / grammatical gender."},
            "class_role": {"type": "string", "description": "Role/archetype (e.g. вольная сыщица, морской досмотрщик)."},
            "background": {"type": "string", "description": "One-line background that ties the hero to this story."}
        }
    });
    let npc_schema = json!({
        "type": "object",
        "additionalProperties": true,
        "description": "One NPC card for the opening cast.",
        "properties": {
            "id": {"type": "string", "description": "Stable ascii snake_case id (a-z, 0-9, _ only; referenced by scene.present_npcs)."},
            "name": {"type": "string", "description": "NPC name (Russian)."},
            "role": {"type": "string", "description": "Their function in the scene (e.g. староста, стражник)."},
            "persona": {"type": "string", "description": "How they present — manner, mood, surface (Russian)."},
            "secret": {"type": "string", "description": "GM-only secret; must not leak to the player directly."}
        }
    });
    let fact_schema = json!({
        "type": "object",
        "additionalProperties": true,
        "description": "One starting public fact.",
        "properties": {
            "id": {"type": "string", "description": "Stable ascii snake_case id (a-z, 0-9, _ only)."},
            "text": {"type": "string", "description": "The fact as the world knows it (Russian)."},
            "kind": {"type": "string", "enum": ["public", "truth", "rumor"], "description": "public (openly known), rumor (unconfirmed), or truth (GM-confirmed)."},
            "keywords": str_arr("Keywords for retrieval."),
            "source": {"type": "string", "description": "Who/what this fact comes from."},
            "confirmed": {"type": "boolean", "description": "Whether the fact is established (default true)."}
        }
    });
    let state_schema = json!({
        "type": "object",
        "additionalProperties": true,
        "description": "One initial state record (a tracked situation/relationship/condition).",
        "properties": {
            "id": {"type": "string", "description": "Stable ascii snake_case id (a-z, 0-9, _ only)."},
            "text": {"type": "string", "description": "The state as text (Russian)."},
            "kind": {"type": "string", "description": "Record kind (e.g. situation, relationship, condition)."},
            "scope": {"type": "string", "description": "Visibility scope of the record."}
        }
    });
    let mut properties = Map::new();
    properties.insert("title".into(), json!({"type": "string", "description": "Story title (a real name, not a placeholder; Russian)."}));
    properties.insert("description".into(), json!({"type": "string", "description": "Short library description of the story (Russian)."}));
    properties.insert("story_brief".into(), json!({"type": "string", "description": "The player-facing setup: who the player is and what pulls them in (a few sentences, Russian)."}));
    properties.insert("public_intro".into(), json!({"type": "string", "description": "Player-safe opening framing of the situation — no GM secrets (Russian)."}));
    properties.insert("hidden_truth".into(), json!({"type": "string", "description": "GM-only truth behind the story that the player must NOT learn directly (Russian)."}));
    properties.insert("player_character".into(), pc_schema);
    properties.insert("scene".into(), scene_schema);
    properties.insert("npcs".into(), json!({"type": "array", "items": npc_schema, "description": "The opening cast (a couple to a handful of NPC cards)."}));
    properties.insert("public_facts".into(), json!({"type": "array", "items": fact_schema, "description": "Facts the world starts knowing (player-safe unless kind=truth)."}));
    properties.insert("state_records".into(), json!({"type": "array", "items": state_schema, "description": "Initial tracked state (situations, relationships, conditions)."}));
    properties.insert(
        "proper_nouns".into(),
        str_arr("Proper nouns this story introduces (names to keep spelled consistently)."),
    );
    properties.insert("time".into(), json!({"type": "integer", "description": "Start time as absolute minutes since midnight (e.g. 480 = 08:00). Avoid 0 (midnight)."}));

    json!({
        "type": "function",
        "function": {
            "name": "draft_story_plot",
            "description": "Create or update the authored story PLOT for the bound world. Author a playable OPENING only (no acts/objectives/endings). Fill every field the story can support. story_brief/public_intro are player-facing; hidden_truth and NPC secrets are GM-only. Reuse the bound world's canon and proper nouns; write all values in Russian.",
            "parameters": {
                "type": "object",
                "additionalProperties": true,
                "properties": properties,
                "required": ["title", "story_brief", "public_intro"]
            }
        }
    })
}

/// The `edit_story_plot` tool schema — targeted patches to an existing plot.
/// `set` overwrites scalars AND whole object sections (scene/player_character/
/// time); `add`/`remove`/`replace` operate on the list sections (`npcs`,
/// `public_facts`, `state_records`, `proper_nouns`, and `scene.present_npcs`,
/// `scene.exits`, `scene.items`).
fn edit_story_plot_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "edit_story_plot",
            "description": "Patch the EXISTING story plot — change only what differs, do NOT resend the whole plot. Prefer this over draft_story_plot once a plot exists. set overwrites scalars (title, description, story_brief, public_intro, hidden_truth) and whole objects (player_character, scene, time). add/remove/replace target list sections: npcs, public_facts, state_records, proper_nouns, and the scene lists scene.present_npcs, scene.exits, scene.items. All text in Russian.",
            // Free-form section maps (properties-less objects) die under the
            // strict Responses conversion — it forces additionalProperties:false
            // + properties:{}, so the model can only ever send {}. Same reason
            // invoke_loaded_tool is non-strict.
            "strict": false,
            "parameters": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "set": {
                        "type": "object",
                        "additionalProperties": true,
                        "description": "Overwrite scalars (title/description/story_brief/public_intro/hidden_truth) and whole objects (player_character/scene/time). Example: {\"hidden_truth\": \"...\"} or {\"scene\": {\"title\": \"Ворота\", \"tension\": \"...\"}}."
                    },
                    "add": {
                        "type": "object",
                        "additionalProperties": {"type": "array"},
                        "description": "Append entries to a list section (existing kept). Keys: npcs, public_facts, state_records, proper_nouns, scene.present_npcs, scene.exits, scene.items. scene.items/scene.exits take the same OBJECT entries as draft_story_plot (items: name+portable required; exits: name+destination required). Example: {\"scene.items\": [{\"name\": \"записка\", \"portable\": true}]}."
                    },
                    "remove": {
                        "type": "object",
                        "additionalProperties": {"type": "array"},
                        "description": "Remove entries from a list section. For object sections (npcs/public_facts/state_records/scene.items/scene.exits) pass the id or exact name as a string; for string sections pass the exact strings. Example: {\"npcs\": [\"starosta\"], \"scene.items\": [\"записка\"]}."
                    },
                    "replace": {
                        "type": "object",
                        "additionalProperties": {"type": "array"},
                        "description": "Replace a whole list section. Example: {\"proper_nouns\": [\"Новая Дорога\"]}."
                    }
                }
            }
        }
    })
}

// =========================================================================
// draft folding (mirrors world_architect's merge/apply/finalize for the plot)
// =========================================================================

/// Normalize the incoming plot draft into the canonical shape the loop mutates.
/// The frontend sends the plot object mostly as-is (snake_case, nested), so this
/// is a light pass: keep an object, drop nothing. (Kept as a seam so the frontend
/// can evolve without touching the loop.)
fn normalize_input_plot(draft: &Value) -> Value {
    match draft {
        Value::Object(map) => Value::Object(map.clone()),
        _ => Value::Object(Map::new()),
    }
}

/// Merge a `draft_story_plot` call's arguments into the accumulating plot draft:
/// top-level scalars/lists overwrite; `scene` and `player_character` are merged
/// key-by-key so a partial re-draft refines an object instead of nuking it.
fn merge_plot(prev: Value, args: &Map<String, Value>) -> Value {
    let mut base = match prev {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    for (key, value) in args {
        if matches!(key.as_str(), "scene" | "player_character") {
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

/// Apply an `edit_story_plot` patch (set / add / remove / replace) onto the
/// current plot draft and return the new full draft.
fn apply_story_plot_edit(draft: &Value, args: &Map<String, Value>) -> Value {
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
    // replace / add / remove: list-section operations. A `scene.<name>` key edits
    // the corresponding array INSIDE the scene object; a bare key edits a
    // top-level list.
    if let Some(Value::Object(replace)) = args.get("replace") {
        for (key, value) in replace {
            let Value::Array(items) = value else { continue };
            set_list_section(&mut top, key, items.clone());
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

/// Resolve a list-section key to a mutable `Vec<Value>` slot: a `scene.<name>`
/// key targets the array inside the scene object, a bare key a top-level array.
/// Returns the current items (empty if absent) plus a setter closure applied by
/// the caller — expressed here as helper fns instead to keep borrow-checking simple.
fn set_list_section(top: &mut Map<String, Value>, key: &str, items: Vec<Value>) {
    if let Some(scene_key) = key.strip_prefix("scene.") {
        let scene = top
            .entry("scene".to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        if let Value::Object(scene_obj) = scene {
            scene_obj.insert(scene_key.to_string(), Value::Array(items));
        }
    } else {
        top.insert(key.to_string(), Value::Array(items));
    }
}

fn add_to_list_section(top: &mut Map<String, Value>, key: &str, items: &[Value]) {
    let slot = list_section_slot(top, key);
    for item in items {
        // Object entries dedup by id, else by name (scene items/exits often
        // carry no id); string entries dedup by exact value.
        let dup = match item {
            Value::Object(obj) => {
                if let Some(id) = obj.get("id").and_then(Value::as_str) {
                    slot.iter().any(|e| entry_id(e) == Some(id))
                } else if let Some(name) = obj.get("name").and_then(Value::as_str) {
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
            // A string target removes the object entry whose id OR name matches,
            // or the exact string entry.
            Value::String(s) => {
                entry_id(entry) == Some(s.as_str())
                    || entry_name(entry) == Some(s.as_str())
                    || entry == target
            }
            _ => entry == target,
        })
    });
}

/// Borrow (creating if needed) the `Vec<Value>` slot for a list-section key.
fn list_section_slot<'a>(top: &'a mut Map<String, Value>, key: &str) -> &'a mut Vec<Value> {
    let entry = if let Some(scene_key) = key.strip_prefix("scene.") {
        let scene = top
            .entry("scene".to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        if !scene.is_object() {
            *scene = Value::Object(Map::new());
        }
        scene
            .as_object_mut()
            .expect("scene is an object")
            .entry(scene_key.to_string())
            .or_insert_with(|| Value::Array(Vec::new()))
    } else {
        top.entry(key.to_string())
            .or_insert_with(|| Value::Array(Vec::new()))
    };
    if !entry.is_array() {
        *entry = Value::Array(Vec::new());
    }
    entry.as_array_mut().expect("list section is an array")
}

/// The `id` of a list entry object (for dedup / id-based removal); `None` for a
/// non-object or an object without an id.
fn entry_id(entry: &Value) -> Option<&str> {
    entry.as_object()?.get("id").and_then(Value::as_str)
}

/// The `name` of a list entry object (scene items/exits are name-keyed when the
/// architect omits ids); `None` for a non-object or an object without a name.
fn entry_name(entry: &Value) -> Option<&str> {
    entry.as_object()?.get("name").and_then(Value::as_str)
}

// =========================================================================
// config + public turn entrypoint
// =========================================================================

/// The story-architect [`ArchitectConfig`]. Carries the pre-built read-only
/// world-lore block so the loop can inject it as a stable system message.
struct StoryArchitectConfig {
    world_lore_block: String,
}

impl ArchitectConfig for StoryArchitectConfig {
    fn system_prompt(&self) -> &str {
        STORY_ARCHITECT_SYSTEM
    }

    fn extra_system_blocks(&self) -> Vec<String> {
        // The read-only bound-world lore, as a stable cache-prefix system block.
        if self.world_lore_block.trim().is_empty() {
            Vec::new()
        } else {
            vec![self.world_lore_block.clone()]
        }
    }

    fn tools(&self) -> Vec<Value> {
        story_architect_tools()
    }

    fn normalize_draft(&self, draft: &Value) -> Value {
        normalize_input_plot(draft)
    }

    fn apply_tool(
        &self,
        name: &str,
        args: &Map<String, Value>,
        working_draft: &mut Value,
    ) -> ToolApplication {
        match name {
            "draft_story_plot" => {
                *working_draft = merge_plot(working_draft.clone(), args);
                ToolApplication {
                    args: Value::Object(args.clone()),
                    changed: true,
                    result: "Черновик сюжета создан/обновлён и показан пользователю. Дальше правь точечно через edit_story_plot (не пересылай весь сюжет), либо кратко ответь пользователю в чат.".to_string(),
                }
            }
            "edit_story_plot" => {
                let before = working_draft.clone();
                *working_draft = apply_story_plot_edit(working_draft, args);
                ToolApplication {
                    args: Value::Object(args.clone()),
                    changed: true,
                    result: story_edit_facts(args, &before, working_draft),
                }
            }
            "read_story_plot" => ToolApplication {
                args: Value::Object(args.clone()),
                changed: false,
                result: read_story_plot_result(args, working_draft),
            },
            _ => ToolApplication {
                args: Value::Object(args.clone()),
                changed: false,
                result: format!("Неизвестный инструмент архитектора истории: {name}."),
            },
        }
    }

    fn finalize_draft(&self, draft: Value) -> Value {
        // The plot has no summary fields to mirror (that is a world-bible concern);
        // the draft is already canonical.
        draft
    }
}

/// FACTS about what an `edit_story_plot` call actually changed — never a blind
/// "ok". Replays the list ops over the BEFORE plot in the same order the real
/// apply uses (set → replace → add → remove), so per-op counts stay correct
/// even when one call combines several ops on the same section. A remove that
/// matched nothing says so explicitly (with the read-first nudge).
fn story_edit_facts(args: &Map<String, Value>, before: &Value, _after: &Value) -> String {
    // The staged working copies of each touched list section, keyed by the
    // section addressing name (`npcs`, `scene.items`, …).
    let mut stage: Map<String, Value> = Map::new();
    let staged = |stage: &mut Map<String, Value>, key: &str| -> Value {
        if let Some(v) = stage.get(key) {
            return v.clone();
        }
        let value = if let Some(scene_key) = key.strip_prefix("scene.") {
            before.get("scene").and_then(|s| s.get(scene_key)).cloned()
        } else {
            before.get(key).cloned()
        };
        let value = value.unwrap_or_else(|| Value::Array(Vec::new()));
        stage.insert(key.to_string(), value.clone());
        value
    };
    // Mirrors add/remove_from_list_section matching: object entries key by id
    // (else name); a string target removes by id OR name OR the exact string.
    let matches_target = |entry: &Value, target: &Value| -> bool {
        match target {
            Value::String(s) => {
                entry_id(entry) == Some(s.as_str())
                    || entry_name(entry) == Some(s.as_str())
                    || entry == target
            }
            _ => entry == target,
        }
    };
    let is_dup = |existing: &[Value], item: &Value| -> bool {
        match item {
            Value::Object(obj) => {
                if let Some(id) = obj.get("id").and_then(Value::as_str) {
                    existing.iter().any(|e| entry_id(e) == Some(id))
                } else if let Some(name) = obj.get("name").and_then(Value::as_str) {
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
                    "{key}: НЕ найдено для удаления: {} — совпадения нет (id/имя/точная строка); прочитай раздел read_story_plot и повтори.",
                    misses.join(", ")
                ));
            }
        }
    }

    if lines.is_empty() {
        return "Правка НИЧЕГО не изменила (пустые операции). Прочитай нужный раздел read_story_plot и повтори с точными данными.".to_string();
    }
    lines.push("Продолжай точечные правки или кратко ответь в чат.".to_string());
    lines.join("\n")
}

/// Render the requested plot sections (or the whole plot) from the working
/// draft as MODEL-READY PLAIN TEXT — headed blocks, bullet lists, item/exit
/// conventions — never raw JSON. Section names resolve against the plot's
/// top-level keys plus the `scene.<name>` sub-lists; unknown names are
/// reported back.
fn read_story_plot_result(args: &Map<String, Value>, working_draft: &Value) -> String {
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
    let plot = match working_draft.as_object() {
        Some(m) => m,
        None => return "Сюжет пуст.".to_string(),
    };
    let scene = plot.get("scene").and_then(Value::as_object);

    let mut blocks: Vec<String> = Vec::new();
    let mut unknown: Vec<String> = Vec::new();
    if sections.is_empty() {
        for key in [
            "title",
            "description",
            "story_brief",
            "public_intro",
            "hidden_truth",
        ] {
            if let Some(text) = plot.get(key).and_then(Value::as_str) {
                if !text.trim().is_empty() {
                    blocks.push(format!("## {key}\n{}", text.trim()));
                }
            }
        }
        if let Some(block) = plot
            .get("player_character")
            .and_then(|v| plot_section_block("player_character", v))
        {
            blocks.push(block);
        }
        if let Some(scene_value) = plot.get("scene") {
            if let Some(block) = plot_section_block("scene", scene_value) {
                blocks.push(block);
            }
        }
        for key in ["npcs", "public_facts", "state_records", "proper_nouns"] {
            if let Some(block) = plot.get(key).and_then(|v| plot_section_block(key, v)) {
                blocks.push(block);
            }
        }
    } else {
        for name in sections {
            let value = if let Some(scene_key) = name.strip_prefix("scene.") {
                scene.and_then(|s| s.get(scene_key))
            } else {
                plot.get(&name)
            };
            match value {
                Some(v) => match plot_section_block(&name, v) {
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
            "Нет таких разделов: {}. Доступны: title, description, story_brief, public_intro, \
             hidden_truth, player_character, scene, npcs, public_facts, state_records, \
             proper_nouns, time и scene.<present_npcs|exits|items|constraints>.",
            unknown.join(", ")
        ));
    }
    blocks.join("\n\n")
}

/// One `## section` text block for a plot value: objects as `key: value` lines
/// (nested lists via the entry labels), arrays as `-` bullets rendered through
/// the item/exit conventions, strings as-is.
fn plot_section_block(name: &str, value: &Value) -> Option<String> {
    match value {
        Value::String(s) if !s.trim().is_empty() => Some(format!("## {name}\n{}", s.trim())),
        Value::Number(n) => Some(format!("## {name}\n{n}")),
        Value::Array(items) => {
            if items.is_empty() {
                return None;
            }
            let bullets: Vec<String> = items
                .iter()
                .map(|v| format!("- {}", plot_entry_text(v)))
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
                    Value::Array(items) if !items.is_empty() => {
                        let entries: Vec<String> = items.iter().map(plot_entry_text).collect();
                        lines.push(format!("{key} ({}):", entries.len()));
                        for entry in entries {
                            lines.push(format!("  - {entry}"));
                        }
                    }
                    Value::Object(nested) if !nested.is_empty() => {
                        lines.push(format!("{key}: {}", plot_entry_text(v)));
                        let _ = nested;
                    }
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

/// One line of a plot entry, model-readable: scene items as «имя — детали»
/// (+ пометка непереносимости), exits as «имя -> куда», npcs/facts/records by
/// their fields; plain strings as-is.
fn plot_entry_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Object(o) => {
            // Exit: name -> destination (+blocked note).
            if let Some(dest) = o.get("destination").and_then(Value::as_str) {
                let name = o.get("name").and_then(Value::as_str).unwrap_or("");
                let blocked = o
                    .get("blocked_by")
                    .and_then(Value::as_str)
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| format!(" (заблокирован: {s})"))
                    .unwrap_or_default();
                return format!("{name} -> {dest}{blocked}");
            }
            // Item: имя — детали [где] (+непереносимый).
            if o.contains_key("portable") {
                let name = o.get("name").and_then(Value::as_str).unwrap_or("");
                let details = o.get("details").and_then(Value::as_str).unwrap_or("");
                let location = o
                    .get("location")
                    .and_then(Value::as_str)
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| format!(" [{s}]"))
                    .unwrap_or_default();
                let fixed = if o.get("portable").and_then(Value::as_bool) == Some(false) {
                    " (непереносимый)"
                } else {
                    ""
                };
                return if details.trim().is_empty() {
                    format!("{name}{location}{fixed}")
                } else {
                    format!("{name} — {details}{location}{fixed}")
                };
            }
            // NPC / fact / state record: id + the meaningful text fields.
            let id = o.get("id").and_then(Value::as_str).unwrap_or("");
            let mut parts: Vec<String> = Vec::new();
            for key in ["name", "role", "persona", "secret", "text", "kind", "scope"] {
                if let Some(v) = o.get(key).and_then(Value::as_str) {
                    if !v.trim().is_empty() {
                        parts.push(format!("{key}: {}", v.trim()));
                    }
                }
            }
            if parts.is_empty() {
                return serde_json::to_string(value).unwrap_or_default();
            }
            if id.is_empty() {
                parts.join("; ")
            } else {
                format!("[{id}] {}", parts.join("; "))
            }
        }
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// Run one story-architect turn. The loop injects `world_lore_block` (the
/// read-only, image-field-stripped bound-world context) as a stable system block
/// right after the system prompt (via [`ArchitectConfig::extra_system_blocks`]),
/// so the cache prefix holds across turns.
pub async fn story_architect_turn(
    client: &dyn Backend,
    history: &[Value],
    world_lore_block: &str,
    draft: &Value,
    user_text: &str,
    stream: &mut (dyn ArchitectStream + Send),
) -> Result<StoryArchitectOutput, BackendError> {
    let config = StoryArchitectConfig {
        world_lore_block: world_lore_block.to_string(),
    };
    architect_turn(&config, client, history, draft, user_text, stream).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(base: &Value, args: Value) -> Value {
        apply_story_plot_edit(base, args.as_object().unwrap())
    }

    #[test]
    fn draft_tool_requires_the_launchable_minimum() {
        let tools = story_architect_tools();
        let draft = tools
            .iter()
            .find(|t| t["function"]["name"] == "draft_story_plot")
            .expect("draft_story_plot");
        let required = draft["function"]["parameters"]["required"]
            .as_array()
            .expect("required array");
        let required: Vec<&str> = required.iter().filter_map(Value::as_str).collect();
        assert_eq!(required, vec!["title", "story_brief", "public_intro"]);
        // The plot is NESTED: scene + player_character are object properties.
        let props = draft["function"]["parameters"]["properties"]
            .as_object()
            .expect("properties");
        assert_eq!(props["scene"]["type"], "object");
        assert_eq!(props["player_character"]["type"], "object");
        assert_eq!(props["npcs"]["type"], "array");
        // No acts/objectives/endings leak into the schema.
        for forbidden in ["acts", "objectives", "endings", "chapters"] {
            assert!(
                !props.contains_key(forbidden),
                "{forbidden} must not appear"
            );
        }
    }

    #[test]
    fn edit_tool_has_the_four_ops() {
        let tools = story_architect_tools();
        let edit = tools
            .iter()
            .find(|t| t["function"]["name"] == "edit_story_plot")
            .expect("edit_story_plot");
        let props = edit["function"]["parameters"]["properties"]
            .as_object()
            .expect("properties");
        for op in ["set", "add", "remove", "replace"] {
            assert!(props.contains_key(op), "edit op {op} missing");
        }
    }

    #[test]
    fn merge_plot_deep_merges_scene_and_player_character() {
        let base = json!({
            "title": "Старт",
            "scene": {"title": "Ворота", "tension": "тихо"},
            "player_character": {"name": "Мира", "class_role": "писец"}
        });
        let args = json!({
            "scene": {"tension": "дорога просыпается", "location_id": "gate"},
            "player_character": {"class_role": "странник"}
        })
        .as_object()
        .unwrap()
        .clone();
        let merged = merge_plot(base, &args);
        // scene: title kept, tension overwritten, location_id added.
        assert_eq!(merged["scene"]["title"], "Ворота");
        assert_eq!(merged["scene"]["tension"], "дорога просыпается");
        assert_eq!(merged["scene"]["location_id"], "gate");
        // player_character: name kept, class_role overwritten.
        assert_eq!(merged["player_character"]["name"], "Мира");
        assert_eq!(merged["player_character"]["class_role"], "странник");
    }

    #[test]
    fn edit_set_overwrites_scalar_and_whole_object() {
        let base = json!({"hidden_truth": "старое", "scene": {"title": "A"}});
        let out = edit(
            &base,
            json!({"set": {"hidden_truth": "новое", "scene": {"title": "B", "tension": "T"}}}),
        );
        assert_eq!(out["hidden_truth"], "новое");
        // set on an object REPLACES it wholesale (last-writer-wins).
        assert_eq!(out["scene"]["title"], "B");
        assert_eq!(out["scene"]["tension"], "T");
    }

    #[test]
    fn edit_add_dedups_objects_by_id_and_strings_by_value() {
        let base = json!({
            "npcs": [{"id": "starosta", "name": "Гедд"}],
            "proper_nouns": ["Дорога"]
        });
        let out = edit(
            &base,
            json!({"add": {
                "npcs": [{"id": "starosta", "name": "дубль"}, {"id": "kuznec", "name": "Кузнец"}],
                "proper_nouns": ["Дорога", "Мост"]
            }}),
        );
        // starosta not re-added (dedup by id); kuznec appended.
        let npcs = out["npcs"].as_array().unwrap();
        assert_eq!(npcs.len(), 2);
        assert_eq!(npcs[0]["name"], "Гедд"); // original kept, not the dup
        assert_eq!(npcs[1]["id"], "kuznec");
        // proper_nouns dedup by value.
        assert_eq!(out["proper_nouns"], json!(["Дорога", "Мост"]));
    }

    #[test]
    fn edit_remove_by_id_and_replace_section() {
        let base = json!({
            "npcs": [{"id": "a", "name": "A"}, {"id": "b", "name": "B"}],
            "public_facts": [{"id": "f1", "text": "t"}]
        });
        // remove by id (string target matches the object's id).
        let removed = edit(&base, json!({"remove": {"npcs": ["a"]}}));
        let npcs = removed["npcs"].as_array().unwrap();
        assert_eq!(npcs.len(), 1);
        assert_eq!(npcs[0]["id"], "b");
        // replace swaps a whole section.
        let replaced = edit(
            &base,
            json!({"replace": {"public_facts": [{"id": "f2", "text": "u"}]}}),
        );
        assert_eq!(replaced["public_facts"][0]["id"], "f2");
    }

    #[test]
    fn edit_targets_scene_sub_lists() {
        let base = json!({"scene": {"title": "Ворота", "present_npcs": ["a"]}});
        let out = edit(
            &base,
            json!({"add": {"scene.present_npcs": ["b"], "scene.exits": ["север -> road"]}}),
        );
        assert_eq!(out["scene"]["present_npcs"], json!(["a", "b"]));
        assert_eq!(out["scene"]["exits"], json!(["север -> road"]));
        // scene.title untouched.
        assert_eq!(out["scene"]["title"], "Ворота");
        // remove from a scene sub-list.
        let removed = edit(&out, json!({"remove": {"scene.present_npcs": ["a"]}}));
        assert_eq!(removed["scene"]["present_npcs"], json!(["b"]));
    }

    #[test]
    fn edit_facts_report_hits_and_misses() {
        let config = StoryArchitectConfig {
            world_lore_block: String::new(),
        };
        let mut working = json!({
            "npcs": [{"id": "marya", "name": "Марья"}, {"id": "efim", "name": "Ефим"}],
            "scene": {"items": [{"name": "записка", "portable": true}]}
        });
        let args = json!({
            "set": {"hidden_truth": "новая тайна"},
            "add": {"scene.items": [{"name": "записка", "portable": true}, {"name": "нож", "portable": true}]},
            "remove": {"npcs": ["marya", "нет_такого"]}
        });
        let applied = config.apply_tool("edit_story_plot", args.as_object().unwrap(), &mut working);
        assert!(applied.changed);
        assert!(applied.result.contains("Поля обновлены: hidden_truth."));
        assert!(
            applied.result.contains("scene.items: добавлено 1"),
            "{}",
            applied.result
        );
        assert!(applied.result.contains("пропущено как дубли"));
        assert!(applied.result.contains("npcs: удалено 1 (теперь 1)."));
        assert!(applied
            .result
            .contains("НЕ найдено для удаления: «нет_такого»"));
        assert!(applied.result.contains("read_story_plot"));
    }

    #[test]
    fn user_tail_is_raw_text_matching_stored_history() {
        // CACHE INVARIANT: sent tail == stored history entry, byte for byte.
        let messages = story_architect_messages(&[], "## LORE", "  Усиль улики.  ");
        let tail = messages.last().unwrap();
        assert_eq!(tail["role"], "user");
        assert_eq!(tail["content"], "Усиль улики.");
    }

    #[test]
    fn read_story_plot_renders_plain_text_not_json() {
        let config = StoryArchitectConfig {
            world_lore_block: String::new(),
        };
        let working = json!({
            "hidden_truth": "Староста виноват.",
            "scene": {
                "items": [
                    {"name": "записка", "portable": true, "details": "имя китобоя"},
                    {"name": "стойка", "portable": false}
                ]
            },
            "npcs": [{"id": "marya", "name": "Марья", "role": "хозяйка", "secret": "боится культа"}],
        });
        let mut working = working;
        let mut args = Map::new();
        args.insert(
            "sections".into(),
            json!(["hidden_truth", "scene.items", "npcs", "nope"]),
        );
        let out = config
            .apply_tool("read_story_plot", &args, &mut working)
            .result;
        assert!(out.contains("## hidden_truth\nСтароста виноват."));
        assert!(out.contains("- записка — имя китобоя"));
        assert!(out.contains("стойка (непереносимый)"));
        assert!(out.contains("[marya] name: Марья; role: хозяйка; secret: боится культа"));
        assert!(out.contains("Нет таких разделов: nope"));
        assert!(!out.trim_start().starts_with('{'), "not JSON: {out}");
    }

    #[test]
    fn edit_tool_is_non_strict_so_section_maps_survive_responses_conversion() {
        // The strict Responses conversion rewrites properties-less objects into
        // additionalProperties:false + properties:{} — the model could then only
        // send empty set/add/remove/replace. The edit tool must opt out.
        let tools = story_architect_tools();
        let edit = tools
            .iter()
            .find(|t| t["function"]["name"] == "edit_story_plot")
            .expect("edit_story_plot present");
        assert_eq!(edit["function"]["strict"], json!(false));
    }

    #[test]
    fn edit_scene_items_as_objects_dedups_and_removes_by_name() {
        let base = json!({"scene": {"items": [
            {"name": "записка", "portable": true, "details": "название китобоя"},
            {"id": "counter", "name": "стойка", "portable": false},
        ]}});
        // add: a no-id object entry dedups by name; a new one appends.
        let out = edit(
            &base,
            json!({"add": {"scene.items": [
                {"name": "записка", "portable": true},
                {"name": "нож", "portable": true},
            ]}}),
        );
        let items = out["scene"]["items"].as_array().unwrap();
        assert_eq!(items.len(), 3, "duplicate name not re-added");
        assert_eq!(items[2]["name"], "нож");
        // remove: a string target matches an object by name AND by id.
        let removed = edit(
            &out,
            json!({"remove": {"scene.items": ["записка", "counter"]}}),
        );
        let items = removed["scene"]["items"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["name"], "нож");
    }

    #[test]
    fn world_lore_block_strips_image_fields_and_keeps_hidden() {
        let lore = json!({
            "name": "Порог",
            "hidden_premise": "тайна мира",
            "hidden_secrets": ["секрет"],
            "world_image_prompt_en": "an image prompt",
            "world_map_prompt_en": "a map prompt",
            "world_image_url": "/world-assets/x/overview.png",
            "world_map_url": "/world-assets/x/map.png"
        });
        let block = story_architect_world_lore_block(&lore);
        assert!(block.contains("Порог"));
        // GM-only truths are KEPT (the story architect is GM-trusted).
        assert!(block.contains("тайна мира"));
        assert!(block.contains("секрет"));
        // Image/URL fields are STRIPPED.
        assert!(!block.contains("world_image_prompt_en"));
        assert!(!block.contains("world_map_prompt_en"));
        assert!(!block.contains("world_image_url"));
        assert!(!block.contains("/world-assets/"));
    }

    #[test]
    fn messages_place_world_block_as_second_system() {
        let msgs = story_architect_messages(
            &[],
            "## BOUND WORLD BIBLE (read-only reference)\n{...}",
            "Собери сюжет.",
        );
        assert_eq!(msgs[0]["role"], "system");
        assert!(msgs[0]["content"]
            .as_str()
            .unwrap()
            .contains("story architect"));
        assert_eq!(msgs[1]["role"], "system");
        assert!(msgs[1]["content"]
            .as_str()
            .unwrap()
            .contains("BOUND WORLD BIBLE"));
        assert_eq!(msgs.last().unwrap()["role"], "user");
    }
}
