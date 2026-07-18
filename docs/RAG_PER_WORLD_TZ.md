**English** | [Русский](ru/RAG_PER_WORLD_TZ.md)

# Specification: Per-World RAG (Cache Hygiene, World Sharing, and a Ladybug Index Slot)

Status: Phase A is being implemented; Phase B is deferred until a producer (bake)
exists; Phase C is the RAG redesign track (Ladybug, two-tier store).

## 1. Context and invariants (verified against the code)

- There is no persistent index: retrieval documents are rebuilt for every request
  from the live `World` (`retrieval_documents` includes memory + fact records +
  scene + roster + whereabouts; `memory_documents_for_access` includes memory).
  BM25 is calculated in memory.
- The only persistent artifact is the embedding cache
  (`embeddings(model, text_hash, text, dims, vector_b64, created_at)`, primary key
  `(model, text_hash)`), where
  `model = "{GM_RAG_EMBEDDINGS_MODEL}@{GM_EMBEDDER_QUANT}"` and
  `text_hash = sha256(py_strip(text))`. The cache is only a warm start: a miss is
  recomputed by the sidecar. Sharing correctness is already guaranteed by the key:
  another model produces a miss, never invalid vectors.
- The cache stores the FULL TEXT of documents. Any cache file included in a world
  package therefore sends those texts to the recipient (export recursively zips the
  package directory and excludes only `.*.tmp`).
- Worldgen is seeded AT LAUNCH (`WorldSpec{seed: new_dice_seed()}`), so recipients
  get different procedural-world document text and baked vectors almost always miss
  by content hash. Stable chunks will appear only with Ladybug tier 1.
- The engine used to be a process-global singleton with ONE cache path from the
  first configuration.

## 2. Phase A (NOW): cache isolation by world + GC + export protection

### 2.1 Storage
- Per-world read-write cache: `<GM_RAG_WORLDS_DIR>/<world_id>.sqlite3`.
  `GM_RAG_WORLDS_DIR` is a new environment/config field defaulting to
  `<data_dir>/rag_worlds` (resolved the same way as `GM_RAG_CACHE_PATH`; it is NOT
  derived from the parent of the cache path and does NOT live under `library/`).
  The filename is `world_ref.id`, sanitized to `[A-Za-z0-9_-]`; otherwise use a
  deterministic sha256(id)-based fallback.
- Keep the global `gm_lab_embeddings.sqlite3` unchanged for sessions where
  `world.world_ref == None` (built-in stories).
- HARD RULE: in Phase A, the runtime writes NOTHING RAG-related inside
  `library/worlds/<id>/`. This invariant protects export privacy.
- Do not migrate old rows from the global cache. World-bound sessions lazily
  re-embed into their own file (self-healing with a one-time cost).

### 2.2 Engine routing (ALL THREE entry points)
- Route by `world.world_ref` at the fact path
  (`build_retriever`/`retrieve_world_fact_report`), memory path
  (`retrieve_memory_rows_report` → `..._with_documents`), and the
  `retrieve_memory_rows` wrapper. Memory also contains session text from the world,
  so isolation must cover it or the guarantee has a hole.
- Replace the singleton with a process-wide engine registry keyed by the resolved
  cache path (the global path is the sentinel for `world_ref == None`). Preserve
  "first configuration wins" semantics, now per key. Run `init_db` once per world
  per process; do not rebuild anything on the hot path.
- Concurrency contract (document it in code): two sessions in one world share one
  file; WAL + busy_timeout=10000 + content-addressed `INSERT OR REPLACE` make
  concurrent writers safe.

### 2.3 GC
- World deletion: best-effort `delete_world_cache(world_id)` (the file plus
  `-wal`/`-shm`) at BOTH server deletion sites. Failure is non-fatal, like the
  existing purge hook.
- World import with `overwrite=1`: run the same GC for the reused id, so reimporting
  under the same id never serves cache data from the previous world (poisoning via
  id reuse).

### 2.4 Scoped purge
- Extend `purge_embeddings_for_texts` with world scope: the chat-deletion site in
  gml-persistence reads the session's `world_ref` from its payload and purges ONLY
  that world's file (or the global file for `None`). Do not sweep globally across
  all worlds.

### 2.5 Export safeguards (share.rs)
- Extend the zip traversal filter to skip `*-wal`, `*-shm`, and `*-journal` SQLite
  sidecars throughout the tree. Do NOT filter `rag.sqlite3`: it is the future
  package layer from Phase B and MUST be exported.
- Tests: sidecars are absent from the archive; runtime retrieval for a world-bound
  session creates files only under `GM_RAG_WORLDS_DIR` and never touches the
  package directory.

## 3. Phase B (DEFERRED: requires a producer — bake). The specification is locked to prevent an unsafe implementation

The package warm-start layer is `library/worlds/<id>/rag.sqlite3`, read-only at
runtime and written ONLY by an explicit bake step. Requirements locked by the
adversarial panel:
1. Bake writes the database with `journal_mode=DELETE` (or OFF). NEVER ship WAL:
   readers create `-wal`/`-shm` in the package itself, while a read-only connection
   cannot change journal mode.
2. Runtime open: `SQLITE_OPEN_READ_ONLY` + PRAGMA `query_only=ON`,
   `trusted_schema=OFF`, `cell_size_check=ON`, `mmap_size=0`, low busy_timeout;
   NEVER run the WAL pragma on this connection (`EmbeddingCache` needs a dedicated
   read-only mode, not `connect()`).
3. The file comes from an UNTRUSTED zip: run `PRAGMA quick_check` on first open;
   validate the schema (the database must contain a TABLE named `embeddings` with
   expected columns and no views/triggers); enforce a read-volume budget; ANY
   failure silently skips the layer because it is only a warm start.
4. Store the stamp INSIDE the database: `meta(key,value)` with
   `format=gmlab.rag-cache/1`, `embedder=<model@quant>`, `dims`, `docs`, and a
   reserved `reranker`. Do NOT put it in world.json: `WorldEnvelope::to_file_value`
   rebuilds the manifest from a fixed key set and silently drops unknown keys on the
   next save.
5. Bake is useful only when document text is stable between machines, meaning after
   Ladybug tier 1 (deterministic world-bible chunks). Hit rate for today's
   procedural worlds is approximately zero, so this phase is deferred.
6. When bake is added, audit the `zip_story_with_world` path (a baked world is
   bundled without filtering).

## 4. Phase C (RAG redesign track — Ladybug)

Two-tier store: tier 1 contains deterministic world-bible chunks plus a vector index
in the package as a derived artifact (the same slot and requirements as Phase B);
tier 2 contains local session memory (the Phase A layer). Add cross-encoder rerank
based on benchmark results. Phase A creates exactly the seam—a read-only package
layer plus a read-write local layer—where this fits without rework.

## 5. Rejected alternatives (do not revisit)

- A runtime read-write cache INSIDE the package directory plus an export filter is a
  privacy footgun: one missed filter case ships the player's session text.
- Deriving `rag_worlds_dir` from the parent of `GM_RAG_CACHE_PATH` breaks
  hermeticity (tests put the cache path in the system `%TEMP%`); hygiene must not
  depend on the parent of an arbitrary file path.
- `Config.packages_dir` / resolving the library root again in the orchestrator:
  Phase A does not need a package path at all. In Phase B the server injects the
  path (it already owns `WorldStore`) instead of deriving it again from the
  environment.
- A top-level `"rag"` section in world.json is lost during the `WorldEnvelope`
  round trip (see §3.4).
