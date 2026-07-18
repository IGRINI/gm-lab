"""
gmlab unified inference sidecar — ONE Python process hosting embedder + reranker + STT + TTS + image generation.

Endpoints (drop-in for the Rust gml-rag client + gml-audio TTS proxy):
  POST /v1/embeddings   OpenAI-compatible  {input,[model],[encoding_format],[input_type],[task]} -> {data:[{embedding}]}
  POST /rerank          {query,documents,[top_n],[return_documents]} -> {results:[{index,relevance_score,[document]}]}
  POST /transcribe      raw browser audio (WebM/WAV/...) -> {text}
  POST /speak           {text, voice:"gm|male|female"} -> audio/wav
  POST /speak_stream    {text, voice} -> raw PCM16 mono stream + X-Sample-Rate header
  POST /images/generate {prompt,[steps],[seed],[width],[height],[batch],[cfg],[model]} -> generated image metadata
  GET  /image-files/{run_id}/{filename} -> generated PNG
  GET  /health          -> {status, per-model up flags, voices, embed dim}

Per-model config from env (the Rust app passes these from gml-config):
  EMBEDDER_MODEL   (default Qwen/Qwen3-Embedding-0.6B)   EMBEDDER_QUANT = bf16 | nf4   EMBEDDER_ENABLED=1
  RERANKER_MODEL   (default jinaai/jina-reranker-v3)     RERANKER_QUANT = bf16 | nf4   RERANKER_ENABLED=1
  STT_ENABLED=1    STT_MODEL (managed local Whisper snapshot)
  TTS_ENABLED=1    TTS_MODEL_ID (override) TTS_HOME (refs + qwen17b_base dir) TTS_LANG (default Russian)
  IMAGE_ENABLED=1  IMAGE_RUNTIME_ROOT (default ./image_runtime) IMAGE_OUTPUT_DIR
  USE_FLASH=auto  JINA_COMPILE=1  JINA_MAX_DOC_LEN=2048  JINA_MAX_QUERY_LEN=512
  RERANK_TOP_N         default # of results /rerank returns when the request omits top_n (0/empty = all)
  RERANK_RETURN_DOCS=0 default for echoing document text back (retriever owns the corpus -> off)
  EMBED_QUERY_TASK     English instruction used for query-side embedding (Qwen3 wants it in English)
  GMLAB_SIDECAR_HOST=127.0.0.1  GMLAB_SIDECAR_PORT=8077
  GM_INFERENCE_HOME / HF_HOME / HF_HUB_CACHE
  GMLAB_ALLOW_RUNTIME_DOWNLOADS=0 (set to 1 only for an intentional custom setup)

RAG best-practice notes (researched 2026-06-22):
  - Qwen3-Embedding is ASYMMETRIC: the instruction is prepended to QUERIES ONLY; documents stay bare
    (instructing documents corrupts the corpus vectors). Rust sends no role signal, so the default here
    is document mode (no instruction) — unchanged. A caller opts into the query instruction with
    input_type="query" (or prompt_name="query"). Embeddings are L2-normalized (cosine == dot).
  - jina-reranker-v3 scores are RAW cosine in a 256-dim space, bounded [-1,1], NOT 0..1 probabilities —
    use for ordering / per-query thresholding, never apply an extra sigmoid. Already returns sorted desc.
  - Reranking is a PRECISION stage AFTER first-stage retrieval. This project stores short world memory
    notes, but the Rust default still feeds 64 candidates for rerank quality. int8 dropped
    (5x slower than bf16); bf16 remains the default model-weight profile.

INTEGRATION STATUS (see sidecar/README.md for the full doc) — all components WIRED & LIVE:
  - /v1/embeddings — gml-rag client; /transcribe,/speak,/speak_stream — gml-audio. The Rust app spawns this
    process (gml-audio::Sidecar) at startup; HF_HOME / GMLAB_SIDECAR_PORT / EMBEDDER_QUANT /
    RERANKER_QUANT / *_ENABLED are passed via the spawn env.
  - /rerank — gml-rag/src/engine.rs POSTs the fused top-N (GM_RAG_RERANK_CANDIDATES, default 64)
    candidate texts after dense+BM25+RRF, then reorders the final rag_top_k (default 4) by the
    returned `index`; rerank errors propagate so orchestrator can report degraded retrieval.
    GM_RAG_RERANK_ENABLED toggles it.
"""
import base64
import io
import json
import os
import random
import re
import subprocess
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
from contextlib import asynccontextmanager
from pathlib import Path

# --- Managed inference paths. These must be set before importing torch / HF
#     libraries. The Rust launcher and setup.ps1 use the same directory layout.
if "GM_INFERENCE_HOME" in os.environ:
    _INFERENCE_HOME = Path(os.environ["GM_INFERENCE_HOME"]).resolve()
elif os.name == "nt" and os.environ.get("LOCALAPPDATA"):
    _INFERENCE_HOME = (Path(os.environ["LOCALAPPDATA"]) / "gm-lab" / "inference").resolve()
else:
    _INFERENCE_HOME = (Path.home() / ".local" / "share" / "gm-lab" / "inference").resolve()
_DEFAULT_TTS_HOME = _INFERENCE_HOME / "tts"
_DEFAULT_HF_HOME = _INFERENCE_HOME / "hf"
os.environ.setdefault("HF_HOME", str(_DEFAULT_HF_HOME))
os.environ.setdefault("HF_HUB_CACHE", str(Path(os.environ["HF_HOME"]) / "hub"))

# --- Self-logging: the Rust spawner runs us with stdout/stderr -> null, so a
#     failed/slow boot would be invisible. Tee everything to a file too. Set up
#     BEFORE the heavy imports so an import error (torch/cuda/flash-attn) is
#     captured. Override the path with GMLAB_SIDECAR_LOG.
import sys
_LOG_PATH = os.environ.get("GMLAB_SIDECAR_LOG") or str(_INFERENCE_HOME / "logs" / "sidecar.log")
Path(_LOG_PATH).parent.mkdir(parents=True, exist_ok=True)


class _Tee:
    """A faithful stdout/stderr proxy that also writes to a logfile. Defines the
    stream attrs libraries probe (isatty) and delegates the rest (encoding,
    fileno, writable, ...) to the primary stream so uvicorn/logging don't choke."""

    def __init__(self, *streams):
        self._streams = [s for s in streams if s is not None]

    def write(self, data):
        for s in self._streams:
            try:
                s.write(data)
                s.flush()
            except Exception:
                pass

    def flush(self):
        for s in self._streams:
            try:
                s.flush()
            except Exception:
                pass

    def isatty(self):
        return False  # teeing to a file -> no ANSI colors in the log

    def __getattr__(self, name):
        for s in self._streams:
            if hasattr(s, name):
                return getattr(s, name)
        raise AttributeError(name)


