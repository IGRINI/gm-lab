"""Клиенты моделей: OpenAI-compatible chat/completions, Codex OAuth Responses, mock.

Реальная форма ответа Qwen3.6 через llama.cpp:
  message.reasoning_content -> канал мыслей (thinking)
  message.content           -> текст (пустой при tool-call)
  message.tool_calls         -> [{id, function:{name, arguments: JSON-СТРОКА}}]
  usage / timings            -> токены и скорость

Думалка управляется chat_template_kwargs.enable_thinking. JSON у NPC берём свободным
текстом (Qwen держит reasoning отдельно, content не засоряется), фолбэк — response_format.
"""
from __future__ import annotations

import json
import re
import time

import config
import prompts
import runtime_settings


def make_client():
    if config.BACKEND == "mock":
        return MockClient()
    if config.BACKEND == "codex":
        from codex_client import CodexClient

        return CodexClient()
    return OpenAICompatClient()


def _loads(text: str) -> dict:
    """Достаём JSON из строки (на случай fences/мусора вокруг)."""
    text = (text or "").strip()
    if not text:
        return {}
    try:
        return json.loads(text)
    except Exception:
        m = re.search(r"\{.*\}", text, re.DOTALL)
        if m:
            try:
                return json.loads(m.group(0))
            except Exception:
                return {}
    return {}


def _attr(obj, name, default=None):
    """Достаём поле и из dict, и из объекта."""
    if obj is None:
        return default
    if isinstance(obj, dict):
        return obj.get(name, default)
    return getattr(obj, name, default)


# Служебные токены чат-шаблона, изредка утекающие в текст.
_TOKEN_RE = re.compile(r"</?\|?[a-zA-Z_]+\|?>")
_LEAD_CHANNEL_RE = re.compile(r"^\s*(thought|analysis|final|commentary)\b[\s:]*", re.I)


def _clean(text: str) -> str:
    text = _TOKEN_RE.sub("", text)
    text = _LEAD_CHANNEL_RE.sub("", text)
    return text.strip()


def _think(s: str) -> str:
    """Чистим думалку от огрызков тегов <think>/</think>."""
    return re.sub(r"</?think>", "", s or "").strip()


def extract_json_string(buf: str, field: str):
    """Из РАСТУЩЕГО JSON достаём значение строкового поля. -> (raw_or_None, complete)."""
    key = '"' + field + '"'
    i = buf.find(key)
    if i < 0:
        return None, False
    j = buf.find(":", i + len(key))
    if j < 0:
        return None, False
    k = buf.find('"', j + 1)
    if k < 0:
        return None, False
    out, p, esc = [], k + 1, False
    while p < len(buf):
        c = buf[p]
        if esc:
            out.append(c); esc = False
        elif c == "\\":
            out.append(c); esc = True
        elif c == '"':
            return "".join(out), True
        else:
            out.append(c)
        p += 1
    return "".join(out), False


def json_unescape(s: str) -> str:
    return (s.replace('\\"', '"').replace("\\n", "\n").replace("\\t", "\t")
             .replace("\\/", "/").replace("\\\\", "\\"))


# --- разбор ответов llama.cpp ---------------------------------------------
def _parse_tool_calls(raw) -> list:
    """OpenAI tool_calls -> [{name, arguments(dict), id}] (arguments приходят строкой)."""
    calls = []
    for tc in raw or []:
        fn = (tc.get("function") if isinstance(tc, dict) else None) or {}
        args = fn.get("arguments")
        if isinstance(args, str):
            args = _loads(args)
        calls.append({"name": fn.get("name"), "arguments": args or {}, "id": tc.get("id", "")})
    return calls


def _assistant_msg(content, raw_tool_calls) -> dict:
    """Ассистентское сообщение в историю (OpenAI-формат). reasoning НЕ кладём —
    Qwen не нужно скармливать прошлые мысли в multi-turn."""
    msg = {"role": "assistant", "content": content or ""}
    if raw_tool_calls:
        msg["tool_calls"] = raw_tool_calls
    return msg


