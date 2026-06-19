"""Настройки каркаса.

Бэкенд по умолчанию — ChatGPT Codex OAuth.
Можно переключить на локальный llama.cpp server через:
  GM_BACKEND=llamacpp
  LLAMA_HOST=http://localhost:8080
Или на внешний OpenAI-compatible провайдер через:
  GM_BACKEND=openai
  GM_API_BASE=https://.../v1
  GM_API_KEY=<secret>
"""
import os
import re
from pathlib import Path


def _load_dotenv() -> None:
    """Load local .env once, without overriding real environment variables."""
    paths = []
    for path in (Path(__file__).with_name(".env"), Path.cwd() / ".env"):
        if path not in paths:
            paths.append(path)
    key_re = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")
    for path in paths:
        if not path.exists():
            continue
        for raw in path.read_text(encoding="utf-8").splitlines():
            line = raw.strip()
            if not line or line.startswith("#"):
                continue
            if line.startswith("export "):
                line = line[len("export "):].lstrip()
            if "=" not in line:
                continue
            key, value = line.split("=", 1)
            key = key.strip()
            if not key_re.match(key) or key in os.environ:
                continue
            value = value.strip()
            if len(value) >= 2 and value[0] == value[-1] and value[0] in ("'", '"'):
                value = value[1:-1]
            else:
                value = value.split(" #", 1)[0].strip()
            os.environ[key] = value


_load_dotenv()


def _env_bool(name: str, default: bool) -> bool:
    raw = os.environ.get(name)
    if raw is None:
        return default
    return raw.strip().lower() not in ("0", "false", "no", "off", "")

# --- Модель ---------------------------------------------------------------
# Пусто = АВТООПРЕДЕЛЕНИЕ загруженной модели из GET /v1/models (не зависим от тега —
# поменял модель на сервере, лаб подхватит сам). Или задай явно через $env:GM_MODEL.
MODEL = os.environ.get("GM_MODEL", "")
LLAMA_HOST = os.environ.get("LLAMA_HOST", "http://localhost:8080")  # llama.cpp server
API_BASE = os.environ.get("GM_API_BASE", "").rstrip("/")
API_KEY = os.environ.get("GM_API_KEY", "")
MAX_TOKENS = int(os.environ.get("GM_MAX_TOKENS", "0") or "0")
# Upper bound for the UI "response limit" knob; shared by the backend clamp
# (runtime_settings) and the frontend input (via settings_options) — one source.
MAX_OUTPUT_TOKENS_CAP = int(os.environ.get("GM_MAX_OUTPUT_TOKENS_CAP", "200000") or "200000")

# Бэкенд: "codex" (ChatGPT Codex OAuth Responses API),
# "llamacpp" (локальный llama.cpp), "openai" (OpenAI-compatible API), "mock".
BACKEND = os.environ.get("GM_BACKEND", "codex")

# Внешние OpenAI-compatible провайдеры обычно не принимают llama.cpp-only поля
# chat_template_kwargs/top_k/min_p. Для локального llama.cpp включено по умолчанию.
USE_LLAMA_TEMPLATE_KWARGS = _env_bool("GM_LLAMA_TEMPLATE_KWARGS", BACKEND == "llamacpp")

# --- Поведение оркестратора ----------------------------------------------
# ГМ сам ревьюит действия NPC в своём треде (correction-перевызов ask_npc), поэтому
# за ход бывает несколько вызовов: NPC -> переделка -> кубы -> нарратив.
MAX_TOOL_HOPS = int(os.environ.get("GM_MAX_HOPS", "6"))

# Сырой стрим видимого текста ГМ. Выключен по умолчанию, потому что некоторые
# модели сначала пишут pre-tool фразу, а потом всё же вызывают ask_npc/roll_dice.
# Если нужно видеть текст ГМ сразу и принять этот риск:
#   $env:GM_STREAM_GM_CONTENT="1"
STREAM_GM_CONTENT = _env_bool("GM_STREAM_GM_CONTENT", False)