try:
    _logfile = open(_LOG_PATH, "a", encoding="utf-8", buffering=1)
    sys.stdout = _Tee(sys.__stdout__, _logfile)
    sys.stderr = _Tee(sys.__stderr__, _logfile)
except Exception:
    pass

import numpy as np
import torch
import soundfile as sf
from fastapi import FastAPI, Request
from fastapi.responses import Response, StreamingResponse, JSONResponse, FileResponse
from pydantic import BaseModel
from starlette.concurrency import run_in_threadpool


def _int_or_none(raw):
    try:
        n = int(str(raw).strip())
        return n if n > 0 else None
    except (TypeError, ValueError):
        return None


# ── Config ──────────────────────────────────────────────────────────────────
EMBEDDER_MODEL = os.environ.get("EMBEDDER_MODEL", "Qwen/Qwen3-Embedding-0.6B")
RERANKER_MODEL = os.environ.get("RERANKER_MODEL", "jinaai/jina-reranker-v3")
EMBEDDER_QUANT = os.environ.get("EMBEDDER_QUANT", "bf16").lower()
RERANKER_QUANT = os.environ.get("RERANKER_QUANT", "bf16").lower()
EMBEDDER_ENABLED = os.environ.get("EMBEDDER_ENABLED", "1") == "1"
RERANKER_ENABLED = os.environ.get("RERANKER_ENABLED", "1") == "1"
STT_ENABLED = os.environ.get("STT_ENABLED", "0") == "1"
TTS_ENABLED = os.environ.get("TTS_ENABLED", "1") == "1"
IMAGE_ENABLED = os.environ.get("IMAGE_ENABLED", "1") == "1"
ALLOW_RUNTIME_DOWNLOADS = os.environ.get("GMLAB_ALLOW_RUNTIME_DOWNLOADS", "0") == "1"
_USE_FLASH_RAW = os.environ.get("USE_FLASH", "auto").strip().lower()
if _USE_FLASH_RAW == "auto":
    try:
        import flash_attn  # noqa: F401
        USE_FLASH = True
    except Exception:
        USE_FLASH = False
else:
    USE_FLASH = _USE_FLASH_RAW in {"1", "true", "yes", "on"}
JINA_COMPILE = os.environ.get("JINA_COMPILE", "1") == "1"
JINA_MAX_DOC_LEN = int(os.environ.get("JINA_MAX_DOC_LEN", "2048"))
JINA_MAX_QUERY_LEN = int(os.environ.get("JINA_MAX_QUERY_LEN", "512"))
# Default # of results /rerank returns when the request omits top_n (None = return all, reranked).
RERANK_TOP_N = _int_or_none(os.environ.get("RERANK_TOP_N", ""))
# Echo document text back by default? Off — the retriever owns the corpus and maps by index.
RERANK_RETURN_DOCS = os.environ.get("RERANK_RETURN_DOCS", "0") == "1"
# Qwen3 query-side instruction (kept in ENGLISH even for RU corpora — model card guidance).
EMBED_QUERY_TASK = os.environ.get(
    "EMBED_QUERY_TASK",
    "Given a web search query, retrieve relevant passages that answer the query",
)
HOST = os.environ.get("GMLAB_SIDECAR_HOST", "127.0.0.1")
PORT = int(os.environ.get("GMLAB_SIDECAR_PORT", "8077"))
STT_MODEL = os.environ.get("STT_MODEL") or str(_INFERENCE_HOME / "models" / "stt")
STT_MAX_BYTES = int(os.environ.get("STT_MAX_BYTES", str(32 * 1024 * 1024)))
STT_MAX_SECONDS = int(os.environ.get("STT_MAX_SECONDS", "600"))
STT_SAMPLE_RATE = 16_000

_IMAGE_RUNTIME_DEFAULT = _INFERENCE_HOME / "image"
IMAGE_RUNTIME_ROOT = Path(os.environ.get("IMAGE_RUNTIME_ROOT", str(_IMAGE_RUNTIME_DEFAULT))).resolve()
IMAGE_COMFY_DIR = Path(os.environ.get("IMAGE_COMFY_DIR", str(IMAGE_RUNTIME_ROOT / "ComfyUI"))).resolve()
IMAGE_PYTHON = Path(os.environ.get(
    "IMAGE_PYTHON",
    str(IMAGE_RUNTIME_ROOT / ".venv" / ("Scripts/python.exe" if os.name == "nt" else "bin/python")),
)).resolve()
IMAGE_OUTPUT_DIR = Path(os.environ.get("IMAGE_OUTPUT_DIR", str(IMAGE_RUNTIME_ROOT / "generated"))).resolve()
IMAGE_HF_HOME = Path(os.environ.get("IMAGE_HF_HOME", str(IMAGE_RUNTIME_ROOT / "hf"))).resolve()
IMAGE_COMFY_HOST = os.environ.get("IMAGE_COMFY_HOST", "127.0.0.1").strip() or "127.0.0.1"
IMAGE_COMFY_PORT = int(os.environ.get("IMAGE_COMFY_PORT", "8188") or "8188")
IMAGE_COMFY_URL = os.environ.get("IMAGE_COMFY_URL", f"http://{IMAGE_COMFY_HOST}:{IMAGE_COMFY_PORT}").rstrip("/")
IMAGE_TIMEOUT_SECONDS = float(os.environ.get("IMAGE_TIMEOUT_SECONDS", "300") or "300")
IMAGE_MAX_WIDTH = int(os.environ.get("IMAGE_MAX_WIDTH", "2048") or "2048")
IMAGE_MAX_HEIGHT = int(os.environ.get("IMAGE_MAX_HEIGHT", "2048") or "2048")
IMAGE_MAX_BATCH = int(os.environ.get("IMAGE_MAX_BATCH", "4") or "4")
IMAGE_MAX_STEPS = int(os.environ.get("IMAGE_MAX_STEPS", "50") or "50")
IMAGE_WARMUP_MODEL = os.environ.get("IMAGE_WARMUP_MODEL", "nvfp4").strip().lower() or "nvfp4"
IMAGE_WARMUP_STEPS = int(os.environ.get("IMAGE_WARMUP_STEPS", "1") or "1")
IMAGE_WARMUP_WIDTH = int(os.environ.get("IMAGE_WARMUP_WIDTH", "1024") or "1024")
IMAGE_WARMUP_HEIGHT = int(os.environ.get("IMAGE_WARMUP_HEIGHT", "1024") or "1024")
IMAGE_COMFY_LOG = Path(os.environ.get("IMAGE_COMFY_LOG", str(IMAGE_COMFY_DIR / "server_image.log"))).resolve()

