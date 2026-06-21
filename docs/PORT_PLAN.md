I'll produce the build spec directly. This is a synthesis task — I have comprehensive subsystem maps and the constraints. Let me write the complete port architecture document.

# GM-Lab: Python → Rust + Tauri Port Architecture & Build Spec

This document is the authoritative build spec for rewriting the entire GM-Lab backend in Rust, packaged as a Tauri desktop app that also runs as a standalone web server. The existing `web/src` React/Vite frontend is reused as-is; the Rust server must satisfy its exact HTTP/SSE contract. Game flow, tool semantics, and prompt text are ported verbatim — only the internal application architecture is re-engineered.

---

## 1. Target architecture

### 1.1 Cargo workspace layout

A single workspace, `gm-lab/`, with the following crates. Decomposition follows the natural dependency layering observed in the Python codebase (config/prompts → world → agents/orchestrator → clients/persistence/rag → server → app), with leaf crates having no internal deps so they compile and test first.

```
gm-lab/
├── Cargo.toml                      # [workspace]
├── crates/
│   ├── gml-types/                  # shared value types, enums, serde shapes, error types
│   ├── gml-config/                 # static Config + dynamic RuntimeSettings
│   ├── gml-prompts/                # verbatim prompt/template strings + tool_guidance fragments
│   ├── gml-world/                  # World, NPC, Scene, facts, state-records, dice (MT19937), entity refs
│   ├── gml-stories/               # static story catalog (RON/JSON data + loaders)
│   ├── gml-rag/                    # embeddings client, SQLite vector cache, hybrid reranker
│   ├── gml-llm/                    # Backend trait, OpenAICompat client, Mock client, JSON stream helpers
│   ├── gml-codex/                  # Codex OAuth (PKCE) + Responses API client (impersonating HTTP)
│   ├── gml-agents/                 # message assembly, GM tool catalog, NPC contract, prompt builders
│   ├── gml-orchestrator/           # Session, run_turn, tool dispatch, compaction, two-phase commit
│   ├── gml-persistence/            # DialogStore (SQLite), snapshot ser/de, embeddings purge wiring
│   ├── gml-audio/                  # STT (impersonating client) + TTS proxy/cache + sidecar manager
│   ├── gml-server/                 # axum HTTP+SSE server, route handlers, TLS sniffing, static SPA
│   ├── gml-mock-server/            # test-only contract mock server (frontend smoke testing)
│   └── gml-app/                    # Tauri app + standalone-server entrypoint (the shipped binary)
└── web/                            # EXISTING React/Vite frontend (unchanged)
```

### 1.2 Crate responsibilities and dependency edges

| Crate | Responsibility | Internal deps |
|---|---|---|
| **gml-types** | `Role` enum (`Gm/Npc/Compact` → `"gm"/"npc"/"compact"`), `Event`/SSE envelope, `ToolExecutionResult`, `NpcResponse {reasoning,speech,action,claims}`, `ParsedCall`, stats structs, error enums (`thiserror`). The single home for cross-crate value types so there are no circular deps. | none |
| **gml-config** | `Config` (immutable, loaded once from env+.env via hand-rolled dotenv loader) and `RuntimeSettings` (mutable, atomic-persisted JSON, cache-backed, `reconcile_for_model`). All ~90 env constants + thresholds. | gml-types |
| **gml-prompts** | `pub const`/`include_str!` of `GM_SYSTEM` (fully spliced), `NPC_SYSTEM_STATIC`, `NPC_CARD_TEMPLATE`, `GM_COMPACT_SYSTEM`, `NPC_COMPACT_SYSTEM`, and the `tool_guidance` fragments. Verbatim byte-for-byte. Golden snapshot tests. | gml-config (for `CHARS_PER_TOKEN` to compute `SYS_EST`) |
| **gml-world** | `World` and all dataclasses; MT19937 RNG (CPython-compatible getstate/setstate); deterministic dice + grading; scope-gated state records; secret/hidden-canon isolation; projection methods (`scene_context`, `retrieval_documents`, `entity_refs`, `npc_scene_slice`, `fact`); `state_record_hash` (canonical JSON + sha256). | gml-types, gml-config, gml-prompts (defaults), gml-rag (trait only, optional) |
| **gml-stories** | The 3-story catalog as embedded RON/JSON, `story_ids/story_metadata/list_stories/story_seed/default_story_seed`. Per-session deep clone. | gml-types, gml-world (seed shapes) |
| **gml-rag** | `RagDocument`, `RagHit`, `EmbeddingCache` (rusqlite), `LocalEmbeddingClient` (reqwest), `HashEmbeddingClient` (tests), `RagEngine` (dense+BM25+RRF), `retrieve_world_fact`, `purge_embeddings_for_texts`. Defines an `Embedder` trait. | gml-types, gml-config |
| **gml-llm** | `Backend` trait (`chat/chat_stream/chat_json/chat_json_stream/summarize/list_models/set_model/set_session_identity/...`), `OpenAICompatClient`, `MockClient`, `extract_json_string`/`json_unescape`, `make_client` factory. | gml-types, gml-config, gml-prompts |
| **gml-codex** | `CodexCredential`, PKCE OAuth flow (loopback callback), `CodexClient` (Responses API), strict-schema transform, SSE event parser, impersonating HTTP client. Implements `gml-llm::Backend`. | gml-types, gml-config, gml-prompts, gml-llm (trait) |
| **gml-agents** | `_gm_request_messages`, `gm_user_message`, `gm_turn_stream`, `gm_prelude_stream`, NPC message assembly, `build_gm_tools`/`build_gm_tools_for_model`/`search_gm_tools`, `NPC_SCHEMA`, `build_world_seed`, `extract_scene_delta`. The model-boundary layer. | gml-types, gml-config, gml-prompts, gml-world, gml-llm |
| **gml-orchestrator** | `Session`, `run_turn` (async `Stream<Event>`), tool dispatch (`_run_tool`), `_ask_npc`, GM/NPC compaction, two-phase draft/commit, visibility boundaries, `context_usage`, token estimation. | gml-types, gml-config, gml-prompts, gml-world, gml-agents, gml-llm |
| **gml-persistence** | `DialogStore`, `DialogRuntime`, snapshot `_session_to/from_payload`, `_world_to/from_payload` (incl. RNG state round-trip), chat lifecycle, active-pointer self-heal, embeddings purge on delete. | gml-types, gml-config, gml-world, gml-orchestrator, gml-rag, gml-llm (factory) |
| **gml-audio** | STT via impersonating client (`transcribe`), TTS proxy + on-disk clip cache, ffmpeg transcode, **sidecar process manager** (spawn/health-poll/kill — new). | gml-types, gml-config, gml-codex (credential) |
| **gml-server** | axum app: every endpoint + SSE `/turn`, static SPA serving, dual HTTP/HTTPS listener with TLS byte-sniffing, per-chat locking. | all domain crates |
| **gml-mock-server** | Deterministic contract mock (port of `mock_server.py`) for frontend smoke tests. Separate bin/feature, never shipped. | gml-types, gml-config, gml-world |
| **gml-app** | The shipped binary: Tauri app by default, `--server` flag for headless server mode. Owns process lifecycle, app-data dirs, sidecar wiring. | gml-server, gml-audio, gml-config |

