"""Codex Responses API adapter for GM-Lab.

The rest of GM-Lab speaks a small chat-completions-like interface. This module
translates that interface to the ChatGPT Codex Responses endpoint and normalizes
streaming, tool calls, model listing, and token usage back to GM-Lab's shape.
"""
from __future__ import annotations

import copy
from dataclasses import dataclass
import json
import re
import time
from typing import Any, Iterable
import uuid

import config
import codex_oauth
import prompts
import runtime_settings


def _loads(text: str) -> dict:
    text = (text or "").strip()
    if not text:
        return {}
    try:
        data = json.loads(text)
        return data if isinstance(data, dict) else {}
    except Exception:
        match = re.search(r"\{.*\}", text, re.DOTALL)
        if not match:
            return {}
        try:
            data = json.loads(match.group(0))
            return data if isinstance(data, dict) else {}
        except Exception:
            return {}


def _clean(text: str) -> str:
    return (text or "").strip()


def _think(text: str) -> str:
    return re.sub(r"</?think>", "", text or "").strip()


def _attr(obj, name, default=None):
    if obj is None:
        return default
    if isinstance(obj, dict):
        return obj.get(name, default)
    return getattr(obj, name, default)


def _assistant_msg(content: str, raw_tool_calls: list[dict]) -> dict:
    msg = {"role": "assistant", "content": content or ""}
    if raw_tool_calls:
        msg["tool_calls"] = raw_tool_calls
    return msg


def _parse_tool_calls(raw: Iterable[dict]) -> list[dict]:
    calls = []
    for tc in raw or []:
        fn = _attr(tc, "function", {}) or {}
        name = _attr(fn, "name", "") or _attr(tc, "name", "")
        args = _attr(fn, "arguments", _attr(tc, "arguments", {}))
        if isinstance(args, str):
            args = _loads(args)
        if name:
            calls.append({"name": name, "arguments": args or {}, "id": _attr(tc, "id", "") or ""})
    return calls


def _raw_tool_calls(calls: list[dict]) -> list[dict]:
    raw = []
    for call in calls:
        name = str(call.get("name") or "").strip()
        if not name:
            continue
        args = call.get("arguments") if isinstance(call.get("arguments"), dict) else {}
        raw.append({
            "id": str(call.get("id") or ""),
            "type": "function",
            "function": {
                "name": name,
                "arguments": json.dumps(args, ensure_ascii=False),
            },
        })
    return raw


def _usage_stats(usage: dict | None, elapsed_ms: float | None = None) -> dict:
    usage = usage or {}
    prompt = int(usage.get("input_tokens") or usage.get("prompt_tokens") or 0)
    output = int(usage.get("output_tokens") or usage.get("completion_tokens") or 0)
    details = (
        usage.get("input_tokens_details")
        or usage.get("prompt_tokens_details")
        or {}
    )
    cached = int(details.get("cached_tokens", 0) or 0) if isinstance(details, dict) else 0
    elapsed_ns = int((elapsed_ms or 0) * 1e6)
    return {
        "prompt_eval_count": prompt,
        "eval_count": output,
        "cached_tokens": cached,
        "prompt_eval_duration": 0,
        "eval_duration": elapsed_ns,
        "total_duration": elapsed_ns,
        "load_duration": 0,
    }


def split_messages_for_responses(messages: list[dict]) -> tuple[str, list[dict]]:
    instructions: list[str] = []
    input_items: list[dict] = []
    for message in messages:
        role = str(_attr(message, "role", "") or "")
        content = _content_text(_attr(message, "content", ""))
        if role == "system":
            if content.strip():
                instructions.append(content.strip())
            continue
        if role == "tool":
            call_id = str(_attr(message, "tool_call_id", "") or "")
            if call_id:
                input_items.append({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": content,
                })
            continue
        if role == "assistant":
            if content.strip():
                input_items.append(_message_item("assistant", "output_text", content))
            for tool_call in _attr(message, "tool_calls", []) or []:
                item = _function_call_item(tool_call)
                if item:
                    input_items.append(item)
            continue
        if role == "user" and content.strip():
            input_items.append(_message_item("user", "input_text", content))
    return "\n\n".join(instructions), input_items


