"""Оркестратор хода: игрок -> ГМ -> (tool) ask_npc -> NPC -> критик -> [доп.раунд] -> ГМ.

run_turn — генератор событий для веб-интерфейса и CLI/smoke-прогонов.
"""
from __future__ import annotations

from dataclasses import dataclass
import json
import re
import time

import config
import agents
import prompts
import world as world_mod
from llm_client import extract_json_string, json_unescape, make_client

# Грубая оценка размера системного промпта ГМ в токенах (~CHARS_PER_TOKEN символа/токен).
_SYS_EST = len(prompts.GM_SYSTEM) // config.CHARS_PER_TOKEN


def ev(kind, agent, data=None, sid=None):
    return {"kind": kind, "agent": agent, "data": data, "sid": sid}


_OMIT = object()


@dataclass(frozen=True)
class ToolExecutionResult:
    full: str
    model: str


def _json_compact(data) -> str:
    return json.dumps(data, ensure_ascii=False, separators=(",", ":"))


def _tool_result(full: str, model: str | None = None) -> ToolExecutionResult:
    full = str(full or "")
    return ToolExecutionResult(full=full, model=str(model if model is not None else full))


def _tool_full_text(result) -> str:
    return result.full if isinstance(result, ToolExecutionResult) else str(result or "")


def _tool_model_text(result) -> str:
    return result.model if isinstance(result, ToolExecutionResult) else str(result or "")


def _compact_sources(sources: list, limit: int = 3) -> list:
    out = []
    for source in sources[:limit] if isinstance(sources, list) else []:
        if not isinstance(source, dict):
            continue
        row = {}
        for key in ("n", "kind", "status", "source"):
            if key in source:
                row[key] = source[key]
        if row:
            out.append(row)
    return out


def _compact_world_fact_payload(payload: dict) -> dict:
    out = {
        "status": payload.get("status", "unknown"),
        "text": payload.get("text", ""),
    }
    sources = _compact_sources(payload.get("sources") or [])
    if sources:
        out["sources"] = sources
    return out


def _compact_tool_search_payload(payload: dict) -> dict:
    out = {
        "loaded_tools": list(payload.get("loaded_tools") or []),
        "missing": list(payload.get("missing") or []),
    }
    if not out["loaded_tools"] and payload.get("message"):
        out["message"] = payload["message"]
    return out


def _compact_whereabouts_payload(payload: dict) -> dict:
    out = {}
    for key in ("npc_id", "name", "present", "current_scene", "whereabouts"):
        if key in payload:
            out[key] = payload[key]
    return out


def _compact_presence_payload(payload: dict) -> dict:
    out = {}
    for key in ("npc_id", "name", "present", "scene", "whereabouts"):
        if key in payload:
            out[key] = payload[key]
    return out


def _compact_scene_item(item: dict) -> dict:
    out = {}
    for key in ("item_id", "name", "visible", "portable"):
        if key in item:
            out[key] = item[key]
    return out


def _compact_scene_exit(exit_: dict) -> dict:
    out = {}
    for key in ("exit_id", "name", "destination", "visible", "blocked_by"):
        if key in exit_:
            out[key] = exit_[key]
    return out


def _compact_scene_payload(payload: dict) -> dict:
    out = {}
    for key in (
        "scene_id", "location_id", "title", "present_npcs", "constraints",
        "tension", "dropped_present_npcs", "repair_hint",
    ):
        if key in payload:
            out[key] = payload[key]
    items = [_compact_scene_item(item) for item in payload.get("items") or [] if isinstance(item, dict)]
    exits = [_compact_scene_exit(exit_) for exit_ in payload.get("exits") or [] if isinstance(exit_, dict)]
    if items:
        out["items"] = items
    if exits:
        out["exits"] = exits
    return out


def _compact_ask_npc_payload(payload: dict) -> dict:
    out = {}
    for key in ("npc_id", "npc_name", "speech_ru", "action_ru"):
        if key in payload:
            out[key] = payload[key]
    out["already_emitted"] = True
    out["final_narration_rule"] = (
        "Do not rewrite, retell, paraphrase, embellish, or mention this NPC's "
        "speech/action/body/emotion again. Final narration may add only non-NPC "
        "scene consequences; output empty if none. For another named NPC reaction, "
        "call ask_npc for that NPC."
    )
    return out


def _clean_text(value) -> str:
    if value is None:
        return ""
    if isinstance(value, str):
        return value.strip()
    return str(value).strip()


def _clean_list(value) -> list[str]:
    if not isinstance(value, list):
        return []
    out = []
    for item in value:
        text = _clean_text(item)
        if text:
            out.append(text)
    return out


def _is_empty_value(value) -> bool:
    return value is None or value == "" or value == [] or value == {}


def _drop_empty(value):
    if isinstance(value, dict):
        out = {}
        for key, child in value.items():
            clean = _drop_empty(child)
            if not _is_empty_value(clean):
                out[key] = clean
        return out
    if isinstance(value, list):
        out = []
        for child in value:
            clean = _drop_empty(child)
            if not _is_empty_value(clean):
                out.append(clean)
        return out
    return value


def _normalize_update_world_state_args(value: dict) -> dict:
    items = value.get("items")
    if not isinstance(items, list):
        return {}
    clean_items = []
    for item in items:
        if not isinstance(item, dict):
            clean_items.append(item)
            continue
        clean_item = {}
        for key, child in item.items():
            if key in {"type", "text"} or not _is_empty_value(child):
                clean_item[key] = child
        clean_items.append(clean_item)
    return {"items": clean_items}


def _clip_text(value, limit: int = 700) -> str:
    text = _clean_text(value)
    if len(text) <= limit:
        return text
    return text[:limit].rstrip() + "..."


def _compact_world_state_update_payload(payload: dict) -> dict:
    applied = []
    for row in payload.get("applied") or []:
        if not isinstance(row, dict):
            continue
        compact = {}
        for source_key, target_key in (
            ("index", "i"),
            ("op", "op"),
            ("type", "type"),
            ("id", "id"),
            ("npc_id", "npc_id"),
            ("target", "target"),
            ("scope", "scope"),
            ("mode", "mode"),
            ("hash", "hash"),
            ("status", "status"),
        ):
            if source_key in row:
                compact[target_key] = row[source_key]
        applied.append(compact)
    errors = []
    for row in payload.get("errors") or []:
        if not isinstance(row, dict):
            continue
        errors.append(_drop_empty({
            "i": row.get("index"),
            "op": row.get("op"),
            "type": row.get("type"),
            "id": row.get("id"),
            "npc_id": row.get("npc_id"),
            "target": row.get("target"),
            "scope": row.get("scope"),
            "existing_id": row.get("existing_id"),
            "existing_hash": row.get("existing_hash"),
            "expected_hash": row.get("expected_hash"),
            "actual_hash": row.get("actual_hash"),
            "status": row.get("status"),
            "error": row.get("error"),
        }))
    return _drop_empty({
        "ok": bool(payload.get("ok")),
        "applied": applied,
        "errors": errors,
    })


def _compact_world_query_payload(payload: dict) -> dict:
    rows = []
    for row in payload.get("results") or []:
        if not isinstance(row, dict):
            continue
        rows.append(_drop_empty({
            "kind": row.get("kind"),
            "id": row.get("id"),
            "npc_id": row.get("npc_id"),
            "target": row.get("target"),
            "scope": row.get("scope") or row.get("visibility"),
            "status": row.get("status"),
            "hash": row.get("hash"),
            "text": _clip_text(row.get("text"), 500),
        }))
    out = {
        "scope": payload.get("scope"),
        "status": payload.get("status"),
        "text": _clip_text(payload.get("text"), 500),
        "results": rows,
        "sources": _compact_sources(payload.get("sources") or []),
        "error": payload.get("error"),
    }
    return _drop_empty(out)


def _visibility(value, default: str) -> str:
    raw = _clean_text(value).lower().replace("-", "_")
    aliases = {
        "public": "player",
        "player_safe": "player",
        "player_private": "shared",
        "private_player": "shared",
        "participants": "shared",
        "participant": "shared",
        "truth": "gm",
        "gm_truth": "gm",
        "private": "npc",
        "npc_private": "npc",
    }
    raw = aliases.get(raw, raw)
    return raw if raw in {"player", "gm", "npc", "shared"} else default


def _resolve_npc_id(world: world_mod.World, raw: str) -> tuple[str, str]:
    npc_ref = _clean_text(raw)
    if not npc_ref:
        return "", "npc_id is required"
    try:
        return world.resolve(npc_ref).npc_id, ""
    except KeyError as e:
        return "", str(e)


def _append_npc_memory(session: "Session", npc_id: str, text: str) -> None:
    session.commitments.setdefault(npc_id, []).append(text)
    session.commitments[npc_id] = session.commitments[npc_id][-_COMMIT_BLOCKS:]


def _append_source(text: str, source: str) -> str:
    return f"{text} (источник: {source})" if source else text


def _supports_state_records(world: world_mod.World) -> bool:
    return callable(getattr(world, "add_state_records", None))


def _state_record_kind(item_type: str) -> str:
    return "goal" if item_type == "goals" else item_type


def _state_record_scope(scope: str) -> str:
    return {
        "player": "public",
        "gm": "gm",
        "npc": "owner",
        "shared": "participants",
    }.get(scope, "public")


