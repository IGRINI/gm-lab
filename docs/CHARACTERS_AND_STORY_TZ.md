**English** | [Русский](ru/CHARACTERS_AND_STORY_TZ.md)

# Specification: Character Packages and the Story Architect

Model: World (bible) → Stories/plots (many per world) → Saves (many per story).
Player characters are an orthogonal package entity: they are selected when a
save is created, live as a snapshot inside the save, and are explicitly exported
back into the library. Phase order: C1 (characters) → C2 (card quality) → S1
(story architect).

## 0. Locked invariants (verified against the code and by the panel)

- `PlayerCharacter` already exists (`gml-world/src/model.rs:114`, 26 fields) and
  is already persisted in the save payload (`player_character_to_payload`). The
  complete card is sent to the GM prompt; NPCs do not see it.
- The dice engine does NOT read the card: the model encodes modifiers in notation.
  This is a locked prompt contract (`tools.rs:196-264`) and MUST NOT be changed in
  this specification (doing so would double-count modifiers and break cache-prefix
  fixtures).
- The current procedural launch path has no player-character hook at all (it always
  uses the default "Seeker").
- `StoryEnvelope` REBUILDS story.json from a fixed list of keys
  (`story_store.rs:566`), so unknown top-level keys are lost on write. Any new state
  must live in an explicit round-trip envelope object.
- Byte identity of old saves: new payload keys must be trailing, and must be emitted
  only when `Some` (follow the `world_ref` pattern and the `package_ref_tests` gate).

## Phase C1 — characters as packages

### C1.1 CharacterStore (gml-persistence, next to WorldStore)
- Follow the StoryStore shape: in-memory cache + `reload()` after import, write
  mutex, shared `GM_PACKAGES_DIR`/`default_library_dir()` root, and
  `library/characters/<id>/character.json`. Do NOT add `ensure_defaults` or
  resurrection behavior: a deleted character must remain deleted.
- Manifest: `{format:"gmlab.character/1", id, version:u64, title, preview,
  created_at, updated_at, world_ref?, story_ref?, payload}`; `payload` is an opaque
  round-trip object (as in WorldEnvelope), containing `payload.player_character`.
- `world_ref`/`story_ref` (2026-07: creation relationships) are the OPTIONAL base
  `{id, version}` (`CharacterBaseRef`) for which the character was created: a world,
  a story, or both. They are pinned on create (architect/manual save/
  save-protagonist/save-character-from-chat), never patched later, and emitted only
  when `Some` (preserving byte identity for old packages). Provenance semantics are
  the same as the save's `char_ref`: the reference may dangle after deletion of the
  base package, and consumers must tolerate that (the studio skips the block; launch
  uses warn-but-allow `character_world_mismatch`). The character architect receives
  ONLY public canon from the base (`## BASE WORLD` — the
  `CHARACTER_WORLD_PUBLIC_FIELDS` whitelist; `## BASE STORY` — the title/
  description/story_brief/public_intro whitelist): it talks to the PLAYER, so GM
  secrets must never enter its context. A procedural story cannot be used as a base
  (gate this in both UI pickers and the server resolver with 400). The system prompt
  has TWO variants with a shared body: standalone (no mention of bases, so the
  unused feature costs zero tokens) and based (generic grounding). All world/story
  specifics live in the blocks themselves and are paid for only when the block is
  present. The binding is fixed at creation, so the prompt variant remains stable
  throughout the conversation and does not break the cache prefix.
- There must be ONE canonical player-character serializer: the
  `player_character_to_payload` form (session payload) is also used for the package.
  `player_character_export` is a UI/tool projection and must NOT be used for the
  package.
- Methods: `list/get/create/delete` plus TWO update operations:
  - `update_metadata(patch)` — shallow-merge the top level (title/preview), dropping
    nulls;
  - `snapshot_character(pc)` — FULL replacement of `payload.player_character`
    (not a merge).
  Both increment version with `saturating_add(1)` and use an atomic temp+rename write.
- Keep a local third copy of the atomic write/scan helpers. Extracting a shared
  `pkgfs` module is a separate cleanup track and is out of scope here.