def _nullable_schema(schema: dict) -> dict:
    out = copy.deepcopy(schema)
    typ = out.get("type")
    if isinstance(typ, list):
        if "null" not in typ:
            out["type"] = typ + ["null"]
    elif isinstance(typ, str) and typ != "null":
        out["type"] = [typ, "null"]
    elif "type" not in out:
        out["anyOf"] = [copy.deepcopy(out), {"type": "null"}]

    enum = out.get("enum")
    if isinstance(enum, list) and None not in enum:
        out["enum"] = enum + [None]
    return out


def strict_schema_for_responses(schema: dict) -> dict:
    """Convert a permissive tool schema into OpenAI strict-tool JSON Schema.

    GM-Lab's source tool schemas use normal optional properties. Strict tools
    require every object property to be listed in `required`; optional values
    are represented as nullable fields for the Responses API only.
    """
    if not isinstance(schema, dict):
        return schema
    out = copy.deepcopy(schema)

    for key in ("anyOf", "oneOf", "allOf"):
        if isinstance(out.get(key), list):
            out[key] = [strict_schema_for_responses(item) for item in out[key]]

    if isinstance(out.get("items"), dict):
        out["items"] = strict_schema_for_responses(out["items"])

    props = out.get("properties")
    typ = out.get("type")
    is_object = typ == "object" or (isinstance(typ, list) and "object" in typ) or isinstance(props, dict)
    if is_object:
        out["type"] = typ or "object"
        out["additionalProperties"] = False
        if isinstance(props, dict):
            original_required = set(out.get("required") or [])
            new_props = {}
            for name, prop in props.items():
                child = strict_schema_for_responses(prop)
                if name not in original_required:
                    child = _nullable_schema(child)
                new_props[name] = child
            out["properties"] = new_props
            out["required"] = list(props.keys())
        else:
            out["properties"] = {}
            out["required"] = []
    return out


def convert_tool_for_responses(tool: dict) -> dict:
    if tool.get("type") != "function":
        return tool
    fn = tool.get("function") if isinstance(tool.get("function"), dict) else tool
    strict = bool(fn.get("strict", True))
    source_parameters = fn.get("parameters") or {}
    parameters = (
        strict_schema_for_responses(source_parameters)
        if strict
        else copy.deepcopy(source_parameters)
    )
    return {
        "type": "function",
        "name": fn.get("name", ""),
        "description": fn.get("description", ""),
        "parameters": parameters,
        "strict": strict,
    }


def extract_output_text(response: dict) -> str:
    text = []
    for item in response.get("output") or []:
        for part in item.get("content") or []:
            if part.get("type") == "output_text" and isinstance(part.get("text"), str):
                text.append(part["text"])
    if not text and isinstance(response.get("output_text"), str):
        text.append(response["output_text"])
    return "".join(text)


def extract_tool_calls(response: dict) -> list[dict]:
    calls = []
    for index, item in enumerate(response.get("output") or []):
        if item.get("type") != "function_call":
            continue
        name = str(item.get("name") or "").strip()
        if not name:
            continue
        args = _loads(str(item.get("arguments") or "{}"))
        call_id = str(item.get("call_id") or item.get("id") or f"responses_call_{index}")
        calls.append({"id": call_id, "name": name, "arguments": args})
    return calls


def _message_item(role: str, kind: str, text: str) -> dict:
    return {"type": "message", "role": role, "content": [{"type": kind, "text": text}]}


def _function_call_item(tool_call: dict) -> dict | None:
    fn = tool_call.get("function") if isinstance(tool_call.get("function"), dict) else {}
    name = str(tool_call.get("name") or fn.get("name") or "").strip()
    if not name:
        return None
    args = tool_call.get("arguments", fn.get("arguments", {}))
    if not isinstance(args, str):
        args = json.dumps(args if isinstance(args, dict) else {}, ensure_ascii=False)
    call_id = str(tool_call.get("id") or tool_call.get("call_id") or f"call_{name}")
    return {"type": "function_call", "call_id": call_id, "name": name, "arguments": args}