def _state_visibility_from_scope(scope: str) -> str:
    return {"public": "player", "gm": "gm", "participants": "shared"}.get(_clean_text(scope), "npc")


def _state_record_by_id(world: world_mod.World, record_id: str):
    wanted = _clean_text(record_id)
    if not wanted:
        return None
    for record in getattr(world, "state_records", []) or []:
        if getattr(record, "record_id", "") == wanted:
            return record
    return None


def _state_record_hash(record) -> str:
    hasher = getattr(world_mod, "state_record_hash", None)
    if callable(hasher):
        return hasher(record)
    return ""


def _expected_hash(item: dict) -> str:
    return _clean_text(
        item.get("expected_hash")
        or item.get("expectedHash")
        or item.get("record_hash")
        or item.get("hash")
    )


def _hash_conflict_error(index: int, op: str, item_type: str, record, expected_hash: str) -> dict:
    actual_hash = _state_record_hash(record)
    return {
        "index": index,
        "op": op,
        "type": item_type or getattr(record, "kind", "state"),
        "id": getattr(record, "record_id", ""),
        "npc_id": getattr(record, "owner", ""),
        "target": getattr(record, "subject", ""),
        "scope": _state_visibility_from_scope(getattr(record, "scope", "")),
        "expected_hash": expected_hash,
        "actual_hash": actual_hash,
        "status": "conflict",
        "error": "record changed; not applied. Re-query world state and retry with the current hash.",
    }


def _apply_state_record_item(
    session: "Session",
    index: int,
    op: str,
    item_type: str,
    text: str,
    scope: str,
    source: str,
    item: dict,
) -> tuple[dict | None, dict | None]:
    world = session.world
    record_id = _clean_text(item.get("id") or item.get("record_id"))
    if op == "delete":
        if not record_id:
            return None, {"index": index, "op": op, "type": item_type, "error": "id is required for delete"}
        record = _state_record_by_id(world, record_id)
        if record is not None:
            expected_hash = _expected_hash(item)
            if expected_hash and not expected_hash.lower() == _state_record_hash(record).lower():
                return None, _hash_conflict_error(index, op, item_type, record, expected_hash)
        deleted = world.delete_state_records([record_id], hard=False)
        if not deleted:
            return None, {"index": index, "op": op, "type": item_type, "id": record_id, "error": "record id not found or already inactive"}
        return {
            "index": index,
            "op": op,
            "type": item_type or "state",
            "id": record_id,
            "hash": _state_record_hash(record) if record is not None else "",
            "status": "deleted",
        }, None

    if op == "update":
        if not record_id:
            return None, {"index": index, "op": op, "type": item_type, "error": "id is required for update"}
        existing_record = _state_record_by_id(world, record_id)
        if existing_record is None:
            return None, {"index": index, "op": op, "type": item_type, "id": record_id, "error": "record id not found"}
        expected_hash = _expected_hash(item)
        if expected_hash and not expected_hash.lower() == _state_record_hash(existing_record).lower():
            return None, _hash_conflict_error(index, op, item_type, existing_record, expected_hash)
        update_payload = {"id": record_id}
        if item_type:
            update_payload["kind"] = _state_record_kind(item_type)
        if text:
            update_payload["text"] = text
        if item.get("scope") or item.get("visibility"):
            update_payload["scope"] = _state_record_scope(scope)
            if scope == "npc" and not _clean_text(item.get("npc_id")):
                return None, {
                    "index": index,
                    "op": op,
                    "type": item_type,
                    "id": record_id,
                    "error": "npc_id is required when changing scope to npc",
                }
            if scope == "shared" and (
                not _clean_text(item.get("npc_id")) or not _clean_text(item.get("target"))
            ):
                return None, {
                    "index": index,
                    "op": op,
                    "type": item_type,
                    "id": record_id,
                    "error": "npc_id and target are required when changing scope to shared",
                }
        owner = ""
        if item.get("npc_id"):
            owner, error = _resolve_npc_id(world, item.get("npc_id", ""))
            if error:
                return None, {"index": index, "op": op, "type": item_type, "id": record_id, "error": error}
        target = _clean_text(item.get("target"))
        if owner:
            update_payload["owner"] = owner
        if target:
            update_payload["subject"] = target
        if source:
            update_payload["source"] = source
        if "active" in item:
            update_payload["active"] = item.get("active")
        records = world.update_state_records([update_payload])
        if not records:
            return None, {"index": index, "op": op, "type": item_type, "id": record_id, "error": "record id not found"}
        record = records[0]
        return {
            "index": index,
            "op": op,
            "type": record.kind,
            "id": record.record_id,
            "npc_id": record.owner,
            "target": record.subject,
            "scope": _state_visibility_from_scope(record.scope),
            "hash": _state_record_hash(record),
            "status": "updated",
        }, None

    owner = ""
    target = _clean_text(item.get("target"))
    needs_npc = item_type in {"npc_memory", "relationship", "goal", "goals"} or scope in {"npc", "shared"}
    if needs_npc:
        owner, error = _resolve_npc_id(world, item.get("npc_id", ""))
        if error:
            return None, {"index": index, "op": op, "type": item_type, "error": error}
    if item_type == "relationship" and not target:
        return None, {
            "index": index,
            "op": op,
            "type": item_type,
            "npc_id": owner,
            "error": "target is required for relationship",
        }
    if scope == "shared" and not target:
        return None, {
            "index": index,
            "op": op,
            "type": item_type,
            "error": "target is required for shared scope",
        }

    if item_type == "relationship":
        state_records_for = getattr(world, "state_records_for", None)
        if callable(state_records_for):
            existing = state_records_for(
                "debug",
                kinds=("relationship",),
                owner=owner,
                subject=target,
                scopes=(_state_record_scope(scope),),
            )
            if existing:
                return None, {
                    "index": index,
                    "op": op,
                    "type": item_type,
                    "npc_id": owner,
                    "target": target,
                    "scope": scope,
                    "existing_id": existing[0].record_id,
                    "existing_hash": _state_record_hash(existing[0]),
                    "status": "not_added",
                    "error": "not added: active relationship already exists; use op=update with existing_id and existing_hash",
                }

    mode = _clean_text(item.get("mode")).lower()
    if item_type in {"goal", "goals"} and mode == "replace":
        existing = []
        state_records_for = getattr(world, "state_records_for", None)
        if callable(state_records_for):
            existing = state_records_for("debug", kinds=("goal",), owner=owner)
        delete_state_records = getattr(world, "delete_state_records", None)
        if callable(delete_state_records) and existing:
            delete_state_records([record.record_id for record in existing], hard=False)

    status = "unconfirmed" if item_type == "rumor" else "known"
    record_payload = {
        "kind": _state_record_kind(item_type),
        "text": text,
        "scope": _state_record_scope(scope),
        "owner": owner,
        "subject": target,
        "source": source or "gm_tool",
        "status": status,
    }
    records = world.add_state_records([record_payload])
    if not records:
        return None, {"index": index, "op": op, "type": item_type, "error": "state record was not stored"}
    record = records[0]
    return {
        "index": index,
        "op": op,
        "type": item_type,
        "id": record.record_id,
        "npc_id": owner,
        "target": target,
        "scope": scope,
        "mode": mode if item_type in {"goal", "goals"} and mode == "replace" else "",
        "hash": _state_record_hash(record),
        "status": "stored",
        "text": record.text,
    }, None