**Dependency edges (acyclic):**

```
gml-types  ←──────────────────────────────────────────────┐
   ↑                                                        │
gml-config ← gml-prompts ← gml-world ← gml-stories          │
   ↑            ↑             ↑                             │
gml-rag ────────┘             │                             │
gml-llm ← gml-codex           │                             │
   ↑                          │                             │
gml-agents ───────────────────┘                             │
   ↑                                                        │
gml-orchestrator ← gml-persistence                          │
   ↑                  ↑                                     │
gml-audio             │                                     │
   ↑                  │                                     │
gml-server ───────────┴──── gml-mock-server ───────────────┘
   ↑
gml-app
```

### 1.3 Technology choices (decisive)

| Concern | Choice | Reason |
|---|---|---|
| **Async runtime** | `tokio` (multi-thread) | SSE streaming, concurrent requests, sidecar/ffmpeg child processes, streaming HTTP all need async; tokio is the de-facto standard and required by axum/reqwest. The Python server held a per-chat lock for the whole streamed turn — we replicate with `tokio::sync::Mutex` per `DialogRuntime`. |
| **HTTP framework** | `axum` | First-class SSE (`axum::response::sse`), tower middleware, hyper-based, integrates cleanly with tokio. **Caveat:** we hand-format SSE frames (`data: {json}\n\n`, no `event:`/`id:` lines) because the frontend parser slices exactly the `data: ` prefix and tolerates only that shape. We use a raw `Body` stream rather than `Sse::keep_alive` to guarantee byte-identical framing. |
| **SQLite layer** | `rusqlite` (bundled) | The Python code uses raw SQL with per-connection PRAGMAs (`busy_timeout=10000`, `journal_mode=WAL`, `synchronous=NORMAL`), one short-lived connection per op, and stores the entire chat as one JSON `payload` column. `rusqlite` maps 1:1 to this style with zero async-runtime impedance; `sqlx`'s compile-time query checking buys nothing for a single-blob schema and its async pool fights the per-op-connection model. We wrap blocking rusqlite calls in `tokio::task::spawn_blocking`. Add `r2d2`+`r2d2_sqlite` only if profiling shows connection-open cost matters. |
| **Serde** | `serde` + `serde_json` (with `preserve_order` feature) | `serde_json` defaults to compact, non-ASCII-preserving output — this **matches** Python's `separators=(',',':')` + `ensure_ascii=False` exactly, critical for prompt-cache prefix byte-identity and `state_record_hash`. `preserve_order` lets us insert payload keys in the same order Python does. For sorted-key settings JSON we use `BTreeMap`. |
| **HTTP client (normal)** | `reqwest` (rustls) with **no read timeout** on streaming paths | Mirrors httpx `Timeout(connect=10, read=None, write=60)` — infinite read is essential for long streaming generations. |
| **HTTP client (impersonating)** | `rquest` (Chrome preset) | STT against ChatGPT's Cloudflare-fronted `/backend-api/transcribe` requires TLS/JA3 impersonation. The Codex Responses backend currently uses plain TLS (header-spoofing only), so `gml-codex` uses `reqwest`; only `gml-audio::transcribe` needs `rquest`. Flagged: if Codex adds JA3 checks, switch `gml-codex` to `rquest` too. |
| **RNG** | custom CPython-compatible MT19937 module + `getrandom` for seeds | Save/restore determinism requires bit-exact MT19937 `getstate/setstate` and Python's `_randbelow_with_getrandbits` rejection sampling. No off-the-shelf crate matches CPython's state layout + bounded-int algorithm. See §4.4. |
| **Hashing** | `sha2` (sha256 for `state_record_hash` + embedding keys), `sha1` (TTS cache key + row keys) | Match Python `hashlib` hexdigests byte-for-byte. |
| **TLS (self-signed LAN)** | `rcgen` + `tokio-rustls` | Replaces `tls_cert.py`/`cryptography`; generate self-signed cert with SANs from local IPs. |
| **Per-OS dirs** | `directories` | Resolve app-data dir for settings/db/cache/credentials instead of next-to-binary (read-only on macOS bundles). |
| **Process/browser** | `tokio::process`, `open` crate | Sidecar/ffmpeg spawning; OAuth browser-open. |
| **Static SPA** | `tower-http::ServeFile` (or embed via `rust-embed`) | Serve `index.html` with `Cache-Control: no-store`. |
| **Tauri** | `tauri` v2 + embedded axum on `127.0.0.1` | Lowest-risk path: the unchanged React app `fetch('/...')`s a real loopback HTTP server, preserving SSE/binary/raw-body semantics exactly. See §3. |

