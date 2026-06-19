"""Codex ChatGPT OAuth credential storage and refresh.

This follows the current Codex browser OAuth flow from the official Codex
client, but keeps storage local to GM-Lab instead of reusing Codex CLI files.
Only non-secret status is exposed to the UI.
"""
from __future__ import annotations

from dataclasses import dataclass
from http.server import BaseHTTPRequestHandler, HTTPServer
import base64
import hashlib
import json
import os
from pathlib import Path
import secrets
import time
from typing import Any
from urllib.parse import parse_qs, urlencode, urlparse
import webbrowser

import config


ISSUER = "https://auth.openai.com"
REVOKE_ENDPOINT = f"{ISSUER}/oauth/revoke"
# Client id lives in config.CODEX_CLIENT_ID (env-overridable) — used directly below.
SCOPE = "openid profile email offline_access api.connectors.read api.connectors.invoke"
DEFAULT_AUTH_PORT = 1455
FALLBACK_AUTH_PORT = 1457
TOKEN_TIMEOUT_SECS = 30.0
OAUTH_TIMEOUT_SECS = 300.0
REFRESH_MARGIN_MS = 5 * 60 * 1000


@dataclass
class CodexCredential:
    access_token: str
    refresh_token: str
    id_token: str | None = None
    expires_at: int | None = None
    account_id: str | None = None
    credential_type: str = "openai_codex_oauth"

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "CodexCredential":
        return cls(
            access_token=str(data.get("access_token") or ""),
            refresh_token=str(data.get("refresh_token") or ""),
            id_token=data.get("id_token") if data.get("id_token") else None,
            expires_at=_int_or_none(data.get("expires_at")),
            account_id=data.get("account_id") if data.get("account_id") else None,
            credential_type=str(data.get("type") or data.get("credential_type") or "openai_codex_oauth"),
        )

    def to_dict(self) -> dict[str, Any]:
        return {
            "type": self.credential_type,
            "access_token": self.access_token,
            "refresh_token": self.refresh_token,
            "id_token": self.id_token,
            "expires_at": self.expires_at,
            "account_id": self.account_id,
        }


def credential_path() -> Path:
    override = os.environ.get("GM_CODEX_CREDENTIAL_PATH", "").strip()
    if override:
        return Path(override).expanduser()
    appdata = os.environ.get("APPDATA", "").strip()
    if appdata:
        return Path(appdata) / "gm-lab" / "codex-oauth.json"
    return Path.home() / ".config" / "gm-lab" / "codex-oauth.json"


def load_credential() -> CodexCredential | None:
    path = credential_path()
    if not path.exists():
        return None
    with path.open("r", encoding="utf-8") as f:
        data = json.load(f)
    if not isinstance(data, dict):
        raise ValueError("Codex credential file is not a JSON object")
    credential = CodexCredential.from_dict(data)
    if not credential.access_token:
        raise ValueError("Codex credential file has no access_token")
    return credential


def save_credential(credential: CodexCredential) -> None:
    path = credential_path()
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8") as f:
        json.dump(credential.to_dict(), f, ensure_ascii=False, indent=2)


def delete_credential() -> None:
    try:
        credential_path().unlink()
    except FileNotFoundError:
        pass


def auth_status() -> dict[str, Any]:
    try:
        credential = load_credential()
    except Exception as exc:
        return {
            "authenticated": False,
            "account_id": None,
            "expires_at": None,
            "message": f"Codex OAuth credential is invalid: {exc}",
        }
    if credential is None:
        return {
            "authenticated": False,
            "account_id": None,
            "expires_at": None,
            "message": "Codex OAuth не авторизован",
        }
    return {
        "authenticated": True,
        "account_id": credential.account_id,
        "expires_at": credential.expires_at,
        "message": "Codex OAuth авторизован",
    }


def ensure_fresh_credential(http=None) -> CodexCredential:
    credential = load_credential()
    if credential is None:
        raise RuntimeError("Codex OAuth не авторизован. Подключи Codex в интерфейсе.")
    if _is_near_expiry(credential):
        if not credential.refresh_token.strip():
            raise RuntimeError("Codex OAuth token expired; reconnect Codex.")
        credential = refresh_credential(credential, http=http)
        save_credential(credential)
    return credential