def _apply_world_state_item(session: "Session", index: int, item: dict) -> tuple[dict | None, dict | None]:
    world = session.world
    op = _clean_text(item.get("op")).lower() or "add"
    if op not in {"add", "update", "delete"}:
        return None, {"index": index, "op": op, "error": "unsupported op"}
    item_type = _clean_text(item.get("type")).lower()
    text = _clean_text(item.get("text"))
    source = _clean_text(item.get("source"))
    if item_type == "goals":
        item_type = "goal"
    if op == "delete":
        if _supports_state_records(world):
            return _apply_state_record_item(session, index, op, item_type, text, "player", source, item)
        return None, {"index": index, "op": op, "type": item_type, "error": "delete requires state-record support"}
    if item_type and item_type not in {"fact", "rumor", "npc_memory", "relationship", "goal"}:
        return None, {"index": index, "type": item_type, "error": "unsupported item type"}
    if op == "add" and not item_type:
        return None, {"index": index, "op": op, "error": "type is required for add"}
    if op == "add" and not text:
        return None, {"index": index, "op": op, "type": item_type, "error": "text is required"}

    default_scope = "player" if item_type in {"fact", "rumor"} else "npc"
    scope = _visibility(item.get("scope", item.get("visibility")), default_scope)
    if _supports_state_records(world):
        return _apply_state_record_item(session, index, op, item_type, text, scope, source, item)

    if op != "add":
        return None, {"index": index, "op": op, "type": item_type, "error": "update/delete requires state-record support"}

    if item_type == "fact":
        if scope == "npc":
            npc_id, error = _resolve_npc_id(world, item.get("npc_id", ""))
            if error:
                return None, {"index": index, "type": item_type, "error": error}
            _append_npc_memory(session, npc_id, _append_source(f"Факт: {text}", source))
            return {
                "index": index, "type": item_type, "npc_id": npc_id,
                "scope": scope, "status": "stored",
            }, None
        record = world.add_fact(text, "truth" if scope == "gm" else "public")
        if record is None:
            return None, {"index": index, "type": item_type, "error": "fact was empty"}
        if source:
            record.source = source
        return {
            "index": index, "type": item_type, "id": record.fact_id,
            "scope": scope, "status": "stored", "text": record.text,
        }, None

    if item_type == "rumor":
        if scope == "player":
            speaker_id = _clean_text(item.get("npc_id")) or source or "gm"
            if speaker_id in world.npcs:
                speaker_id = world.npcs[speaker_id].npc_id
            elif item.get("npc_id"):
                try:
                    speaker_id = world.resolve(speaker_id).npc_id
                except KeyError:
                    speaker_id = _clean_text(item.get("npc_id")) or "gm"
            witnesses = set(_clean_list(item.get("witnesses")) or ["player"])
            if "player" not in witnesses:
                witnesses.add("player")
            seq = session._next_seq()
            world.record_rumor(seq, session.turn, speaker_id, text, frozenset(witnesses))
            return {
                "index": index, "type": item_type, "id": f"rumor:{len(world.rumors)}",
                "scope": scope, "status": "stored",
            }, None
        if scope == "npc":
            npc_id, error = _resolve_npc_id(world, item.get("npc_id", ""))
            if error:
                return None, {"index": index, "type": item_type, "error": error}
            _append_npc_memory(session, npc_id, _append_source(f"Слух: {text}", source))
            return {
                "index": index, "type": item_type, "npc_id": npc_id,
                "scope": scope, "status": "stored",
            }, None
        world.hidden_events.append(_append_source(f"GM-only rumor: {text}", source))
        return {
            "index": index, "type": item_type, "id": f"hidden:{len(world.hidden_events)}",
            "scope": scope, "status": "stored",
        }, None

    npc_id, error = _resolve_npc_id(world, item.get("npc_id", ""))
    if error:
        return None, {"index": index, "type": item_type, "error": error}

    if item_type == "npc_memory":
        _append_npc_memory(session, npc_id, _append_source(f"Память: {text}", source))
        return {
            "index": index, "type": item_type, "npc_id": npc_id,
            "scope": "npc", "status": "stored",
        }, None

    if item_type == "relationship":
        target = _clean_text(item.get("target")) or "unknown"
        _append_npc_memory(session, npc_id, _append_source(f"Отношение к {target}: {text}", source))
        return {
            "index": index, "type": item_type, "npc_id": npc_id,
            "target": target, "scope": "npc", "status": "stored",
        }, None

    mode = _clean_text(item.get("mode")).lower()
    if mode not in {"replace", "append"}:
        mode = "append"
    npc = world.npc(npc_id)
    current = _clean_text(npc.goals)
    updated = text if mode == "replace" or not current else current + "\n" + text
    world.update_npc(npc_id, {"goals": updated})
    return {
        "index": index, "type": item_type, "npc_id": npc_id,
        "scope": "npc", "mode": mode, "status": "stored",
    }, None


def _apply_world_state_batch(session: "Session", args: dict) -> dict:
    items = args.get("items")
    if not isinstance(items, list):
        return {"ok": False, "applied": [], "errors": [{"index": 0, "error": "items[] is required"}]}
    applied = []
    errors = []
    for index, raw_item in enumerate(items, start=1):
        if not isinstance(raw_item, dict):
            errors.append({"index": index, "error": "item must be an object"})
            continue
        row, error = _apply_world_state_item(session, index, raw_item)
        if row:
            applied.append(row)
        if error:
            errors.append(error)
    return _drop_empty({"ok": not errors, "applied": applied, "errors": errors})


def _query_terms(query: str) -> list[str]:
    return [
        term for term in re.findall(r"[\wа-яА-ЯёЁ-]+", _clean_text(query).lower())
        if len(term) > 1
    ]


def _score_query_text(query: str, terms: list[str], text: str) -> int:
    haystack = _clean_text(text).lower()
    if not haystack:
        return 0
    score = 0
    clean_query = _clean_text(query).lower()
    if clean_query and clean_query in haystack:
        score += 100
    for term in terms:
        if term in haystack:
            score += 10
    return score


def _query_row(kind: str, text: str, **extra) -> dict:
    row = {"kind": kind, "text": _clip_text(text)}
    row.update(extra)
    return _drop_empty(row)


def _scored_rows(query: str, rows: list[dict], limit: int) -> list[dict]:
    terms = _query_terms(query)
    scored = []
    for row in rows:
        search_text = " ".join(
            _clean_text(row.get(key))
            for key in ("kind", "id", "npc_id", "target", "scope", "visibility", "status", "text")
            if _clean_text(row.get(key))
        )
        score = _score_query_text(query, terms, search_text)
        if score > 0:
            scored.append((score, row))
    scored.sort(key=lambda item: item[0], reverse=True)
    return [row for _score, row in scored[:limit]]


def _player_query_payload(world: world_mod.World, query: str, limit: int = 5) -> dict:
    fact = world.fact(query, actor_id="player")
    payload = fact.as_tool_payload()
    rows = _scored_rows(query, _state_record_rows(world, "player"), limit)
    status = payload.get("status", "unknown")
    if rows and status == "unknown":
        status = "known"
    out = {
        "scope": "player",
        "status": status,
        "text": payload.get("text", ""),
        "results": rows,
        "sources": _compact_sources(payload.get("sources") or []),
    }
    return _drop_empty(out)


def _state_record_rows(world: world_mod.World, actor_id: str) -> list[dict]:
    state_records_for = getattr(world, "state_records_for", None)
    if not callable(state_records_for):
        return []
    rows = []
    for record in state_records_for(actor_id):
        rows.append(_query_row(
            f"state_{record.kind}",
            record.text,
            id=record.record_id,
            npc_id=record.owner,
            target=record.subject,
            visibility=_state_visibility_from_scope(record.scope),
            status=record.status,
            hash=_state_record_hash(record),
        ))
    return rows


def _gm_query_rows(session: "Session") -> list[dict]:
    world = session.world
    rows = [
        _query_row("public_intro", world.public, visibility="player"),
        _query_row("gm_canon", world.canon, visibility="gm"),
    ]
    rows.extend(_state_record_rows(world, "debug"))
    for index, event_text in enumerate(getattr(world, "hidden_events", []) or [], start=1):
        rows.append(_query_row("hidden_event", event_text, id=f"hidden:{index}", visibility="gm"))
    for record in getattr(world, "fact_records", []) or []:
        visibility = "gm" if record.kind == "truth" else "player"
        rows.append(_query_row(
            f"{record.kind}_fact",
            record.text,
            id=record.fact_id,
            visibility=visibility,
            status="known" if record.confirmed else "unconfirmed",
        ))
    for index, rumor in enumerate(getattr(world, "rumors", []) or [], start=1):
        rows.append(_query_row(
            "rumor",
            rumor.text,
            id=f"rumor:{index}",
            npc_id=rumor.speaker,
            visibility="player",
            status="unconfirmed",
        ))
    for npc_id, npc in world.npcs.items():
        rows.extend([
            _query_row("npc_role", f"{npc.name}: {npc.role}", npc_id=npc_id, visibility="player"),
            _query_row("npc_goals", npc.goals, npc_id=npc_id, visibility="npc"),
            _query_row("npc_knowledge", npc.knowledge, npc_id=npc_id, visibility="npc"),
            _query_row("npc_secret", npc.secret, npc_id=npc_id, visibility="npc"),
        ])
        if session.npc_summaries.get(npc_id):
            rows.append(_query_row(
                "npc_summary", session.npc_summaries[npc_id], npc_id=npc_id, visibility="npc"
            ))
        for index, block in enumerate(session.commitments.get(npc_id, [])[-_COMMIT_BLOCKS:], start=1):
            rows.append(_query_row(
                "npc_memory", block, id=f"{npc_id}:memory:{index}",
                npc_id=npc_id, visibility="npc",
            ))
    if session.gm_summary.strip():
        rows.append(_query_row("gm_summary", session.gm_summary, visibility="gm"))
    return rows


def _npc_query_rows(session: "Session", npc_id: str) -> list[dict]:
    npc = session.world.npc(npc_id)
    rows = _state_record_rows(session.world, npc_id)
    rows.extend([
        _query_row("npc_goals", npc.goals, npc_id=npc_id, visibility="npc"),
        _query_row("npc_knowledge", npc.knowledge, npc_id=npc_id, visibility="npc"),
        _query_row("npc_secret", npc.secret, npc_id=npc_id, visibility="npc"),
    ])
    if session.npc_summaries.get(npc_id):
        rows.append(_query_row("npc_summary", session.npc_summaries[npc_id], npc_id=npc_id, visibility="npc"))
    for index, block in enumerate(session.commitments.get(npc_id, [])[-_COMMIT_BLOCKS:], start=1):
        rows.append(_query_row(
            "npc_memory", block, id=f"{npc_id}:memory:{index}",
            npc_id=npc_id, visibility="npc",
        ))
    return rows


