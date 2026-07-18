**English** | [Русский](ru/MODS_PACKAGES_TZ.md)

# Specification: Worlds and Stories as Portable Packages ("Mods")

## Status

- [x] Define the package model and on-disk layout. *(Specified below.)*
- [x] Move worlds from the `worlds` table to file packages (folders are the source
  of truth). *(Phase 1: WorldStore in gml-persistence,
  `library/worlds/<id>/world.json`, migration from SQLite, byte-identical `/worlds`
  contract, workspace green.)*
- [x] Copy images into the world package and serve them through a static route
  independently of `image_enabled`. *(Phase 2: ingest into `assets/` on save,
  ungated `/world-assets/{id}/{file}` route, read-time relative→servable rewrite,
  no-fallback 502, tests green.)*
- [x] Replace story loading via `include_str!(catalog.json)` with runtime package
  scanning; the three built-in stories ship as default packages. *(Phase 3:
  StoryStore in gml-stories, `library/stories/<id>/story.json`; defaults materialize
  from embedded catalog.json, which is no longer a live read path; builtin-order
  preserves `/stories` ordering; drop-in packages work; workspace green. There is
  no global `DEFAULT_STORE`: every caller constructs `StoryStore` explicitly (the
  server through `AppState`, tests over `tempfile::tempdir()`), so bare `cargo test`
  never materializes defaults in the real `library`.)*
- [x] Link stories to worlds (`world_ref`) and support a baked self-contained form.
  *(Phase 4 backend: StoryStore stores/returns `kind`+`world_ref`; `POST /stories`
  creates a story linked to a world, validates that world_id exists, and has no
  fallback; self-contained built-in stories remain `from_seed`.)*
- [x] Complete launch of saved worlds/stories into a game session. *(Phase 4
  backend: `/chats` accepts `world_id` to play a saved world procedurally, with
  world_id taking precedence over inline world_lore; story launch uses
  procedural=worldgen(lore)+overlay or authored=`World::compose_authored`
  (worldgen(lore) plus plot overlay: PC/hidden_truth/scene/NPCs/facts; the authored
  scene is upserted into canon via set_scene). Save payload provenance records
  `world_ref`/`story_ref` as trailing keys, preserving byte identity for old saves.
  Frontend (`web/`) is the next step.)*
- [x] Migrate existing worlds from SQLite into folders without data loss. *(Phase 1:
  `WorldStore::migrate_from_sqlite` is the sole reader of the old table and runs
  idempotently at startup.)*
- [x] Update tests/contracts for the new format. *(Phases 1–4: WorldStore/StoryStore
  unit tests, updated contract.rs (37), golden `/stories` through builtin-order,
  golden payload round trip green.)*
- [x] Phase 5 — sharing UX: reveal the library folder, export a package to zip
  (including "bake world"), and import zip. *(Deflate-only zip crate;
  `POST /library/reveal`, `GET /worlds|stories/{id}/export` (story `?bake=1` bakes
  the world under `world/`), `POST /library/import?overwrite=1` with staging+atomic
  swap, zip-slip guard, and no fallback; frontend buttons for export/import/open
  folder; tests green.)*

This continues `docs/WORLD_CREATION_TAB_TZ.md` (the "Out of scope for now" section:
creating a story from a world and selecting a world when creating a story). Those
items enter scope here together with the storage migration to files.

## Core idea

A world and a story must be **portable artifacts** that can be shared with another
person like a game mod (Minecraft / Project Zomboid). Drop in a folder and the app
discovers it.

Three artifact types:

| Type | Analogy | Contents | Storage | Shareable |
|---|---|---|---|---|
| **World** | mod / data pack | world bible (`WorldLore`) + images | file package | yes |
| **Story** | scenario mod that depends on a world | plot overlay (player role, hidden_truth, opening scene) + `world_ref` + its own images | file package | yes, together with the world |
| **Save** (playthrough) | save game | live `WorldCanon` + transcript + state | SQLite (`dialog_chats`) | optionally (export later) |

The mod-versus-save separation follows games: **worlds and stories are shareable
content (mods)**, while **a playthrough is a personal save** kept in the database
for reliable, frequent turn writes.

## Locked decisions

