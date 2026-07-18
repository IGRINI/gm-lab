**English** | [Русский](ru/ITEMS_AND_SPELLS_TZ.md)

# Specification: Items (Descriptions + Scene↔Inventory Transfer) and Spells (5e Subset)

Status: Phase I (items) → Phase S (spells). The design passed an adversarial
panel; key decisions and rejected alternatives are recorded in §5.

## 0. Locked facts (verified against the code and by the panel)

- Items are NOT canonical entities: their bodies live only in `SceneState.items`;
  `Containment::Inventory` is dead code; item actions, validation, and events do
  not exist.
- `view.rs:117` clones the PREVIOUS scene's items into every view rebuild, so items
  follow the player during `move_player` until the GM replaces them with `set_scene`.
- The player-character inventory is `Vec<String>`; its load path uses manual
  `str_list`/`py_str` conversion (object → JSON string), while its apply path uses
  `as_str` coercion (object → corrupted value). Any object-based entry format would
  require an atomic migration across roughly 10 locations and is out of scope for v1.
- `normalize_stat_value` does NOT recurse into objects (`other => other`), so nested
  `{current,max}` values are not numerically coerced. `hp` works because it is FLAT.
- The tool catalog has two tiers: `build_gm_tools()` is byte-gated by a fixture;
  `build_canon_gm_tools()` (which contains `move_player`, among others) is NOT in
  that fixture. New tools placed there require only updates to `CANON_GM_TOOL_NAMES`
  and two length assertions.
- The precedent that the engine does not calculate rolls (CHARACTERS_AND_STORY_TZ
  §C2) remains in force: slots/concentration are STATE (like hp), while cast
  resolution uses `roll_dice` notation.
- The orchestrator (`turn.rs`), not a world method, emits the
  PLAYER_CHARACTER_UPDATE event.

## 1. Phase I — items

### I1. Item descriptions: the "name — description" convention (WITHOUT changing the type)
- `inventory`/`equipment` entries remain strings. Convention: an optional suffix
  separated by " — " (an em dash surrounded by spaces), for example
  `"dagger — 1d4, hidden in a boot"`.
- The head (before the first " — ") is the item name; the tail describes what it
  does. Delta operations and tools match ONLY the head (trim + lowercase; Unicode
  text is supported).
- Prompt: document the convention in the `update_player_character` tool description
  and in one line of GM_SYSTEM (the card already renders lists with "; ").
- UI (cosmetic, non-blocking): DebugPanel/WorldDetailModal may bold the head by
  splitting on " — "; nothing breaks without this treatment.
- Leave `features` unchanged (they are traits, not items).
- A typed `ItemEntry {name, desc, charges…}` is a FUTURE track gated on a real
  charges/attunement feature. Do not introduce it before then (see §5).

### I2. Per-location storage for scene items (leak fix — IN SCOPE)
- Add `World.place_items: BTreeMap<String /*place_id*/, Vec<SceneItem>>`.
- When rebuilding the view for a LOCATION CHANGE
  (`scene.location_id != anchor_id`), stash the current `scene.items` under the old
  location_id, then load the new items from `place_items[anchor_id]` (or an empty
  list when absent). At the same location, keep current behavior (`scene.items`
  remain live). Do NOT change the `set_scene` path: it stages items for the
  destination location BEFORE refresh, and the `location_id == anchor_id` guard
  preserves them.
- Persistence: add a trailing `place_items` key to the world payload and emit it
  only when non-empty (old saves remain byte-identical); parse with a default. Add
  round-trip tests.
- Regression gate: extend canonical tests to prove that items do NOT move with
  `move_player`, items return when revisiting a location, and `set_scene` overrides
  the store.

### I3. take_item / drop_item tools (at the end of build_canon_gm_tools)
- `take_item { item_id?, name?, reason? }`:
  1) When `item_id` is supplied, match it exactly (otherwise return tool_error
     unknown_item). Invisible items may be taken ONLY by item_id (GM-trusted path).
  2) Otherwise, candidates are visible scene items whose names match
     `name.trim().to_lowercase()`. Zero candidates returns item_not_here (with a hint
     listing visible items); more than one returns ambiguous_item with
     `[{item_id,name,location}]` candidates. NEVER silently take the first match.
  3) `portable == false` returns not_portable (the tool description teaches that a
     rejection is fiction to narrate, not a reason to retry; mirror the
     `move_player` wording).
  4) On success, remove the SceneItem from scene.items and append
     `"{name} — {details}"` to `pc.inventory` (or just name when details is empty)
     through the existing changed set (card_revision, deduplicate by head).
- `drop_item { name, location?, reason? }`:
  - Match the inventory entry by its head (zero matches returns tool_error), remove
    the entry, and insert a SceneItem into the current scene with
    `{item_id: generated, name: head, details: tail, portable: true, visible: true,
    location: location|"nearby"}`.
- Both handlers emit PLAYER_CHARACTER_UPDATE + SCENE_UPDATE from the orchestrator.
  There are NO CANONICAL EVENTS in this phase: private event helpers and the
  validator are the canon gate, and the append-only log must not be written around
  them. A real `Action::TakeItem` belongs to the canonical-item-body track.
- GM_SYSTEM must include a model-actionable tool-selection rule:
  "If the item EXISTS in CURRENT SCENE STATE → use take_item (it transfers the same
  object and preserves its details). If it is NOT in the scene (a gift, crafted or
  purchased item, or something found in narration without staging) → use
  inventory_add. Never use inventory_add for a visible scene item; never use
  take_item for an item absent from the scene. To put an item down → drop_item; when
  destroyed/consumed without leaving a trace in the scene → inventory_remove."
  Add a cross-reference to the `update_player_character` description as well.

## 2. Phase S — spells (pragmatic 5e subset)