### C1.2 Package mechanics
- share.rs: `CHARACTER_FORMAT="gmlab.character/1"`, `PackageKind::Character`, a
  `detect_kind` branch for `character.json`, and an arm in `manifest_id`.
- `import_character_into` should follow `import_world_into` (staging + swap_in + 409
  without overwrite), with STRUCTURAL validation BEFORE swap_in: the format is
  correct, `payload` is an object, `payload.player_character` is an object, and
  `title` is non-empty. Otherwise return 400 and do not add anything to the library.
  Do NOT deeply validate stats on import; use lazy coercion at launch as worlds do.
  Call `character_store.reload()` after import.
- Endpoints: `GET/POST /characters`, `POST /characters/{id}` (metadata),
  `POST /characters/{id}/delete`, `GET /characters/{id}/export` →
  `{id}.gmchar.zip`.
- Add `AppState.character_store`.

### C1.3 Launching a save with a character
- `POST /chats`: optional `character_id`. A nonexistent id returns 400
  (no fallback).
- Refactor `post_create_chat`: all three branches (brief / procedural / named story)
  return `(World, warnings)`; a SINGLE shared tail applies the character overlay and
  then builds the session once. Precedence: selected package > `player_character`
  from plot/seed. There is NO default; the protagonist gate returns 400
  `protagonist_required`.
- Overlay = `seed_player_character(payload.player_character)`: full replacement,
  with NO event and NO increment (events belong only to the tool path). Accept the
  package's `card_revision` as-is (the hero's education counter travels with it;
  package version is a separate counter; the UI displays version).
- Provenance: `World.char_ref: Option<PackageRef>` (the fourth ref field). Add the
  `char_ref` payload key in `world_to_payload` immediately after
  `world_ref_authored_version`, only when `Some`; parse it in `world_from_payload`;
  add tests next to `package_ref_tests` (round trip + absent emits no key). Do NOT
  put it in `player_character_to_payload`.