1. **Location**: the library lives inside the current application data directory at
   `<data_dir>/library/`, where `data_dir` =
   `directories::ProjectDirs("gm-lab").data_dir()` (on Windows,
   `%APPDATA%\Roaming\gm-lab\data`). Portable mode beside the `.exe` is out of
   scope (a separate task; do not change the README invariant that nothing is kept
   beside the binary). Override the path with `GM_PACKAGES_DIR`, analogous to
   `GM_DIALOG_DB`/`GM_RAG_CACHE_PATH`.
2. **Source of truth**: file packages (folders) scanned at startup for worlds and
   stories. SQLite remains unchanged for playthroughs.
3. **Story↔world relationship**: default to a `world_ref` dependency (both folders
   are required, as with a mod dependency). Export offers a "bake world inside"
   option for a self-contained bundle. This also lets the three current
   self-contained stories ship as packages with a baked world.

## On-disk layout

```
%APPDATA%\Roaming\gm-lab\data\          ← existing data_dir (already contains gm_lab_dialogs.sqlite3 and .tls)
├─ gm_lab_dialogs.sqlite3               ← UNCHANGED: chats/saves + guest_dialog_state
├─ gm_lab_embeddings.sqlite3            ← UNCHANGED (global RAG cache; per-world is future work)
└─ library/                             ← NEW; override: GM_PACKAGES_DIR
   ├─ worlds/
   │  └─ <world_id>/                    ← folder name = world_id (URL-safe and stable)
   │     ├─ world.json                  ← manifest + world bible
   │     ├─ architect.json              ← architect chat history + cache id (for later editing)
   │     └─ assets/
   │        ├─ cover.png                ← formerly world_image_url
   │        └─ map.png                  ← formerly world_map_url
   └─ stories/
      └─ <story_id>/
         ├─ story.json                  ← manifest: world_ref + plot overlay
         ├─ assets/ …
         └─ world/                      ← OPTIONAL: bundled world copy (self-contained variant)
```

## File formats

### `world.json`
```json
{
  "format": "gmlab.world/1",
  "id": "porog-vtorogo-neba",
  "version": 3,
  "status": "ready",
  "title": "Threshold of the Second Sky",
  "genre": "dark isekai",
  "tone": "…",
  "world_size": "…",
  "population": "…",
  "lore": { "…complete WorldLore from crates/gml-world/src/canon/lore.rs…" },
  "assets": { "cover": "assets/cover.png", "map": "assets/map.png" },
  "created_at": "…", "updated_at": "…"
}
```
- `lore` is exactly the `WorldLore` structure. The `world_image_url`/
  `world_map_url` fields now store a **relative path inside the package**
  (`assets/cover.png`) instead of a volatile `/image-files/<run_id>/…` path.
- `version` (integer) increments on every save and supports story `world_ref` values
  and "world updated" detection.
- `architect.json` is separate because it is large and needed only in the studio:
  `architect_messages`, `architect_model_history`,
  `architect_cache_session_id/thread_id`.

### `story.json`
```json
{
  "format": "gmlab.story/1",
  "id": "derevnya-u-zhivoy-dorogi",
  "version": 1,
  "kind": "authored",
  "world_ref": { "id": "porog-vtorogo-neba", "version": ">=3" },
  "world_embedded": false,
  "title": "Village by the Living Road",
  "plot": {
    "player_character": { "…" },
    "hidden_truth": "…",
    "scene": { "…opening…" },
    "story_brief": "…",
    "public_intro": "…",
    "proper_nouns": [], "public_facts": [], "npcs": [], "state_records": [], "time": 480
  }
}
```
- `kind`: `"authored"` (handwritten plot) | `"procedural"` (generated from the
  world at runtime, with a minimal `plot`).
- `world_embedded: true` plus a `world/` folder forms a self-contained bundle (the
  "bake world" option).
- `plot` contains the authored seed fields from the current `catalog.json` **minus**
  the world-bible portion, which comes from the world through `world_ref`. For
  self-contained legacy stories, the world bible ships baked under `world/`.

## Mapping to the current code (1:1)

- `world.json.lore` ← `WorldLore` (`crates/gml-world/src/canon/lore.rs`), with no
  structural changes.
- `story.json.plot` ← seed fields from `crates/gml-stories/src/catalog.json`
  (`player_character`, `hidden_truth`, `scene`, …).
