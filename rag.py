from __future__ import annotations

import array
import base64
import hashlib
import json
import math
import sqlite3
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import httpx

import config

_STOPWORDS = {
    "the", "and", "or", "of", "to", "in", "on", "at", "a", "an", "is", "are", "was", "were",
    "who", "what", "where", "when", "why", "how", "exactly", "about",
    "это", "или", "что", "кто", "где", "когда", "зачем", "как", "какой", "какая", "какие",
    "искать", "найти", "про", "при", "для", "над", "под", "без", "уже", "сейчас", "тут",
    "здесь", "там", "его", "её", "она", "они", "он", "мне", "тебе", "меня", "тебя",
}


@dataclass(frozen=True)
class RagDocument:
    doc_id: str
    kind: str
    text: str
    status: str = "known"
    source: str = ""
    visibility: str = "player"
    tags: tuple[str, ...] = field(default_factory=tuple)
    metadata: dict[str, Any] = field(default_factory=dict)

    def contextual_text(self) -> str:
        meta = [
            "RPG world memory block.",
            f"Kind: {self.kind}.",
            f"Status: {self.status}.",
        ]
        if self.source:
            meta.append(f"Source: {self.source}.")
        if self.tags:
            meta.append("Tags: " + ", ".join(self.tags) + ".")
        return "\n".join(meta) + "\nText: " + self.text.strip()


@dataclass(frozen=True)
class RagHit:
    document: RagDocument
    score: float
    dense_score: float
    keyword_score: float


def _tokens(text: str) -> list[str]:
    words = []
    for raw in __import__("re").findall(r"[a-zA-Zа-яА-ЯёЁ0-9_«»\-]+", (text or "").lower()):
        word = raw.strip("«»-.").lower()
        if len(word) >= 3 and word not in _STOPWORDS:
            words.append(word)
    return words


def _sha(text: str) -> str:
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def _normalize(vec: list[float]) -> list[float]:
    norm = math.sqrt(sum(v * v for v in vec))
    if not norm:
        return vec
    return [v / norm for v in vec]


def _decode_embedding(value: Any) -> list[float]:
    if isinstance(value, list):
        return _normalize([float(v) for v in value])
    if not isinstance(value, str):
        raise ValueError("embedding must be a base64 string or float array")
    raw = base64.b64decode(value)
    floats = array.array("f")
    floats.frombytes(raw)
    return _normalize([float(v) for v in floats])


def _encode_vector(vec: list[float]) -> str:
    floats = array.array("f", vec)
    return base64.b64encode(floats.tobytes()).decode("ascii")


class EmbeddingCache:
    def __init__(self, path: str):
        self.path = str(Path(path))
        self._init_db()

    def _connect(self) -> sqlite3.Connection:
        con = sqlite3.connect(self.path, timeout=10)
        con.execute("PRAGMA journal_mode = WAL")
        con.execute("PRAGMA synchronous = NORMAL")
        return con

    def _init_db(self) -> None:
        parent = Path(self.path).parent
        parent.mkdir(parents=True, exist_ok=True)
        with self._connect() as con:
            con.execute(
                """
                CREATE TABLE IF NOT EXISTS embeddings (
                    model TEXT NOT NULL,
                    text_hash TEXT NOT NULL,
                    text TEXT NOT NULL,
                    dims INTEGER NOT NULL,
                    vector_b64 TEXT NOT NULL,
                    created_at REAL NOT NULL,
                    PRIMARY KEY (model, text_hash)
                )
                """
            )

    def get_many(self, model: str, texts: list[str]) -> dict[str, list[float]]:
        hashes = [_sha(text) for text in texts]
        if not hashes:
            return {}
        placeholders = ",".join("?" for _ in hashes)
        params = [model, *hashes]
        found: dict[str, list[float]] = {}
        with self._connect() as con:
            for text_hash, vector_b64 in con.execute(
                f"""
                SELECT text_hash, vector_b64 FROM embeddings
                WHERE model = ? AND text_hash IN ({placeholders})
                """,
                params,
            ):
                found[str(text_hash)] = _decode_embedding(str(vector_b64))
        return found

    def put_many(self, model: str, rows: list[tuple[str, list[float]]]) -> None:
        if not rows:
            return
        now = time.time()
        payload = [
            (model, _sha(text), text, len(vec), _encode_vector(vec), now)
            for text, vec in rows
        ]
        with self._connect() as con:
            con.executemany(
                """
                INSERT OR REPLACE INTO embeddings
                    (model, text_hash, text, dims, vector_b64, created_at)
                VALUES (?, ?, ?, ?, ?, ?)
                """,
                payload,
            )


