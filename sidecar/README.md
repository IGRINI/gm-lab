# TaleShift inference sidecar

`serve.py` is the local inference process used by the Rust application. It can
host five optional components in one API:

- Qwen3 embeddings (`POST /v1/embeddings`);
- Jina reranking (`POST /rerank`);
- multilingual Whisper Small STT (`POST /transcribe` with raw WebM/WAV audio);
- Qwen3 TTS (`POST /speak`, `POST /speak_stream`);
- ComfyUI/FLUX.2 image generation (`POST /images/generate`).

The application starts this process on demand and waits for `GET /health`.
PyAV decodes browser audio using libraries bundled in its wheel, so local STT
does not require a system ffmpeg executable. When local STT is unavailable or
disabled, the Rust server keeps the existing connector transcription fallback.

## Installation

Do not assemble the Python environments or model folders by hand. From the
repository root run:

```powershell
.\setup.cmd -Profile Rag
```

Available cumulative profiles are `Minimal`, `Rag`, `Voice`, `Images` and
`Full`. The setup script creates reproducible environments from the lock files,
downloads immutable model revisions from `models.json`, verifies SHA-256 values
and writes readiness markers only after each component is complete.

Default layout:

```text
%LOCALAPPDATA%\gm-lab\inference\
  runtime\.venv\
  models\embedder\
  models\reranker\
  models\stt\
  tts\qwen17b_base\
  image\.venv\
  image\ComfyUI\
  logs\sidecar.log
```

The legacy `gm-lab` directory name is intentionally kept for compatibility
with existing installations.

Use `setup.cmd -InferenceHome <path>` to place this tree elsewhere. Model
downloads resume when setup is rerun. Hugging Face credentials are optional for
the current public artifacts and are never persisted by setup.

## Important compatibility notes

- Text models and TTS require NVIDIA Ampere/RTX 30 or newer with BF16 support.
- The image profile uses NVFP4 and is supported on NVIDIA Blackwell / RTX 50
  (compute capability 10 or newer).
- Flash Attention is optional. `USE_FLASH=auto` enables it only when an
  importable compatible package is present.
- Jina Reranker is CC BY-NC 4.0. Image encoder/VAE license metadata is currently
  incomplete. Review [`../THIRD_PARTY_NOTICES.md`](../THIRD_PARTY_NOTICES.md).

## Standalone development

After setup, activate the managed runtime or call its Python directly:

```powershell
$root = "$env:LOCALAPPDATA\gm-lab\inference"
$env:GM_INFERENCE_HOME = $root
$env:EMBEDDER_ENABLED = "1"
$env:RERANKER_ENABLED = "1"
$env:STT_ENABLED = "1"
$env:EMBEDDER_MODEL = "$root\models\embedder"
$env:RERANKER_MODEL = "$root\models\reranker"
$env:STT_MODEL = "$root\models\stt"
$env:TTS_ENABLED = "0"
$env:IMAGE_ENABLED = "0"
& "$root\runtime\.venv\Scripts\python.exe" .\sidecar\serve.py
```

The default API address is `http://127.0.0.1:8077`. Logs are written to
`<inference-home>/logs/sidecar.log`.

Main environment variables:

| Variable | Purpose |
|---|---|
| `GM_INFERENCE_HOME` | Root of the managed installation |
| `EMBEDDER_ENABLED`, `RERANKER_ENABLED` | Enable RAG components |
| `STT_ENABLED`, `TTS_ENABLED`, `IMAGE_ENABLED` | Enable optional media components |
| `EMBEDDER_MODEL`, `RERANKER_MODEL`, `STT_MODEL` | Preinstalled local snapshot directory |
| `EMBEDDER_QUANT`, `RERANKER_QUANT` | `bf16` or `nf4` |
| `STT_MAX_BYTES`, `STT_MAX_SECONDS` | Raw body and decoded-duration limits (32 MiB / 600 s by default) |
| `USE_FLASH` | `auto`, `1` or `0` |
| `GMLAB_SIDECAR_PORT` | API port, default `8077` |
| `GMLAB_ALLOW_RUNTIME_DOWNLOADS` | Opt in to ad-hoc embedder/reranker downloads; disabled by default. STT always loads locally only |
| `IMAGE_COMFY_PORT` | ComfyUI port, default `8188` |

For integrity checks without downloading:

```powershell
.\setup.cmd -Profile Rag -VerifyOnly
```