# model presets: (diffusion_model, text_encoder)
IMAGE_PRESETS = {
    "nvfp4": ("flux-2-klein-4b-nvfp4.safetensors", "qwen_3_4b_fp4_flux2.safetensors"),
}

TTS_HOME = Path(os.environ.get("TTS_HOME", str(_DEFAULT_TTS_HOME)))
TTS_LANG = os.environ.get("TTS_LANG", "Russian")
# Setup installs the complete 1.7B model here. Runtime downloads are disabled
# by default so enabling a UI toggle never starts an unplanned network transfer.
_LOCAL_17B = TTS_HOME / "qwen17b_base"
TTS_MODEL_ID = os.environ.get("TTS_MODEL_ID") or str(_LOCAL_17B)

# Three voices, all clones on one model (prompt cached per reference at warmup):
#   gm     -> ref_audio_2.wav         (x-vector clone)  narrator
#   male   -> ref_audio.wav           (x-vector clone)  male characters
#   female -> ref_audio_3.wav         (x-vector clone)  female characters
VOICES = {
    "gm":     dict(ref_audio=str(TTS_HOME / "ref_audio_2.wav"), ref_text="", xvec_only=True),
    "male":   dict(ref_audio=str(TTS_HOME / "ref_audio.wav"), ref_text="", xvec_only=True),
    "female": dict(ref_audio=str(TTS_HOME / "ref_audio_3.wav"), ref_text="", xvec_only=True),
}
SAMPLING = dict(temperature=0.75, top_k=50, top_p=1.0, repetition_penalty=1.1)

STATE = {
    "embedder": None,
    "reranker": None,
    "stt": None,
    "tts": None,
    "image_process": None,
    "image_warm": False,
    "image_error": "",
}
# The single GPU is shared; these models are not guaranteed re-entrant and the
# handlers are sync `def` (Starlette runs them in a threadpool), so serialize
# each model's inference with its own lock. (GIL + one GPU => no real parallel
# throughput anyway; rely on the models' internal batching.)
_embed_lock = threading.Lock()
_rerank_lock = threading.Lock()
_stt_lock = threading.Lock()
_tts_lock = threading.Lock()
_image_lock = threading.Lock()
# Bounded wait so a stuck / leaked TTS generation degrades to 503 instead of an
# unbounded hang (e.g. if a streamed response's generator frame is abandoned on
# client disconnect and its lock release is deferred).
_TTS_LOCK_TIMEOUT = float(os.environ.get("TTS_LOCK_TIMEOUT", "180"))
_STT_LOCK_TIMEOUT = float(os.environ.get("STT_LOCK_TIMEOUT", "300"))


class ImageRequestError(RuntimeError):
    def __init__(self, status_code: int, message: str):
        super().__init__(message)
        self.status_code = status_code
        self.message = message


class SttRequestError(RuntimeError):
    def __init__(self, status_code: int, message: str):
        super().__init__(message)
        self.status_code = status_code
        self.message = message


# ── Loaders ───────────────────────────────────────────────────────────────────
def _quant_kwargs(quant):
    """transformers model_kwargs for a quant mode (bf16 | nf4)."""
    mk = {}
    if quant == "nf4":
        from transformers import BitsAndBytesConfig
        mk["quantization_config"] = BitsAndBytesConfig(
            load_in_4bit=True, bnb_4bit_quant_type="nf4",
            bnb_4bit_compute_dtype=torch.bfloat16, bnb_4bit_use_double_quant=True)
    else:  # bf16 / full
        mk["dtype"] = torch.bfloat16
    if USE_FLASH:
        mk["attn_implementation"] = "flash_attention_2"
    return mk


def _require_local_model(model: str, component: str) -> None:
    if ALLOW_RUNTIME_DOWNLOADS or Path(model).exists():
        return
    raise RuntimeError(
        f"{component} is not installed at {model!r}; rerun setup.ps1 with the required profile"
    )


def _load_embedder():
    from sentence_transformers import SentenceTransformer
    _require_local_model(EMBEDDER_MODEL, "embedder")
    mk = _quant_kwargs(EMBEDDER_QUANT)
    m = SentenceTransformer(EMBEDDER_MODEL, model_kwargs=mk, device="cuda")
    print(f"[sidecar] embedder {EMBEDDER_MODEL} quant={EMBEDDER_QUANT} loaded", flush=True)
    return m


def _load_reranker():
    from transformers import AutoModel
    _require_local_model(RERANKER_MODEL, "reranker")
    mk = _quant_kwargs(RERANKER_QUANT)
    m = AutoModel.from_pretrained(RERANKER_MODEL, trust_remote_code=True, **mk).eval()
    try:
        m = m.to("cuda")
    except Exception:
        pass
    # torch.compile only helps full-precision (bnb 4-bit doesn't compile well).
    if JINA_COMPILE and RERANKER_QUANT in ("bf16", "fp16", "full"):
        try:
            m = torch.compile(m)
        except Exception as e:
            print(f"[sidecar] torch.compile skipped: {e}", flush=True)
    print(f"[sidecar] reranker {RERANKER_MODEL} quant={RERANKER_QUANT} loaded", flush=True)
    return m


def _load_stt():
    import av  # noqa: F401 - fail startup if the bundled decoder is unavailable
    from transformers import AutoModelForSpeechSeq2Seq, AutoProcessor, pipeline

    model_path = Path(STT_MODEL)
    if not model_path.is_dir():
        raise RuntimeError(
            f"STT model is not installed at {STT_MODEL!r}; rerun setup.cmd with the Voice or Full profile"
        )
    device = 0 if torch.cuda.is_available() else -1
    dtype = torch.float16 if device >= 0 else torch.float32
    processor = AutoProcessor.from_pretrained(model_path, local_files_only=True)
    model = AutoModelForSpeechSeq2Seq.from_pretrained(
        model_path,
        local_files_only=True,
        use_safetensors=True,
        low_cpu_mem_usage=True,
        dtype=dtype,
    ).eval()
    recognizer = pipeline(
        "automatic-speech-recognition",
        model=model,
        tokenizer=processor.tokenizer,
        feature_extractor=processor.feature_extractor,
        device=device,
        dtype=dtype,
        chunk_length_s=30,
        stride_length_s=(5, 5),
    )
    print(f"[sidecar] STT {STT_MODEL} loaded ({'cuda' if device >= 0 else 'cpu'})", flush=True)
    return recognizer


