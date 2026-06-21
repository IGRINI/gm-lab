"""Codex ChatGPT speech-to-text (dictation) over the subscription OAuth token.

Posts audio to the ChatGPT backend `/backend-api/transcribe` route — the same
undocumented endpoint Codex Desktop uses for its spacebar dictation. It is
authenticated with the Codex *subscription* OAuth access token (see
`codex_oauth`); the endpoint rejects OpenAI Platform API keys. This is
undocumented and may change without notice; treat failures as expected and let
the UI retry.

Wire shape mirrors OpenAI's `/v1/audio/transcriptions`: multipart/form-data with
a `file` part and a `model` field; the response is JSON `{"text": ...}`.
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


class TranscribeError(RuntimeError):
    def __init__(self, message: str, status: int | None = None):
        super().__init__(message)
        self.status = status


def transcribe_url() -> str:
    override = (os.environ.get("GM_CODEX_TRANSCRIBE_URL") or "").strip()
    if override:
        return override
    base = config.CODEX_BASE_URL.rstrip("/")
    # CODEX_BASE_URL is .../backend-api/codex; the transcribe route is its sibling
    # under .../backend-api (no /codex segment).
    if base.endswith("/codex"):
        base = base[: -len("/codex")]
    return base + "/transcribe"


def transcribe(
    audio: bytes,
    content_type: str = "audio/webm",
    filename: str = "audio.webm",
    http=None,
) -> str:
    if not audio:
        raise TranscribeError("empty audio")

    try:
        credential = codex_oauth.ensure_fresh_credential(http)
    except Exception as ex:  # not authorized / refresh failed
        raise TranscribeError(str(ex)) from ex

    headers = {
        "Authorization": "Bearer " + credential.access_token.strip(),
        "originator": config.CODEX_ORIGINATOR,
        "User-Agent": config.CODEX_USER_AGENT,
        "version": config.CODEX_CLIENT_VERSION,
        "x-codex-installation-id": str(uuid.uuid4()),
        # Do NOT set Content-Type here — httpx fills the multipart boundary.
    }
    if credential.account_id:
        headers["ChatGPT-Account-Id"] = credential.account_id

    files = {"file": (filename or "audio.webm", audio, content_type or "audio/webm")}
    data: dict[str, str] = {"model": TRANSCRIBE_MODEL}
    if TRANSCRIBE_LANGUAGE:
        data["language"] = TRANSCRIBE_LANGUAGE

    client = http or _http_client()
    try:
        response = client.post(
            transcribe_url(),
            headers=headers,
            files=files,
            data=data,
            timeout=TRANSCRIBE_TIMEOUT_SECS,
        )
    except Exception as ex:
        raise TranscribeError(f"transcribe request failed: {ex}") from ex

    if not response.is_success:
        body = (response.text or "").replace("\n", " ").strip()
        if len(body) > 500:
            body = body[:500] + "…"
        raise TranscribeError(
            f"transcribe HTTP {response.status_code}: {body}",
            status=response.status_code,
        )

    try:
        payload = response.json()
    except Exception:
        # Some variants return text/plain.
        text = (response.text or "").strip()
        if text:
            return text
        raise TranscribeError("transcribe returned no JSON and no text")

    if isinstance(payload, dict):
        text = payload.get("text") or payload.get("transcript") or ""
        if isinstance(text, str) and text.strip():
            return text.strip()
    raise TranscribeError("transcribe response had no text")


def _http_client():
    import httpx

    return httpx.Client(
        timeout=httpx.Timeout(connect=10.0, read=TRANSCRIBE_TIMEOUT_SECS, write=60.0, pool=None)
    )