# Компакт истории ГМ: делаем поздно, когда длинный контекст уже реально подходит к
# рабочему потолку. До этого выгоднее держать append-only историю и давать
# prompt/KV-cache переиспользовать одинаковый префикс.
GM_HISTORY_TOKENS = int(os.environ.get("GM_HISTORY_TOKENS", "100000"))
GM_KEEP_TURNS = int(os.environ.get("GM_KEEP_TURNS", "3"))
NPC_HISTORY_TOKENS = int(os.environ.get("NPC_HISTORY_TOKENS", "64000"))
NPC_KEEP_EXCHANGES = int(os.environ.get("NPC_KEEP_EXCHANGES", "6"))
EVENTS_CAP = int(os.environ.get("GM_EVENTS_CAP", "400"))   # сколько Event'ов держим в памяти
RUMORS_CAP = int(os.environ.get("GM_RUMORS_CAP", "80"))    # сколько слухов держим в памяти мира
# Грубый оценщик "символов на токен" для прикидки давления контекста (один источник
# для _SYS_EST, _estimate_tokens и гейтов компакта ГМ/NPC).
CHARS_PER_TOKEN = int(os.environ.get("GM_CHARS_PER_TOKEN", "3") or "3")
# Сколько символов истории отдаём в summarize-вызов компакта (ГМ и NPC, все бэкенды).
COMPACT_INPUT_CHARS = int(os.environ.get("GM_COMPACT_INPUT_CHARS", "12000") or "12000")
# Сколько символов pending-resolution-контекста кладём в pre-tool прелюдию ГМ.
PRELUDE_CALLBRIEF_CHARS = int(os.environ.get("GM_PRELUDE_CALLBRIEF_CHARS", "4000") or "4000")

# --- RAG / embeddings -----------------------------------------------------
# Локальный OpenAI-compatible embeddings server. Индекс держит только открытые для
# ГМа/игрока документы мира: публичные факты, видимую сцену, местонахождение NPC,
# неподтверждённые слухи. Hidden canon и NPC secrets туда не попадают.
RAG_ENABLED = _env_bool("GM_RAG_ENABLED", True)
RAG_EMBEDDINGS_URL = os.environ.get(
    "GM_RAG_EMBEDDINGS_URL", "http://127.0.0.1:8080/v1/embeddings"
)
RAG_EMBEDDINGS_MODEL = os.environ.get("GM_RAG_EMBEDDINGS_MODEL", "qwen3-embedding-4b-q4")
RAG_ENCODING_FORMAT = os.environ.get("GM_RAG_ENCODING_FORMAT", "base64")
RAG_CACHE_PATH = os.environ.get(
    "GM_RAG_CACHE_PATH",
    str(Path(__file__).with_name("gm_lab_embeddings.sqlite3")),
)
RAG_BATCH_SIZE = int(os.environ.get("GM_RAG_BATCH_SIZE", "16") or "16")
RAG_TIMEOUT_SECONDS = float(os.environ.get("GM_RAG_TIMEOUT_SECONDS", "20") or "20")
RAG_TOP_K = int(os.environ.get("GM_RAG_TOP_K", "6") or "6")
RAG_MIN_DENSE_SCORE = float(os.environ.get("GM_RAG_MIN_DENSE_SCORE", "0.60") or "0.60")
RAG_RRF_K = int(os.environ.get("GM_RAG_RRF_K", "60") or "60")
# Hybrid-reranker tuning knobs (RRF tiebreaks + status boost + final fact count).
RAG_KEYWORD_TIEBREAK = float(os.environ.get("GM_RAG_KEYWORD_TIEBREAK", "0.002") or "0.002")
RAG_DENSE_TIEBREAK = float(os.environ.get("GM_RAG_DENSE_TIEBREAK", "0.001") or "0.001")
RAG_STATUS_BOOST = float(os.environ.get("GM_RAG_STATUS_BOOST", "1.04") or "1.04")
RAG_FACT_SELECT_K = int(os.environ.get("GM_RAG_FACT_SELECT_K", "4") or "4")