def _query_world_state(session: "Session", args: dict) -> dict:
    scope = _visibility(args.get("scope"), "player")
    query = _clean_text(args.get("query"))
    if not query:
        return {"scope": scope, "status": "error", "error": "query is required"}
    try:
        limit = max(1, min(int(args.get("max_results") or 5), 12))
    except (TypeError, ValueError):
        limit = 5

    if scope == "player":
        return _player_query_payload(session.world, query, limit)

    if scope == "npc":
        npc_id, error = _resolve_npc_id(session.world, args.get("npc_id", ""))
        if error:
            return {"scope": scope, "status": "error", "error": error}
        rows = _scored_rows(query, _npc_query_rows(session, npc_id), limit)
        public_payload = session.world.fact(query, actor_id="public").as_tool_payload()
        public_text = _clean_text(public_payload.get("text"))
        if public_payload.get("status") != "unknown" and public_text:
            rows.insert(0, _query_row(
                "public_lookup",
                public_text,
                visibility="player",
                status=public_payload.get("status", ""),
            ))
        return _drop_empty({
            "scope": scope,
            "status": "known" if rows else "unknown",
            "results": rows[:limit],
            "text": "" if rows else "Nothing in this NPC scope matched the query.",
        })

    rows = _scored_rows(query, _gm_query_rows(session), limit)
    return _drop_empty({
        "scope": "gm",
        "status": "known" if rows else "unknown",
        "results": rows,
        "text": "" if rows else "Nothing in GM scope matched the query.",
    })


def _schema_types(schema: dict) -> set[str]:
    typ = schema.get("type") if isinstance(schema, dict) else None
    if isinstance(typ, str):
        return {typ}
    if isinstance(typ, list):
        return {str(item) for item in typ}
    return set()


def _compact_tool_value(schema: dict, value, required: bool):
    if value is None:
        return None if required else _OMIT
    if not isinstance(schema, dict):
        return value

    props = schema.get("properties")
    types = _schema_types(schema)
    if "object" in types or isinstance(props, dict):
        if not isinstance(value, dict):
            return value
        props = props if isinstance(props, dict) else {}
        required_keys = set(schema.get("required") or [])
        out = {}
        for key, prop_schema in props.items():
            if key not in value:
                continue
            child = _compact_tool_value(prop_schema, value.get(key), key in required_keys)
            if child is not _OMIT:
                out[key] = child
        if required or out:
            return out
        return _OMIT

    if "array" in types:
        if not isinstance(value, list):
            return value
        item_schema = schema.get("items") if isinstance(schema.get("items"), dict) else None
        out = []
        for item in value:
            child = _compact_tool_value(item_schema, item, True) if item_schema else item
            if child is not _OMIT:
                out.append(child)
        return out

    return value


def _tool_parameters_schema(world: world_mod.World | None, name: str) -> dict | None:
    if world is None:
        return None
    tool = agents.gm_tool_catalog(world).get(name)
    fn = tool.get("function") if isinstance(tool, dict) else {}
    schema = fn.get("parameters") if isinstance(fn, dict) else None
    return schema if isinstance(schema, dict) else None


def _normalize_tool_args(name: str, args: dict, parameters_schema: dict | None = None) -> dict:
    """Return model tool arguments in the source schema shape."""
    if not isinstance(args, dict):
        return {}
    if not isinstance(parameters_schema, dict):
        return dict(args)
    normalized = _compact_tool_value(parameters_schema, args, True)
    if not isinstance(normalized, dict):
        return {}
    if name == "update_world_state":
        normalized = _normalize_update_world_state_args(normalized)
    return normalized if isinstance(normalized, dict) else {}


def _normalize_tool_calls(calls: list, world: world_mod.World | None = None, id_prefix: str = "call") -> list:
    out = []
    for index, call in enumerate(calls or [], start=1):
        if not isinstance(call, dict):
            continue
        name = str(call.get("name") or "")
        normalized = dict(call)
        call_id = str(call.get("id") or "").strip()
        normalized["id"] = call_id or f"{id_prefix}_{index}"
        normalized["arguments"] = _normalize_tool_args(
            name,
            call.get("arguments"),
            _tool_parameters_schema(world, name),
        )
        out.append(normalized)
    return out


def _assistant_with_tool_calls(assistant_msg: dict, calls: list) -> dict:
    if not isinstance(assistant_msg, dict) or not calls:
        return assistant_msg
    msg = dict(assistant_msg)
    raw_calls = []
    for call in calls:
        name = str(call.get("name") or "").strip()
        if not name:
            continue
        args = call.get("arguments") if isinstance(call.get("arguments"), dict) else {}
        raw_calls.append({
            "id": str(call.get("id") or ""),
            "type": "function",
            "function": {
                "name": name,
                "arguments": _json_compact(args),
            },
        })
    if raw_calls:
        msg["tool_calls"] = raw_calls
    return msg


def _meta(label, stats, scope="npc"):
    pin = stats.get("prompt_eval_count") or 0
    pout = stats.get("eval_count") or 0
    cached = stats.get("cached_tokens") or 0
    ed = (stats.get("eval_duration") or 0) / 1e9
    pd = (stats.get("prompt_eval_duration") or 0) / 1e9
    td = (stats.get("total_duration") or 0) / 1e9
    ld = (stats.get("load_duration") or 0) / 1e9
    # `scope` ("gm"|"npc"|"other") drives token accounting (add_turn_usage) so that
    # bucketing is decoupled from the human-facing `label` text.
    return {"label": label, "scope": scope, "in": pin, "out": pout, "secs": round(td, 2),
            "cached": cached,
            "tps": round(pout / ed) if ed > 0 else 0,
            "prompt_secs": round(pd, 2), "eval_secs": round(ed, 2), "load_secs": round(ld, 2)}


def _meta_total(metas, total_secs):
    return {"calls": metas, "in": sum(m["in"] for m in metas),
            "out": sum(m["out"] for m in metas),
            "cached": sum(m.get("cached", 0) for m in metas),
            "tokens": sum(m["in"] + m["out"] for m in metas),
            "peak_context": max((m["in"] for m in metas), default=0),
            "secs": total_secs, "sys_estimate": _SYS_EST}


def _empty_usage() -> dict:
    return {
        "turns": 0,
        "calls": 0,
        "in": 0,
        "out": 0,
        "cached": 0,
        "tokens": 0,
        "secs": 0.0,
        "peak_context": 0,
        "gm_calls": 0,
        "gm_tokens": 0,
        "npc_calls": 0,
        "npc_tokens": 0,
        "other_calls": 0,
        "other_tokens": 0,
    }


def _usage_from_payload(value) -> dict:
    usage = _empty_usage()
    if isinstance(value, dict):
        for key in usage:
            if key in value:
                usage[key] = value[key]
    usage["turns"] = int(usage["turns"] or 0)
    usage["calls"] = int(usage["calls"] or 0)
    usage["in"] = int(usage["in"] or 0)
    usage["out"] = int(usage["out"] or 0)
    usage["cached"] = int(usage["cached"] or 0)
    usage["tokens"] = int(usage["tokens"] or 0)
    usage["peak_context"] = int(usage["peak_context"] or 0)
    usage["gm_calls"] = int(usage["gm_calls"] or 0)
    usage["gm_tokens"] = int(usage["gm_tokens"] or 0)
    usage["npc_calls"] = int(usage["npc_calls"] or 0)
    usage["npc_tokens"] = int(usage["npc_tokens"] or 0)
    usage["other_calls"] = int(usage["other_calls"] or 0)
    usage["other_tokens"] = int(usage["other_tokens"] or 0)
    usage["secs"] = round(float(usage["secs"] or 0), 2)
    return usage


def _msg_text(m) -> str:
    role = m.get("role") if isinstance(m, dict) else getattr(m, "role", "")
    content = m.get("content") if isinstance(m, dict) else getattr(m, "content", "")
    return f"{role}: {content}".strip()


