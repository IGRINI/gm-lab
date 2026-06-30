# gm-lab-rs unified inference sidecar

One Python process (`serve.py`) that hosts **embeddings + reranker + TTS + image generation** for the
gm-lab-rs app, on a single FastAPI server (`:8077`). It replaces the old llama.cpp
embedder/reranker, the standalone TTS server, and the old local image sandbox.

```
                          ┌────────────────────────────────────────────┐
  Rust app (gml-app)      │  serve.py  (Python 3.12, ONE process)        │
  ─ build_app()           │                                              │
     spawns ──────────────┼─►  uvicorn :8077                             │
  ─ gml-rag client  ──HTTP─┼─►  POST /v1/embeddings   Qwen3-Embedding-0.6B│
  ─ gml-audio tts.rs ─HTTP─┼─►  POST /speak | /speak_stream  Qwen3-TTS 1.7B│
  ─ gml-rag engine ──HTTP─┼─►  POST /rerank          jina-reranker-v3     │
  ─ gml-server      ──HTTP─┼─►  POST /images/generate  ComfyUI FLUX.2 klein│
  ─ gml-server      ──HTTP─┼─►  GET  /image-files/{run}/{file} generated PNG│
  ─ Sidecar health poll ──┼─►  GET  /health                              │
                          └────────────────────────────────────────────┘
```

