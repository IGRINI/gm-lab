"""Codex ChatGPT speech-to-text (dictation) over the subscription OAuth token.

Posts audio to the ChatGPT backend `/backend-api/transcribe` route — the same
undocumented endpoint Codex Desktop uses for its spacebar dictation —
authenticated with the Codex *subscription* OAuth access token (see
`codex_oauth`). The endpoint rejects OpenAI Platform API keys.

Cloudflare fronts this path with bot mitigation that flags ordinary Python TLS
(httpx/requests) and returns a 403 challenge page (`cf-mitigated: challenge`).
The block is largely TLS-fingerprint based, so we send the request through
`curl_cffi` impersonating a real browser's TLS/HTTP2 fingerprint — which passes
the same way Codex Desktop (Chromium) does, with no actual browser. See
openai/codex issue #17860.

Wire shape: multipart/form-data with a `file` part + a `model` field; the
response is JSON `{"text": ...}`.
"""
from __future__ import annotations

import os
import uuid

import config
import codex_oauth

TRANSCRIBE_MODEL = os.environ.get("GM_CODEX_TRANSCRIBE_MODEL", "gpt-4o-mini-transcribe")
# Empty string disables the language hint (let the model auto-detect).
TRANSCRIBE_LANGUAGE = os.environ.get("GM_CODEX_TRANSCRIBE_LANGUAGE", "ru")
TRANSCRIBE_TIMEOUT_SECS = float(os.environ.get("GM_CODEX_TRANSCRIBE_TIMEOUT", "120"))
# Browser TLS fingerprint to impersonate (curl_cffi target). Chrome passes CF.
TRANSCRIBE_IMPERSONATE = os.environ.get("GM_CODEX_TRANSCRIBE_IMPERSONATE", "chrome")


class TranscribeError(RuntimeError):
    def __init__(self, message: str, status: int | None = None):
        super().__init__(message)
        self.status = status


def transcribe_url() -> str:
    override = (os.environ.get("GM_CODEX_TRANSCRIBE_URL") or "").strip()
    if override:
        return override
    base = config.CODEX_BASE_URL.rstrip("/")
    # CODEX_BASE_URL is .../backend-api/codex; the transcribe route sits directly
    # under .../backend-api (NOT under /codex — that path 404s).
    if base.endswith("/codex"):
        base = base[: -len("/codex")]
    return base + "/transcribe"


def _filename_for(content_type: str) -> str:
    ct = (content_type or "").lower()
    if "webm" in ct:
        return "audio.webm"
    if "ogg" in ct:
        return "audio.ogg"
    if "mp4" in ct or "m4a" in ct or "aac" in ct:
        return "audio.mp4"
    if "mpeg" in ct or "mp3" in ct:
        return "audio.mp3"
    if "wav" in ct or "wave" in ct or "pcm" in ct:
        return "audio.wav"
    return "audio.webm"


def transcribe(audio: bytes, content_type: str = "audio/webm", filename: str | None = None) -> str:
    if not audio:
        raise TranscribeError("empty audio")

    try:
        credential = codex_oauth.ensure_fresh_credential()
    except Exception as ex:  # not authorized / refresh failed
        raise TranscribeError(str(ex)) from ex

    try:
        from curl_cffi import requests as cffi
        from curl_cffi import CurlMime
    except Exception as ex:  # dependency missing
        raise TranscribeError(
            "curl_cffi не установлен (нужен для обхода Cloudflare на /transcribe): "
            f"{ex}"
        ) from ex

    # NOTE: do NOT set User-Agent here — curl_cffi's impersonate profile sets a
    # browser UA + client hints that must match its TLS fingerprint. A codex CLI
    # UA over a Chrome TLS fingerprint looks inconsistent and can re-trigger CF.
    headers = {
        "Authorization": "Bearer " + credential.access_token.strip(),
        "originator": config.CODEX_ORIGINATOR,
        "version": config.CODEX_CLIENT_VERSION,
        "session-id": str(uuid.uuid4()),
        "thread-id": str(uuid.uuid4()),
        "x-codex-installation-id": str(uuid.uuid4()),
    }
    if credential.account_id:
        headers["ChatGPT-Account-Id"] = credential.account_id

    mime = CurlMime()
    mime.addpart(
        name="file",
        filename=filename or _filename_for(content_type),
        content_type=content_type or "audio/webm",
        data=audio,
    )
    mime.addpart(name="model", data=TRANSCRIBE_MODEL.encode("utf-8"))
    if TRANSCRIBE_LANGUAGE:
        mime.addpart(name="language", data=TRANSCRIBE_LANGUAGE.encode("utf-8"))

    try:
        with cffi.Session(impersonate=TRANSCRIBE_IMPERSONATE) as session:
            response = session.post(
                transcribe_url(),
                headers=headers,
                multipart=mime,
                timeout=TRANSCRIBE_TIMEOUT_SECS,
            )
    except Exception as ex:
        raise TranscribeError(f"transcribe request failed: {ex}") from ex
    finally:
        try:
            mime.close()
        except Exception:
            pass

    status = response.status_code
    if response.headers.get("cf-mitigated"):
        raise TranscribeError(
            "Cloudflare заблокировал транскрипцию (challenge) — TLS-обход не прошёл",
            status=status,
        )
    if not (200 <= status < 300):
        body = (response.text or "").replace("\n", " ").strip()
        if len(body) > 400:
            body = body[:400] + "…"
        raise TranscribeError(f"transcribe HTTP {status}: {body}", status=status)

    try:
        payload = response.json()
    except Exception:
        text = (response.text or "").strip()
        if text:
            return text
        raise TranscribeError("transcribe returned no JSON and no text")

    if isinstance(payload, dict):
        text = payload.get("text")
        if text is None:
            text = payload.get("transcript") or ""
        # Empty string is a valid result (e.g. silence) — return it as-is so the
        # caller can decide; non-empty is the normal case.
        if isinstance(text, str):
            return text.strip()
    raise TranscribeError("transcribe response had no text field")