class LocalEmbeddingClient:
    def __init__(self):
        self.url = config.RAG_EMBEDDINGS_URL
        self.model = config.RAG_EMBEDDINGS_MODEL
        self.encoding_format = config.RAG_ENCODING_FORMAT
        self.batch_size = max(1, int(config.RAG_BATCH_SIZE))
        self.timeout = float(config.RAG_TIMEOUT_SECONDS)
        self.cache = EmbeddingCache(config.RAG_CACHE_PATH)

    def embed(self, texts: list[str]) -> list[list[float]]:
        normalized_texts = [text.strip() for text in texts]
        cached = self.cache.get_many(self.model, normalized_texts)
        out: dict[str, list[float]] = {}
        missing: list[str] = []
        for text in normalized_texts:
            text_hash = _sha(text)
            if text_hash in cached:
                out[text_hash] = cached[text_hash]
            else:
                missing.append(text)

        for start in range(0, len(missing), self.batch_size):
            batch = missing[start:start + self.batch_size]
            payload = {
                "model": self.model,
                "input": batch,
                "encoding_format": self.encoding_format,
            }
            response = httpx.post(self.url, json=payload, timeout=self.timeout)
            response.raise_for_status()
            data = response.json().get("data") or []
            vectors_by_index = {
                int(item.get("index", idx)): _decode_embedding(item.get("embedding"))
                for idx, item in enumerate(data)
            }
            rows = []
            for idx, text in enumerate(batch):
                vec = vectors_by_index[idx]
                out[_sha(text)] = vec
                rows.append((text, vec))
            self.cache.put_many(self.model, rows)

        return [out[_sha(text)] for text in normalized_texts]


class HashEmbeddingClient:
    """Deterministic tiny embedder for tests and offline fallback checks."""

    def __init__(self, dims: int = 128):
        self.dims = dims

    def embed(self, texts: list[str]) -> list[list[float]]:
        vectors = []
        for text in texts:
            vec = [0.0] * self.dims
            for token in _tokens(text):
                digest = hashlib.blake2b(token.encode("utf-8"), digest_size=8).digest()
                idx = int.from_bytes(digest[:4], "little") % self.dims
                sign = 1.0 if digest[4] % 2 == 0 else -1.0
                vec[idx] += sign
            vectors.append(_normalize(vec))
        return vectors


class RagEngine:
    def __init__(self, embedder=None):
        self.embedder = embedder or LocalEmbeddingClient()

    def search(self, query: str, documents: list[RagDocument],
               top_k: int | None = None) -> list[RagHit]:
        docs = [doc for doc in documents if doc.text.strip()]
        if not query.strip() or not docs:
            return []
        top_k = top_k or config.RAG_TOP_K

        query_text = _query_instruction(query)
        vectors = self.embedder.embed([query_text] + [doc.contextual_text() for doc in docs])
        query_vec, doc_vecs = vectors[0], vectors[1:]
        dense_scores = [
            sum(a * b for a, b in zip(query_vec, doc_vec))
            for doc_vec in doc_vecs
        ]
        keyword_scores = _bm25_scores(query, docs)
        max_keyword = max(keyword_scores) if keyword_scores else 0.0

        dense_rank = _rank_map(dense_scores)
        keyword_rank = _rank_map(keyword_scores)
        hits = []
        for idx, doc in enumerate(docs):
            final = 0.0
            if idx in dense_rank:
                final += 1.0 / (config.RAG_RRF_K + dense_rank[idx])
            if idx in keyword_rank:
                final += 1.0 / (config.RAG_RRF_K + keyword_rank[idx])
            if max_keyword > 0:
                final += config.RAG_KEYWORD_TIEBREAK * (keyword_scores[idx] / max_keyword)
            final += config.RAG_DENSE_TIEBREAK * max(0.0, dense_scores[idx])
            if doc.status in _GOOD_STATUS:
                final *= config.RAG_STATUS_BOOST
            hits.append(RagHit(doc, final, dense_scores[idx], keyword_scores[idx]))

        hits.sort(key=lambda hit: (hit.score, hit.dense_score, hit.keyword_score), reverse=True)
        filtered = [
            hit for hit in hits
            if hit.keyword_score > 0 or hit.dense_score >= config.RAG_MIN_DENSE_SCORE
        ]
        return filtered[:top_k]


