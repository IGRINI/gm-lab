"""Persisted runtime inference settings for GM-Lab.

These settings are intentionally separate from config.py: config.py is startup
configuration, while this module stores UI-controlled knobs that must survive
scene resets and server restarts.
"""
from __future__ import annotations

from pathlib import Path
import json
import os
import tempfile
import threading
from typing import Any

import config


REASONING_EFFORTS = ("none", "minimal", "low", "medium", "high", "xhigh")
REASONING_SUMMARIES = ("auto", "concise", "detailed", "none")
REASONING_ROLES = config.REASONING_ROLES  # single source: config.ROLE_GM/NPC/COMPACT
TEXT_VERBOSITIES = ("default", "low", "medium", "high")
TOOL_CHOICES = ("auto", "required", "none")

_BASE_REASONING_EFFORT = (config.CODEX_REASONING_EFFORT or "low").strip().lower() or "low"
_BASE_REASONING_SUMMARY = (
    config.CODEX_REASONING_SUMMARY or "auto"
).strip().lower() or "auto"

# Per-role default effort/summary base. GM/NPC inherit the Codex reasoning base;
# compaction defaults to no reasoning. Keyed by the role constants so adding or
# renaming a role here (config.REASONING_ROLES) keeps the keys in lockstep.
_ROLE_REASONING_BASE = {
    config.ROLE_GM: (_BASE_REASONING_EFFORT, _BASE_REASONING_SUMMARY),
    config.ROLE_NPC: (_BASE_REASONING_EFFORT, _BASE_REASONING_SUMMARY),
    config.ROLE_COMPACT: ("none", "none"),
}


def _role_default(role: str, kind: str, base: str) -> str:
    env = os.environ.get(f"GM_{role.upper()}_REASONING_{kind.upper()}", base)
    return env.strip().lower() or base


_DEFAULTS: dict[str, Any] = {}
for _role_key in REASONING_ROLES:
    _eff_base, _sum_base = _ROLE_REASONING_BASE.get(_role_key, ("none", "none"))
    _DEFAULTS[f"{_role_key}_reasoning_effort"] = _role_default(_role_key, "effort", _eff_base)
    _DEFAULTS[f"{_role_key}_reasoning_summary"] = _role_default(_role_key, "summary", _sum_base)
_DEFAULTS.update({
    "text_verbosity": os.environ.get("GM_TEXT_VERBOSITY", "default").strip().lower()
    or "default",
    "tool_choice": os.environ.get("GM_TOOL_CHOICE", "auto").strip().lower() or "auto",
    "parallel_tool_calls": config._env_bool("GM_PARALLEL_TOOL_CALLS", True),
    "gm_suggest_options": config._env_bool("GM_SUGGEST_OPTIONS", False),
    "max_output_tokens": int(config.MAX_TOKENS or 0),
})

_SETTINGS_PATH = Path(
    os.environ.get("GM_SETTINGS_PATH")
    or Path(__file__).with_name("gm_lab_settings.json")
)
_LOCK = threading.RLock()
_CACHE: dict[str, Any] | None = None


def settings_path() -> Path:
    return _SETTINGS_PATH


def defaults() -> dict[str, Any]:
    return dict(_DEFAULTS)


def options() -> dict[str, Any]:
    return {
        "reasoning_efforts": list(REASONING_EFFORTS),
        "reasoning_summaries": list(REASONING_SUMMARIES),
        "reasoning_roles": list(REASONING_ROLES),
        "text_verbosities": list(TEXT_VERBOSITIES),
        "tool_choices": list(TOOL_CHOICES),
        "max_output_tokens_max": config.MAX_OUTPUT_TOKENS_CAP,
    }


def get() -> dict[str, Any]:
    global _CACHE
    with _LOCK:
        if _CACHE is None:
            _CACHE = _load()
        return dict(_CACHE)


def update(values: dict[str, Any] | None) -> dict[str, Any]:
    global _CACHE
    with _LOCK:
        current = get()
        current.update(_clean(values or {}, base=current))
        _CACHE = _normalize(current)
        _save(_CACHE)
        return dict(_CACHE)