- Procedural launch already supports "world + overlay":
  `World::from_worldgen_with_lore` plus `story_*` values from the request body
  (`crates/gml-server/src/lib.rs:1583`). Story launch means loading
  `world.json.lore` and applying `story.json.plot`.
- Preserve the `/worlds` payload contract, so the frontend does not change in
  Phase 1.

## Phase plan

### Phase 1 — worlds as packages (no API or frontend change)
- Introduce a `WorldStore` abstraction (trait) with a **file-backed** implementation:
  scan `library/worlds/`, load/save `world.json` (+ `architect.json`), and write
  atomically with temp + rename.
- Move `/worlds` handlers (`post_create_world`/`post_update_world`/`list_worlds`/
  `delete_world` in `crates/gml-server/src/lib.rs`) from the `worlds` table to
  `WorldStore`. Keep the existing payload shape.
- **Migration**: a one-time importer reads rows from the `worlds` table in
  `gm_lab_dialogs.sqlite3` and writes packages under `library/worlds/`. The
  `worlds` table may then be marked deprecated, but must not be deleted immediately.
- Remove guest scoping from worlds (they are shareable content, not per-guest);
  retain guest scoping for saves.

### Phase 2 — images inside packages (fixes ephemerality)
- When saving a world or receiving a generated image, the server fetches bytes from
  the sidecar (`/image-files/<run_id>/<file>`), writes them under
  `library/worlds/<id>/assets/`, and rewrites the lore URL to a relative path.
- Add static routes `GET /world-assets/<world_id>/<file>` and
  `/story-assets/<id>/<file>` that serve package files independently of
  `image_enabled` and sidecar liveness.
- Frontend: switch `<img src>` to the new route (`ImagePreview.jsx`,
  `WorldArchitectPanel.jsx`).

### Phase 3 — stories as packages
- Replace `include_str!(catalog.json)` with runtime scanning of `library/stories/`
  in `crates/gml-stories`.
- Ship the three built-in stories as default packages. On first startup, when
  `library/stories/` is empty, unpack them with a baked `world/` because they are
  self-contained.
- Update byte-exact catalog tests (`gml-stories/src/lib.rs` count==3 / id-set /
  byte-length) for the new source.

### Phase 4 — launch content into the game (pulling a deferred layer from WORLD_CREATION_TAB_TZ)
- "Play world" procedurally: world package → `world_lore` → existing `/chats`
  procedural path.
- "Play story": resolve `world_ref` (or a baked `world/`), combine it with
  `story.json.plot`, then build `World` using `from_seed`/`from_worldgen` + overlay.
- Select a world when creating an authored story (UI is a later step, previously
  deferred by the earlier specification).
- Write `world_ref`/`story_ref` into the save payload so the playthrough remains
  reproducible and linked to its package.

### Phase 5 — sharing UX
- "Open library folder" button.
- Export a package to zip; for stories, provide a "bake world inside" checkbox
  (`world_embedded`).
- Import: drop a folder/zip into `library/` and discover it on the next scan (or via
  a watcher).

## Acceptance criteria

- Worlds are stored as folders under `library/worlds/`, read/written as files, and
  the `worlds` table is no longer the source of truth.
- Existing SQLite worlds migrate to folders without losing fields, architect chat
  history, or cache id.
- World images live inside the package and open even when image generation and the
  sidecar are disabled.
- Stories are loaded from `library/stories/` at runtime; a new story can be added by
  dropping in a folder, without recompilation.
- A story references its world through `world_ref`; export can produce a
  self-contained bundle with a baked world.
- A saved world can launch procedurally, and a saved story can launch into a game
  session.
- Playthroughs remain in `gm_lab_dialogs.sqlite3`; frequent turn writes do not
  regress.
- The `/worlds` contract and frontend remain intact in Phase 1.
- Rust tests/clippy and the frontend build pass for affected packages.

## Out of scope for now

- Portable mode beside the `.exe` and arbitrary library-folder selection (separate
  task; AppData remains fixed for now).
- Per-world RAG index (the current `gm_lab_embeddings.sqlite3` is global).
- Exporting playthroughs (saves) to folders as their primary format.
- A shared world timeline across stories (as in the previous specification).
- File watcher for hot reload (startup scan + manual refresh is sufficient).