def refresh_credential(credential: CodexCredential, http=None) -> CodexCredential:
    data = _post_token(
        {
            "grant_type": "refresh_token",
            "refresh_token": credential.refresh_token,
            "client_id": config.CODEX_CLIENT_ID,
        },
        http=http,
    )
    access_token = str(data.get("access_token") or "")
    if not access_token:
        raise RuntimeError("Codex OAuth refresh response has no access_token")
    refresh_token = str(data.get("refresh_token") or credential.refresh_token)
    id_token = data.get("id_token") or credential.id_token
    next_credential = CodexCredential(
        access_token=access_token,
        refresh_token=refresh_token,
        id_token=id_token,
        expires_at=_expires_at(data.get("expires_in")),
        account_id=_account_id_from_tokens(id_token, access_token) or credential.account_id,
    )
    return next_credential


def run_oauth(http=None) -> CodexCredential:
    pkce_verifier = _random_url_token(32)
    code_challenge = _code_challenge(pkce_verifier)
    state = _random_url_token(32)
    server, port = _bind_callback_server()
    redirect_uri = f"http://localhost:{port}/auth/callback"
    auth_url = _authorize_url(redirect_uri, code_challenge, state)

    if config.CODEX_AUTO_OPEN_BROWSER:
        webbrowser.open(auth_url)

    server.timeout = OAUTH_TIMEOUT_SECS
    server.handle_request()
    callback = getattr(server, "callback", None)
    callback_error = getattr(server, "callback_error", None)
    server.server_close()

    if callback_error:
        raise RuntimeError(callback_error)
    if not callback:
        raise RuntimeError("Timed out waiting for Codex OAuth callback")
    if callback["state"] != state:
        raise RuntimeError("OAuth state mismatch")

    data = _post_token(
        {
            "grant_type": "authorization_code",
            "code": callback["code"],
            "redirect_uri": redirect_uri,
            "client_id": config.CODEX_CLIENT_ID,
            "code_verifier": pkce_verifier,
        },
        http=http,
    )
    access_token = str(data.get("access_token") or "")
    refresh_token = str(data.get("refresh_token") or "")
    if not access_token:
        raise RuntimeError("Codex OAuth token response has no access_token")

    id_token = data.get("id_token")
    credential = CodexCredential(
        access_token=access_token,
        refresh_token=refresh_token,
        id_token=id_token,
        expires_at=_expires_at(data.get("expires_in")),
        account_id=_account_id_from_tokens(id_token, access_token),
    )
    save_credential(credential)
    return credential


def revoke_credential(http=None) -> None:
    credential = load_credential()
    if credential is None:
        return
    token = credential.refresh_token.strip() or credential.access_token.strip()
    if not token:
        delete_credential()
        return
    token_type = "refresh_token" if credential.refresh_token.strip() else "access_token"
    payload: dict[str, Any] = {"token": token, "token_type_hint": token_type}
    if token_type == "refresh_token":
        payload["client_id"] = config.CODEX_CLIENT_ID
    try:
        client = http or _http_client()
        client.post(REVOKE_ENDPOINT, json=payload, timeout=10.0)
    finally:
        delete_credential()


def _bind_callback_server() -> tuple[HTTPServer, int]:
    preferred = int(config.CODEX_AUTH_PORT or DEFAULT_AUTH_PORT)
    ports = [preferred]
    if preferred != FALLBACK_AUTH_PORT:
        ports.append(FALLBACK_AUTH_PORT)
    last_error: Exception | None = None
    for port in ports:
        try:
            server = HTTPServer(("127.0.0.1", port), _CallbackHandler)
            return server, port
        except OSError as exc:
            last_error = exc
    raise RuntimeError(f"Cannot bind Codex OAuth callback server: {last_error}")