---

## 2. Module-by-module mapping

| Python file / feature | Rust home |
|---|---|
| `config.py` (static constants, `_load_dotenv`, `_env_bool`) | `gml-config::config` (`Config` struct, `load_dotenv`, `env_bool`) |
| `runtime_settings.py` (persisted UI knobs, normalize/clean/migrate, `reconcile_for_model`) | `gml-config::runtime_settings` (`RuntimeSettings`, atomic save) |
| Role enums `ROLE_GM/NPC/COMPACT`, `REASONING_ROLES` | `gml-types::Role` (single source; JSON keys via `format!`) |
| `prompts.py` (`GM_SYSTEM`, `NPC_SYSTEM_STATIC`, `NPC_CARD_TEMPLATE`, compact systems) | `gml-prompts` (`include_str!` sidecar `.txt` + named-placeholder render) |
| `tool_guidance.py` (capability/world-state fragments) | `gml-prompts::tool_guidance` (consts + assembly, golden-tested) |
| `world.py` (`World`, `NPC`, `SceneState`, facts, state-records, dice, entity refs) | `gml-world` (`world`, `npc`, `scene`, `facts`, `state_record`, `dice`, `entity`) |
| MT19937 RNG / `_rng` state | `gml-world::rng` (CPython-compatible) |
| `state_record_hash` | `gml-world::state_record::hash` |
| `stories.py` | `gml-stories` (embedded data + loaders) |
| `rag.py` (`EmbeddingCache`, `LocalEmbeddingClient`, `RagEngine`, `retrieve_world_fact`, `purge_embeddings_for_texts`) | `gml-rag` (`cache`, `client`, `engine`, `retrieve`) |
| `llm_client.py` (`OpenAICompatClient`, `MockClient`, `make_client`, `extract_json_string`, `json_unescape`) | `gml-llm` (`backend` trait, `openai_compat`, `mock`, `json_stream`, `factory`) |
| `codex_oauth.py` (PKCE, credential storage, refresh) | `gml-codex::oauth` |
| `codex_client.py` (`CodexClient`, `split_messages_for_responses`, strict-schema, SSE) | `gml-codex::client` |
| `codex_transcribe.py` (STT) | `gml-audio::stt` (uses `rquest`) |
| `agents.py` (message assembly, tool catalog, NPC contract, seed/scene-delta) | `gml-agents` (`gm`, `npc`, `tools`, `tool_search`, `seed`, `scene_delta`) |
| `orchestrator.py` (`Session`, `run_turn`, `_drive`, `_run_tool`, `_ask_npc`, compaction, `context_usage`) | `gml-orchestrator` (`session`, `turn`, `tools`, `ask_npc`, `compact`, `usage`) |
| `dialog_store.py` (`DialogStore`, snapshot ser/de) | `gml-persistence` (`store`, `payload`, `world_payload`, `session_payload`) |
| `server.py` (routes, SSE, TTS proxy, `_npc_voice`, dual listener) | `gml-server` (`routes`, `sse`, `tts`, `state`, `debug`, `listener`, `tls`) |
| `tls_cert.py` | `gml-server::tls` (`rcgen`) |
| TTS proxy/cache + ffmpeg + sidecar launch (new) | `gml-audio::tts` + `gml-audio::sidecar` |
| `mock_server.py` | `gml-mock-server` |
| `index.html` / `web/src/*` | unchanged; served by `gml-server` |
| `test_contracts.py` | `gml-orchestrator/tests/contracts.rs` + per-crate `#[test]` modules (§8) |
| App entry / launch / `--server` | `gml-app::main` |

---

## 3. Cross-platform strategy

### 3.1 One binary, two modes

`gml-app` is the single shipped artifact. Mode is selected at runtime:

- **Default (no flag):** launch Tauri. Tauri starts an embedded axum server bound to `127.0.0.1:<ephemeral>` (or fixed `GM_PORT`), then opens the webview pointing at that origin. The React app runs unchanged; all `fetch('/...')` calls hit the loopback server, so SSE, binary TTS responses, and raw-body STT all work with real HTTP semantics.
- **`gml-app --server` (headless):** skip Tauri entirely; run axum on `GM_HOST:GM_PORT` (default `127.0.0.1:8000`), optionally enabling the LAN HTTPS listener. People play via browser.

**Rationale:** routing the frontend through a real embedded HTTP server (rather than Tauri `invoke` commands) means **zero frontend changes** and one server implementation serving both modes. The alternative (Tauri commands + a fetch shim) would require re-implementing SSE/streaming/raw-body over the IPC bridge — high risk for the exact-contract requirement.

`gml-server` is a library exposing `fn build_router(state: AppState) -> axum::Router` and `async fn run_http(...)` / `async fn run_https(...)`. Both `gml-app` modes call into it. `main.rs` is thin.

### 3.2 Per-OS bridge plan

