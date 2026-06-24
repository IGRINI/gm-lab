"""
gmlab unified inference sidecar — ONE Python process hosting embedder + reranker + TTS.

Runs on the Python 3.12 env at E:\\gemma\\rag312
  (torch 2.7 cu128 + flash-attn 2.7.4 + transformers 4.57.3 + sentence-transformers + qwen-tts).

Endpoints (drop-in for the Rust gml-rag client + gml-audio TTS proxy):
  POST /v1/embeddings   OpenAI-compatible  {input,[model],[encoding_format],[input_type],[task]} -> {data:[{embedding}]}
  POST /rerank          {query,documents,[top_n],[return_documents]} -> {results:[{index,relevance_score,[document]}]}
  POST /speak           {text, voice:"gm|male|female"} -> audio/wav
  POST /speak_stream    {text, voice} -> raw PCM16 mono stream + X-Sample-Rate header
  GET  /health          -> {status, per-model up flags, voices, embed dim}

Per-model config from env (the Rust app passes these from gml-config):
  EMBEDDER_MODEL   (default Qwen/Qwen3-Embedding-0.6B)   EMBEDDER_QUANT = bf16 | nf4   EMBEDDER_ENABLED=1
  RERANKER_MODEL   (default jinaai/jina-reranker-v3)     RERANKER_QUANT = bf16 | nf4   RERANKER_ENABLED=1
  TTS_ENABLED=1    TTS_MODEL_ID (override) TTS_HOME (refs + qwen17b_base dir) TTS_LANG (default Russian)
  USE_FLASH=1  JINA_COMPILE=1  JINA_MAX_DOC_LEN=2048  JINA_MAX_QUERY_LEN=512
  RERANK_TOP_N         default # of results /rerank returns when the request omits top_n (0/empty = all)
  RERANK_RETURN_DOCS=0 default for echoing document text back (retriever owns the corpus -> off)
  EMBED_QUERY_TASK     English instruction used for query-side embedding (Qwen3 wants it in English)
  GMLAB_SIDECAR_HOST=127.0.0.1  GMLAB_SIDECAR_PORT=8077
  HF_HOME / HF_HUB_CACHE (default the faster-qwen3-tts cache on E:)

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

INTEGRATION STATUS (see sidecar/README.md for the full doc) — all three WIRED & LIVE:
  - /v1/embeddings — gml-rag client; /speak,/speak_stream — gml-audio. The Rust app spawns this
    process (gml-audio::Sidecar) at startup; HF_HOME / GMLAB_SIDECAR_PORT / EMBEDDER_QUANT /
    RERANKER_QUANT / *_ENABLED are passed via the spawn env.
  - /rerank — gml-rag/src/engine.rs POSTs the fused top-N (GM_RAG_RERANK_CANDIDATES, default 64)
    candidate texts after dense+BM25+RRF, then reorders the final rag_top_k (default 4) by the
    returned `index`; rerank errors propagate so orchestrator can report degraded retrieval.
    GM_RAG_RERANK_ENABLED toggles it.
"""
import base64
import io
import os
import threading
import time
from contextlib import asynccontextmanager
from pathlib import Path

# --- HF cache: default to the faster-qwen3-tts cache on E: (where TTS weights live).
#     MUST be set before importing torch / faster_qwen3_tts. Env wins.
_DEFAULT_TTS_HOME = r"E:/gemma/gm-lab/hf_models/faster-qwen3-tts"
_DEFAULT_HF_HOME = r"E:/gemma/gm-lab/hf_models/.hf-home"
os.environ.setdefault("HF_HOME", _DEFAULT_HF_HOME)
os.environ.setdefault("HF_HUB_CACHE", str(Path(os.environ["HF_HOME"]) / "hub"))

# --- Self-logging: the Rust spawner runs us with stdout/stderr -> null, so a
#     failed/slow boot would be invisible. Tee everything to a file too. Set up
#     BEFORE the heavy imports so an import error (torch/cuda/flash-attn) is
#     captured. Override the path with GMLAB_SIDECAR_LOG.
import sys
_LOG_PATH = os.environ.get("GMLAB_SIDECAR_LOG") or str(Path(__file__).with_name("serve.log"))


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
from fastapi import FastAPI
from fastapi.responses import Response, StreamingResponse, JSONResponse
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
TTS_ENABLED = os.environ.get("TTS_ENABLED", "1") == "1"
USE_FLASH = os.environ.get("USE_FLASH", "1") == "1"
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

