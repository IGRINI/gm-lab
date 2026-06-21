# Server endpoint reference fixtures

Captured from the Python `server.py` running with `GM_BACKEND=mock` and a **fresh
temp DB** (clean default `turnvale-murder` game), except `turn_sse_raw.txt` (see below).

These drive the `gml-server` port: the Rust axum server must satisfy the same wire
contract the React frontend (`web/src`) expects.

| file | what to validate |
|------|------------------|
| `settings.json` | `GET /settings` → `{ok, settings, settings_options}` — **deterministic**, match shape + values |
| `stories.json`  | `GET /stories` → `{ok, default_story_id, stories[]}` — **deterministic**, match exactly |
| `models.json`   | `GET /models` → models list shape (values depend on backend) |
| `state.json`    | `GET /state` → keys: model, backend, stream_gm_content, settings, settings_options, run_usage, context_usage, story_id, story_title, public, time, player_character, scene, entities, status_labels, npcs. Validate **shape**; some values (run_usage/context_usage) depend on session state |
| `debug.json`    | `GET /debug` → `{ok, meta, ...}` full state dump. Validate **shape** |
| `turn_sse_raw.txt` | `POST /turn` SSE **wire format only**: each frame is `data: {json}\n\n`, stream ends with `data: {"kind": "done"}\n\n`. NOTE: this one was captured against a loaded save (its event *content* reflects that game) — use it ONLY for frame framing; use `tests/reference/turns/*.json` for event-content goldens. SSE JSON uses Python default `json.dumps` spacing (`", "`/`": "`); the frontend `JSON.parse`s each frame so spacing is not load-bearing (compact is equivalent). |

The definitive `/turn` event-content goldens are in `../turns/` (captured in-process
with a pinned dice seed). The SSE endpoint must produce the same event sequence wrapped
as `data: {json}\n\n` frames.