def _estimate_tokens(text: str) -> int:
    return max(0, len(text or "") // config.CHARS_PER_TOKEN)


def _messages_tokens(messages: list) -> int:
    return sum(_estimate_tokens(_msg_text(m)) for m in messages or [])


def _world_context_tokens(world: world_mod.World) -> int:
    parts = [
        world.public,
        world.canon,
        "\n".join(world.constraints),
        json.dumps(world.scene_export(), ensure_ascii=False, default=str),
        "\n".join(
            f"{npc.name}: {npc.role}; {npc.pronouns}; {npc.persona}; {npc.knowledge}"
            for npc in world.npcs.values()
        ),
    ]
    return _estimate_tokens("\n".join(str(part or "") for part in parts))


def context_usage(session: "Session") -> dict:
    """Approximate active prompt pressure and the nearest compact threshold."""
    world_tokens = _world_context_tokens(session.world)
    gm_history = _messages_tokens(session.gm_messages)
    gm_summary = _estimate_tokens(session.gm_summary)
    gm_active = _SYS_EST + world_tokens + gm_summary + gm_history
    gm_limit = int(config.GM_HISTORY_TOKENS)
    gm_remaining = max(0, gm_limit - gm_history)

    npc_entries = []
    npc_ids = set(session.world.npcs) | set(session.npc_messages) | set(session.npc_summaries)
    npc_ids |= set(getattr(session, "npc_client_state", {}) or {})
    for npc_id in sorted(npc_ids):
        messages = session.npc_messages.get(npc_id, [])
        npc = session.world.npcs.get(npc_id)
        name = npc.name if npc else npc_id
        history = _messages_tokens(messages)
        summary = _estimate_tokens(session.npc_summaries.get(npc_id, ""))
        persona = _estimate_tokens(
            ""
            if npc is None
            else f"{npc.name} {npc.role} {npc.pronouns} {npc.persona} {npc.voice} {npc.goals} {npc.knowledge}"
        )
        active = world_tokens + persona + summary + history
        has_session = bool(messages or summary or (getattr(session, "npc_client_state", {}) or {}).get(npc_id))
        npc_entries.append({
            "id": npc_id,
            "name": name,
            "color": (npc.color if npc else ""),
            "has_session": has_session,
            "active": active,
            "history": history,
            "summary": summary,
            "limit": int(config.NPC_HISTORY_TOKENS),
            "remaining": max(0, int(config.NPC_HISTORY_TOKENS) - history),
        })
    npc_entries.sort(key=lambda item: (not item["has_session"], -item["history"], item["name"]))
    npc = max(npc_entries, key=lambda item: item["active"], default=None)

    candidates = [{
        "scope": "gm",
        "label": "ГМ",
        "used": gm_history,
        "limit": gm_limit,
        "remaining": gm_remaining,
    }]
    for entry in npc_entries:
        if not entry["has_session"]:
            continue
        candidates.append({
            "scope": "npc",
            "label": entry["name"],
            "used": entry["history"],
            "limit": entry["limit"],
            "remaining": entry["remaining"],
        })
    next_compact = min(candidates, key=lambda item: item["remaining"])
    current = max(gm_active, int((npc or {}).get("active") or 0))
    return {
        "current": current,
        "world": world_tokens,
        "next_compact": next_compact,
        "gm": {
            "active": gm_active,
            "history": gm_history,
            "summary": gm_summary,
            "limit": gm_limit,
            "remaining": gm_remaining,
        },
        "npc": npc or {
            "id": "",
            "name": "",
            "active": 0,
            "history": 0,
            "summary": 0,
            "limit": int(config.NPC_HISTORY_TOKENS),
            "remaining": int(config.NPC_HISTORY_TOKENS),
        },
        "npcs": npc_entries,
    }


def _maybe_compact(session):
    """Свернуть старые ходы истории ГМ в gm_summary, если она разрослась.
    Дословно остаются последние GM_KEEP_TURNS ходов (границы — user-сообщения),
    всё старше + прошлая сводка сжимаются одним вызовом модели."""
    msgs = session.gm_messages
    if _messages_tokens(msgs) < config.GM_HISTORY_TOKENS:
        return
    starts = [i for i, m in enumerate(msgs)
              if (m.get("role") if isinstance(m, dict) else getattr(m, "role", None)) == "user"]
    if len(starts) <= config.GM_KEEP_TURNS:
        return
    cut = starts[-config.GM_KEEP_TURNS]
    old, recent = msgs[:cut], msgs[cut:]
    old_text = "\n".join(t for t in (_msg_text(m) for m in old) if t)
    base = (session.gm_summary + "\n" + old_text).strip()
    session.gm_summary = session.client.summarize(base, proper_nouns=session.world.proper_nouns())
    session.gm_messages = recent


def _summarize_npc_history(client, npc: world_mod.NPC, world: world_mod.World,
                           text: str) -> str:
    """Сжать личный тред NPC без добавления новых фактов."""
    system = prompts.NPC_COMPACT_SYSTEM.format(proper_nouns=", ".join(world.proper_nouns()))
    _, content, _, _ = client.chat(
        [{"role": "system", "content": system},
         {"role": "user", "content": text[:config.COMPACT_INPUT_CHARS]}],
        think=True,
        reasoning_role=config.ROLE_COMPACT,
    )
    return content.strip()


def _maybe_compact_npc(session: "Session", npc: world_mod.NPC, client) -> None:
    """Свернуть старую личную историю NPC, если она стала большой."""
    if client is None:
        return
    msgs = session.npc_messages.get(npc.npc_id, [])
    if _messages_tokens(msgs) < config.NPC_HISTORY_TOKENS:
        return
    starts = [
        i for i, m in enumerate(msgs)
        if (m.get("role") if isinstance(m, dict) else getattr(m, "role", None)) == "user"
    ]
    if len(starts) <= config.NPC_KEEP_EXCHANGES:
        return
    cut = starts[-config.NPC_KEEP_EXCHANGES]
    old, recent = msgs[:cut], msgs[cut:]
    old_text = "\n".join(t for t in (_msg_text(m) for m in old) if t)
    base = (session.npc_summaries.get(npc.npc_id, "") + "\n" + old_text).strip()
    session.npc_summaries[npc.npc_id] = _summarize_npc_history(client, npc, session.world, base)
    session.npc_messages[npc.npc_id] = recent


def _assistant_json_message(out: dict) -> dict:
    return {"role": "assistant", "content": json.dumps(out, ensure_ascii=False)}


def _sync_scene_delta(session: "Session", narration: str, metas: list):
    """Apply explicit NPC enter/leave facts from accepted narration to SceneState.

    This does not reject or rewrite text. It keeps code state aligned with the scene the
    player was just shown.
    """
    if not narration.strip():
        return
    if not any(npc.name and npc.name in narration for npc in session.world.npcs.values()):
        return
    before = len(getattr(session.client, "call_log", []))
    try:
        delta = agents.extract_scene_delta(session.client, session.world, narration)
    except Exception as e:
        yield ev("error", "scene_sync", f"Scene state sync failed: {e}")
        return
    for row in getattr(session.client, "call_log", [])[before:]:
        metas.append(_meta("scene sync", row, scope="other"))
    for move in (delta.get("moves") if isinstance(delta, dict) else []) or []:
        if not isinstance(move, dict):
            continue
        try:
            payload = session.world.set_npc_presence(
                move.get("npc_id", ""),
                bool(move.get("present")),
                location=move.get("location", ""),
                visible=bool(move.get("visible", True)),
                can_hear=bool(move.get("can_hear", True)),
                activity=move.get("activity", ""),
                attitude=move.get("attitude", ""),
            )
        except KeyError:
            continue
        yield ev("scene_update", "scene_sync", payload)


# Наблюдений (строк) и коммитментов (блоков-реплик) держим в промпте NPC (lean-контекст).
_OBS_CAP = 12
_COMMIT_BLOCKS = 8


class Session:
    """Состояние партии между ходами."""
    def __init__(self, client, world: world_mod.World | None = None):
        self.client = client
        self.client_backend = config.BACKEND
        self.client_model = getattr(client, "model", "") if client is not None else ""
        self.client_session_id = getattr(client, "session_id", "") if client is not None else ""
        self.client_thread_id = getattr(client, "thread_id", "") if client is not None else ""
        self.npc_clients: dict[str, object] = {}       # npc_id -> отдельный model client/thread
        self.npc_client_state: dict[str, dict] = {}    # сериализуемые model/session/thread ids
        self.world = world or world_mod.World()
        self.gm_messages: list = []                 # история ГМ (последние ходы дословно)
        self.gm_summary = ""                        # сжатая сводка старых ходов (компакт)
        self.npc_messages: dict[str, list] = {}      # npc_id -> личная история LLM-сессии NPC
        self.npc_summaries: dict[str, str] = {}      # npc_id -> компакт старой личной истории
        self.loaded_gm_tools: set[str] = agents.initial_gm_tool_names()
        self.run_usage = _empty_usage()              # накопительная статистика за текущий ран
        self.last_player_action = ""                # дословное действие игрока этого хода
        self._sid = 0                               # счётчик id для стрим-элементов
        # --- Лог событий и память сцены ---
        self.events: list = []                      # ЗАКОММИЧЕННЫЕ события (игрок/кубы сразу; реплики NPC — в конце хода)
        self._seq = 0                               # монотонный счётчик seq
        self.turn = 0                               # текущий номер хода
        self.delivered: dict[str, int] = {}         # npc_id -> макс seq, ПОКАЗАННЫЙ ему (прошлые ходы)
        self._shown: dict[str, int] = {}            # npc_id -> seq-граница на его пробуждении этого хода
        self.pending: dict[str, dict] = {}          # npc_id -> {seq, speech, action, claims}: провизорная реплика хода
        self.commitments: dict[str, list[str]] = {} # npc_id -> блоки его собственной памяти
        self._turn_player_event: world_mod.Event | None = None

    def ensure_npc_client(self, npc_id: str):
        """Вернуть отдельный клиент/тред NPC для cache key и личной истории."""
        client = self.npc_clients.get(npc_id)
        if client is not None:
            return client
        if self.client is None:
            return None
        client = make_client()
        state = self.npc_client_state.setdefault(npc_id, {})
        model = (
            state.get("model")
            or self.client_model
            or getattr(self.client, "model", "")
            or ""
        )
        if model and hasattr(client, "set_model"):
            client.set_model(model)
        if hasattr(client, "set_session_identity"):
            client.set_session_identity(
                str(state.get("session_id") or ""),
                str(state.get("thread_id") or ""),
            )
        self.npc_clients[npc_id] = client
        self.remember_npc_client(npc_id)
        return client

    def remember_npc_client(self, npc_id: str) -> None:
        client = self.npc_clients.get(npc_id)
        if client is None:
            return
        self.npc_client_state[npc_id] = {
            "model": str(getattr(client, "model", "") or self.client_model or ""),
            "session_id": str(getattr(client, "session_id", "") or ""),
            "thread_id": str(getattr(client, "thread_id", "") or ""),
        }

    def reset_npc_memory(self, npc_id: str) -> bool:
        """Явный ручной сброс памяти ОДНОГО NPC.

        Чистит личную историю, компакт и сериализованный client state, а также
        роняет живой клиент/тред этого NPC, чтобы при следующем вызове поднялся
        свежий Codex/OAuth thread и новый prompt_cache_key. Никогда не вызывается
        автоматически на обычной правке карточки — только по явному reset_memory.

        Возвращает True для любого реального NPC (даже без накопленной памяти) и False
        только для неизвестного/пустого id.
        """
        npc_id = str(npc_id or "")
        # Return contract: False ONLY for an unknown/empty id; True for any real NPC, even one
        # with no stored memory yet. The method always mutates for a valid NPC (clears the
        # stores below AND pins delivered/_shown), so reporting "had_state" would lie — it could
        # return False after a real mutation and mislead a caller into not persisting state.
        if not npc_id or npc_id not in self.world.npcs:
            return False
        self.npc_messages.pop(npc_id, None)
        self.npc_summaries.pop(npc_id, None)
        self.npc_client_state.pop(npc_id, None)
        self.npc_clients.pop(npc_id, None)
        self.commitments.pop(npc_id, None)
        self.pending.pop(npc_id, None)
        # delivered/_shown are visibility boundaries into the shared event log, NOT private
        # memory. Deleting them lets observations() fall back to 0 and re-surface every past
        # event as a "new" observation after a reset. Pin them to the current max seq so the
        # freshly-reset NPC starts from the present and only sees events that happen next.
        self.delivered[npc_id] = self._seq
        self._shown[npc_id] = self._seq
        return True

    def apply_debug_edit(self, npc_id: str, data: dict) -> bool:
        """Apply one /debug/npc edit (card fields, presence, whereabouts, optional memory
        reset). This is the single source of truth the HTTP handler delegates to, so the
        guard logic is unit-testable without an HTTP harness.

        Returns False and mutates nothing for an unknown npc_id. Presence is touched ONLY on
        an actual present-state change, so a card-only save never clobbers visible/can_hear/
        activity. Memory reset runs after the edits and only on an explicit truthy
        reset_memory flag — never automatically.
        """
        world = self.world
        npc_id = str(npc_id or "")
        if not npc_id or npc_id not in world.npcs:
            return False
        data = data if isinstance(data, dict) else {}
        fields = data.get("fields")
        if isinstance(fields, dict):
            world.update_npc(npc_id, fields)
        if "present" in data:
            requested = bool(data.get("present"))
            if requested != (npc_id in world.scene.present_npcs):
                world.set_npc_presence(npc_id, requested)
        wb = data.get("whereabouts")
        if isinstance(wb, dict):
            world.set_npc_whereabouts(
                npc_id,
                location_id=str(wb.get("location_id") or ""),
                location_name=str(wb.get("location_name") or ""),
                status=str(wb.get("status") or ""),
                details=str(wb.get("details") or ""),
            )
        if data.get("reset_memory"):
            self.reset_npc_memory(npc_id)
        return True

    def set_model_for_all_clients(self, model: str) -> None:
        model = (model or "").strip()
        if not model:
            return
        self.client_model = model
        if self.client is not None and hasattr(self.client, "set_model"):
            self.client.set_model(model)
        for client in self.npc_clients.values():
            if hasattr(client, "set_model"):
                client.set_model(model)
        for state in self.npc_client_state.values():
            state["model"] = model

    def set_run_usage(self, usage: dict | None) -> None:
        self.run_usage = _usage_from_payload(usage)

    def add_turn_usage(self, turn_total: dict) -> dict:
        usage = _usage_from_payload(self.run_usage)
        usage["turns"] += 1
        usage["calls"] += len(turn_total.get("calls") or [])
        usage["in"] += int(turn_total.get("in") or 0)
        usage["out"] += int(turn_total.get("out") or 0)
        usage["cached"] += int(turn_total.get("cached") or 0)
        usage["tokens"] += int(turn_total.get("tokens") or 0)
        usage["secs"] = round(usage["secs"] + float(turn_total.get("secs") or 0), 2)
        usage["peak_context"] = max(
            usage["peak_context"],
            int(turn_total.get("peak_context") or 0),
        )
        for call in turn_total.get("calls") or []:
            scope = str(call.get("scope") or "npc")
            tokens = int(call.get("in") or 0) + int(call.get("out") or 0)
            if scope == "gm":
                usage["gm_calls"] += 1
                usage["gm_tokens"] += tokens
            elif scope == "other":
                usage["other_calls"] += 1
                usage["other_tokens"] += tokens
            else:
                usage["npc_calls"] += 1
                usage["npc_tokens"] += tokens
        self.run_usage = usage
        return dict(usage)

    def npc_history_text(self, npc_id: str, max_messages: int = 8) -> str:
        npc = self.world.npcs.get(npc_id)
        name = npc.name if npc else npc_id
        parts = []
        summary = self.npc_summaries.get(npc_id, "").strip()
        if summary:
            parts.append("Сжатая память:\n" + summary)
        history = self.npc_messages.get(npc_id, [])[-max_messages:]
        if history:
            rendered = []
            for msg in history:
                role = msg.get("role", "?") if isinstance(msg, dict) else getattr(msg, "role", "?")
                content = msg.get("content", "") if isinstance(msg, dict) else getattr(msg, "content", "")
                if role == "user":
                    rendered.append("Ситуация для NPC:\n" + str(content).strip())
                elif role == "assistant":
                    rendered.append(f"Ответ {name}:\n" + str(content).strip())
                else:
                    rendered.append(f"{role}:\n" + str(content).strip())
            parts.append("Последние сообщения:\n" + "\n\n".join(rendered))
        return "\n\n".join(parts) if parts else "История NPC пока пустая."

    def next_sid(self) -> str:
        self._sid += 1
        return f"s{self._sid}"

    def _next_seq(self) -> int:
        self._seq += 1
        return self._seq

    def _present(self) -> frozenset:
        """Свидетели события — только игрок и NPC, реально присутствующие в сцене."""
        return self.world.present_witnesses()

    # --- запись событий -------------------------------------------------
    def record_public(self, actor: str, kind: str, speech: str = "", action: str = ""):
        """Публичное событие игрока/кубов — сразу в лог."""
        e = world_mod.Event(seq=self._next_seq(), turn=self.turn, actor=actor, kind=kind,
                            speech=speech, action=action, witnesses=self._present())
        self.events.append(e)
        return e

    def record_player_for(self, npc_id: str):
        """Запомнить текущую реплику игрока как услышанную конкретным NPC.

        Кто слышит приватный обмен, задаётся маршрутизацией ГМ: если ГМ вызвал
        ask_npc для NPC, этот NPC является адресатом/свидетелем. Если ГМ никого
        не вызвал, действие игрока в конце хода сохраняется как публичное.
        """
        witnesses = frozenset({"player", npc_id})
        if self._turn_player_event is None:
            self._turn_player_event = world_mod.Event(
                seq=self._next_seq(),
                turn=self.turn,
                actor="player",
                kind="speech",
                speech=self.last_player_action,
                witnesses=witnesses,
            )
            self.events.append(self._turn_player_event)
        else:
            self._turn_player_event.witnesses = frozenset(
                set(self._turn_player_event.witnesses) | set(witnesses)
            )
        return self._turn_player_event

    def draft(self, npc_id: str, speech: str, action: str, claims: list,
              user_message: dict | None = None, assistant_message: dict | None = None,
              witnesses: frozenset | None = None):
        """Провизорная реплика NPC этого хода (в лог попадёт только в КОНЦЕ хода).
        Коррекция перезаписывает (seq сохраняется). Пустую реплику не храним."""
        if not speech and not action:
            self.pending.pop(npc_id, None)
            return
        prev = self.pending.get(npc_id)
        seq = prev["seq"] if prev else self._next_seq()
        event_witnesses = witnesses or (prev.get("witnesses") if prev else None) or self._present()
        self.pending[npc_id] = {"seq": seq, "speech": speech, "action": action,
                                "claims": list(claims or []),
                                "witnesses": frozenset(event_witnesses),
                                "user_message": user_message,
                                "assistant_message": assistant_message}

    def snapshot_shown(self, npc_id: str):
        """Запомнить, до какого seq NPC реально видел на этом пробуждении."""
        self._shown[npc_id] = self._seq

    # --- чтение памяти для промпта NPC ----------------------------------
    def observations(self, npc_id: str) -> str:
        """Что NPC увидел/услышал с прошлого раза: закоммиченные события + ПРОВИЗОРНЫЕ
        реплики ДРУГИХ NPC этого хода (внутриходовая осведомлённость). Только речь/действие."""
        seen = self.delivered.get(npc_id, 0)
        items = []  # (seq, rendered)
        for e in self.events:
            if e.seq <= seen or npc_id not in e.witnesses or e.actor == npc_id:
                continue
            if e.actor == "player" and e.turn == self.turn:
                continue   # текущее действие игрока ГМ описывает в situation — не дублируем
            items.append((e.seq, self._render_event(e)))
        for k, d in self.pending.items():
            if k != npc_id and d["seq"] > seen:
                if npc_id not in d.get("witnesses", self._present()):
                    continue
                items.append((d["seq"], self._render_npc(k, d["speech"], d["action"])))
        items.sort(key=lambda x: x[0])
        lines = [r for _, r in items if r]
        return "\n".join(lines[-_OBS_CAP:])

    def _render_event(self, e) -> str:
        if e.actor == "player":
            if e.speech and e.action:
                return f'Player: «{e.speech}» [{e.action}]'
            if e.speech:
                return f'Player: «{e.speech}»'
            return f'[{e.action}]' if e.action else ""
        if e.kind == "dice":
            return f'(roll) {e.action}'
        return self._render_npc(e.actor, e.speech, e.action)

    def _render_npc(self, npc_id: str, speech: str, action: str) -> str:
        if not speech and not action:
            return ""
        name = self._npc_name(npc_id)
        sp = f'«{speech}»' if speech else ""
        ac = f' [{action}]' if action else ""
        return f'{name}: {sp}{ac}'.strip()

    def _npc_name(self, npc_id: str) -> str:
        npc = self.world.npcs.get(npc_id)
        return npc.name if npc else npc_id

    def commit_text(self, npc_id: str) -> str:
        """Собственная память NPC — последние блоки-реплики (не строки)."""
        return "\n".join(self.commitments.get(npc_id, [])[-_COMMIT_BLOCKS:])

    def commit_turn(self):
        """Конец хода: провизорные реплики NPC -> в лог + в их память; delivered двигаем
        до того, что NPC реально видел (_shown), НЕ до конца лога. Затем очистка.
        Отвергнутые редрафты перезаписали pending и в лог не попадают."""
        for npc_id, d in self.pending.items():
            speech, action = d["speech"], d["action"]
            if not speech and not action:
                continue
            witnesses = frozenset(d.get("witnesses") or self._present())
            self.events.append(world_mod.Event(
                seq=d["seq"], turn=self.turn, actor=npc_id,
                kind="speech" if speech else "action",
                speech=speech, action=action, witnesses=witnesses))
            block = f'Я сказал: {speech or "—"}; сделал: {action or "—"}'
            for c in d["claims"]:
                block += f'\n  (опираюсь на: {c})'
            self.commitments.setdefault(npc_id, []).append(block)
            self.commitments[npc_id] = self.commitments[npc_id][-_COMMIT_BLOCKS:]
            if d.get("user_message") and d.get("assistant_message"):
                self.npc_messages.setdefault(npc_id, []).extend([
                    d["user_message"], d["assistant_message"],
                ])
            self.world.record_rumor(d["seq"], self.turn, npc_id, speech, witnesses)
            self.delivered[npc_id] = self._shown.get(npc_id, self.delivered.get(npc_id, 0))
            self.remember_npc_client(npc_id)
        self.events.sort(key=lambda e: e.seq)
        if len(self.events) > config.EVENTS_CAP:     # кап памяти событий
            self.events = self.events[-config.EVENTS_CAP:]
        self.pending.clear()
        self._shown.clear()


def run_turn(session: Session, player_text: str):
    """Один ход игрока: поток событий + метаинфа (meta по вызовам, meta_total в конце)."""
    t0 = time.perf_counter()
    metas: list = []
    yield from _drive(session, player_text, metas)
    total = _meta_total(metas, round(time.perf_counter() - t0, 2))
    total["context"] = context_usage(session)
    total["run"] = session.add_turn_usage(total)
    yield ev("meta_total", None, total)


def _drive(session: Session, player_text: str, metas: list):
    world = session.world
    session.turn += 1
    session.last_player_action = player_text
    session._turn_player_event = None
    session.gm_messages.append(agents.gm_user_message(world, player_text))
    yield ev("player", "Игрок", player_text)
    _maybe_compact(session)                          # держим историю ГМ в рамках num_ctx

    fell_through = True
    for _ in range(config.MAX_TOOL_HOPS):
        sid = session.next_sid()
        gen = agents.gm_turn_stream(
            session.client,
            world,
            session.gm_messages,
            session.gm_summary,
            session.loaded_gm_tools,
        )
        content_deltas: list[str] = []
        try:
            while True:
                ch, text = next(gen)
                if ch == "thinking":
                    yield ev("delta", "ГМ", {"channel": "gm_thinking", "text": text}, sid)
                else:
                    # Some local chat templates stream assistant content before a tool-call
                    # ("Let's call ask_npc..."). Buffer it until we know whether this turn
                    # is actual narration or a tool decision.
                    if config.STREAM_GM_CONTENT:
                        yield ev("delta", "ГМ", {"channel": "gm_narration", "text": text}, sid)
                    else:
                        content_deltas.append(text)
        except StopIteration as e:
            thinking, content, calls, assistant_msg, stats = e.value
        except Exception as e:
            yield ev("error", "ГМ", f"Ошибка вызова модели: {e}")
            fell_through = False
            break

        if thinking.strip():
            yield ev("gm_thinking", "ГМ", thinking.strip(), sid)
        m = _meta("ГМ — нарратив" if not calls else "ГМ — решение", stats, scope="gm")
        metas.append(m); yield ev("meta", "ГМ", m, sid)

        if not calls:
            final_text = content.strip()
            session.gm_messages.append(assistant_msg)   # каноничный echo в историю
            if final_text:
                if content_deltas and not config.STREAM_GM_CONTENT:
                    yield ev("delta", "ГМ", {"channel": "gm_narration", "text": final_text}, sid)
                yield ev("gm_narration", "ГМ", final_text, sid)
                yield from _sync_scene_delta(session, final_text, metas)
            fell_through = False
            break

        calls = _normalize_tool_calls(calls, world, id_prefix=f"gm_{sid}")
        assistant_msg = _assistant_with_tool_calls(assistant_msg, calls)
        prelude_text = content.strip()
        if not prelude_text and _should_generate_tool_prelude(calls):
            prelude_text = yield from _generate_pre_tool_prelude(
                session, world, player_text, calls, metas
            )
            if prelude_text:
                assistant_msg = dict(assistant_msg)
                assistant_msg["content"] = prelude_text

        session.gm_messages.append(assistant_msg)   # каноничный echo в историю
        if prelude_text:
            if content_deltas and not config.STREAM_GM_CONTENT:
                yield ev("delta", "ГМ", {"channel": "gm_narration", "text": prelude_text}, sid)
            yield ev("gm_narration", "ГМ", prelude_text, sid)

        for call in calls:
            name, args = call["name"], call["arguments"]
            yield ev("gm_tool_call", "ГМ", {"name": name, "arguments": args})
            result = yield from _run_tool(session, name, args, metas)
            yield ev("tool_result", name, _tool_full_text(result))
            session.gm_messages.append({"role": "tool", "tool_call_id": call.get("id", ""),
                                        "content": _tool_model_text(result)})

    if fell_through:
        yield ev("error", "ГМ", "Превышен лимит вызовов инструментов за ход.")

    if session._turn_player_event is None:
        session.record_public("player", "speech", speech=player_text)

    # Конец хода: зафиксировать черновики NPC в лог + их память (видны на след. ходу).
    session.commit_turn()


_VISIBLE_PRELUDE_TOOLS = {
    "ask_npc",
    "move_npc",
    "set_npc_presence",
    "set_npc_whereabouts",
    "set_scene",
    "roll_dice",
}


def _should_generate_tool_prelude(calls: list) -> bool:
    return any(
        isinstance(call, dict) and call.get("name") in _VISIBLE_PRELUDE_TOOLS
        for call in calls or []
    )


def _generate_pre_tool_prelude(
    session: Session,
    world: world_mod.World,
    player_text: str,
    calls: list,
    metas: list,
) -> str:
    sid = session.next_sid()
    gen = agents.gm_prelude_stream(session.client, world, player_text, calls)
    try:
        while True:
            ch, text = next(gen)
            if ch == "thinking":
                yield ev("delta", "ГМ", {"channel": "gm_thinking", "text": text}, sid)
            elif config.STREAM_GM_CONTENT:
                yield ev("delta", "ГМ", {"channel": "gm_narration", "text": text}, sid)
    except StopIteration as e:
        thinking, content, _calls, _assistant_msg, stats = e.value
    except Exception as e:
        yield ev("error", "ГМ", f"Ошибка прелюдии перед инструментом: {e}")
        return ""

    if thinking.strip():
        yield ev("gm_thinking", "ГМ", thinking.strip(), sid)
    metas.append(_meta("ГМ — прелюдия", stats, scope="gm"))
    return content.strip()


def _run_tool(session: Session, name: str, args: dict, metas: list):
    """Исполнение инструмента. Yield events for UI/debug; return full + model-history text."""
    world = session.world
    args = args if isinstance(args, dict) else {}
    if name == "tool_search":
        payload = agents.search_gm_tools(
            world,
            args.get("query", ""),
            args.get("max_results", 5),
            session.loaded_gm_tools,
        )
        for tool_name in payload.get("loaded_tools") or []:
            session.loaded_gm_tools.add(str(tool_name))
        lines = [payload.get("message", "")]
        matches = payload.get("matches") or []
        if matches:
            lines.append("Загружено:")
            lines.extend(f"- {row['name']}: {row['description']}" for row in matches)
        if payload.get("missing"):
            lines.append("Не найдено: " + ", ".join(payload["missing"]))
        yield ev("tool_search", "ГМ", "\n".join(line for line in lines if line))
        return _tool_result(_json_compact(payload), _json_compact(_compact_tool_search_payload(payload)))
    if name == "roll_dice":
        total, detail = world.roll_for_outcome(
            args.get("notation", "1d20"),
            target_number=args.get("target_number"),
            target_kind=args.get("target_kind", ""),
            roll_kind=args.get("roll_kind", ""),
        )
        yield ev("dice", "ГМ", detail)
        session.record_public("gm", "dice", action=detail)
        return _tool_result(detail)
    if name == "get_world_fact":
        fact = world.fact(args.get("query", ""))
        payload = fact.as_tool_payload()
        source_lines = []
        for source in payload.get("sources") or []:
            source_lines.append(
                f"[{source.get('n')}] {source.get('kind')} · {source.get('status')} · "
                f"{source.get('source')} · score {source.get('score')}"
            )
        debug_text = f"{payload['status']}: {payload['text']}"
        if source_lines:
            debug_text += "\n\nsources:\n" + "\n".join(source_lines)
        yield ev("world_fact", "ГМ", debug_text)
        return _tool_result(_json_compact(payload), _json_compact(_compact_world_fact_payload(payload)))
    if name == "update_world_state":
        payload = _apply_world_state_batch(session, args)
        if payload.get("errors"):
            for error in payload.get("errors") or []:
                yield ev("error", "ГМ", error.get("error", "world-state update failed"))
        yield ev("world_state_update", "ГМ", payload)
        return _tool_result(
            _json_compact(payload),
            _json_compact(_compact_world_state_update_payload(payload)),
        )
    if name == "query_world_state":
        payload = _query_world_state(session, args)
        if payload.get("error"):
            yield ev("error", "ГМ", payload["error"])
        else:
            yield ev("world_query", "ГМ", payload)
        return _tool_result(
            _json_compact(payload),
            _json_compact(_compact_world_query_payload(payload)),
        )
    if name == "set_npc_whereabouts":
        try:
            payload = world.set_npc_whereabouts(
                args.get("npc_id", ""),
                location_id=args.get("location_id", ""),
                location_name=args.get("location_name", ""),
                status=args.get("status", ""),
                details=args.get("details", ""),
                source=args.get("source", ""),
            )
        except KeyError as e:
            yield ev("error", "ГМ", str(e))
            return _tool_result(f"(tool error: {e})")
        yield ev("npc_whereabouts", "ГМ", payload)
        return _tool_result(_json_compact(payload), _json_compact(_compact_whereabouts_payload(payload)))
    if name in ("move_npc", "set_npc_presence"):
        try:
            payload = world.set_npc_presence(
                args.get("npc_id", ""),
                bool(args.get("present")),
                location=args.get("location", ""),
                visible=bool(args.get("visible", True)),
                can_hear=bool(args.get("can_hear", True)),
                activity=args.get("activity", ""),
                attitude=args.get("attitude", ""),
            )
        except KeyError as e:
            yield ev("error", "ГМ", str(e))
            return _tool_result(f"(tool error: {e})")
        yield ev("scene_update", "ГМ", payload)
        return _tool_result(_json_compact(payload), _json_compact(_compact_presence_payload(payload)))
    if name == "set_scene":
        payload = world.set_scene(
            args.get("title", ""),
            args.get("description", ""),
            location_id=args.get("location_id", ""),
            present_npcs=args.get("present_npcs", []),
            items=args.get("items", []),
            exits=args.get("exits", []),
            constraints=args.get("constraints", []),
            tension=args.get("tension", ""),
        )
        if payload.get("repair_hint"):
            yield ev("error", "ГМ", payload["repair_hint"])
        yield ev("scene_update", "ГМ", payload)
        return _tool_result(_json_compact(payload), _json_compact(_compact_scene_payload(payload)))
    if name == "ask_npc":
        npc_id = args.get("npc_id", "")
        situation = (args.get("situation") or "").strip()
        if not situation:
            msg = ("ask_npc requires a non-empty `situation`; call ask_npc again with "
                   "`npc_id` and a neutral third-person situation.")
            yield ev("error", "ГМ", msg)
            return _tool_result(f"(tool error: {msg})")
        correction = args.get("correction")
        if correction and npc_id not in session.pending:
            correction = None
        line = yield from _ask_npc(session, npc_id, situation, correction, metas)
        return line
    return _tool_result(f"(unknown tool: {name})")


def _ask_npc(session: Session, npc_id: str, situation: str,
             correction: str | None = None, metas: list | None = None):
    """NPC отыгрывает черновик. correction != None -> ГМ (в своём треде) вернул
    прошлый черновик на переделку со своим замечанием.

    Двухфазно: черновик копится в session.pending весь ход (коррекция перезаписывает
    тот же seq), а в лог/память коммитится ТОЛЬКО в конце хода (commit_turn). Другие
    NPC видят провизорные черновики через observations() — внутриходовая осведомлённость."""
    correction = (correction or "").strip() or None
    world = session.world
    try:
        npc = world.resolve(npc_id)
    except KeyError as e:
        yield ev("error", "ГМ", str(e))
        return _tool_result(f"(no such NPC: {npc_id})")
    if not world.npc_can_react(npc.npc_id):
        whereabouts = world.npc_whereabouts_summary(npc.npc_id)
        msg = (
            f"{npc.name} is not present and able to hear in the current scene. "
            "Do not invent their reaction here. Do not write speech/action for any other "
            "named NPC unless you first call ask_npc for that exact present NPC. Narrate "
            "only absence, travel/search, or generic scene response."
        )
        if whereabouts:
            msg += " Known whereabouts: " + whereabouts
        yield ev("error", "ГМ", msg)
        return _tool_result(f"(tool error: {msg})")

    if correction:
        yield ev("gm_reject", npc.name, correction)

    player_event = session.record_player_for(npc.npc_id)
    exchange_witnesses = frozenset(player_event.witnesses)
    sid = session.next_sid()
    yield ev("npc_start", npc.name, None, sid)

    observations = session.observations(npc.npc_id)
    commitments = session.commit_text(npc.npc_id)
    session.snapshot_shown(npc.npc_id)   # запомнить, до какого seq NPC видел
    brief = (situation or "").strip()
    if not brief:
        yield ev("error", npc.name, "NPC was called without a situation.")
        return _tool_result(f"({npc.name} has no situation to react to)")
    npc_client = session.ensure_npc_client(npc.npc_id) or session.client
    _maybe_compact_npc(session, npc, npc_client)
    yield ev("npc_history", npc.name, {
        "npc_id": npc.npc_id,
        "messages": len(session.npc_messages.get(npc.npc_id, [])),
        "has_summary": bool(session.npc_summaries.get(npc.npc_id, "").strip()),
        "text": session.npc_history_text(npc.npc_id, max_messages=6),
    })
    user_message = agents.npc_user_message(
        npc, brief, observations, commitments, correction,
        constraints=world.constraints, scene_slice=world.npc_scene_slice(npc.npc_id))
    gen = agents.npc_turn_stream(
        npc_client, npc, brief,
        observations=observations, commitments=commitments, feedback=correction,
        constraints=world.constraints, scene_slice=world.npc_scene_slice(npc.npc_id),
        history=session.npc_messages.get(npc.npc_id, []),
        summary=session.npc_summaries.get(npc.npc_id, ""))
    buf, emitted, out, stats = "", 0, None, {}
    try:
        while True:
            ch, text = next(gen)
            if ch != "content":
                continue
            buf += text
            val, _done = extract_json_string(buf, "speech")
            if val is not None:
                disp = json_unescape(val)
                if len(disp) > emitted:
                    yield ev("delta", npc.name,
                             {"channel": "npc_speech", "text": disp[emitted:]}, sid)
                    emitted = len(disp)
    except StopIteration as e:
        out, stats = e.value
    except Exception as e:
        if correction:                       # провал редрафта не воскрешает отвергнутый черновик
            session.pending.pop(npc.npc_id, None)
        yield ev("error", npc.name, f"Ошибка NPC: {e}")
        return _tool_result(f"({npc.name} stays silent)")

    yield ev("npc_thinking", npc.name, out["reasoning"], sid)
    yield ev("npc_speech", npc.name,
             {"speech": out["speech"], "action": out["action"], "claims": out["claims"]}, sid)
    m = _meta(npc.name, stats, scope="npc")
    if metas is not None:
        metas.append(m)
    yield ev("meta", npc.name, m, sid)
    # Черновик в pending (коррекция перезапишет тот же seq; пустой не сохранится).
    session.remember_npc_client(npc.npc_id)
    session.draft(
        npc.npc_id, out["speech"], out["action"], out["claims"],
        user_message=user_message,
        assistant_message=_assistant_json_message(out),
        witnesses=exchange_witnesses,
    )
    payload = {
        "npc_id": npc.npc_id,
        "npc_name": npc.name,
        "speech_ru": out["speech"],
        "action_ru": out["action"],
        "gm_instruction": (
            "This exact NPC speech/action has already been emitted to the player by the "
            "engine. If more NPCs should react, call ask_npc for them now. In final "
            "narration, do not rewrite, retell, embellish, or paraphrase this NPC "
            "speech/action. Do not mention this NPC's name, body, speech, action, "
            "expression, posture, gesture, or emotion again. Final narration should be "
            "only 0-2 short sentences about surrounding scene consequences. If there is no "
            "new non-NPC consequence, produce empty final narration. Do not add another "
            "named NPC's reaction; call ask_npc for that NPC if you need it."
        ),
    }
    return _tool_result(_json_compact(payload), _json_compact(_compact_ask_npc_payload(payload)))