class _CallbackHandler(BaseHTTPRequestHandler):
    def log_message(self, *args) -> None:
        pass

    def do_GET(self) -> None:
        parsed = urlparse(self.path)
        ok = False
        try:
            if parsed.path != "/auth/callback":
                raise ValueError("Unexpected OAuth callback path")
            params = parse_qs(parsed.query)
            if params.get("error"):
                raise ValueError("Codex OAuth rejected authorization")
            code = (params.get("code") or [""])[0].strip()
            state = (params.get("state") or [""])[0].strip()
            if not code:
                raise ValueError("OAuth callback has no code")
            if not state:
                raise ValueError("OAuth callback has no state")
            self.server.callback = {"code": code, "state": state}
            ok = True
        except Exception as exc:
            self.server.callback_error = str(exc)

        body = (
            "<!doctype html><meta charset=utf-8><title>GM-Lab Codex</title>"
            "<body style=\"font-family:system-ui;background:#14161c;color:#e7e9ef\">"
            "<p>Codex авторизован. Можно закрыть вкладку и вернуться в GM-Lab.</p>"
            if ok
            else "<!doctype html><meta charset=utf-8><title>GM-Lab Codex</title>"
                 "<body style=\"font-family:system-ui;background:#14161c;color:#e7e9ef\">"
                 "<p>Авторизация Codex не удалась. Вернись в GM-Lab и попробуй снова.</p>"
        )
        encoded = body.encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(encoded)))
        self.end_headers()
        self.wfile.write(encoded)


def _authorize_url(redirect_uri: str, code_challenge: str, state: str) -> str:
    params = {
        "response_type": "code",
        "client_id": config.CODEX_CLIENT_ID,
        "redirect_uri": redirect_uri,
        "scope": SCOPE,
        "code_challenge": code_challenge,
        "code_challenge_method": "S256",
        "id_token_add_organizations": "true",
        "codex_cli_simplified_flow": "true",
        "state": state,
        "originator": config.CODEX_ORIGINATOR,
    }
    return f"{ISSUER}/oauth/authorize?{urlencode(params)}"


def _post_token(form: dict[str, str], http=None) -> dict[str, Any]:
    client = http or _http_client()
    response = client.post(f"{ISSUER}/oauth/token", data=form, timeout=TOKEN_TIMEOUT_SECS)
    if not response.is_success:
        raise RuntimeError(f"Codex OAuth token endpoint failed with status {response.status_code}")
    data = response.json()
    if not isinstance(data, dict):
        raise RuntimeError("Codex OAuth token endpoint returned non-object JSON")
    return data


def _http_client():
    import httpx

    return httpx.Client(timeout=httpx.Timeout(connect=10.0, read=TOKEN_TIMEOUT_SECS, write=30.0, pool=None))


def _random_url_token(byte_len: int) -> str:
    return base64.urlsafe_b64encode(secrets.token_bytes(byte_len)).decode("ascii").rstrip("=")


def _code_challenge(verifier: str) -> str:
    digest = hashlib.sha256(verifier.encode("ascii")).digest()
    return base64.urlsafe_b64encode(digest).decode("ascii").rstrip("=")


def _decode_jwt_claims(token: str | None) -> dict[str, Any] | None:
    if not token:
        return None
    parts = token.split(".")
    if len(parts) < 2:
        return None
    payload = parts[1]
    payload += "=" * (-len(payload) % 4)
    try:
        raw = base64.urlsafe_b64decode(payload.encode("ascii"))
        data = json.loads(raw.decode("utf-8"))
    except Exception:
        return None
    return data if isinstance(data, dict) else None


def _account_id_from_tokens(id_token: str | None, access_token: str | None) -> str | None:
    for token in (id_token, access_token):
        claims = _decode_jwt_claims(token)
        if not claims:
            continue
        account_id = (
            claims.get("chatgpt_account_id")
            or (claims.get("https://api.openai.com/auth") or {}).get("chatgpt_account_id")
            or claims.get("https://api.openai.com/auth.chatgpt_account_id")
        )
        if isinstance(account_id, str) and account_id.strip():
            return account_id.strip()
    return None


def _expires_at(expires_in: Any) -> int | None:
    seconds = _int_or_none(expires_in)
    if seconds is None:
        return None
    return int(time.time() * 1000) + seconds * 1000


def _is_near_expiry(credential: CodexCredential) -> bool:
    if credential.expires_at is None:
        return False
    return credential.expires_at <= int(time.time() * 1000) + REFRESH_MARGIN_MS


def _int_or_none(value: Any) -> int | None:
    try:
        if value is None or value == "":
            return None
        return int(value)
    except (TypeError, ValueError):
        return None