The sidecar is **stateless** — it holds the *models*, not the document corpus
(the corpus lives in the Rust app's SQLite). Every `/rerank` request must carry
its candidate texts; the sidecar only re-scores what it's given.

---

## Environment — Python 3.12

| Package | Version | Note |
|---|---|---|
| torch | **2.7.0+cu128** | flash-attn wheel is pinned to this — **do not move torch** |
| flash-attn | 2.7.4.post1 | cp312 wheel for torch 2.7 |
| transformers | **4.57.3** | **hard-pinned by `qwen-tts==0.1.1`**; jina-reranker-v3 verified working on it |
| sentence-transformers | 5.6.0 | embedder |
| bitsandbytes | 0.49.2 | nf4 quant |
| accelerate | 1.12.0 | pinned by qwen-tts |
| torchaudio | 2.7.0 | matches torch |
| qwen-tts | 0.1.1 | TTS backbone |
| faster-qwen3-tts | 0.2.6 (editable, `--no-deps`) | CUDA-graph TTS wrapper |

`ruaccent` was **removed** — Qwen3-TTS ignores stress hints, so it added nothing.

`HF_HOME = E:\gemma\gm-lab\hf_models\.hf-home` (TTS weights live on E:).
Voice refs + the local 1.7B model live in `E:\gemma\gm-lab\hf_models\faster-qwen3-tts`.

---

## Models

| Role | Model | Notes |
|---|---|---|
| Embedder | `Qwen/Qwen3-Embedding-0.6B` | 1024-dim, last-token pooling, L2-normalized, asymmetric (query instruction) |
| Reranker | `jinaai/jina-reranker-v3` | listwise 0.6B, multilingual incl. Russian, **raw-cosine scores [-1,1]** |
| TTS | local `qwen17b_base/` (Qwen3-TTS-12Hz-1.7B-Base) | CUDA-graph capture, 24 kHz, voice-clone |
| Image | ComfyUI FLUX.2 klein NVFP4 runtime in `sidecar/image_runtime/` | ComfyUI + NVFP4 workflow warm at sidecar startup, generated PNGs in `image_runtime/generated/<run_id>/` |

TTS voices (all voice-clones on the one model, primed at startup):

| voice | reference | mode |
|---|---|---|
| `gm` | `ref_audio_2.wav` | x-vector clone (narrator) |
| `male` | `ref_audio.wav` | x-vector clone |
| `female` | `female_ref_deepwarm.wav` | ICL (needs exact `ref_text`) |

Default RAG profile keeps the original quality-oriented runtime limits: embedder
and reranker stay in `bf16`, rerank receives 64 candidates, and Jina accepts
candidate/query text up to 2048/512 tokens. Reference bf16 footprint: embedder
~1.3 GB, jina ~1.3 GB, TTS 1.7B ~3.4 GB. Cold start is still dominated by the
TTS CUDA-graph capture.

---

## Endpoints

### `POST /v1/embeddings` (OpenAI-compatible)
```jsonc
// request
{ "input": ["text", ...] | "text",
  "model": "…",            // ignored (server uses EMBEDDER_MODEL)
  "encoding_format": "base64" | "float", // OPTIONAL; base64 = little-endian f32 bytes
  "input_type": "query",   // OPTIONAL → apply Qwen3 query instruction. Absent = document mode.
  "prompt_name": "query",  // alias for input_type
  "task": "…" }            // OPTIONAL English instruction override
// response
{ "object": "list", "data": [ { "object":"embedding", "index":0, "embedding":[…] | "base64…" } ], "model":"…" }
```
**Asymmetric (important):** the Qwen3 instruction is applied to **queries only**;
documents stay bare (instructing documents corrupts the corpus vectors). Default
(no field) = document mode. Embeddings are L2-normalized (cosine == dot). The Rust
client may request `encoding_format:"base64"`; the sidecar returns little-endian
f32 base64 strings and `gml-rag` decodes them back to normalized vectors.

### `POST /rerank` (Jina/Cohere shape)
```jsonc
// request
{ "query": "…", "documents": ["…", …],
  "top_n": 5,               // OPTIONAL; default = env RERANK_TOP_N (0/unset = all)
  "return_documents": false } // OPTIONAL; default = env RERANK_RETURN_DOCS (off)
// response (sorted by score, descending)
{ "results": [ { "index": 8, "relevance_score": 0.50, "document": "…"/*if return_documents*/ }, … ] }
```
**Scores are RAW cosine in [-1,1], NOT 0..1 probabilities** — use them for
ordering / per-query thresholding only; never apply an extra sigmoid.

### `POST /speak` → `audio/wav`
```jsonc
{ "text": "…", "voice": "gm" | "male" | "female" }
```

### `POST /speak_stream` → raw PCM16 mono, header `X-Sample-Rate: 24000`
Same body as `/speak`; streams chunks head-first as the model generates.

### `POST /images/generate`
```jsonc
// request
{ "prompt": "…",
  "steps": 4, "seed": 42,
  "width": 1024, "height": 1024,
  "batch": 1,
  "cfg": 1.0,
  "model": "nvfp4" }
// response
{ "ok": true, "run_id": "…", "seed": 42,
  "images": [ { "filename": "image_0.png", "url": "/image-files/…/image_0.png", "bytes": 123 } ] }
```

The endpoint creates `IMAGE_OUTPUT_DIR/<run_id>/` and writes the final PNGs
there. When `IMAGE_ENABLED=1`, sidecar startup starts ComfyUI and runs a minimal
NVFP4 workflow so the image model is already resident in VRAM before `/health`
reports `image.up=true`. `/health.image.runtime_ready` means the files/models
exist on disk; `/health.image.comfy_up` means the ComfyUI server is listening;
`/health.image.warm` and `up` mean the warmup workflow succeeded.

### `POST /images/start`

Starts ComfyUI and runs the same idempotent NVFP4 warmup workflow used by
startup. It returns immediately with `skipped:true` when the model is already
warm in the current sidecar process.

### `GET /image-files/{run_id}/{filename}` → `image/png`
Safe file server for generated PNGs. The Rust server proxies the same path, so
response URLs are same-origin for the app.

### `GET /health`
Per-model `up` flags, embedder `dim`, reranker score note, TTS voices,
image runtime/ComfyUI status, `rerank_top_n_default`.

---

## Configuration (env vars)

All are read by `serve.py` at startup. In production the Rust app passes a subset
via the sidecar's spawn env (see "Rust wiring").

| Env | Default | Meaning |
|---|---|---|
| `EMBEDDER_MODEL` | `Qwen/Qwen3-Embedding-0.6B` | embedder id |
| `EMBEDDER_QUANT` | `bf16` | `bf16` \| `nf4` (from `GM_EMBEDDER_QUANT`) |
| `EMBEDDER_ENABLED` | `1` | load embedder (off when RAG disabled) |
| `RERANKER_MODEL` | `jinaai/jina-reranker-v3` | reranker id |
| `RERANKER_QUANT` | `bf16` | `bf16` \| `nf4` (from `GM_RERANKER_QUANT`) |
| `RERANKER_ENABLED` | `1` | load reranker |
| **`RERANK_TOP_N`** | unset (= all) | default # of results `/rerank` returns when `top_n` is omitted |
| `RERANK_RETURN_DOCS` | `0` | default for echoing document text back |
| `JINA_MAX_DOC_LEN` | `2048` | passed to `rerank(max_doc_length=…)` (works on this version) |
| `JINA_MAX_QUERY_LEN` | `512` | passed to `rerank(max_query_length=…)` |
| `EMBED_QUERY_TASK` | "Given a web search query…" | English instruction for query-mode embeds |
| `TTS_ENABLED` | `1` | load TTS (off when TTS disabled) |
| `TTS_MODEL_ID` | local `qwen17b_base` else 0.6B | TTS model |
| `TTS_HOME` | `…\faster-qwen3-tts` | voice refs + local model dir |
| `TTS_LANG` | `Russian` | synthesis language |
| `USE_FLASH` | `1` | flash_attention_2 for embedder/reranker |
| `JINA_COMPILE` | `1` | torch.compile the reranker in bf16 |
| `HF_HOME` / `HF_HUB_CACHE` | `…\.hf-home` | model cache on E: |
| `GMLAB_SIDECAR_HOST` / `GMLAB_SIDECAR_PORT` | `127.0.0.1` / `8077` | bind |
| `GMLAB_SIDECAR_LOG` | `<dir>/serve.log` | tee of stdout/stderr (spawner nulls the pipes) |
| `TTS_LOCK_TIMEOUT` | `180` | bounded wait for the TTS lock → 503 on timeout, never hangs |
| `IMAGE_ENABLED` | `1` | enable image generation component |
| `IMAGE_RUNTIME_ROOT` | `<sidecar>/image_runtime` | local ComfyUI + venv + model runtime |
| `IMAGE_COMFY_DIR` | `<runtime>/ComfyUI` | ComfyUI checkout |
| `IMAGE_PYTHON` | `<runtime>/.venv-flux/Scripts/python.exe` | Python used to run ComfyUI |
| `IMAGE_OUTPUT_DIR` | `<runtime>/generated` | per-run generated PNG folders |
| `IMAGE_HF_HOME` | `<runtime>/hf-cache` | HF cache for image runtime |
| `IMAGE_COMFY_HOST` / `IMAGE_COMFY_PORT` | `127.0.0.1` / `8188` | ComfyUI bind |
| `IMAGE_TIMEOUT_SECONDS` | `300` | ComfyUI startup/generation timeout |
| `IMAGE_MAX_WIDTH` / `IMAGE_MAX_HEIGHT` | `2048` / `2048` | request limits |
| `IMAGE_MAX_BATCH` / `IMAGE_MAX_STEPS` | `4` / `50` | request limits |
| `IMAGE_WARMUP_MODEL` | `nvfp4` | model preset warmed at sidecar startup |
| `IMAGE_WARMUP_WIDTH` / `IMAGE_WARMUP_HEIGHT` | `1024` / `1024` | warmup workflow image size |
| `IMAGE_WARMUP_STEPS` | `1` | warmup workflow steps |

`int8` quant was dropped (≈5× slower than bf16). `nf4` remains available as an
explicit override, but the default is `bf16`.

---

## Rust wiring (already done — informational)

- **Spawn:** `gml-audio::Sidecar` (`crates/gml-audio/src/sidecar.rs`) launches
  `serve.py` with `PYTHON`, then `python`/`python3`, from the resolved `sidecar/`
  directory; `GM_TTS_SPAWN_CMD` / `GM_TTS_SPAWN_DIR` are the explicit overrides.
  It health-polls `GET /health` (ready timeout 300 s) and kills the process tree on exit.
- **Start point:** `gml-app build_app()` builds the `SidecarConfig`, pushes
  `EMBEDDER_QUANT` / `RERANKER_QUANT` / `EMBEDDER_ENABLED` / `RERANKER_ENABLED` /
  `TTS_ENABLED` / `IMAGE_ENABLED` / `IMAGE_*` limits into `SidecarConfig.envs`
  from `Config` + runtime settings, then starts it when
  `rag_enabled || tts_enabled || image_enabled`.
- **Config (gml-config):** `GM_EMBEDDER_QUANT` / `GM_RERANKER_QUANT` (`bf16`|`nf4`,
  default `bf16`). **`GM_INFER_URL`** (default `http://127.0.0.1:8077`)
  is the single source: `rag_embeddings_url`, `rag_rerank_url`, and the TTS base
  all derive from it (so they never disagree on host:port).
- **Images (wired & live):** `gml-server` exposes `POST /images/generate` and
  `GET /image-files/{run_id}/{filename}` as same-origin proxies to the sidecar.
  `GM_IMAGE_ENABLED` and `GM_IMAGE_*` limits are read by `gml-config` and passed
  to `serve.py`. `GM_IMAGE_ENABLED` is the default/global switch; the UI/runtime
  setting `image_enabled` gates whether the Rust server starts and accepts image
  requests. When enabled, Python sidecar startup blocks until the ComfyUI NVFP4
  warmup workflow succeeds, so the first generation request should not pay the
  model-load cost. Switching `image_enabled` from off to on restarts the sidecar
  with the same eager image warmup.
- **Reranking (wired & live):** `GM_RAG_RERANK_ENABLED` (default **true**),
  `GM_RAG_RERANK_CANDIDATES` (default **64** — jina single-pass sweet spot) = how
  many fused RRF candidates `engine.rs` sends to `/rerank`; results reorder the
  final `rag_top_k` (default **4** facts to the prompt).

---

## RAG best practices applied (researched 2026-06-22)

- **Asymmetric embeddings** — instruction on queries only; docs bare. Worth
  ~1-5% retrieval ([Qwen3-Embedding card](https://huggingface.co/Qwen/Qwen3-Embedding-0.6B)).
- **Raw-cosine rerank scores** — order/threshold per query, no sigmoid
  ([jina-reranker-v3 paper](https://arxiv.org/html/2509.25085v1)).
- **Rerank is a precision stage after fusion**; feed top-64 (single pass) .. top-100
  (best quality) candidates, emit a small final top-k
  ([Anthropic](https://www.anthropic.com/news/contextual-retrieval), [Pinecone](https://www.pinecone.io/learn/series/rag/rerankers/)).
- **RRF k=60** is the robust standard; fuse rank positions, don't normalize raw
  dense/BM25 scores ([BigDataBoutique](https://bigdataboutique.com/blog/reciprocal-rank-fusion-how-it-works-and-when-to-use-it)).
- **Per-model GPU locks + startup warmup + load-once lifespan** — standard
  single-GPU FastAPI serving ([FastAPI async](https://fastapi.tiangolo.com/async/)).

---

## Reranking (wired & live)

`crates/gml-rag/src/engine.rs::search`, after building the dense+BM25+RRF fused
ranking, sends the fused **top-N** candidate texts (`GM_RAG_RERANK_CANDIDATES`,
default **64** — jina's single-pass sweet spot; <=64 = one forward pass, up to 100
= best published quality but 2 batches) to `rag_rerank_url`, then reorders and
keeps the final **`rag_top_k`** (default **4**) by the returned order (mapping
back via `index`). Scores are raw cosine — order only, no sigmoid threshold.
Rerank errors are not swallowed inside `gml-rag`: the orchestrator reports
degraded retrieval and falls back to the deterministic lexical path. Toggle with
`GM_RAG_RERANK_ENABLED`. Covered by the live integration test
`rerank_documents_orders_by_relevance_live`
(`cargo test -p gml-rag -- --ignored`, needs the sidecar up).

For embeddings, the Qwen3 query asymmetry is **wired & live**:
`gml-rag`'s `LocalEmbeddingClient::embed_query` sends the **bare query** with
`input_type:"query"` + the domain `task` (`engine.rs::QUERY_TASK`), so the sidecar
builds the `Instruct: {task}\nQuery:{q}` template; documents are embedded bare via
`embed`. The `HashEmbeddingClient` test stub uses the client-side template default
([`engine.rs::query_instruction`]) so the golden fixtures stay byte-stable. Covered
by `embed_query_uses_sidecar_query_instruction_live` (`cargo test -p gml-rag -- --ignored`).

---

## Run / troubleshoot

```powershell
$env:HF_HOME="E:\gemma\gm-lab\hf_models\.hf-home"; $env:GMLAB_SIDECAR_PORT="8077"
Push-Location .\sidecar
python .\serve.py
Pop-Location
```
- **Logs**: the Rust spawner runs the sidecar with stdout/stderr → null, so the
  process tees everything to **`serve.log`** (next to `serve.py`, override with
  `GMLAB_SIDECAR_LOG`). Check it first when a boot fails or stalls.
- `'sox' not recognized` at startup is **harmless** (the `sox` python wrapper looks
  for a CLI binary it doesn't need for our path).
- A model that fails to load is non-fatal: its endpoint returns 503, the others
  keep serving. `GET /health` shows which are up.
- Port 8077 busy → the previous instance (or another process) is still bound; the
  Rust manager binds it via uvicorn, so only one should run at a time.

## Gotchas

- **transformers is hard-pinned to 4.57.3 by qwen-tts** — upgrading breaks TTS deps.
- **`rerank(max_doc_length=, max_query_length=)` works** on the installed
  `modeling.py` (verified empirically) even though the HF card omits them — do not
  "fix" by removing.
- **Don't move torch off 2.7.0** — the flash-attn wheel is built for it.
- Putting TTS in the same process as embed/rerank is the user's deliberate choice;
  a long synthesis holds the GPU and can briefly head-of-line-block embed/rerank
  (mitigated by the per-model locks, not eliminated).
