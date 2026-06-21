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
```

See `docs/PORT_PLAN.md` for the full architecture and the invariants this port
must preserve (prompt-cache prefix ordering, history compaction, deterministic
dice, secret isolation, RAG ranking, SQLite on-disk compatibility).