def _content_text(content: Any) -> str:
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        parts = []
        for item in content:
            if isinstance(item, dict) and isinstance(item.get("text"), str):
                parts.append(item["text"])
            elif isinstance(item, str):
                parts.append(item)
        return "\n".join(parts)
    if content is None:
        return ""
    return str(content)


@dataclass
class _StreamResult:
    thinking: str
    content: str
    calls: list[dict]
    usage: dict | None
    elapsed_ms: float


class CodexClient:
    def __init__(self):
        import httpx

        base = config.CODEX_BASE_URL.rstrip("/")
        self._responses_url = base + "/responses"
        self._models_url = base + "/models"
        self._http = httpx.Client(
            timeout=httpx.Timeout(connect=10.0, read=None, write=60.0, pool=None)
        )
        self._model = config.CODEX_MODEL or config.MODEL
        self._session_id = str(uuid.uuid4())
        self._thread_id = str(uuid.uuid4())
        self._installation_id = str(uuid.uuid4())
        self._turn_state = ""
        self.call_log: list[dict] = []

    @property
    def model(self) -> str:
        return self._model

    def set_model(self, model: str) -> None:
        model = (model or "").strip()
        if model:
            self._model = model

    @property
    def session_id(self) -> str:
        return self._session_id

    @property
    def thread_id(self) -> str:
        return self._thread_id

    def set_session_identity(self, session_id: str = "", thread_id: str = "") -> None:
        if (session_id or "").strip():
            self._session_id = session_id.strip()
        if (thread_id or "").strip():
            self._thread_id = thread_id.strip()

    def list_models(self) -> list[dict]:
        r = self._http.get(
            self._models_url,
            params={"client_version": config.CODEX_CLIENT_VERSION},
            headers=self._auth_headers(),
            timeout=20.0,
        )
        if not r.is_success:
            raise RuntimeError(f"Codex models endpoint failed with status {r.status_code}")
        data = r.json()
        raw_models = data.get("models") or data.get("data") or []
        models = []
        for raw in raw_models:
            if not isinstance(raw, dict):
                continue
            slug = raw.get("slug") or raw.get("id") or raw.get("model")
            if not slug:
                continue
            visibility = raw.get("visibility") or "list"
            if visibility != "list" and slug != self._model:
                continue
            models.append({
                "id": slug,
                "slug": slug,
                "name": raw.get("display_name") or slug,
                "description": raw.get("description") or "",
                "supported": bool(raw.get("supported_in_api", True)),
                "visibility": visibility,
                "priority": raw.get("priority", 0),
                "context_window": raw.get("context_window"),
                "default_reasoning_level": raw.get("default_reasoning_level"),
                "default_reasoning_summary": raw.get("default_reasoning_summary"),
                "supports_reasoning_summaries": raw.get("supports_reasoning_summaries"),
                "supported_reasoning_levels": (
                    raw.get("supported_reasoning_levels")
                    or raw.get("supported_reasoning_efforts")
                    or []
                ),
                "default_verbosity": raw.get("default_verbosity"),
                "support_verbosity": raw.get("support_verbosity"),
            })
        models.sort(key=lambda m: (not m["supported"], -int(m.get("priority") or 0), m["name"]))
        return models

    def chat(self, messages, tools=None, think=False, reasoning_role=config.ROLE_GM):
        result = self._collect_stream(
            self._payload(messages, tools=tools, think=think, reasoning_role=reasoning_role)
        )
        stats = self._remember("chat", result.usage, result.elapsed_ms)
        raw = _raw_tool_calls(result.calls)
        return (
            _think(result.thinking),
            _clean(result.content),
            _parse_tool_calls(raw),
            _assistant_msg(result.content, raw),
        )

    def chat_stream(self, messages, tools=None, think=False, reasoning_role=config.ROLE_GM):
        result_box: dict[str, Any] = {}
        for channel, text in self._stream_generator(
            self._payload(messages, tools=tools, think=think, reasoning_role=reasoning_role),
            result_box,
        ):
            yield channel, text
        result: _StreamResult = result_box["result"]
        stats = self._remember("chat_stream", result.usage, result.elapsed_ms)
        raw = _raw_tool_calls(result.calls)
        return (
            _think(result.thinking),
            _clean(result.content),
            _parse_tool_calls(raw),
            _assistant_msg(result.content, raw),
            stats,
        )

    def chat_json(self, messages, schema, think=True, reasoning_role=config.ROLE_GM):
        result = self._collect_stream(
            self._payload(messages, think=think, schema=schema, reasoning_role=reasoning_role)
        )
        self._remember("chat_json", result.usage, result.elapsed_ms)
        return _loads(result.content)

    def chat_json_stream(self, messages, schema, think=True, reasoning_role=config.ROLE_GM):
        result_box: dict[str, Any] = {}
        for channel, text in self._stream_generator(
            self._payload(messages, think=think, schema=schema, reasoning_role=reasoning_role),
            result_box,
            content_channel="content",
        ):
            if channel == "content":
                yield channel, text
        result: _StreamResult = result_box["result"]
        stats = self._remember("chat_json_stream", result.usage, result.elapsed_ms)
        return _loads(result.content), stats

    def summarize(self, text: str, proper_nouns=None) -> str:
        names = [str(name).strip() for name in (proper_nouns or []) if str(name).strip()]
        proper_nouns_line = (
            "Keep proper nouns exactly as written; never translate or transliterate them."
            if not names
            else "Keep these proper nouns exactly as written: " + ", ".join(names) + "."
        )
        messages = [
            {"role": "system", "content": prompts.GM_COMPACT_SYSTEM.format(
                proper_nouns_line=proper_nouns_line)},
            {"role": "user", "content": text[:config.COMPACT_INPUT_CHARS]},
        ]
        _, content, _, _ = self.chat(messages, think=True, reasoning_role=config.ROLE_COMPACT)
        return content.strip()

    def _payload(self, messages, tools=None, think=False, schema=None,
                 reasoning_role=config.ROLE_GM) -> dict:
        settings = runtime_settings.get()
        instructions, input_items = split_messages_for_responses(messages)
        converted_tools = [convert_tool_for_responses(tool) for tool in (tools or [])]
        has_tools = bool(converted_tools)
        tool_choice = runtime_settings.tool_choice_for_request(has_tools)
        payload: dict[str, Any] = {
            "model": self._model,
            "instructions": instructions,
            "input": input_items,
            "tools": converted_tools,
            "tool_choice": tool_choice,
            "parallel_tool_calls": (
                runtime_settings.parallel_tool_calls_for_request(has_tools)
                and tool_choice != "none"
            ),
            "store": False,
            "stream": True,
            "include": [],
            "client_metadata": {
                "application": "gm-lab",
                "provider": "codex-oauth",
            },
        }
        payload["prompt_cache_key"] = config.CODEX_PROMPT_CACHE_KEY or self._thread_id
        reasoning = runtime_settings.reasoning_for_request(think, reasoning_role)
        if reasoning:
            payload["reasoning"] = reasoning
            payload["include"] = ["reasoning.encrypted_content"]
        text: dict[str, Any] = {}
        if settings["text_verbosity"] != "default":
            text["verbosity"] = settings["text_verbosity"]
        if schema:
            text["format"] = {
                "type": "json_schema",
                "name": "gm_lab_json",
                "strict": False,
                "schema": schema,
            }
        if text:
            payload["text"] = text
        max_output_tokens = runtime_settings.max_output_tokens()
        if max_output_tokens > 0:
            payload["max_output_tokens"] = max_output_tokens
        return payload

    def _collect_stream(self, payload: dict) -> _StreamResult:
        result = _StreamAccumulator()
        t0 = time.perf_counter()
        for event in self._iter_events(payload):
            for _channel, _text in result.handle(event):
                pass
            if result.done:
                break
        return result.finish((time.perf_counter() - t0) * 1000)

    def _stream_generator(self, payload: dict, result_box: dict, content_channel: str = "content"):
        result = _StreamAccumulator()
        t0 = time.perf_counter()
        for event in self._iter_events(payload):
            for channel, text in result.handle(event):
                if channel == "thinking":
                    yield "thinking", text
                elif channel == "content":
                    yield content_channel, text
            if result.done:
                break
        result_box["result"] = result.finish((time.perf_counter() - t0) * 1000)

    def _iter_events(self, payload: dict):
        with self._http.stream(
            "POST",
            self._responses_url,
            json=payload,
            headers=self._auth_headers(accept_sse=True),
        ) as r:
            if not r.is_success:
                body = r.read().decode("utf-8", errors="replace")
                raise RuntimeError(_redacted_provider_error(r.status_code, body))
            turn_state = r.headers.get("x-codex-turn-state")
            if turn_state:
                self._turn_state = turn_state
            data_lines: list[str] = []
            for line in r.iter_lines():
                line = line.strip()
                if not line:
                    if data_lines:
                        payload_text = "\n".join(data_lines)
                        data_lines.clear()
                        if payload_text == "[DONE]":
                            break
                        yield _json_event(payload_text)
                    continue
                if line.startswith("data:"):
                    data_lines.append(line[5:].strip())
            if data_lines:
                payload_text = "\n".join(data_lines)
                if payload_text != "[DONE]":
                    yield _json_event(payload_text)

    def _auth_headers(self, accept_sse: bool = False) -> dict[str, str]:
        credential = codex_oauth.ensure_fresh_credential(self._http)
        headers = {
            "Authorization": "Bearer " + credential.access_token.strip(),
            "originator": config.CODEX_ORIGINATOR,
            "User-Agent": config.CODEX_USER_AGENT,
            "version": config.CODEX_CLIENT_VERSION,
            "session-id": self._session_id,
            "thread-id": self._thread_id,
            "x-client-request-id": self._thread_id,
            "x-codex-installation-id": self._installation_id,
        }
        if accept_sse:
            headers["Accept"] = "text/event-stream"
        if credential.account_id:
            headers["ChatGPT-Account-Id"] = credential.account_id
        if self._turn_state:
            headers["x-codex-turn-state"] = self._turn_state
        return headers

    def _remember(self, label: str, usage: dict | None, elapsed_ms: float) -> dict:
        stats = _usage_stats(usage, elapsed_ms)
        row = {"label": label, **stats, "tokens": stats["prompt_eval_count"] + stats["eval_count"]}
        self.call_log.append(row)
        return stats