- Emit the `story_pc_override` warning when `character_id` is supplied AND the
  story's plot/seed contains its own `player_character`: use `launch_warnings`
  ("the story was written for its own protagonist; the plot/clues/NPCs may refer to
  them"). This is warn-but-allow, like `world_version_drift`.
- A save's character cannot be changed mid-run. Progress lives in the save.

### C1.4 Exporting progress to the library
- `POST /chats/{chat_id}/save-character`, body `{character_id?}`:
  - without an id, create a new character from the current snapshot (title = player
    character name);
  - with an id, call `snapshot_character` on the existing character (+version bump);
    a nonexistent id returns 400 (the frontend offers "create new").
- Read the player character consistently through the cache: `ensure_cached` +
  `with_runtime` under the per-chat lock. Bare `load_chat` returns a stale database
  row for an active chat.
- Carry the snapshot's `card_revision` into the package unchanged.
- Deleting a character must NEVER affect saves: `char_ref` may dangle because it is
  provenance, while the snapshot is self-contained. Do not integrate this with
  purge or embedding scopes.

### C1.5 UI (minimal v1)
- Add a fourth "Characters" tab: list (name, version, preview), rename, delete, and
  ↓ export. Import remains shared, with a three-way notice label:
  World/Story/Character; extend `onImportPackage` with `refreshCharacters()`.
- Add an optional character picker below the story picker in the new-chat block
  (empty = the story's/default player character); it must not affect `createLocked`.
  Add it to `onPlayWorld` as well.
- Place "Save player character to library" on the USER-facing surface (the player
  block in WorldHud), NOT in DebugPanel (which is gated by `developerMode`). Offer
  "new / update source"; enable "source" only when `char_ref` exists and resolves.
  To support this, add `char_ref {id, version} | null` to the state payload.
- Character creation in v1 is save-back from a chat or import. A full editor for all
  26 fields belongs to the NEXT phase and must not be built now; the developer
  editor in DebugPanel remains unchanged.
- Portraits (`assets/`) come later. WorldStore asset mechanics can be added
  incrementally without changing the format. Do NOT introduce an `assets` payload
  key now.

## Phase C2 — card quality (reduced by the panel to an honest fix)

- Do NOT add an engine skill lookup to `roll_dice`: it would double-count notation
  modifiers, contradict the prompt contract, and break the cache prefix. A separate
  future "engine-authoritative checks" track must rewrite the entire contract.
- C2.1 Normalization: in `apply_player_character_fields`, numerically coerce values
  in `abilities/skills/saving_throws/hp` (numeric strings → numbers; discard NaN
  garbage). This makes the existing notation path reliable. Apply the same logic
  during seeding.
- C2.2 Inventory: add delta operations `inventory_add/inventory_remove` and
  `equipment_add/equipment_remove` to `update_player_character` (strings; remove is
  trim-exact and removes ALL occurrences). Apply them in this order: full rewrite →
  remove → add. Compare the result with the original and feed it into `changed`, so
  revision/event behavior remains standard. Full replacement remains for
  compatibility.
- Synchronization with canonical scene items (take/drop, player-as-actor) is a
  future track and is out of scope here.

## Phase S1 — story architect

### S1.1 StoryStore: editing
- Extend `StoryEnvelope` with a round-trip `meta` object (emit it only when non-empty
  so builtin-package bytes do not change) and `created_at/updated_at` (parse with a
  default, emit only when set). Architect fields (`architect_messages`,
  `architect_model_history`, `architect_cache_*`) live IN `meta`, NEVER in `seed`
  (which would leak into worldgen/byte gates) or at the top level (where they would
  be lost).
- `update_story(id, patch)`: shallow-merge title/description/seed(plot)/meta while
  dropping nulls; bump version; update the in-memory cache; add a new `StoryNotFound`
  error variant.
- Add `POST /stories/{id}` + `persist_story_payload` (draft-first: persist BEFORE
  invoking the model, as for worlds).
- The architect works ONLY with authored stories bound to a world (`world_ref`).
  Builtins (self-contained) cannot be edited by the architect.

### S1.2 Agent (gml-agents) — generalize, do not fork
- Extract a generic `architect_turn(system, tools, apply_tool, ...)` loop from
  `world_architect.rs` (`HopSink`/`ArchitectStream`/call normalization/stats are
  already generic); world and story become two thin configurations. Regression
  gate: world-architect goldens/tests must not change.
- Add `STORY_ARCHITECT_SYSTEM` and the `draft_story_plot`/`edit_story_plot` tools.
  The schema targets ONLY the existing authored-plot runtime contract:
  `title, description, story_brief, public_intro, hidden_truth,
  player_character{...}, scene{title,description,location_id,present_npcs,exits,
  items,constraints,tension}, npcs[], public_facts[], state_records[],
  proper_nouns[], time`. Do NOT add acts/objectives/endings; the runtime does not
  read them (future "plot progression engine" track).
- Context: the linked world's complete INTERNAL WorldLore (including
  hidden_premise/hidden_secrets because this is a GM-trusted agent) as a stable
  system block for caching (`cache_session_id/thread_id`); do not inject lore image
  fields.
- Player character in the story: the architect may propose `player_character` (an
  authored protagonist); the launch picker overrides it with the C1.3 warning.

### S1.3 SSE + frontend
- `POST /story-architect/chat` mirrors the world endpoint (draft-first;
  architect_delta/tool/done/error events; reuse the frontend's `streamArchitect`).
- Do NOT fork the panel: parameterize `WorldArchitectPanel` with a config prop
  (endpoint, tools, form-field descriptors, optional read-only world block).
- Entry points: "+ Story" continues to open CreateStoryModal, but the modal becomes
  PROCEDURAL-ONLY (remove its authored branch). Its "✨ Open in architect" link is
  the sole path to authored creation. In the story list, "✎" opens the architect
  for an existing authored story.

## Future tracks (recorded; do not implement now)
- Plot progression engine (acts/objectives/reveals/endings + tracker on World +
  injection into the GM prompt).
- Engine-authoritative checks (engine-side stat lookup + rewrite of the `roll_dice`
  contract).
- Synchronization of the player-character inventory with canonical items
  (player as canonical actor).
- Full character editor in the tab; portraits (`assets/`).
- STORY version drift for existing saves (`story_ref.version` vs live), analogous
  to `world_version_drift`, once story editing becomes common.
- Deduplicate package stores (`pkgfs` module: atomic write/scan/abspath).