def _stats(usage, timings) -> dict:
    """Нормализуем статы llama.cpp под формат _meta (длительности в наносекундах)."""
    s = {"load_duration": 0, "cached_tokens": 0}
    if usage:
        s["prompt_eval_count"] = usage.get("prompt_tokens", 0)
        s["eval_count"] = usage.get("completion_tokens", 0)
        prompt_details = usage.get("prompt_tokens_details") or usage.get("input_tokens_details") or {}
        s["cached_tokens"] = int(prompt_details.get("cached_tokens", 0) or 0)
    if timings:
        pm, em = timings.get("prompt_ms", 0) or 0, timings.get("predicted_ms", 0) or 0
        s["prompt_eval_duration"] = int(pm * 1e6)
        s["eval_duration"] = int(em * 1e6)
        s["total_duration"] = int((pm + em) * 1e6)
    return s


def _mock_stats():
    return {"prompt_eval_count": 760, "eval_count": 120, "prompt_eval_duration": 80_000_000,
            "eval_duration": 640_000_000, "total_duration": 730_000_000, "load_duration": 0}


def _proper_nouns_line(proper_nouns=None) -> str:
    names = [str(name).strip() for name in (proper_nouns or []) if str(name).strip()]
    if not names:
        return "Keep proper nouns exactly as written in the transcript; never transliterate them."
    return ("Keep these proper nouns exactly as written if they appear; never translate or "
            "transliterate them: " + ", ".join(names) + ".")