### S1. Card fields (PLAYER_CHARACTER_FIELDS 26 → 29; all #[serde(default)],
emit unconditionally according to the 26-field discipline → re-bless goldens; old
saves load defaults; opaque character packages gain the fields on the next save-back,
which is additive)
- `spells: Vec<SpellEntry>`; `SpellEntry { name: String, level: u8 (0 = cantrip),
  concentration: bool, ritual: bool, effect: String }`. Put school/range/duration/
  casting time/upcast IN PROSE inside effect (the engine does not read them; the
  panel removed five dead fields).
- `spell_slots: Map<String,Value>` — a FLAT map from level to REMAINING slots
  (`{"1": 3, "2": 1}`); include it in dict_fields so C2 coercion applies for free.
- `spell_slots_max: Map<String,Value>` — authored maxima, also flat and in
  dict_fields. An absent level means there are no slots. The 5e table is only author
  guidance (in the tool description/docs), NOT an engine mechanic.
- `concentration: String` — name of the active concentration spell; `""` means
  none. This is a text field DOCUMENTED in the `update_player_character` schema
  (ending concentration without casting is an explicit field update; mention this
  in GM_SYSTEM).
- Complete growth-site checklist (from the panel): model.rs struct+Default;
  PLAYER_CHARACTER_FIELDS; field classification in `apply_player_character_fields`
  (`spells` is a new "list of objects" category with its own serde coercion, NOT
  `as_str`); pc_field_value/set_pc_field; player_character_export;
  player_character_context; player_character_to_payload + from_payload (`str_list`
  must NOT process spells; parse them separately); tool schema; PlayerEditor (JSON
  textarea); WorldDetailModal/PlayerCard (read-only block); re-bless chat_payload
  goldens, gm_tools fixtures, and server state/debug goldens when necessary.

### S2. cast_spell tool (at the end of build_canon_gm_tools)
- `cast_spell { name, slot_level?, reason? }`:
  1) Find the spell in pc.spells by name (trim+lowercase); if absent, return
     tool_error "the character does not know this spell" and include known spells
     in the hint.
  2) If level == 0 (cantrip), do not consume a slot.
  3) Otherwise, `lvl = max(spell.level, slot_level|spell.level)`; when
     `spell_slots[lvl] > 0`, decrement it; otherwise return tool_error "no available
     level N spell slots" (the tool description says rejection is fiction to
     narrate as a failed cast, not a reason to retry).
  4) Concentration: when `spell.concentration`, set `pc.concentration = name` and
     return the previous value as concentration_ended (the GM narrates the break).
  5) Result: `{spell, level, slot_spent_level|null, slots_remaining,
     concentration_started|null, concentration_ended|null}` + card_revision bump +
     PLAYER_CHARACTER_UPDATE.
  6) Do NOT add roll mathematics: attack/save/damage uses the existing `roll_dice`
     notation contract; upcast effects are prose in effect. Ritual casting in v1 is
     narrative (the GM simply does not call cast_spell); do not add an `as_ritual`
     argument.
- GM_SYSTEM: add a spell section requiring every cast to go through cast_spell;
  slots and concentration are engine-authoritative; damage/saves continue to use
  roll_dice; clear concentration through the concentration field.

### S3. UI (minimal v1)
- PlayerCard (DebugPanel) + WorldDetailModal: read-only "Spells" block (name, level,
  concentration badge, effect), plus a slot row and active concentration.
- PlayerEditor: JSON textareas for spells/spell_slots/spell_slots_max, following
  abilities. Structured editors and authoring in the character architect come later.
- NPC spells are out of scope (prose may be placed in knowledge/gm_notes).

## 3. Order and process
Phase I (I1+I2+I3 as one chunk: the convention is prompt/descriptions, while the
main code is the per-location store plus two tools) → Phase S (one chunk). For each
chunk: Opus implementer following this specification → tests/clippy/byte gates →
adversarial reviews → checkpoint commit.

## 4. Future tracks (do not implement now)
- Canonical item bodies: Item entities in WorldCanon + Action::TakeItem/DropItem/
  GiveItem through the validator and engine::apply + canonical events + view from
  Place.item_ids. Add give_item and NPC inventories at the same time.
- Player as a canonical actor (requires an is_player gate in three NPC synthesis
  sites).
- Typed ItemEntry (gated on charges/attunement).
- Automatic rest behavior (slot restoration); until then, the GM edits spell_slots
  manually.
- NPC casting (innate-style "spell → uses/day", per 5e-2024).
- Import of an SRD spell compendium (CC-BY-4.0 attribution is mandatory; mechanics
  are free, Product Identity names are not).

## 5. Rejected by the panel (do not revisit)
- ItemEntry structure in v1: the manual str_list/py_str load path mangles objects
  into JSON strings; the apply path's as_str coercion is a second independent
  corruption path; the UI renders `[object Object]`. An atomic migration across
  roughly 10 sites is disproportionate until charges exist.
- Nested `spell_slots {"1":{current,max}}`: normalize_stat_value does not recurse,
  so nested string values are never coerced and decrementing breaks.
- Canonical take/drop events from the tool handler: private event() helpers and the
  validator are mandatory gates; raw EventLog::append violates append-only
  invariants.
- Dropping without fixing the scene leak: a dropped item follows the player, an
  absurdity introduced by the feature itself (therefore the per-location store is
  in Phase I scope).
- Engine-side as_ritual branch ("outside combat pressure" is a fictional judgment
  the engine cannot make).
- Ten-field SpellEntry (school/casting_time/range/duration/upcast are dead free-form
  strings the engine does not read; keep them in effect prose).
- take/drop/cast in byte-gated build_gm_tools: the canonical builder is cheaper
  (no re-bless) and works in legacy scenes, following the move_player precedent.