# Опциональные cache-hints для провайдеров, которые их понимают. По умолчанию
# выключены: многие OpenAI-compatible прокси отклоняют незнакомые поля.
PROMPT_CACHE_KEY = os.environ.get("GM_PROMPT_CACHE_KEY", "")
PROMPT_CACHE_RETENTION = os.environ.get("GM_PROMPT_CACHE_RETENTION", "")
LLAMA_CACHE_REUSE = int(os.environ.get("GM_LLAMA_CACHE_REUSE", "0") or "0")

# --- ChatGPT Codex OAuth --------------------------------------------------
# Контракт взят из свежего official OpenAI Codex: browser OAuth -> ChatGPT
# Codex backend-api/responses. Токены хранятся в локальном app data GM-Lab, не в репо.
CODEX_BASE_URL = os.environ.get(
    "GM_CODEX_BASE_URL", "https://chatgpt.com/backend-api/codex"
).rstrip("/")
CODEX_CLIENT_ID = os.environ.get("GM_CODEX_CLIENT_ID", "app_EMoamEEZ73f0CkXaXp7hrann")
CODEX_CLIENT_VERSION = os.environ.get("GM_CODEX_CLIENT_VERSION", "0.133.0")
CODEX_ORIGINATOR = os.environ.get("GM_CODEX_ORIGINATOR", "codex_cli_rs")
CODEX_USER_AGENT = os.environ.get(
    "GM_CODEX_USER_AGENT", f"codex_cli_rs/{CODEX_CLIENT_VERSION} (GM-Lab)"
)
CODEX_AUTH_PORT = int(os.environ.get("GM_CODEX_AUTH_PORT", "1455") or "1455")
CODEX_AUTO_OPEN_BROWSER = _env_bool("GM_CODEX_AUTO_OPEN_BROWSER", True)
CODEX_MODEL = os.environ.get("GM_CODEX_MODEL", "gpt-5.4-mini")
CODEX_REASONING_EFFORT = os.environ.get("GM_CODEX_REASONING_EFFORT", "low").strip() or "low"
CODEX_REASONING_SUMMARY = (
    os.environ.get("GM_CODEX_REASONING_SUMMARY", "auto").strip().lower() or "auto"
)
# Пусто = CodexClient использует стабильный thread-id текущего диалога, как official Codex.
CODEX_PROMPT_CACHE_KEY = os.environ.get("GM_CODEX_PROMPT_CACHE_KEY", PROMPT_CACHE_KEY)

# --- Роли reasoning: ЕДИНЫЙ источник значений enum ------------------------
# Все строковые значения ролей берутся отсюда. Переименовал тут — поменялось
# везде: runtime_settings (ключи настроек, валидатор), клиенты, оркестратор,
# агенты. Нигде больше эти строки не должны хардкодиться.
ROLE_GM = "gm"
ROLE_NPC = "npc"
ROLE_COMPACT = "compact"
REASONING_ROLES = (ROLE_GM, ROLE_NPC, ROLE_COMPACT)

# --- Думалка (reasoning) по ролям ----------------------------------------
# ГМ планирует tool-calls, поэтому думалка включена по умолчанию: без неё Qwen/Gemma
# чаще пытаются отыгрывать NPC напрямую. NPC держим без думалки: им важнее короткий
# JSON speech/action без лишних токенов. Промпты просят все видимые debug-поля и
# аргументы тулов писать по-русски, даже если сами инструкции на английском. Можно
# переопределить:
#   $env:GM_THINK="0" / "1"
#   $env:NPC_THINK="0" / "1"
GM_THINK = _env_bool("GM_THINK", True)
NPC_THINK = _env_bool("NPC_THINK", False)

# --- Сэмплинг Qwen3.6 (рекомендованный, ПО РЕЖИМУ) ------------------------
# Задаётся В ЗАПРОСЕ по флагу think (режим думалки меняется от вызова к вызову).
SAMPLING_THINK = {"temperature": 0.6, "top_p": 0.95, "top_k": 20, "min_p": 0,
                  "presence_penalty": 1.5}                      # думающий режим
SAMPLING_PLAIN = {"temperature": 0.7, "top_p": 0.80, "top_k": 20, "min_p": 0,
                  "presence_penalty": 1.5}                      # без думалки (instruct)