def _decode_audio(blob: bytes) -> np.ndarray:
    if not blob:
        raise SttRequestError(400, "empty audio")
    if len(blob) > STT_MAX_BYTES:
        raise SttRequestError(413, f"audio exceeds {STT_MAX_BYTES} bytes")
    try:
        import av

        with av.open(io.BytesIO(blob), mode="r") as container:
            stream = next((item for item in container.streams if item.type == "audio"), None)
            if stream is None:
                raise SttRequestError(400, "input has no audio stream")
            resampler = av.AudioResampler(format="fltp", layout="mono", rate=STT_SAMPLE_RATE)
            chunks = []
            sample_count = 0

            def append_frames(frames) -> None:
                nonlocal sample_count
                for frame in frames or []:
                    chunk = np.asarray(frame.to_ndarray(), dtype=np.float32).reshape(-1)
                    if chunk.size == 0:
                        continue
                    sample_count += int(chunk.size)
                    if sample_count > STT_MAX_SECONDS * STT_SAMPLE_RATE:
                        raise SttRequestError(413, f"audio exceeds {STT_MAX_SECONDS} seconds")
                    chunks.append(chunk)

            for frame in container.decode(stream):
                append_frames(resampler.resample(frame))
            append_frames(resampler.resample(None))
    except SttRequestError:
        raise
    except Exception as exc:
        raise SttRequestError(400, f"cannot decode audio: {exc}") from exc
    if not chunks:
        raise SttRequestError(400, "decoded audio is empty")
    return np.nan_to_num(np.concatenate(chunks), copy=False)


def _transcribe_audio(blob: bytes) -> str:
    audio = _decode_audio(blob)
    if not _stt_lock.acquire(timeout=_STT_LOCK_TIMEOUT):
        raise SttRequestError(503, "stt busy")
    try:
        result = STATE["stt"](
            {"raw": audio, "sampling_rate": STT_SAMPLE_RATE},
            return_timestamps=True,
            generate_kwargs={"task": "transcribe"},
        )
    finally:
        _stt_lock.release()
    text = str(result.get("text", "")).strip() if isinstance(result, dict) else ""
    if not text:
        raise SttRequestError(422, "speech was not recognized")
    return text


def _load_tts():
    from faster_qwen3_tts import FasterQwen3TTS
    _require_local_model(TTS_MODEL_ID, "TTS model")
    missing_refs = [cfg["ref_audio"] for cfg in VOICES.values() if not Path(cfg["ref_audio"]).is_file()]
    if missing_refs:
        raise RuntimeError(
            "TTS voice references are missing; rerun setup.ps1 with the Voice or Full profile: "
            + ", ".join(missing_refs)
        )
    print(f"[sidecar] loading TTS {TTS_MODEL_ID} ...", flush=True)
    m = FasterQwen3TTS.from_pretrained(TTS_MODEL_ID, device="cuda", dtype=torch.bfloat16)
    m._warmup(prefill_len=100)  # capture CUDA graphs
    # Prime each voice's reference-prompt cache (and surface missing refs early).
    for name, cfg in VOICES.items():
        try:
            m.generate_voice_clone(text="Тест.", language=TTS_LANG,
                                   ref_audio=cfg["ref_audio"], ref_text=cfg["ref_text"],
                                   xvec_only=cfg["xvec_only"], max_new_tokens=8, **SAMPLING)
            print(f"[sidecar] voice '{name}' warm", flush=True)
        except Exception as e:
            print(f"[sidecar] voice '{name}' warmup failed: {e}", flush=True)
    print(f"[sidecar] TTS ready (sr={m.sample_rate})", flush=True)
    return m


def _warmup_text_models():
    """First real request shouldn't pay the torch.compile / CUDA cold start."""
    if STATE["embedder"] is not None:
        try:
            STATE["embedder"].encode(["warmup"], normalize_embeddings=True,
                                     convert_to_numpy=True, show_progress_bar=False)
            print("[sidecar] embedder warm", flush=True)
        except Exception as e:
            print(f"[sidecar] embedder warmup skipped: {e}", flush=True)
    if STATE["reranker"] is not None:
        try:
            with torch.inference_mode():
                STATE["reranker"].rerank("warmup", ["warmup document"], top_n=1)
            print("[sidecar] reranker warm", flush=True)
        except Exception as e:
            print(f"[sidecar] reranker warmup skipped: {e}", flush=True)


def _image_runtime_error() -> str | None:
    checks = [
        (IMAGE_RUNTIME_ROOT, "image runtime root"),
        (IMAGE_COMFY_DIR, "ComfyUI dir"),
        (IMAGE_COMFY_DIR / "main.py", "ComfyUI main.py"),
        (IMAGE_PYTHON, "image Python"),
        (IMAGE_COMFY_DIR / "models" / "vae" / "flux2-vae.safetensors", "Flux VAE"),
    ]
    for path, label in checks:
        if not path.exists():
            return f"missing {label}: {path}"
    for model, (unet, clip) in IMAGE_PRESETS.items():
        if not (IMAGE_COMFY_DIR / "models" / "diffusion_models" / unet).exists():
            return f"missing image diffusion model for {model}: {unet}"
        if not (IMAGE_COMFY_DIR / "models" / "text_encoders" / clip).exists():
            return f"missing image text encoder for {model}: {clip}"
    return None


def _image_server_up(timeout: float = 2.0) -> bool:
    try:
        urllib.request.urlopen(f"{IMAGE_COMFY_URL}/system_stats", timeout=timeout).read()
        return True
    except Exception:
        return False


def _image_status() -> dict:
    runtime_error = None if not IMAGE_ENABLED else _image_runtime_error()
    error = runtime_error or str(STATE.get("image_error") or "")
    comfy_up = _image_server_up(timeout=0.2) if IMAGE_ENABLED else False
    warm = bool(IMAGE_ENABLED and runtime_error is None and STATE.get("image_warm") and comfy_up)
    return {
        "up": warm,
        "enabled": IMAGE_ENABLED,
        "warm": warm,
        "runtime_ready": bool(IMAGE_ENABLED and runtime_error is None),
        "runtime_root": str(IMAGE_RUNTIME_ROOT),
        "output_dir": str(IMAGE_OUTPUT_DIR),
        "comfy_url": IMAGE_COMFY_URL,
        "comfy_up": comfy_up,
        "models": list(IMAGE_PRESETS),
        "max_width": IMAGE_MAX_WIDTH,
        "max_height": IMAGE_MAX_HEIGHT,
        "max_batch": IMAGE_MAX_BATCH,
        "max_steps": IMAGE_MAX_STEPS,
        "error": error,
    }