def reconcile_for_model(model: dict[str, Any] | None) -> dict[str, Any]:
    """Adjust selected reasoning settings only when model metadata makes them invalid."""
    if not isinstance(model, dict):
        return get()
    settings = get()
    supports = model.get("supports_reasoning_summaries")
    supported = supported_reasoning_efforts(model)
    next_settings: dict[str, Any] = {}

    if supports is False:
        supported = []
    default_effort = (
        _string(model.get("default_reasoning_level")).lower()
        or _string(model.get("default_reasoning_effort")).lower()
        or (supported[0] if supported else "none")
    )
    default_summary = (
        _string(model.get("default_reasoning_summary")).lower()
        or _BASE_REASONING_SUMMARY
    )
    if default_summary not in REASONING_SUMMARIES:
        default_summary = "auto"
    for role in REASONING_ROLES:
        effort_key = f"{role}_reasoning_effort"
        summary_key = f"{role}_reasoning_summary"
        effort = _string(settings.get(effort_key)).lower()
        if supports is False and effort != "none":
            next_settings[effort_key] = "none"
            next_settings[summary_key] = "none"
        elif supported and effort not in supported and effort != "none":
            next_settings[effort_key] = default_effort
            if default_effort == "none":
                next_settings[summary_key] = "none"
            elif settings.get(summary_key) == "none":
                next_settings[summary_key] = default_summary
        elif effort == "none" and settings.get(summary_key) != "none":
            next_settings[summary_key] = "none"
    if next_settings:
        return update(next_settings)
    return settings


def supported_reasoning_efforts(model: dict[str, Any] | None) -> list[str]:
    if not isinstance(model, dict):
        return []
    raw = (
        model.get("supported_reasoning_levels")
        or model.get("supported_reasoning_efforts")
        or model.get("reasoning_efforts")
        or []
    )
    out: list[str] = []
    if isinstance(raw, list):
        for item in raw:
            effort = ""
            if isinstance(item, dict):
                effort = _string(item.get("effort") or item.get("id") or item.get("value"))
            else:
                effort = _string(item)
            if effort and effort not in out:
                out.append(effort)
    return out


def role_settings(role: str, settings: dict[str, Any] | None = None) -> dict[str, str]:
    role = _role(role)
    values = settings or get()
    effort = _string(values.get(f"{role}_reasoning_effort")).lower() or "none"
    summary = _string(values.get(f"{role}_reasoning_summary")).lower() or "none"
    if effort == "none":
        summary = "none"
    if summary not in REASONING_SUMMARIES:
        summary = "none"
    return {"effort": effort, "summary": summary}


def reasoning_enabled(think: bool | None, role: str = "") -> bool:
    return bool(think) and role_settings(role)["effort"] != "none"


def reasoning_for_request(think: bool | None, role: str = "") -> dict[str, str] | None:
    if not bool(think):
        return None
    values = role_settings(role)
    if values["effort"] == "none":
        return None
    out = {"effort": values["effort"]}
    if values["summary"] != "none":
        out["summary"] = values["summary"]
    return out


def role_reasoning_enabled(role: str) -> bool:
    return role_settings(role)["effort"] != "none"


def tool_choice_for_request(has_tools: bool) -> str:
    if not has_tools:
        return "none"
    choice = _string(get().get("tool_choice")).lower()
    return choice if choice in TOOL_CHOICES else "auto"


def parallel_tool_calls_for_request(has_tools: bool) -> bool:
    return bool(has_tools and get().get("parallel_tool_calls", True))


def gm_suggest_options_enabled(settings: dict[str, Any] | None = None) -> bool:
    values = settings or get()
    return bool(values.get("gm_suggest_options", False))


def max_output_tokens() -> int:
    try:
        return max(0, int(get().get("max_output_tokens") or 0))
    except (TypeError, ValueError):
        return 0


def _load() -> dict[str, Any]:
    try:
        data = json.loads(_SETTINGS_PATH.read_text(encoding="utf-8"))
    except FileNotFoundError:
        return _normalize({})
    except Exception:
        return _normalize({})
    return _normalize(data if isinstance(data, dict) else {})