def _query_instruction(query: str) -> str:
    return (
        "Instruct: Given a game master's query, retrieve relevant public world facts, "
        "current scene facts, known NPC whereabouts, evidence, and unconfirmed witness "
        "statements for a tabletop RPG. Do not retrieve hidden canon or private secrets.\n"
        "Query: " + query.strip()
    )


def _rank_map(scores: list[float]) -> dict[int, int]:
    ranked = sorted(
        [(idx, score) for idx, score in enumerate(scores) if score > 0],
        key=lambda row: row[1],
        reverse=True,
    )
    return {idx: rank + 1 for rank, (idx, _score) in enumerate(ranked)}


def _bm25_scores(query: str, documents: list[RagDocument]) -> list[float]:
    query_terms = _tokens(query)
    if not query_terms:
        return [0.0] * len(documents)
    doc_terms = [_tokens(doc.contextual_text()) for doc in documents]
    avgdl = sum(len(terms) for terms in doc_terms) / max(1, len(doc_terms))
    dfs: dict[str, int] = {}
    for terms in doc_terms:
        for term in set(terms):
            dfs[term] = dfs.get(term, 0) + 1
    n_docs = len(documents)
    k1, b = 1.5, 0.75
    scores = []
    for terms in doc_terms:
        counts: dict[str, int] = {}
        for term in terms:
            counts[term] = counts.get(term, 0) + 1
        dl = len(terms) or 1
        score = 0.0
        for term in query_terms:
            tf = counts.get(term, 0)
            if not tf:
                continue
            df = dfs.get(term, 0)
            idf = math.log(1 + (n_docs - df + 0.5) / (df + 0.5))
            denom = tf + k1 * (1 - b + b * dl / max(avgdl, 1))
            score += idf * (tf * (k1 + 1)) / denom
        scores.append(score)
    return scores


# Statuses that count as established/"good" for ranking and fact labeling.
_GOOD_STATUS = ("known", "current", "present")

_DEFAULT_ENGINE: RagEngine | None = None


def default_engine() -> RagEngine:
    global _DEFAULT_ENGINE
    if _DEFAULT_ENGINE is None:
        _DEFAULT_ENGINE = RagEngine()
    return _DEFAULT_ENGINE


def set_default_engine(engine: RagEngine | None) -> None:
    global _DEFAULT_ENGINE
    _DEFAULT_ENGINE = engine


def retrieve_world_fact(query: str, documents: list[RagDocument]) -> dict | None:
    if not config.RAG_ENABLED:
        return None
    hits = default_engine().search(query, documents, config.RAG_TOP_K)
    if not hits:
        return None

    selected = hits[:config.RAG_FACT_SELECT_K]
    if not selected:
        return None

    status = (
        "known"
        if all(hit.document.status in _GOOD_STATUS for hit in selected)
        else "unknown"
    )
    lines = []
    sources = []
    for idx, hit in enumerate(selected, start=1):
        doc = hit.document
        label = "known" if doc.status in _GOOD_STATUS else "unconfirmed"
        lines.append(f"[{idx}] {label}: {doc.text}")
        sources.append({
            "n": idx,
            "doc_id": doc.doc_id,
            "kind": doc.kind,
            "status": doc.status,
            "source": doc.source,
            "score": round(hit.score, 5),
            "dense": round(hit.dense_score, 5),
            "keyword": round(hit.keyword_score, 5),
            "metadata": doc.metadata,
        })
    return {
        "status": status,
        "text": " ".join(lines),
        "sources": sources,
    }


def docs_debug_json(documents: list[RagDocument]) -> str:
    return json.dumps([doc.__dict__ for doc in documents], ensure_ascii=False, indent=2)