| Concern | Python today | Rust bridge | Per-OS notes |
|---|---|---|---|
| **App-data dirs** (settings.json, dialogs.sqlite3, embeddings.sqlite3, tts_cache, .tls, codex creds) | next to source / `%APPDATA%` ad hoc | `directories::ProjectDirs::from("", "", "gm-lab")` → `config_dir()`/`data_dir()`/`cache_dir()`. Honor env overrides (`GM_SETTINGS_PATH`, `GM_DIALOG_DB`, `GM_RAG_CACHE_PATH`, `GM_TTS_CACHE_DIR`, `GM_CODEX_CREDENTIAL_PATH`). | Win: `%APPDATA%/gm-lab`; macOS: `~/Library/Application Support/gm-lab`; Linux: `~/.config/gm-lab`. **Decision:** unify on `directories` (intentional change from Python's mac `~/.config` fallback — migration shim reads the old path once if present). |
| **Atomic writes** (settings, TTS cache, credentials) | `tempfile.mkstemp` + `os.replace` | `tempfile::NamedTempFile` in the **same dir** + `.persist()` (atomic rename) | Same-filesystem rename atomic on all three. Write LF bytes explicitly; never rely on platform newline translation. |
| **.env loading** | `_load_dotenv` (real env wins, inline-comment strip, key regex, quote strip) | Hand-rolled `gml-config::load_dotenv` (NOT `dotenvy` — its precedence/comment semantics differ). Search exe/config dir then cwd, dedup, validate key `^[A-Za-z_][A-Za-z0-9_]*$`, never override existing env. | identical |
| **TTS sidecar spawn** (NEW — Python launches manually) | discovered via `GM_TTS_URL` | `gml-audio::sidecar`: `tokio::process::Command` spawns the Python sidecar once (guarded by `OnceCell<Mutex<Option<Child>>>`), polls `GET /health` until `{ok:true}` or timeout, exposes readiness. Kill on app exit. | Win: `CREATE_NO_WINDOW` flag + **Job Object** to kill the process tree (prevents orphans). Unix: `setpgid` / kill process group. macOS: same as Linux. All TTS calls no-op/return 503 when sidecar disabled or not ready. TTS is optional — the app runs fully without it. |
| **ffmpeg** | `subprocess.run([...])`, fallback to raw WAV | `tokio::process::Command` piping WAV→encoded; on spawn error return raw WAV `(audio/wav, wav)`. Exact arg vectors per format (opus 32k / mp3 56k / passthrough). | Win: `ffmpeg.exe` discovery on PATH; bundle optional. Absence must degrade, never crash. |
| **Browser-open** (OAuth, `GM_OPEN_BROWSER`) | `webbrowser.open` | `open::that(url)`; print URL as fallback (headless/remote). | maps to ShellExecute / `open` / `xdg-open`. |
| **OAuth loopback** | `http.server` on 1455 (fallback 1457) | `tiny_http`/`hyper` single-request server on `127.0.0.1:1455`→1457, 300s timeout, parse `/auth/callback?code&state`, return the same RU HTML. | Handle port-in-use → fallback. |
| **TLS (LAN HTTPS)** | self-signed via `cryptography`, byte-sniffing server | `rcgen` self-signed (SANs = `localhost` + local IPv4 via `local-ip-address`), `tokio-rustls` acceptor with a 1-byte `peek` (`0x16`→TLS wrap, else 308-redirect to https). | Tauri desktop skips this; only `--server` with `GM_HOST=0.0.0.0` enables it. |
| **STT Cloudflare bypass** | `curl_cffi(impersonate='chrome')` | `rquest` Chrome preset (sets matching UA + client-hints; do **not** override UA). Detect `cf-mitigated` response header → treat as challenge even on 2xx. | The single highest-risk cross-platform dependency — see §9. |

---

## 4. Caching & compression preservation plan

This is the core load-bearing requirement. Every invariant below maps to a specific Rust location, with the mechanism for byte-for-byte fidelity.

### 4.1 Prompt-cache prefix ordering

**Invariant:** GM request = `[system: GM_SYSTEM (static)] [system: world_setup (PUBLIC INTRO only)] [optional system: "STORY SO FAR (compact): "+summary] [*append-only gm_messages] [late user turn = CURRENT TURN CONTEXT]`. All mutable per-turn data lives in the late tail; old turns stay byte-identical.

**Rust home:** `gml-agents::gm::request_messages(world, gm_messages, summary) -> Vec<Message>`.
- `GM_SYSTEM` is a `const &str` returned unchanged regardless of args (golden-tested for byte-identity across turns).
- `world_setup` contains only `world.public` (`gml-agents` asserts it never includes roster/facts — those go in `gm_user_message`/`gm_turn_context`).
- `Message` is an enum `{System(String), User(String), Assistant{content, tool_calls}, Tool{call_id, content}}`. The builder emits system blocks in fixed order, then appends history, never reordering.
- Tool-call arguments re-serialized with `serde_json::to_string` (compact, no spaces) to match `_json_compact` byte-for-byte — required for prefix cache reuse on re-sent history.
- `_assistant_msg` deliberately **omits** `reasoning_content` from history; `gml-agents` reproduces this (only `role/content/tool_calls` stored).

**NPC prefix:** `gml-agents::npc::request_messages` = `[system: NPC_SYSTEM_STATIC] [optional summary system] [*history (rewritten via _historical_npc_message)] [final user = card prepended to a COPY]`. History stays card-free; the `CURRENT NPC CARD` (with `revision`) leads only the last user message.

**Codex flattening:** `gml-codex::client::split_messages_for_responses` joins ALL system messages into one `instructions` string with `"\n\n"` and converts the rest to ordered input items. This is reproduced exactly — Codex cache reuse hinges on byte-identical `instructions`; we do **not** "fix" it.

### 4.2 Compaction thresholds & triggers

**Rust home:** `gml-orchestrator::compact`.

- `maybe_compact(session)`: fire iff `messages_tokens(gm_messages) >= GM_HISTORY_TOKENS (100000)` **AND** user-boundary count `> GM_KEEP_TURNS (3)`. `cut = starts[len-GM_KEEP_TURNS]`; `recent = msgs[cut..]` kept verbatim; `old + prior summary` → `client.summarize(base, proper_nouns)`; then `gm_messages = recent`; then **`reset_world_query_cache()`** (clears `world_query_seen`).
- `maybe_compact_npc(...)`: fire iff `messages_tokens(npc_messages[id]) >= NPC_HISTORY_TOKENS (64000)` AND boundaries `> NPC_KEEP_EXCHANGES (6)`. Summarize via `NPC_COMPACT_SYSTEM` (`Role::Compact`, think=true). Does **not** reset world-query cache or touch NPC client identity.
- GM uses `msg_text_for_summary` (strips the `PLAYER ACTION ...` prefix); NPC uses plain `msg_text`. This asymmetry is preserved.
- Summarize input clipped to `COMPACT_INPUT_CHARS (12000)` **by char count** (`text.chars().take(12000)`), not bytes.
- Ordering: `maybe_compact` runs **after** appending the new user turn, **before** the tool loop. Current turn's assistant/tool messages are not compacted until next turn.

These constants live in `gml-config::Config` with exact env names/defaults.

### 4.3 Token estimation

**Invariant:** `estimate_tokens(text) = max(0, text.chars().count() / CHARS_PER_TOKEN)`, `CHARS_PER_TOKEN = 3`. Per-message: render `"{role}: {content}"`, count chars (Unicode scalars, **not** bytes — critical for Cyrillic histories). `SYS_EST = GM_SYSTEM.chars().count() / 3`.

**Rust home:** `gml-orchestrator::usage::estimate_tokens` (one function, used by both compaction gates and `context_usage`). `SYS_EST` is a `LazyLock` computed from the actual `GM_SYSTEM` string. **Mandate:** use `.chars().count()`, never `.len()`.

### 4.4 Deterministic dice & RNG state

**Invariant:** per-campaign `dice_seed = SystemRandom().getrandbits(64)` once at creation; thereafter `random.Random(dice_seed)` (MT19937); full RNG state persisted (`version`, 624-word internal + index, `gauss`) and restored via `setstate` (never reseeded on restore). `randint(1,sides)` uses CPython's `_randbelow_with_getrandbits` rejection sampling. `forced_die_next` (one-shot) / `forced_die_all` (sticky), face clamped `[1,sides]`, never surfaced in `detail`.

**Rust home:** `gml-world::rng` — a **custom CPython-compatible MT19937** implementing: `init_by_array` seeding (for new campaigns), exact `getstate`/`setstate` 625-int layout + `gauss`, and `_randbelow_with_getrandbits` for bounded `randint`. Validate against captured Python `rng_state` vectors and roll sequences in tests. `gml-world::dice` parses notation (`(\d*)d(\d+)(k[hl]\d+)?([+-]\d+)?`), applies forced overrides, computes grade ladder (`_grade_from_margin` exact thresholds) and nat20/nat1 attack overrides.

This is the single hardest fidelity item. Fallback (only if MT proves infeasible): on first load, re-seed from `dice_seed` and accept a one-time determinism break for in-progress campaigns (rolls are recorded in transcripts, so history is preserved). **Primary plan is bit-exact MT.**

### 4.5 Thread-id / prompt_cache_key stability

**Invariant:** `prompt_cache_key = CODEX_PROMPT_CACHE_KEY (default "") or self.thread_id`. `thread_id`/`session_id` are uuid4 per client, persisted (`client_thread_id`/`client_session_id` for GM, `npc_client_state[id]={model,session_id,thread_id}` per NPC) and restored via `set_session_identity`. `reset_npc_memory` drops the live client → fresh thread/key. Backend mismatch wipes ids. OpenAI-compat `prompt_cache_key`/`prompt_cache_retention` sent only when non-empty; `n_cache_reuse` only when `USE_LLAMA_TEMPLATE_KWARGS && LLAMA_CACHE_REUSE > 0`.

**Rust home:**
- `gml-codex::client`: `prompt_cache_key = config.codex_prompt_cache_key.filter(|s|!s.is_empty()).unwrap_or(self.thread_id.clone())`. `set_session_identity(session_id, thread_id)` overrides only when non-empty.
- `gml-orchestrator::session`: `NpcClientState { model, session_id, thread_id }` in a `HashMap`; `ensure_npc_client` get-or-create + restore identity; `reset_npc_memory` removes maps + pins `delivered/_shown` to current seq.
- `gml-persistence`: round-trips all identity fields exactly.
- `gml-llm::openai_compat::payload`: conditional field gating reproduced exactly, with keys inserted in the same order (serde_json `preserve_order`).

### 4.6 RAG ranking & secret isolation

**Rust home:** `gml-rag::engine` reproduces RRF+tiebreak+boost (`RRF_K=60`, keyword tiebreak `0.002`, dense tiebreak `0.001`, status boost `1.04`), BM25 (`k1=1.5, b=0.75`), the exact tokenizer regex + stopwords, and `contextual_text()` byte-format. Embedding cache key = `sha256(stripped text)`; vectors base64 of little-endian f32, L2-normalized on encode and decode. Secret/hidden-canon isolation lives **upstream in `gml-world::retrieval_documents`** (skip `kind=="truth"`, rumors only if player witnessed, whereabouts only if `status!="unknown"`) — `gml-rag` trusts its input. Embeddings DB never contains hidden canon or NPC secrets.

---

## 5. HTTP/SSE contract

The Rust `gml-server` must implement exactly these. JSON is UTF-8, non-ASCII raw. SSE frames are literally `data: {compact-json}\n\n`; `/turn` ends with a `{"kind":"done"}` frame that the client skips.

### 5.1 Endpoints

**GET:** `/` (+ `/index*` → `index.html`, `Cache-Control: no-store`), `/state`, `/debug`, `/models`, `/settings`, `/transcript`, `/stories`, `/chats`, `/export` (download), `/codex/status`. Unknown → 404 `{error:"not found"}`.

**POST:** `/chats` (create), `/chats/{id}/activate`, `/chats/{id}/delete`, `/model`, `/settings`, `/cmd`, `/turn` (SSE), `/transcribe` (raw audio body; codex-only), `/tts` (binary/stream), `/codex/login`, `/codex/logout`, and the debug mutators: `/debug/roll`, `/debug/fact`, `/debug/fact_delete`, `/debug/player`, `/debug/npc`, `/debug/story`, `/debug/scene`, `/debug/state_record`, `/debug/rumor`. Unknown → 404.

**Headers that matter:** `/turn` → `text/event-stream`, `Cache-Control: no-cache`, `X-Accel-Buffering: no`. `/tts` non-stream → `audio/ogg|mpeg|wav` + `Content-Length` + `X-TTS-Voice`. `/tts` PCM stream → `audio/pcm` + `X-Sample-Rate` + `X-TTS-Voice` + `Cache-Control: no-store`. No CORS (same-origin SPA / Tauri webview).

### 5.2 SSE event kinds (`/turn`)

Every event is the envelope `{kind, agent, data, sid}` (all four keys always present; `agent`/`sid` may be null). The complete `kind` set the server must emit:

`player`, `delta` (`data.channel ∈ {gm_thinking, gm_narration, npc_speech}`), `gm_thinking`, `gm_narration`, `meta`, `meta_total`, `gm_tool_call` (suppressed for `ask_player`), `tool_result` (suppressed for `ask_player`; full text, not the model-facing compact text), `tool_search`, `player_options`, `dice`, `world_fact`, `world_state_update`, `world_query`, `npc_profile`, `time`, `player_character_update`, `npc_whereabouts`, `scene_update` (agent `"scene_sync"` for auto-applied deltas), `npc_start`, `npc_history`, `npc_thinking`, `npc_speech` (`data={speech,action,claims,npc_id}`), `gm_reject`, `error` (non-terminal), and the terminal `done`.

**Event ordering contracts** (frontend depends on these): `gm_tool_call` before running, `tool_result` after; pre-tool prelude `gm_narration` before its `gm_tool_call`; `player_options` before final `gm_narration`; `delta`s and finalized rows share the same `sid` so finals replace deltas in place. `done` is push-only (never appended to transcript). On replay (`/transcript`), `kind=="delta"` is filtered and NPC names sanitized to current player labels.

**Implementation:** `run_turn` is `async fn -> impl Stream<Item = Event>`. Nested Python `yield from` (over `_run_tool`/`_ask_npc`/`_sync_scene_delta`/`_generate_pre_tool_prelude`) is refactored to helpers taking `&mut mpsc::Sender<Event>` and returning `ToolExecutionResult` — flattening into one event stream without stream-nesting.

---

## 6. Data / persistence plan

### 6.1 SQLite schema (preserve exactly)

**Dialogs DB** (`gm_lab_dialogs.sqlite3`, `GM_DIALOG_DB`):

```sql
CREATE TABLE IF NOT EXISTS dialog_chats (
  guest_id TEXT NOT NULL, chat_id TEXT NOT NULL,
  title TEXT NOT NULL, preview TEXT NOT NULL,
  turn_count INTEGER NOT NULL DEFAULT 0,
  payload TEXT NOT NULL,
  created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
  PRIMARY KEY (guest_id, chat_id));
CREATE INDEX IF NOT EXISTS idx_dialog_chats_guest_updated ON dialog_chats(guest_id, updated_at);
CREATE TABLE IF NOT EXISTS guest_dialog_state (
  guest_id TEXT PRIMARY KEY, active_chat_id TEXT,
  created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
```

**Embeddings DB** (`gm_lab_embeddings.sqlite3`, separate, `GM_RAG_CACHE_PATH`):

```sql
CREATE TABLE IF NOT EXISTS embeddings (
  model TEXT NOT NULL, text_hash TEXT NOT NULL, text TEXT NOT NULL,
  dims INTEGER NOT NULL, vector_b64 TEXT NOT NULL, created_at REAL NOT NULL,
  PRIMARY KEY (model, text_hash));
```

All connections: `PRAGMA busy_timeout=10000; journal_mode=WAL; synchronous=NORMAL`. One short-lived connection per op (via `spawn_blocking`). Timestamps via `datetime('now')` (UTC `YYYY-MM-DD HH:MM:SS`) — reproduce exactly so string ordering matches. Ordering everywhere: `(updated_at DESC, created_at DESC, chat_id DESC)`.

### 6.2 Snapshot (de)serialization compatibility

`SCHEMA_VERSION = 1`, hard-checked on load (error on mismatch; no migrations exist). Top-level payload `{schema_version, turn_count, session, transcript}` serialized with `serde_json::to_string` (compact, UTF-8 raw) to match Python's `ensure_ascii=False, separators=(',',':')`.

`gml-persistence` defines serde structs for the full `session`/`world` payloads with **exact field names**, including the renames `_sid→sid`, `_seq→seq`, `_shown→shown`. Use `#[serde(default)]` generously (old snapshots omit `card_revision` etc. → default 0). `ac`/`hp`/`abilities`/`metadata` are `serde_json::Value` (loosely typed). `witnesses` serialized sorted (deterministic) → restored as set. `world_query_seen` serialized as sorted `Vec` per scope → restored as `HashSet`. `pending[npc]` **must** persist `user_message`+`assistant_message` (commit_turn needs both).

World restore mirrors Python's `__new__` bypass: build the struct from explicit fields only, set RNG from `rng_state` (never reseed), call `ensure_npc_whereabouts`. `dice_seed` and `rng_state` are **required** on load (error if missing) — except for the MT fallback path (§4.4). Live NPC clients are **not** recreated on load (only ids); rebuilt lazily via the `make_client` factory in `ensure_npc_client`.

Atomicity: each store method = one connection, multi-statement methods wrapped in an explicit `rusqlite::Transaction` (`BEGIN`...`COMMIT`) to reproduce single-commit-on-clean-exit. In-memory cache: `Mutex<HashMap<(String,String), DialogRuntime>>`; `save()` updates the cache **after** the DB write; `merge` clears the whole cache. Embeddings purge on delete is best-effort and never raises.

---

## 7. Dependency-ordered implementation roadmap

Vertical slices that compile+test incrementally. Within a wave, items are parallelizable; waves are sequential.

**Wave 0 — Foundations (parallel):**
1. `gml-types` — enums, envelope, error types. (unblocks everything)
2. `gml-config` — `Config` + `RuntimeSettings`, dotenv loader, atomic save. Tests: clamp/normalize/migrate, env_bool truthiness.
3. `gml-prompts` — embed verbatim strings + tool_guidance assembly. Tests: golden snapshots (byte-equal to Python source).

**Wave 1 — World & data (sequential after 0):**
4. `gml-world::rng` — CPython MT19937. **Test first** against captured Python `getstate`/roll vectors. (gates dice fidelity)
5. `gml-world` — structs, dice+grading, state records + `state_record_hash`, scope visibility, projections, secret isolation. Tests: hash byte-equality, dice grade ladder, forced-die, scope gating.
6. `gml-stories` — embedded catalog + loaders. Tests: `DEFAULT_STORY_ID`, 3 stories, deep-clone isolation.

**Wave 2 — Retrieval & LLM transport (parallel):**
7. `gml-rag` — cache (reuse existing sqlite file), engine (RRF/BM25/tokenizer), `HashEmbeddingClient` for tests. Tests: ranking parity, cache key, isolation.
8. `gml-llm` — `Backend` trait, `MockClient` (scenario-driven), `OpenAICompatClient`, json-stream helpers. Tests: `extract_json_string` partial-JSON, payload field gating, mock scenario.
9. `gml-codex` — OAuth PKCE, `CodexClient`, strict-schema, SSE parser, impersonating headers. (parallel with 7/8; only trait dep on `gml-llm`)

**Wave 3 — Model boundary (after 1,2):**
10. `gml-agents` — message assembly (prefix ordering!), tool catalog (static, golden-tested), `tool_search`, NPC contract, seed/scene-delta. Tests: prefix byte-stability across turns, tool schema staticness (no dynamic enums/names), `_norm_npc` coercion.

**Wave 4 — Orchestration (after 3):**
11. `gml-orchestrator` — `Session`, token estimation, compaction gates, `run_turn` stream + tool dispatch, `_ask_npc` two-phase draft/commit, visibility boundaries, `context_usage`, generator-critic. Tests: the bulk of `test_contracts.py` (§8).

**Wave 5 — Persistence & audio (parallel after 4):**
12. `gml-persistence` — DialogStore, snapshot ser/de, chat lifecycle, active-pointer self-heal, purge wiring. Tests: round-trip a Python-written DB row; card_revision defaults; rng round-trip.
13. `gml-audio` — STT (`rquest`), TTS proxy + cache, ffmpeg, sidecar manager. Tests: cache key scheme, voice-by-gender, graceful degradation.

**Wave 6 — Server & app (after 5):**
14. `gml-server` — all routes, SSE framing, state/debug payload assembly, TLS sniffing listener, static SPA. Tests: contract-shape tests per endpoint; SSE frame byte-format.
15. `gml-mock-server` — port `mock_server.py`; run the **existing frontend** against it to validate the wire contract early (can start as soon as `gml-types` exists — pull forward to Wave 1 if frontend validation is wanted sooner).
16. `gml-app` — Tauri + `--server`, app-data dirs, sidecar wiring, browser-open.

**Critical path:** `gml-types → gml-world(rng) → gml-agents → gml-orchestrator → gml-server → gml-app`. The MT19937 (4) and prefix assembly (10) are the two riskiest gates and should start early. `gml-mock-server` (15) can be built in Wave 1 to exercise the real frontend against the wire contract long before the real backend exists.

---

## 8. Testing strategy

### 8.1 `test_contracts.py` → Rust tests

`test_contracts.py` is the executable spec. Each `assert` becomes a Rust `#[test]`, primarily in `gml-orchestrator/tests/contracts.rs` (driven by `MockClient`, mirroring `GM_BACKEND=mock`), with leaf invariants pushed to their owning crate:

- **Prefix ordering / staticness** → `gml-agents` tests: `GM_SYSTEM` byte-identical across turns; `world_setup` excludes roster/facts; roster/facts present only in turn context; `TURN RESOLUTION CHECK`/`<system-reminder>` precede `PLAYER ACTION`; NPC card absent from history, present in last user message; tool schemas have no dynamic enums / no NPC names; `initial_gm_tool_names` == the exact 8.
- **Compaction** → `gml-orchestrator`: trigger conditions (token gate AND boundary gate), keep-last-N verbatim, summarize-old, `reset_world_query_cache` after GM compaction, GM vs NPC `msg_text` asymmetry.
- **Dice/grading** → `gml-world`: forced-die one-shot consume, grade ladder, crit overrides, `[forced]` never in detail.
- **Scope isolation** → `gml-world` + `gml-orchestrator`: player scope hides gm/npc/goal; shared visible to participants only; RAG corpus excludes truth/secrets; query already_delivered de-dup + pagination + reset on compaction; hash/conflict optimistic concurrency.
- **Tool-result two-channel** → `gml-orchestrator`: `.full` is JSON without reminder; `.model` is compact text with trailing `<system-reminder>`, non-empty, not JSON; `ask_player` engine-handled (no tool_call/tool_result events, exact `PLAYER OPTIONS` tool message).
- **NPC reset / debug edits** → `gml-orchestrator`: `reset_npc_memory` returns true for any real NPC, pins delivered/_shown; `apply_debug_edit` presence guard + reset-only-on-flag.
- **card_revision** → `gml-world` + `gml-persistence`: bumps only on content change, not color, idempotent; defaults 0.
- **Settings/stories** → `gml-config`/`gml-stories`: clamps, defaults, `DEFAULT_STORY_ID`.

A top-level integration test `gml-app/tests/all_contracts.rs` runs the full suite and prints the equivalent of `ALL CONTRACT TESTS PASSED`.

### 8.2 Mock backend

`gml-llm::MockClient` ports the deterministic scenario (tool-message-count-driven GM steps, canned world-seed/scene-delta/NPC JSON). It's the default backend for contract tests, exactly as Python.

### 8.3 Byte-for-byte validation against Python reference

A `tests/reference/` directory holds golden fixtures captured from the Python implementation:
- **Prompt strings:** the spliced `GM_SYSTEM` etc. — `insta` snapshot tests assert byte-equality.
- **RNG vectors:** captured `rng_state` + the next N `randint(1,sides)` outputs for d4/d6/d8/d20/d100 — assert the Rust MT reproduces them exactly (this validates the rejection sampling).
- **`state_record_hash`:** sample records + their Python sha256 hex — assert identical.
- **Snapshot round-trip:** a real Python-written `dialog_chats.payload` JSON — load into Rust structs, re-serialize, assert byte-identical (validates field names, key order, compact separators, UTF-8 rawness).
- **RAG ranking:** with `HashEmbeddingClient`, assert hit order + rounded scores match Python.
- **SSE frames:** capture a Python `/turn` stream, assert the Rust stream produces the same sequence of `kind`s in the same order (data shapes compared structurally; deltas may differ in chunking but finalized rows must match).

A small Python harness (`tools/capture_fixtures.py`) regenerates these from the reference implementation so they can be refreshed.

---

## 9. Risks & open questions (ranked)

| # | Risk | Impact | Mitigation |
|---|---|---|---|
| 1 | **CPython MT19937 bit-exact port** (getstate/setstate layout + `_randbelow_with_getrandbits` rejection sampling). Naive modulo or a generic MT crate diverges silently after restore. | High — breaks deterministic dice on save/restore. | Test-first against captured Python vectors (8.3). Hand-roll the module. Documented fallback: re-seed from `dice_seed` on first load (one-time break; transcripts preserve history). Decide before Wave 1 ships. |
| 2 | **STT Cloudflare/JA3 impersonation** — `rquest` Chrome fingerprint must match closely enough to pass CF. Hard to test without live ChatGPT creds. | High — STT silently 403s. | Prototype `rquest` against the live endpoint early (spike in Wave 2). Detect `cf-mitigated` header. Keep STT optional (gate to `BACKEND==codex`, 400 otherwise). Fallback: ship without STT initially. |
| 3 | **Prompt-cache prefix byte-identity** spans agents+codex+orchestrator+persistence; any drift in whitespace/key-order/serialization kills cache reuse. | High — silent perf regression (no functional break). | Golden snapshot tests for `GM_SYSTEM` and assembled messages. `serde_json` compact + `preserve_order`. Treat `prompts.py`+`tool_guidance.py` as canonical source — embed verbatim, never paraphrase. Verify with byte-diff, not visual. |
| 4 | **Generator/`yield from` → Rust Stream** refactor changes shape (the `(sender, return-value)` pattern); event order must match the UI protocol exactly. | High — subtle UI breakage. | Refactor to `&mut Sender` helpers returning `ToolExecutionResult`. Validate event-by-event against captured Python `/turn` streams (8.3). |
| 5 | **Unread Python source** — `prompts.py` full text, exact `<system-reminder>` strings (in orchestrator result builders), world `*_export` field names, full GM tool JSON-schemas, dialog_store DDL details, `_grade_from_margin` thresholds. | Med-High — invented details drift behavior. | **Open question / action:** before each affected wave, read the exact source files (the maps give call sites, not always full bodies). Port reminder strings and tool schemas verbatim from source, not from test substrings. |
| 6 | **Tauri webview ↔ embedded server** — SSE/binary/raw-body over loopback inside the bundle; macOS read-only bundle paths. | Med | Embedded axum on `127.0.0.1` (chosen) sidesteps IPC limitations. App-data via `directories` (not bundle dir). Spike Tauri+SSE in Wave 6 early. |
| 7 | **TTS sidecar lifecycle is new design** (no Python reference) — spawn/health/orphan-kill cross-platform. | Med | Win Job Objects, Unix process groups. `OnceCell` guard. All TTS paths degrade to 503 when unavailable. Cache readable offline. |
| 8 | **`json_unescape` lacks `\uXXXX`** (latent Python bug) — partial NPC speech may mis-render unicode escapes. | Low | Replicate the limited escape set for byte-fidelity; document the deviation; consider adding `\u` handling behind a flag later. |
| 9 | **Codex `prompt_cache_key` per-process vs persisted** — `thread_id` defaults to fresh uuid4 unless `set_session_identity` is called from the restore path. `installation_id` regenerates per process. | Med | Persist `thread_id`/`session_id` per chat and per NPC; restore via `set_session_identity`. **Open question:** make `installation_id` a persisted per-install uuid (closer to real Codex) vs per-process — confirm with maintainer. |
| 10 | **Python `round()` banker's rounding** in RAG `sources` scores vs Rust. | Low | Match round-half-to-even only if exact debug parity needed; otherwise tolerance-compare. |
| 11 | **Multi-guest merge-on-startup** (`merge_all_chats_into_scope`) — legacy single-tenant fold. | Low | **Open question:** keep or drop in a desktop app? Recommend keeping the merge for DB compatibility, then operating single-scope. |

**Decisive defaults for the open questions** (override only if the maintainer objects): (5) read source before porting each wave — non-negotiable; (9) persist `installation_id` per-install; (11) keep the merge for compatibility; (1) primary = bit-exact MT, fallback = reseed; (2) ship STT as optional from day one.