TTS_HOME = Path(os.environ.get("TTS_HOME", _DEFAULT_TTS_HOME))
TTS_LANG = os.environ.get("TTS_LANG", "Russian")
# Model: env override > local 1.7B-Base (qwen17b_base/) if weights present > cached 0.6B-Base.
_LOCAL_17B = TTS_HOME / "qwen17b_base"
TTS_MODEL_ID = (
    os.environ.get("TTS_MODEL_ID")
    or (str(_LOCAL_17B) if (_LOCAL_17B / "model.safetensors").exists()
        else "Qwen/Qwen3-TTS-12Hz-0.6B-Base")
)

# female reference text — MUST match female_ref_deepwarm.wav exactly (ICL clone).
FEMALE_REF_TEXT = (
    "Над рекой медленно поднимался густой туман. Где-то в чаще ухнула сова, "
    "и ветер качнул верхушки сосен. Я шла по тропе, прислушиваясь к шороху "
    "листьев под ногами."
)
# Three voices, all clones on one model (prompt cached per reference at warmup):
#   gm     -> ref_audio_2.wav         (x-vector clone)  narrator
#   male   -> ref_audio.wav           (x-vector clone)  male characters
#   female -> female_ref_deepwarm.wav (ICL, exact text) female characters
VOICES = {
    "gm":     dict(ref_audio=str(TTS_HOME / "ref_audio_2.wav"),          ref_text="",              xvec_only=True),
    "male":   dict(ref_audio=str(TTS_HOME / "ref_audio.wav"),           ref_text="",              xvec_only=True),
    "female": dict(ref_audio=str(TTS_HOME / "female_ref_deepwarm.wav"), ref_text=FEMALE_REF_TEXT, xvec_only=False),
}
SAMPLING = dict(temperature=0.75, top_k=50, top_p=1.0, repetition_penalty=1.1)

STATE = {"embedder": None, "reranker": None, "tts": None}
# The single GPU is shared; these models are not guaranteed re-entrant and the
# handlers are sync `def` (Starlette runs them in a threadpool), so serialize
# each model's inference with its own lock. (GIL + one GPU => no real parallel
# throughput anyway; rely on the models' internal batching.)
_embed_lock = threading.Lock()
_rerank_lock = threading.Lock()
_tts_lock = threading.Lock()
# Bounded wait so a stuck / leaked TTS generation degrades to 503 instead of an
# unbounded hang (e.g. if a streamed response's generator frame is abandoned on
# client disconnect and its lock release is deferred).
_TTS_LOCK_TIMEOUT = float(os.environ.get("TTS_LOCK_TIMEOUT", "180"))


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


def _load_embedder():
    from sentence_transformers import SentenceTransformer
    mk = _quant_kwargs(EMBEDDER_QUANT)
    m = SentenceTransformer(EMBEDDER_MODEL, model_kwargs=mk, device="cuda")
    print(f"[sidecar] embedder {EMBEDDER_MODEL} quant={EMBEDDER_QUANT} loaded", flush=True)
    return m


def _load_reranker():
    from transformers import AutoModel
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


def _load_tts():
    from faster_qwen3_tts import FasterQwen3TTS
    print(f"[sidecar] loading TTS {TTS_MODEL_ID} ...", flush=True)
    m = FasterQwen3TTS.from_pretrained(TTS_MODEL_ID, device="cuda", dtype=torch.bfloat16)
    m._warmup(prefill_len=100)  # capture CUDA graphs
    # Prime each voice's reference-prompt cache (and surface missing refs early).
    for name, cfg in VOICES.items():
        if not Path(cfg["ref_audio"]).exists():
            print(f"[sidecar] WARNING missing ref for '{name}': {cfg['ref_audio']}", flush=True)
            continue
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
    if TTS_ENABLED:
        try:
            STATE["tts"] = _load_tts()
        except Exception as e:
            print(f"[sidecar] TTS load FAILED (/speak disabled): {e}", flush=True)
    _warmup_text_models()
    up = [k for k, v in STATE.items() if v is not None]
    print(f"[sidecar] ready in {time.time()-t0:.1f}s on {HOST}:{PORT} — up: {up}", flush=True)
    yield
    STATE.clear()


app = FastAPI(title="gmlab-sidecar", lifespan=lifespan)


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
        "tts": {"up": STATE["tts"] is not None, "model": TTS_MODEL_ID,
                "voices": list(VOICES) if STATE["tts"] is not None else []},
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


if __name__ == "__main__":
    import uvicorn
    uvicorn.run(app, host=HOST, port=PORT, log_level="warning")