class OpenAICompatClient:
    """OpenAI-compatible /v1/chat/completions client.

    Works with local llama.cpp and external OpenAI-compatible providers. llama.cpp-only
    request fields are sent only when config.USE_LLAMA_TEMPLATE_KWARGS is enabled.
    """

    def __init__(self):
        import httpx
        base = (config.API_BASE or config.LLAMA_HOST).rstrip("/")
        if base.endswith("/v1"):
            self._chat_url = base + "/chat/completions"
            self._models_url = base + "/models"
        else:
            self._chat_url = base + "/v1/chat/completions"
            self._models_url = base + "/v1/models"
        headers = {}
        if config.API_KEY:
            headers["Authorization"] = "Bearer " + config.API_KEY
        self._http = httpx.Client(
            headers=headers,
            timeout=httpx.Timeout(connect=10.0, read=None, write=60.0, pool=None))
        self._model = config.MODEL or self._detect_model(base)
        self.call_log: list[dict] = []

    @property
    def model(self) -> str:
        return self._model

    def set_model(self, model: str) -> None:
        model = (model or "").strip()
        if model:
            self._model = model

    def list_models(self) -> list[dict]:
        try:
            r = self._http.get(self._models_url, timeout=10.0)
            r.raise_for_status()
            data = r.json()
        except Exception:
            return [{"id": self._model, "name": self._model, "supported": True}]
        raw_models = data.get("data") or data.get("models") or []
        models = []
        for raw in raw_models:
            if not isinstance(raw, dict):
                continue
            model_id = raw.get("id") or raw.get("slug") or raw.get("model")
            if model_id:
                models.append({"id": model_id, "name": raw.get("name") or model_id,
                               "supported": True})
        return models or [{"id": self._model, "name": self._model, "supported": True}]

    def _remember(self, label: str, usage, timings, elapsed_ms: float | None = None) -> dict:
        if not timings and elapsed_ms is not None:
            timings = {"prompt_ms": 0, "predicted_ms": elapsed_ms}
        stats = _stats(usage, timings)
        row = {"label": label, **stats,
               "tokens": stats.get("prompt_eval_count", 0) + stats.get("eval_count", 0)}
        self.call_log.append(row)
        return stats

    def _detect_model(self, base):
        """Берём загруженную моделью id с сервера — не зависим от хардкод-тега."""
        try:
            r = self._http.get(self._models_url, timeout=5.0)
            return r.json()["data"][0]["id"]
        except Exception:
            return "default"

    def _payload(self, messages, tools=None, think=None, response_format=None, stream=False,
                 reasoning_role=config.ROLE_GM):
        p = {"model": self._model, "messages": messages, "stream": stream}
        if config.PROMPT_CACHE_KEY:
            p["prompt_cache_key"] = config.PROMPT_CACHE_KEY
        if config.PROMPT_CACHE_RETENTION:
            p["prompt_cache_retention"] = config.PROMPT_CACHE_RETENTION
        max_output_tokens = runtime_settings.max_output_tokens()
        if max_output_tokens > 0:
            p["max_tokens"] = max_output_tokens
        if tools:
            p["tools"] = tools
            tool_choice = runtime_settings.tool_choice_for_request(True)
            p["tool_choice"] = tool_choice
            p["parallel_tool_calls"] = (
                runtime_settings.parallel_tool_calls_for_request(True)
                and tool_choice != "none"
            )
        if response_format:
            p["response_format"] = response_format
        if think is not None:                       # вкл/выкл reasoning + сэмплинг по режиму
            effective_think = runtime_settings.reasoning_enabled(think, reasoning_role)
            sampling = config.SAMPLING_THINK if effective_think else config.SAMPLING_PLAIN
            if config.USE_LLAMA_TEMPLATE_KWARGS:
                p["chat_template_kwargs"] = {"enable_thinking": bool(effective_think)}
                p.update(sampling)
                if config.LLAMA_CACHE_REUSE > 0:
                    p["n_cache_reuse"] = config.LLAMA_CACHE_REUSE
            else:
                # Standard OpenAI-compatible providers usually reject top_k/min_p and
                # llama.cpp chat_template_kwargs. Keep only widely-supported fields.
                for key in ("temperature", "top_p", "presence_penalty"):
                    if key in sampling:
                        p[key] = sampling[key]
        if stream:
            p["stream_options"] = {"include_usage": True}
        return p

    def _post(self, payload):
        t0 = time.perf_counter()
        r = self._http.post(self._chat_url, json=payload)
        r.raise_for_status()
        data = r.json()
        data["_client_elapsed_ms"] = (time.perf_counter() - t0) * 1000
        return data

    def _stream(self, payload):
        t0 = time.perf_counter()
        with self._http.stream("POST", self._chat_url, json=payload) as r:
            r.raise_for_status()
            for line in r.iter_lines():
                if not line or not line.startswith("data:"):
                    continue
                chunk = line[5:].strip()
                if chunk == "[DONE]":
                    break
                try:
                    yield json.loads(chunk)
                except Exception:
                    continue
        self._last_stream_elapsed_ms = (time.perf_counter() - t0) * 1000

    # --- нестримящие ----------------------------------------------------
    def chat(self, messages, tools=None, think=False, reasoning_role=config.ROLE_GM):
        """Возвращает (thinking, content, calls, assistant_msg)."""
        data = self._post(
            self._payload(messages, tools=tools, think=think, reasoning_role=reasoning_role)
        )
        self._remember("chat", data.get("usage"), data.get("timings"),
                       data.get("_client_elapsed_ms"))
        msg = data["choices"][0]["message"]
        raw = msg.get("tool_calls")
        return (_think(msg.get("reasoning_content")), _clean(msg.get("content") or ""),
                _parse_tool_calls(raw), _assistant_msg(msg.get("content") or "", raw))

    def chat_json(self, messages, schema, think=True, reasoning_role=config.ROLE_GM):
        """JSON-вывод: свободный текст; если не распарсилось — response_format без думалки."""
        data = self._post(self._payload(messages, think=think, reasoning_role=reasoning_role))
        self._remember("chat_json", data.get("usage"), data.get("timings"),
                       data.get("_client_elapsed_ms"))
        out = _loads(data["choices"][0]["message"].get("content") or "")
        if out:
            return out
        data = self._post(self._payload(
            messages, think=False, reasoning_role=reasoning_role,
            response_format={"type": "json_object"},
        ))
        self._remember("chat_json_fallback", data.get("usage"), data.get("timings"),
                       data.get("_client_elapsed_ms"))
        return _loads(data["choices"][0]["message"].get("content") or "{}")

    def summarize(self, text: str, proper_nouns=None) -> str:
        sys = prompts.GM_COMPACT_SYSTEM.format(proper_nouns_line=_proper_nouns_line(proper_nouns))
        _, content, _, _ = self.chat(
            [{"role": "system", "content": sys},
             {"role": "user", "content": text[:config.COMPACT_INPUT_CHARS]}],
            think=True,
            reasoning_role=config.ROLE_COMPACT,
        )
        return content.strip()

    # --- стримящие ------------------------------------------------------
    def chat_stream(self, messages, tools=None, think=False, reasoning_role=config.ROLE_GM):
        """yield ('thinking'|'content', delta); return (thinking, content, calls, assistant_msg, stats)."""
        t_parts, c_parts, tool_acc, usage, timings = [], [], {}, None, None
        for obj in self._stream(self._payload(
            messages, tools=tools, think=think, stream=True, reasoning_role=reasoning_role,
        )):
            usage = obj.get("usage") or usage
            timings = obj.get("timings") or timings
            ch = (obj.get("choices") or [None])[0]
            if not ch:
                continue
            delta = ch.get("delta") or {}
            t, c = delta.get("reasoning_content"), delta.get("content")
            if t:
                t_parts.append(t); yield ("thinking", t)
            if c:
                c_parts.append(c); yield ("content", c)
            for tc in delta.get("tool_calls") or []:
                acc = tool_acc.setdefault(tc.get("index", 0), {"id": "", "name": "", "args": ""})
                if tc.get("id"):
                    acc["id"] = tc["id"]
                fn = tc.get("function") or {}
                if fn.get("name"):
                    acc["name"] = fn["name"]
                if fn.get("arguments"):
                    acc["args"] += fn["arguments"]
        raw = [{"id": a["id"], "type": "function",
                "function": {"name": a["name"], "arguments": a["args"]}}
               for a in (tool_acc[i] for i in sorted(tool_acc)) if a["name"]]
        stats = self._remember("chat_stream", usage, timings,
                               getattr(self, "_last_stream_elapsed_ms", None))
        return (_think("".join(t_parts)), _clean("".join(c_parts)),
                _parse_tool_calls(raw), _assistant_msg("".join(c_parts), raw),
                stats)

    def chat_json_stream(self, messages, schema, think=True, reasoning_role=config.ROLE_GM):
        """yield ('content', delta); return (parsed dict, stats)."""
        parts, usage, timings = [], None, None
        for obj in self._stream(self._payload(
            messages, think=think, stream=True, reasoning_role=reasoning_role,
        )):
            usage = obj.get("usage") or usage
            timings = obj.get("timings") or timings
            ch = (obj.get("choices") or [None])[0]
            if ch:
                c = (ch.get("delta") or {}).get("content")
                if c:
                    parts.append(c); yield ("content", c)
        data = _loads("".join(parts))
        stats = self._remember("chat_json_stream", usage, timings,
                               getattr(self, "_last_stream_elapsed_ms", None))
        if not data:
            data = self.chat_json(messages, schema, reasoning_role=reasoning_role)   # фолбэк
        return data, stats