def _ensure_image_server() -> None:
    if not IMAGE_ENABLED:
        raise ImageRequestError(503, "image generation disabled")
    runtime_error = _image_runtime_error()
    if runtime_error:
        raise ImageRequestError(503, runtime_error)
    IMAGE_OUTPUT_DIR.mkdir(parents=True, exist_ok=True)
    IMAGE_HF_HOME.mkdir(parents=True, exist_ok=True)
    IMAGE_COMFY_LOG.parent.mkdir(parents=True, exist_ok=True)

    proc = STATE.get("image_process")
    if proc is not None and proc.poll() is not None:
        STATE["image_process"] = None

    if _image_server_up():
        return

    env = dict(os.environ)
    env.setdefault("HF_HOME", str(IMAGE_HF_HOME))
    env.setdefault("HF_HUB_CACHE", str(IMAGE_HF_HOME / "hub"))
    flags = 0
    if os.name == "nt":
        flags |= getattr(subprocess, "CREATE_NO_WINDOW", 0)

    print(f"[sidecar] starting ComfyUI image server at {IMAGE_COMFY_URL}", flush=True)
    with open(IMAGE_COMFY_LOG, "ab", buffering=0) as log:
        proc = subprocess.Popen(
            [str(IMAGE_PYTHON), "main.py", "--port", str(IMAGE_COMFY_PORT), "--listen", IMAGE_COMFY_HOST, "--fast"],
            cwd=str(IMAGE_COMFY_DIR),
            stdin=subprocess.DEVNULL,
            stdout=log,
            stderr=log,
            env=env,
            creationflags=flags,
        )
    STATE["image_process"] = proc

    deadline = time.time() + IMAGE_TIMEOUT_SECONDS
    while time.time() < deadline:
        if proc.poll() is not None:
            STATE["image_process"] = None
            raise ImageRequestError(503, f"ComfyUI exited during startup; see {IMAGE_COMFY_LOG}")
        if _image_server_up(timeout=3.0):
            print("[sidecar] ComfyUI image server ready", flush=True)
            return
        time.sleep(0.75)
    raise ImageRequestError(503, f"ComfyUI did not become ready within {IMAGE_TIMEOUT_SECONDS:.0f}s")


def _build_image_workflow(prompt: str, steps: int, seed: int, width: int, height: int,
                          batch: int, cfg: float, model: str, prefix: str) -> dict:
    unet, clip = IMAGE_PRESETS[model]
    return {
        "10": {"class_type": "UNETLoader", "inputs": {"unet_name": unet, "weight_dtype": "default"}},
        "11": {"class_type": "CLIPLoader", "inputs": {"clip_name": clip, "type": "flux2", "device": "default"}},
        "12": {"class_type": "VAELoader", "inputs": {"vae_name": "flux2-vae.safetensors"}},
        "13": {"class_type": "CLIPTextEncode", "inputs": {"text": prompt, "clip": ["11", 0]}},
        "14": {"class_type": "ConditioningZeroOut", "inputs": {"conditioning": ["13", 0]}},
        "15": {"class_type": "CFGGuider", "inputs": {"model": ["10", 0], "positive": ["13", 0], "negative": ["14", 0], "cfg": cfg}},
        "16": {"class_type": "KSamplerSelect", "inputs": {"sampler_name": "euler"}},
        "17": {"class_type": "Flux2Scheduler", "inputs": {"steps": steps, "width": width, "height": height}},
        "18": {"class_type": "EmptyFlux2LatentImage", "inputs": {"width": width, "height": height, "batch_size": batch}},
        "19": {"class_type": "RandomNoise", "inputs": {"noise_seed": seed}},
        "20": {"class_type": "SamplerCustomAdvanced", "inputs": {"noise": ["19", 0], "guider": ["15", 0], "sampler": ["16", 0], "sigmas": ["17", 0], "latent_image": ["18", 0]}},
        "21": {"class_type": "VAEDecode", "inputs": {"samples": ["20", 0], "vae": ["12", 0]}},
        "22": {"class_type": "SaveImage", "inputs": {"images": ["21", 0], "filename_prefix": prefix}},
    }