class _StreamAccumulator:
    def __init__(self):
        self.thinking_parts: list[str] = []
        self.content_parts: list[str] = []
        self.tool_calls = _ToolCallAccumulator()
        self.completed_tool_calls: list[dict] = []
        self.usage: dict | None = None
        self.done = False

    def handle(self, event: dict):
        kind = event.get("type") or ""
        if kind == "response.output_text.delta":
            delta = event.get("delta") if isinstance(event.get("delta"), str) else ""
            if delta:
                self.content_parts.append(delta)
                yield "content", delta
        elif kind in ("response.reasoning_summary_text.delta", "response.reasoning_text.delta"):
            delta = event.get("delta") if isinstance(event.get("delta"), str) else ""
            if delta:
                self.thinking_parts.append(delta)
                yield "thinking", delta
        elif kind == "response.output_item.added":
            self.tool_calls.merge_item(event.get("item"), _output_index(event))
        elif kind == "response.function_call_arguments.delta":
            self.tool_calls.merge_arguments_delta(
                _output_index(event),
                event.get("item_id") or event.get("call_id"),
                event.get("delta") or "",
            )
        elif kind == "response.function_call_arguments.done":
            self.tool_calls.merge_done(event)
        elif kind == "response.output_item.done":
            self.tool_calls.merge_item(event.get("item"), _output_index(event))
        elif kind == "response.completed":
            response = event.get("response") if isinstance(event.get("response"), dict) else {}
            self.usage = response.get("usage") if isinstance(response.get("usage"), dict) else None
            if not self.content_parts:
                text = extract_output_text(response)
                if text:
                    self.content_parts.append(text)
            self.completed_tool_calls = extract_tool_calls(response)
            self.done = True
        elif kind in ("response.failed", "response.incomplete", "error"):
            raise RuntimeError(_event_error_message(event))

    def finish(self, elapsed_ms: float) -> _StreamResult:
        calls = self.tool_calls.finish()
        if not calls:
            calls = self.completed_tool_calls
        return _StreamResult(
            thinking="".join(self.thinking_parts),
            content="".join(self.content_parts),
            calls=calls,
            usage=self.usage,
            elapsed_ms=elapsed_ms,
        )