# --------------------------------------------------------------------------
# Мок-бэкенд: сценарий «NPC заявил невозможное действие -> ГМ вернул на
# переделку -> NPC переиграл». Прогоняет весь каркас без модели.
# --------------------------------------------------------------------------
class MockClient:
    def __init__(self):
        self.call_log: list[dict] = []
        self._model = "mock"

    @property
    def model(self) -> str:
        return self._model

    def set_model(self, model: str) -> None:
        self._model = (model or "").strip() or self._model

    def list_models(self) -> list[dict]:
        return [{"id": self._model, "name": self._model, "supported": True}]

    def _remember(self, label: str):
        s = _mock_stats()
        self.call_log.append({"label": label, **s,
                              "tokens": s["prompt_eval_count"] + s["eval_count"]})
        return s

    def chat(self, messages, tools=None, think=False, reasoning_role=config.ROLE_GM):
        self._remember("chat")
        n_tool = sum(1 for m in messages if _attr(m, "role") == "tool")

        def toolmsg(calls):
            return {"role": "assistant", "content": "",
                    "tool_calls": [{"id": f"mock{i}", "type": "function",
                                    "function": {"name": c["name"], "arguments": c["arguments"]}}
                                   for i, c in enumerate(calls)]}

        if n_tool == 0:                       # первый ход: зовём NPC
            calls = [{"name": "ask_npc", "id": "mock0", "arguments": {
                "npc_id": "borin", "situation": "Игрок громко обвиняет Борина в убийстве."}}]
            return ("Нужен Борин — зову ask_npc.", "", calls, toolmsg(calls))
        if n_tool == 1:                       # ГМ ревьюит черновик -> возврат с correction
            calls = [{"name": "ask_npc", "id": "mock1", "arguments": {
                "npc_id": "borin", "situation": "Игрок громко обвиняет Борина в убийстве.",
                "correction": "Задней двери у «Грифона» нет — выход только через зал, на виду. "
                              "Так не улизнёшь, отыграй иначе."}}]
            return ("Борин рвётся в несуществующую заднюю дверь — возвращаю на переделку.",
                    "", calls, toolmsg(calls))
        content = "Борин мнётся за стойкой, так и не сумев улизнуть. Зал притих и смотрит на вас."
        return ("NPC отыграл, завершаю сцену.", content, [],
                {"role": "assistant", "content": content})

    def chat_json(self, messages, schema, think=True, reasoning_role=config.ROLE_GM):
        self._remember("chat_json")
        system_text = " ".join(str(_attr(m, "content", "")) for m in messages
                               if _attr(m, "role") == "system")
        if "current-scene NPC roster changes" in system_text:
            return {"moves": []}
        if "starting scene" in system_text or "WorldSeed" in system_text:
            return {
                "public_intro": "Ледяной порт Нордхольм. В таверне пахнет мокрыми канатами; "
                                "порт шепчется о пропавшем корабле «Северная свеча».",
                "hidden_truth": "The ship was hidden in a frozen cove by smugglers.",
                "proper_nouns": ["Нордхольм", "«Северная свеча»"],
                "public_facts": [
                    "Корабль «Северная свеча» не вернулся в порт Нордхольм.",
                    "Ива держит портовую таверну.",
                    "Рун служил на пристани и знает моряков.",
                ],
                "npcs": [
                    {"id": "iva", "name": "Ива", "role": "tavern keeper",
                     "persona": "Practical keeper of the port tavern, tired but observant.",
                     "voice": "Dry, direct, with sea-port slang.",
                     "goals": "Keep order and learn what happened to the missing ship.",
                     "knowledge": "Knows dock gossip and who drank in the tavern last night.",
                     "secret": "Owes money to people tied to the frozen cove."},
                    {"id": "run", "name": "Рун", "role": "sailor",
                     "persona": "Young sailor, nervous and superstitious.",
                     "voice": "Fast, hushed, often glances at the door.",
                     "goals": "Avoid blame for the missing ship.",
                     "knowledge": "Saw a strange lantern signal before dawn.",
                     "secret": "Skipped his watch for a few minutes."},
                ],
                "scene": {
                    "id": "northolm_tavern",
                    "location_id": "northolm_port",
                    "title": "Портовая таверна Нордхольма",
                    "description": "Игрок стоит в портовой таверне. За окнами ледяные причалы; "
                                   "Ива у стойки, Рун у печи.",
                    "present_npcs": ["iva", "run"],
                    "items": [
                        {"id": "counter", "name": "стойка", "location": "у стены",
                         "visible": True, "portable": False},
                        {"id": "harbor_map", "name": "карта гавани", "location": "на стене",
                         "visible": True, "portable": False},
                    ],
                    "exits": [
                        {"id": "dock_door", "name": "дверь к причалам",
                         "destination": "ледяные причалы", "visible": True},
                    ],
                    "constraints": [
                        "Only Ива and Рун are present as named NPCs in the tavern.",
                        "The missing ship is not visible from here.",
                    ],
                    "tension": "People are cold, worried, and watching strangers.",
                },
            }
        user = " ".join(str(_attr(m, "content", "")) for m in messages
                        if _attr(m, "role") == "user")
        if "REDO" in user:                     # переигровка после замечания ГМ (метка REDO в фидбеке)
            return {"reasoning": "Чёрт, незаметно не выйти. Придётся тянуть время.",
                    "speech": "Сейчас, дружище, эль принесу, обожди-ка.",
                    "action": "медленно бредёт к бочкам, не сводя глаз с гостя",
                    "claims": []}
        return {"reasoning": "Надо предупредить своих, пока не поздно.",
                "speech": "Я... э-э, мне на кухню надо, отойду на минутку.",
                "action": "пытается незаметно выскользнуть через заднюю дверь трактира",
                "claims": ["В трактире есть задняя дверь"]}

    def chat_stream(self, messages, tools=None, think=False, reasoning_role=config.ROLE_GM):
        thinking, content, calls, msg = self.chat(messages, tools, think, reasoning_role)
        for w in (thinking or "").split():
            yield ("thinking", w + " ")
        for w in (content or "").split():
            yield ("content", w + " ")
        return thinking, content, calls, msg, self._remember("chat_stream")

    def chat_json_stream(self, messages, schema, think=True, reasoning_role=config.ROLE_GM):
        data = self.chat_json(messages, schema, think, reasoning_role)
        s = json.dumps(data, ensure_ascii=False)
        for i in range(0, len(s), 6):
            yield ("content", s[i:i + 6])
        return data, self._remember("chat_json_stream")

    def summarize(self, text: str, proper_nouns=None) -> str:
        return "(compressed summary of previous turns)"