def _comfy_post(path: str, payload: dict, timeout: float | None = None) -> dict:
    data = json.dumps(payload).encode("utf-8")
    req = urllib.request.Request(
        f"{IMAGE_COMFY_URL}{path}",
        data=data,
        headers={"Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(req, timeout=timeout or IMAGE_TIMEOUT_SECONDS) as resp:
            return json.loads(resp.read().decode("utf-8"))
    except urllib.error.HTTPError as e:
        detail = e.read().decode("utf-8", errors="replace")
        raise ImageRequestError(e.code, detail[:2000])


def _comfy_get_json(path: str, timeout: float | None = None) -> dict:
    with urllib.request.urlopen(f"{IMAGE_COMFY_URL}{path}", timeout=timeout or IMAGE_TIMEOUT_SECONDS) as resp:
        return json.loads(resp.read().decode("utf-8"))


def _submit_image_workflow(graph: dict) -> tuple[float, str, list[tuple[str, str, str]], dict]:
    response = _comfy_post("/prompt", {"prompt": graph})
    prompt_id = response.get("prompt_id")
    if not prompt_id:
        raise ImageRequestError(502, f"ComfyUI did not return prompt_id: {response}")
    t0 = time.time()
    deadline = t0 + IMAGE_TIMEOUT_SECONDS
    while time.time() < deadline:
        history = _comfy_get_json(f"/history/{prompt_id}", timeout=10)
        if prompt_id in history:
            item = history[prompt_id]
            status = item.get("status", {})
            images = [
                (img["filename"], img.get("subfolder", ""), img.get("type", "output"))
                for output in item.get("outputs", {}).values()
                for img in output.get("images", [])
                if img.get("filename")
            ]
            return time.time() - t0, status.get("status_str", ""), images, item
        time.sleep(0.1)
    raise ImageRequestError(504, f"image generation timed out after {IMAGE_TIMEOUT_SECONDS:.0f}s")


def _delete_comfy_outputs(images: list[tuple[str, str, str]]) -> None:
    output_root = (IMAGE_COMFY_DIR / "output").resolve()
    for filename, subfolder, typ in images:
        if typ != "output":
            continue
        path = (output_root / subfolder / filename).resolve()
        try:
            path.relative_to(output_root)
        except ValueError:
            continue
        try:
            path.unlink(missing_ok=True)
        except Exception:
            pass


def _warmup_image_model(model: str | None = None) -> dict:
    model = (model or IMAGE_WARMUP_MODEL or "nvfp4").strip().lower()
    if model not in IMAGE_PRESETS:
        raise ImageRequestError(400, f"unknown image warmup model: {model}")
    if STATE.get("image_warm") and _image_server_up():
        return {"ok": True, "warm": True, "model": model, "skipped": True, "image": _image_status()}

    try:
        STATE["image_warm"] = False
        STATE["image_error"] = ""
        _ensure_image_server()
        steps = _clamp_int(IMAGE_WARMUP_STEPS, 1, 1, IMAGE_MAX_STEPS, "warmup steps")
        width = _clamp_int(IMAGE_WARMUP_WIDTH, 1024, 64, IMAGE_MAX_WIDTH, "warmup width")
        height = _clamp_int(IMAGE_WARMUP_HEIGHT, 1024, 64, IMAGE_MAX_HEIGHT, "warmup height")
        seed = 1
        prefix = f"gmlab_warmup_{uuid.uuid4().hex}"
        graph = _build_image_workflow(
            "warmup, simple neutral test image",
            steps,
            seed,
            width,
            height,
            1,
            1.0,
            model,
            prefix,
        )
        elapsed, status, images, full = _submit_image_workflow(graph)
        if status != "success" or not images:
            detail = full.get("status", {}) if isinstance(full, dict) else full
            raise ImageRequestError(502, f"ComfyUI image warmup failed: {detail}")
        _delete_comfy_outputs(images)
        STATE["image_warm"] = True
        STATE["image_error"] = ""
        print(
            f"[sidecar] image model warm ({model}, {width}x{height}, steps={steps}, {elapsed:.1f}s)",
            flush=True,
        )
        return {
            "ok": True,
            "warm": True,
            "model": model,
            "steps": steps,
            "width": width,
            "height": height,
            "elapsed_seconds": elapsed,
            "image": _image_status(),
        }
    except Exception as e:
        STATE["image_warm"] = False
        STATE["image_error"] = str(e)
        raise


def _fetch_image_blob(filename: str, subfolder: str, typ: str) -> bytes:
    query = urllib.parse.urlencode({"filename": filename, "subfolder": subfolder, "type": typ})
    with urllib.request.urlopen(f"{IMAGE_COMFY_URL}/view?{query}", timeout=IMAGE_TIMEOUT_SECONDS) as resp:
        return resp.read()


def _clamp_int(value: int | None, default: int, min_value: int, max_value: int, label: str) -> int:
    if value is None:
        return default
    if value < min_value or value > max_value:
        raise ImageRequestError(400, f"{label} must be between {min_value} and {max_value}")
    return value


def _generate_images(req) -> dict:
    prompt = (req.prompt or "").strip()
    if not prompt:
        raise ImageRequestError(400, "empty prompt")
    model = (req.model or "nvfp4").strip().lower()
    if model not in IMAGE_PRESETS:
        raise ImageRequestError(400, f"unknown image model: {model}")

    steps = _clamp_int(req.steps, 4, 1, IMAGE_MAX_STEPS, "steps")
    width = _clamp_int(req.width, 1024, 64, IMAGE_MAX_WIDTH, "width")
    height = _clamp_int(req.height, 1024, 64, IMAGE_MAX_HEIGHT, "height")
    batch_raw = req.batch if req.batch is not None else req.batch_size
    batch = _clamp_int(batch_raw, 1, 1, IMAGE_MAX_BATCH, "batch")
    cfg = 1.0 if req.cfg is None else float(req.cfg)
    if cfg <= 0.0 or cfg > 20.0:
        raise ImageRequestError(400, "cfg must be > 0 and <= 20")
    seed = int(req.seed) if req.seed is not None else random.randint(0, 2**31 - 1)

    with _image_lock:
        _ensure_image_server()
        run_id = uuid.uuid4().hex
        graph = _build_image_workflow(prompt, steps, seed, width, height, batch, cfg, model, f"gmlab_{run_id}")
        elapsed, status, images, full = _submit_image_workflow(graph)
        if status != "success" or not images:
            detail = full.get("status", {}) if isinstance(full, dict) else full
            raise ImageRequestError(502, f"ComfyUI generation failed: {detail}")

        run_dir = IMAGE_OUTPUT_DIR / run_id
        run_dir.mkdir(parents=True, exist_ok=False)
        out = []
        for index, (filename, subfolder, typ) in enumerate(images):
            blob = _fetch_image_blob(filename, subfolder, typ)
            out_name = f"image_{index}.png"
            out_path = run_dir / out_name
            out_path.write_bytes(blob)
            out.append({
                "filename": out_name,
                "url": f"/image-files/{run_id}/{out_name}",
                "bytes": len(blob),
            })
        _delete_comfy_outputs(images)
        STATE["image_warm"] = True
        STATE["image_error"] = ""

    return {
        "ok": True,
        "run_id": run_id,
        "seed": seed,
        "model": model,
        "steps": steps,
        "width": width,
        "height": height,
        "batch": batch,
        "cfg": cfg,
        "elapsed_seconds": elapsed,
        "images": out,
    }


def _shutdown_image_server() -> None:
    proc = STATE.get("image_process")
    if proc is None:
        return
    if proc.poll() is None:
        try:
            proc.terminate()
            proc.wait(timeout=10)
        except Exception:
            try:
                proc.kill()
            except Exception:
                pass
    STATE["image_process"] = None


def _generated_image_path(run_id: str, filename: str) -> Path:
    if not re.fullmatch(r"[A-Za-z0-9_-]{1,80}", run_id or ""):
        raise ImageRequestError(400, "invalid run_id")
    if not re.fullmatch(r"[A-Za-z0-9_.-]{1,120}", filename or ""):
        raise ImageRequestError(400, "invalid filename")
    root = IMAGE_OUTPUT_DIR.resolve()
    path = (root / run_id / filename).resolve()
    try:
        path.relative_to(root)
    except ValueError:
        raise ImageRequestError(400, "invalid image path")
    if not path.is_file():
        raise ImageRequestError(404, "image not found")
    return path


@asynccontextmanager
async def lifespan(app: FastAPI):
    t0 = time.time()
    if EMBEDDER_ENABLED:
        try:
            STATE["embedder"] = _load_embedder()
        except Exception as e:
            print(f"[sidecar] embedder load FAILED (RAG embeddings disabled): {e}", flush=True)
    if RERANKER_ENABLED:
        try:
            STATE["reranker"] = _load_reranker()
        except Exception as e:
            print(f"[sidecar] reranker load FAILED (/rerank disabled): {e}", flush=True)
    if STT_ENABLED:
        try:
            STATE["stt"] = _load_stt()
        except Exception as e:
            print(f"[sidecar] STT load FAILED (/transcribe disabled): {e}", flush=True)
    if TTS_ENABLED:
        try:
            STATE["tts"] = _load_tts()
        except Exception as e:
            print(f"[sidecar] TTS load FAILED (/speak disabled): {e}", flush=True)
    if IMAGE_ENABLED:
        try:
            with _image_lock:
                result = _warmup_image_model()
            print(f"[sidecar] image runtime ready at {IMAGE_RUNTIME_ROOT} ({result['elapsed_seconds']:.1f}s warmup)", flush=True)
        except Exception as e:
            STATE["image_error"] = str(e)
            print(f"[sidecar] image warmup FAILED (/images/generate disabled): {e}", flush=True)
    _warmup_text_models()
    up = [k for k in ("embedder", "reranker", "stt", "tts") if STATE.get(k) is not None]
    if IMAGE_ENABLED and STATE.get("image_warm"):
        up.append("image")
    print(f"[sidecar] ready in {time.time()-t0:.1f}s on {HOST}:{PORT} — up: {up}", flush=True)
    yield
    _shutdown_image_server()
    STATE.clear()


app = FastAPI(title="taleshift-sidecar", lifespan=lifespan)


# ── Schemas ─────────────────────────────────────────────────────────────────
class EmbedReq(BaseModel):
    input: list[str] | str
    model: str | None = None
    encoding_format: str | None = None
    # Optional, non-standard (Cohere `input_type` / sentence-transformers `prompt_name`).
    # Absent => document mode (no instruction) — keeps generic OpenAI/Rust callers unchanged.
    input_type: str | None = None      # "query"|"search_query" => apply Qwen3 query instruction
    prompt_name: str | None = None     # alias: "query"
    task: str | None = None            # optional English task override for the query instruction


class RerankReq(BaseModel):
    query: str
    documents: list[str]
    top_n: int | None = None
    return_documents: bool | None = None


class SpeakReq(BaseModel):
    text: str
    voice: str | None = "gm"


class ImageGenerateReq(BaseModel):
    prompt: str
    steps: int | None = None
    seed: int | None = None
    width: int | None = None
    height: int | None = None
    batch: int | None = None
    batch_size: int | None = None
    cfg: float | None = None
    model: str | None = None


# ── Health ──────────────────────────────────────────────────────────────────
@app.get("/health")
def health():
    dim = None
    if STATE["embedder"] is not None:
        try:
            emb = STATE["embedder"]
            dim = (emb.get_embedding_dimension() if hasattr(emb, "get_embedding_dimension")
                   else emb.get_sentence_embedding_dimension())
        except Exception:
            dim = None
    return {
        "status": "ok",
        "embedder": {"up": STATE["embedder"] is not None, "model": EMBEDDER_MODEL,
                     "quant": EMBEDDER_QUANT, "dim": dim},
        "reranker": {"up": STATE["reranker"] is not None, "model": RERANKER_MODEL,
                      "quant": RERANKER_QUANT, "scores": "raw cosine [-1,1], not 0..1"},
        "stt": {"up": STATE["stt"] is not None, "model": STT_MODEL,
                "sample_rate": STT_SAMPLE_RATE, "max_seconds": STT_MAX_SECONDS},
        "tts": {"up": STATE["tts"] is not None, "model": TTS_MODEL_ID,
                "voices": list(VOICES) if STATE["tts"] is not None else []},
        "image": _image_status(),
        "flash": USE_FLASH,
        "rerank_top_n_default": RERANK_TOP_N,
    }


# ── Embeddings (OpenAI-compatible) ────────────────────────────────────────────
# WIRED: gml-rag's LocalEmbeddingClient::embed_query sends the bare query with
# input_type:"query" + the domain `task`, so THIS endpoint applies the Qwen3
# template (`Instruct: {task}\nQuery:{q}`); documents are embedded bare. (The
# HashEmbeddingClient test stub uses the client-side template default for golden
# parity.) See README.
def _format_embedding(vec, encoding_format: str | None):
    fmt = (encoding_format or "float").strip().lower()
    arr = np.asarray(vec, dtype="<f4")
    if fmt == "base64":
        return base64.b64encode(arr.tobytes()).decode("ascii")
    if fmt in ("", "float"):
        return arr.tolist()
    raise ValueError(f"unsupported encoding_format: {encoding_format}")


@app.post("/v1/embeddings")
def embeddings(req: EmbedReq):
    if STATE["embedder"] is None:
        return JSONResponse({"error": "embedder not loaded"}, status_code=503)
    texts = [req.input] if isinstance(req.input, str) else list(req.input)
    if not texts or any((t is None) for t in texts):
        return JSONResponse({"error": "empty input"}, status_code=400)
    try:
        encoding_format = (req.encoding_format or "float").strip().lower()
        if encoding_format not in ("", "float", "base64"):
            raise ValueError(f"unsupported encoding_format: {req.encoding_format}")
    except ValueError as e:
        return JSONResponse({"error": str(e)}, status_code=400)

    it = (req.input_type or "").strip().lower()
    is_query = it in ("query", "search_query") or (req.prompt_name or "").strip().lower() == "query"
    task = (req.task or EMBED_QUERY_TASK).strip()
    # A caller-supplied task must be honored — the model's registered "query"
    # prompt ignores per-request overrides, so route custom tasks to the manual
    # template path.
    custom_task = bool(req.task and req.task.strip() and req.task.strip() != EMBED_QUERY_TASK)

    with _embed_lock:
        if is_query and custom_task:
            # Honor the override (exact model-card template: NO space after "Query:").
            instructed = [f"Instruct: {task}\nQuery:{t}" for t in texts]
            vecs = STATE["embedder"].encode(
                instructed, normalize_embeddings=True,
                convert_to_numpy=True, show_progress_bar=False)
        elif is_query:
            # Default query instruction via the model's registered prompt.
            try:
                vecs = STATE["embedder"].encode(
                    texts, prompt_name="query", normalize_embeddings=True,
                    convert_to_numpy=True, show_progress_bar=False)
            except Exception:
                # No registered "query" prompt -> exact model-card template.
                instructed = [f"Instruct: {task}\nQuery:{t}" for t in texts]
                vecs = STATE["embedder"].encode(
                    instructed, normalize_embeddings=True,
                    convert_to_numpy=True, show_progress_bar=False)
        else:
            # Document / passage mode — bare text, no instruction.
            vecs = STATE["embedder"].encode(
                texts, normalize_embeddings=True,
                convert_to_numpy=True, show_progress_bar=False)

    data = [
        {"object": "embedding", "index": i, "embedding": _format_embedding(v, encoding_format)}
        for i, v in enumerate(vecs)
    ]
    return {"object": "list", "data": data, "model": EMBEDDER_MODEL}


# ── Rerank (jina-reranker-v3) ─────────────────────────────────────────────────
# WIRED: gml-rag/src/engine.rs POSTs the fused top-N (GM_RAG_RERANK_CANDIDATES)
# candidate texts here after dense+BM25+RRF and reorders the final top-k by the
# returned `index`. Scores are RAW cosine [-1,1] — the caller uses ORDER only (no
# sigmoid). Rerank errors propagate so degraded retrieval is visible in tool payloads.
@app.post("/rerank")
def rerank(req: RerankReq):
    if STATE["reranker"] is None:
        return JSONResponse({"error": "reranker not loaded"}, status_code=503)
    if not req.documents:
        return {"results": []}
    top_n = req.top_n if req.top_n is not None else RERANK_TOP_N
    return_docs = RERANK_RETURN_DOCS if req.return_documents is None else bool(req.return_documents)

    with _rerank_lock, torch.inference_mode():
        ranked = STATE["reranker"].rerank(
            req.query, req.documents, top_n=top_n,
            max_doc_length=JINA_MAX_DOC_LEN, max_query_length=JINA_MAX_QUERY_LEN)
    # jina returns a score-sorted (desc) list of {document, relevance_score, index, ...}.
    # Scores are RAW cosine [-1,1] — pass through verbatim, no sigmoid.
    out = []
    for r in ranked:
        idx = int(r["index"])
        item = {"index": idx, "relevance_score": float(r["relevance_score"])}
        if return_docs:
            item["document"] = req.documents[idx]
        out.append(item)
    return {"results": out}


# ── STT ───────────────────────────────────────────────────────────────────────
@app.post("/transcribe")
async def transcribe(request: Request):
    if STATE["stt"] is None:
        return JSONResponse({"error": "stt not loaded"}, status_code=503)
    content_length = request.headers.get("content-length")
    if content_length:
        try:
            if int(content_length) > STT_MAX_BYTES:
                return JSONResponse(
                    {"error": f"audio exceeds {STT_MAX_BYTES} bytes"}, status_code=413
                )
        except ValueError:
            return JSONResponse({"error": "invalid content-length"}, status_code=400)
    chunks = []
    received = 0
    async for chunk in request.stream():
        received += len(chunk)
        if received > STT_MAX_BYTES:
            return JSONResponse({"error": f"audio exceeds {STT_MAX_BYTES} bytes"}, status_code=413)
        chunks.append(chunk)
    if not chunks:
        return JSONResponse({"error": "empty audio"}, status_code=400)
    blob = b"".join(chunks)
    try:
        text = await run_in_threadpool(_transcribe_audio, blob)
        return {"text": text}
    except SttRequestError as exc:
        return JSONResponse({"error": exc.message}, status_code=exc.status_code)
    except Exception as exc:
        print(f"[sidecar] STT request failed: {exc}", flush=True)
        return JSONResponse({"error": "stt inference failed"}, status_code=503)


# ── TTS ───────────────────────────────────────────────────────────────────────
def _synth_wav(text: str, voice: str) -> bytes:
    cfg = VOICES.get(voice) or VOICES["gm"]
    if not _tts_lock.acquire(timeout=_TTS_LOCK_TIMEOUT):
        raise TimeoutError("tts busy")
    try:
        audio_list, sr = STATE["tts"].generate_voice_clone(
            text=text, language=TTS_LANG,
            ref_audio=cfg["ref_audio"], ref_text=cfg["ref_text"],
            xvec_only=cfg["xvec_only"], append_silence=True, **SAMPLING)
    finally:
        _tts_lock.release()
    audio = np.concatenate([np.asarray(a).reshape(-1) for a in audio_list]).astype(np.float32)
    buf = io.BytesIO()
    sf.write(buf, audio, sr, format="WAV", subtype="PCM_16")
    return buf.getvalue()


def _pcm16(chunk) -> bytes:
    arr = np.asarray(chunk).reshape(-1)
    return (np.clip(arr, -1.0, 1.0) * 32767.0).astype("<i2").tobytes()


@app.post("/speak")
async def speak(req: SpeakReq):
    if STATE["tts"] is None:
        return JSONResponse({"error": "tts not loaded"}, status_code=503)
    text = (req.text or "").strip()
    if not text:
        return JSONResponse({"error": "empty text"}, status_code=400)
    try:
        wav = await run_in_threadpool(_synth_wav, text, (req.voice or "gm").strip())
    except TimeoutError:
        return JSONResponse({"error": "tts busy"}, status_code=503)
    return Response(content=wav, media_type="audio/wav")


@app.post("/speak_stream")
def speak_stream(req: SpeakReq):
    if STATE["tts"] is None:
        return JSONResponse({"error": "tts not loaded"}, status_code=503)
    text = (req.text or "").strip()
    if not text:
        return JSONResponse({"error": "empty text"}, status_code=400)
    voice = (req.voice or "gm").strip()
    cfg = VOICES.get(voice) or VOICES["gm"]
    sr = int(STATE["tts"].sample_rate)

    # Acquire BEFORE returning the response: a busy/stuck model degrades to 503
    # here instead of hanging. The generator's `finally` always releases — on
    # normal completion and on GeneratorExit when the client disconnects.
    if not _tts_lock.acquire(timeout=_TTS_LOCK_TIMEOUT):
        return JSONResponse({"error": "tts busy"}, status_code=503)

    def gen():
        try:
            for chunk, _sr, _timing in STATE["tts"].generate_voice_clone_streaming(
                    text=text, language=TTS_LANG,
                    ref_audio=cfg["ref_audio"], ref_text=cfg["ref_text"],
                    xvec_only=cfg["xvec_only"], chunk_size=8, **SAMPLING):
                yield _pcm16(chunk)
        finally:
            _tts_lock.release()

    return StreamingResponse(gen(), media_type="audio/pcm", headers={"X-Sample-Rate": str(sr)})


@app.post("/images/generate")
async def images_generate(req: ImageGenerateReq):
    try:
        return await run_in_threadpool(_generate_images, req)
    except ImageRequestError as e:
        return JSONResponse({"ok": False, "error": e.message}, status_code=e.status_code)
    except Exception as e:
        return JSONResponse({"ok": False, "error": str(e)}, status_code=500)


@app.post("/images/start")
async def images_start():
    try:
        def start():
            with _image_lock:
                return _warmup_image_model()
        return await run_in_threadpool(start)
    except ImageRequestError as e:
        return JSONResponse({"ok": False, "error": e.message}, status_code=e.status_code)
    except Exception as e:
        return JSONResponse({"ok": False, "error": str(e)}, status_code=500)


@app.get("/image-files/{run_id}/{filename}")
def image_file(run_id: str, filename: str):
    try:
        path = _generated_image_path(run_id, filename)
    except ImageRequestError as e:
        return JSONResponse({"ok": False, "error": e.message}, status_code=e.status_code)
    return FileResponse(path, media_type="image/png")


if __name__ == "__main__":
    import uvicorn
    uvicorn.run(app, host=HOST, port=PORT, log_level="warning")