class _ToolCallAccumulator:
    def __init__(self):
        self.calls: list[dict[str, Any]] = []

    def merge_item(self, item: Any, output_index: int) -> None:
        if not isinstance(item, dict) or item.get("type") != "function_call":
            return
        call = self._find_or_create(output_index, item.get("id"))
        if item.get("id"):
            call["item_id"] = str(item["id"])
        if item.get("call_id"):
            call["id"] = str(item["call_id"])
        if item.get("name"):
            call["name"] = str(item["name"])
        if isinstance(item.get("arguments"), str):
            call["arguments_raw"] = item["arguments"]

    def merge_arguments_delta(self, output_index: int, item_id: str | None, delta: str) -> None:
        call = self._find_or_create(output_index, item_id)
        call["arguments_raw"] = str(call.get("arguments_raw") or "") + str(delta or "")

    def merge_done(self, event: dict) -> None:
        call = self._find_or_create(_output_index(event), event.get("item_id") or event.get("call_id"))
        if event.get("call_id"):
            call["id"] = str(event["call_id"])
        if event.get("name"):
            call["name"] = str(event["name"])
        if isinstance(event.get("arguments"), str):
            call["arguments_raw"] = event["arguments"]
        item = event.get("item")
        if isinstance(item, dict):
            self.merge_item(item, _output_index(event))

    def finish(self) -> list[dict]:
        out = []
        for call in sorted(self.calls, key=lambda c: int(c.get("output_index") or 0)):
            name = str(call.get("name") or "").strip()
            if not name:
                continue
            raw_args = str(call.get("arguments_raw") or "{}")
            out.append({
                "id": str(call.get("id") or call.get("item_id") or f"responses_call_{call['output_index']}"),
                "name": name,
                "arguments": _loads(raw_args),
            })
        return out

    def _find_or_create(self, output_index: int, item_id: Any) -> dict[str, Any]:
        item_id = str(item_id or "") or None
        if item_id:
            for call in self.calls:
                if call.get("item_id") == item_id or call.get("id") == item_id:
                    return call
        for call in self.calls:
            if call.get("output_index") == output_index:
                return call
        call = {"output_index": output_index, "item_id": item_id, "id": None, "name": "", "arguments_raw": ""}
        self.calls.append(call)
        return call


def _output_index(event: dict) -> int:
    try:
        return int(event.get("output_index") or event.get("index") or 0)
    except (TypeError, ValueError):
        return 0


def _json_event(text: str) -> dict:
    try:
        data = json.loads(text)
    except json.JSONDecodeError as exc:
        raise RuntimeError(f"Codex returned invalid SSE JSON: {exc}") from exc
    return data if isinstance(data, dict) else {}


def _event_error_message(event: dict) -> str:
    candidates = [
        event.get("message"),
        (event.get("error") or {}).get("message") if isinstance(event.get("error"), dict) else None,
        ((event.get("response") or {}).get("error") or {}).get("message")
        if isinstance(event.get("response"), dict) and isinstance((event.get("response") or {}).get("error"), dict)
        else None,
    ]
    for message in candidates:
        if isinstance(message, str) and message.strip():
            return message.strip()
    return "Codex stream failed"


def _redacted_provider_error(status_code: int, body: str) -> str:
    compact = (body or "").replace("\n", " ").strip()
    if len(compact) > 2000:
        compact = compact[:2000] + "..."
    return f"Codex API error {status_code}: {compact}"
