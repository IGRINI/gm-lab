# GM-Lab (Rust + Tauri)

Rust rewrite of the GM-Lab D&D game-master orchestrator (a GM LLM that calls NPC
sub-agent LLMs as tools, with prompt-caching, history compaction, RAG, SQLite
persistence and a React/SSE web UI). Same game flow, tools and prompts as the
Python original — only the internal application architecture is re-engineered.

Ships as **one binary, two modes**:

- **Desktop app (default):** a Tauri window whose webview loads an embedded
  loopback HTTP server, so the React frontend runs unchanged.
- **Headless server (`--server`):** run the HTTP/SSE server and play in a browser.

TTS is **optional**: the app manages the `faster-qwen3-tts` Python sidecar when
enabled and runs fully without it. Cross-platform: Windows, macOS, Linux.

## Workspace layout

```
crates/
  gml-types         shared value types, enums, SSE envelope, errors
  gml-config        static Config (env/.env) + persisted RuntimeSettings
  gml-prompts       verbatim prompt/template strings + tool_guidance
  gml-world         World, NPC cards, Scene, facts, dice (CPython MT19937), entity refs
  gml-stories       embedded story/scenario catalog
  gml-rag           embeddings client + SQLite cache + dense/BM25/RRF reranker
  gml-llm           Backend trait, OpenAI-compatible client, Mock client
  gml-codex         Codex ChatGPT OAuth (PKCE) + Responses API client
  gml-agents        GM/NPC message assembly, tool catalog, NPC contract
  gml-orchestrator  Session, turn loop, tool dispatch, compaction, generator-critic
  gml-persistence   DialogStore (SQLite), snapshot ser/de
  gml-audio         STT + TTS proxy/cache + sidecar process manager
  gml-server        axum HTTP+SSE server, TLS sniffing, static SPA
  gml-mock-server   deterministic contract mock (tests / frontend smoke)
  gml-app           shipped binary: Tauri app + --server mode
web/                React/Vite frontend (reused from the Python project)
docs/               PORT_PLAN.md (build spec) + subsystem_maps.json
tests/reference/    golden fixtures captured from the Python implementation
tools/              capture_fixtures.py (regenerates the golden fixtures)
```

## Build

```powershell
# frontend (single inlined index.html -> web/dist/)
cd web; npm install; npm run build; cd ..

# backend
cargo build --workspace

# run
cargo run -p gml-app                 # desktop (Tauri)
cargo run -p gml-app -- --server     # headless server on 127.0.0.1:8000
cargo run -p gml-app -- --help       # usage + environment variables
```

### Run modes (`gml-app`)

- **Desktop (default):** opens a native window whose webview points at an
  embedded loopback HTTP server (ephemeral port, or `GM_PORT` if set). The React
  app runs unchanged with real HTTP/SSE/binary semantics. The TTS sidecar and
  embedded server are killed on window close.
- **Headless (`--server` or `GM_HEADLESS=1`):** serves on `GM_HOST:GM_PORT`
  (default `127.0.0.1:8000`). Set `GM_HOST=0.0.0.0` to expose on the LAN — this
  auto-enables a second HTTPS listener on `GM_HTTPS_PORT` (default `8443`) so a
  phone/tablet gets the secure context its mic needs (`GM_HTTPS=1`/`0` forces
  on/off). `GM_OPEN_BROWSER=1` opens the URL.

App-data lives in per-OS dirs (`directories` → `ProjectDirs "gm-lab"`):
`%APPDATA%/gm-lab` (Windows), `~/Library/Application Support/gm-lab` (macOS),
`~/.config/gm-lab` + `~/.local/share/gm-lab` (Linux). Each `GM_*` path env var
(`GM_SETTINGS_PATH`, `GM_DIALOG_DB`, `GM_RAG_CACHE_PATH`, `GM_TTS_CACHE_DIR`,
`GM_CODEX_CREDENTIAL_PATH`) overrides its default. macOS bundles are read-only,
so nothing is ever written next to the binary.

### Cross-platform system dependencies (Tauri GUI)

The default `gui` feature pulls in a native webview per OS:

- **Windows:** WebView2 (the Edge WebView2 runtime ships with Win10/Win11 — no
  extra install). MSVC build tools.
- **macOS:** WKWebView (system; nothing to install). Xcode command-line tools.
- **Linux:** `webkit2gtk-4.1` + `libsoup-3.0` at runtime; build deps
  `libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev librsvg2-dev
  build-essential pkg-config` (Debian/Ubuntu names; see the Tauri v2 prereqs for
  other distros).

If the webview deps are unavailable, build the headless-only binary — the
`--server` mode never needs them:

```bash
cargo build -p gml-app --no-default-features   # headless server only
```

See `docs/PORT_PLAN.md` for the full architecture and the invariants this port
must preserve (prompt-cache prefix ordering, history compaction, deterministic
dice, secret isolation, RAG ranking, SQLite on-disk compatibility).