def _save(data: dict[str, Any]) -> None:
    _SETTINGS_PATH.parent.mkdir(parents=True, exist_ok=True)
    body = json.dumps(data, ensure_ascii=False, indent=2, sort_keys=True)
    fd, tmp_name = tempfile.mkstemp(
        prefix=_SETTINGS_PATH.name + ".",
        suffix=".tmp",
        dir=str(_SETTINGS_PATH.parent),
        text=True,
    )
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as f:
            f.write(body)
            f.write("\n")
        os.replace(tmp_name, _SETTINGS_PATH)
    finally:
        try:
            os.unlink(tmp_name)
        except FileNotFoundError:
            pass


def _normalize(data: dict[str, Any]) -> dict[str, Any]:
    normalized = dict(_DEFAULTS)
    normalized.update(_clean(_migrate_legacy_reasoning(data), base=normalized))
    normalized.update(_clean(data, base=normalized))
    for role in REASONING_ROLES:
        effort_key = f"{role}_reasoning_effort"
        summary_key = f"{role}_reasoning_summary"
        if normalized.get(effort_key) == "none":
            normalized[summary_key] = "none"
    return normalized


def _clean(data: dict[str, Any], base: dict[str, Any]) -> dict[str, Any]:
    out: dict[str, Any] = {}

    for role in REASONING_ROLES:
        effort_key = f"{role}_reasoning_effort"
        summary_key = f"{role}_reasoning_summary"
        if effort_key in data:
            effort = _string(data.get(effort_key)).lower()
            # Codex accepts custom non-empty effort values from model catalogs.
            if effort:
                out[effort_key] = effort
        if summary_key in data:
            summary = _string(data.get(summary_key)).lower()
            if summary in REASONING_SUMMARIES:
                out[summary_key] = summary

    if "text_verbosity" in data:
        verbosity = _string(data.get("text_verbosity")).lower()
        if verbosity in TEXT_VERBOSITIES:
            out["text_verbosity"] = verbosity

    if "tool_choice" in data:
        tool_choice = _string(data.get("tool_choice")).lower()
        if tool_choice in TOOL_CHOICES:
            out["tool_choice"] = tool_choice

    if "parallel_tool_calls" in data:
        out["parallel_tool_calls"] = _bool(data.get("parallel_tool_calls"))

    if "gm_suggest_options" in data:
        out["gm_suggest_options"] = _bool(data.get("gm_suggest_options"))

    if "max_output_tokens" in data:
        try:
            out["max_output_tokens"] = min(
                config.MAX_OUTPUT_TOKENS_CAP, max(0, int(data.get("max_output_tokens") or 0))
            )
        except (TypeError, ValueError):
            out["max_output_tokens"] = int(base.get("max_output_tokens") or 0)

    return out


def _migrate_legacy_reasoning(data: dict[str, Any]) -> dict[str, Any]:
    """Map the old global reasoning settings to GM and NPC only.

    Compact used to run with reasoning disabled, so it keeps the new compact default
    unless the settings file explicitly contains compact_* keys.
    """
    out: dict[str, Any] = {}
    legacy_effort = _string(data.get("reasoning_effort")).lower()
    legacy_summary = _string(data.get("reasoning_summary")).lower()
    for role in (config.ROLE_GM, config.ROLE_NPC):
        effort_key = f"{role}_reasoning_effort"
        summary_key = f"{role}_reasoning_summary"
        if legacy_effort and effort_key not in data:
            out[effort_key] = legacy_effort
        if legacy_summary and summary_key not in data:
            out[summary_key] = legacy_summary
    return out


def _role(role: str) -> str:
    role = _string(role).lower()
    return role if role in REASONING_ROLES else config.ROLE_GM


def _string(value: Any) -> str:
    return str(value or "").strip()


def _bool(value: Any) -> bool:
    if isinstance(value, bool):
        return value
    if value is None:
        return False
    return str(value).strip().lower() not in ("0", "false", "no", "off", "")
