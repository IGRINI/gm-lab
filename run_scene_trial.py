"""Real-model trial for the free-RP scene-state prototype.

Runs a few turns against the local llama.cpp server and prints:
- tool calls and NPC visible output,
- final GM narration,
- per-turn token totals and peak context,
- world seed call cost for a fresh story.
"""
from __future__ import annotations

import json
import os
from pathlib import Path

os.environ.setdefault("GM_BACKEND", "llamacpp")

import agents
import config
import world as world_mod
from llm_client import make_client
from orchestrator import Session, run_turn


OUT = Path(__file__).with_name("scene_trial_last.json")


def _n_ctx() -> int:
    try:
        import httpx
        base = config.LLAMA_HOST.rstrip("/")
        data = httpx.get(base + "/v1/models", timeout=5).json()
        return int(data["data"][0].get("meta", {}).get("n_ctx") or 0)
    except Exception:
        return 0


def _call_costs(client, start: int) -> list[dict]:
    return list(getattr(client, "call_log", [])[start:])


def _print_costs(rows: list[dict], n_ctx: int, prefix: str = "   "):
    for row in rows:
        pin = row.get("prompt_eval_count", 0)
        pout = row.get("eval_count", 0)
        pct = round(pin / n_ctx * 100, 2) if n_ctx else 0
        print(
            f"{prefix}{row.get('label')}: in={pin} out={pout} "
            f"tokens={row.get('tokens', pin + pout)} ctx={pct}%"
        )


def play(session: Session, text: str, n_ctx: int) -> dict:
    print(f"\n### ИГРОК: {text}")
    record = {
        "input": text,
        "tools": [],
        "npc": [],
        "narration": "",
        "errors": [],
        "meta_total": {},
    }
    for event in run_turn(session, text):
        kind, agent, data = event["kind"], event["agent"], event["data"]
        if kind == "gm_tool_call":
            args = dict(data["arguments"])
            if "situation" in args:
                args["situation"] = args["situation"][:120]
            record["tools"].append({"name": data["name"], "arguments": args})
            print(f"   tool: {data['name']} {args}")
        elif kind == "npc_speech":
            row = {"npc": agent, **data}
            record["npc"].append(row)
            act = f" [{data['action']}]" if data.get("action") else ""
            print(f"   {agent}: «{data.get('speech', '')}»{act}")
        elif kind == "gm_narration":
            record["narration"] = data
            print(f"   ГМ: {data[:420]}")
        elif kind == "error":
            record["errors"].append({"agent": agent, "data": data})
            print(f"   ERROR {agent}: {data[:260]}")
        elif kind == "meta_total":
            record["meta_total"] = data
            peak = data.get("peak_context", 0)
            pct = round(peak / n_ctx * 100, 2) if n_ctx else 0
            print(
                f"   TOKENS: calls={len(data.get('calls', []))} "
                f"in={data.get('in')} out={data.get('out')} "
                f"total={data.get('tokens')} peak_ctx={peak} ({pct}%) "
                f"secs={data.get('secs')}"
            )
    return record


def main():
    n_ctx = _n_ctx()
    client = make_client()
    print(f"backend={config.BACKEND} model={config.MODEL or 'auto'} n_ctx={n_ctx or 'unknown'}")

    results = {"default_scene": [], "fresh_world": [], "seed": {}, "n_ctx": n_ctx}

    default_session = Session(client)
    print("\n=== DEFAULT SCENE ===")
    print(default_session.world.scene_context())
    for text in [
        "Подхожу к капитану Марет у стойки и шепчу ей: расскажи, что видела у тела Алдрика.",
        "Борин, ты сбежал через заднюю дверь после убийства? Признавайся.",
        "Я кричу: Наль, кузнец, выходи сюда, я знаю, что ты прячешься за дверью!",
    ]:
        results["default_scene"].append(play(default_session, text, n_ctx))

    print("\n=== FRESH WORLD SEED ===")
    brief = (
        "Новый мир: ледяной порт Нордхольм. Пропал корабль «Северная свеча». "
        "В стартовой таверне видимые персонажи: хозяйка Ива и моряк Рун."
    )
    start = len(getattr(client, "call_log", []))
    seed = agents.build_world_seed(client, brief)
    seed_costs = _call_costs(client, start)
    print(json.dumps(seed, ensure_ascii=False, indent=2)[:1800])
    _print_costs(seed_costs, n_ctx)
    results["seed"] = {"brief": brief, "seed": seed, "costs": seed_costs}

    fresh_session = Session(client, world_mod.World.from_seed(seed))
    print("\n=== FRESH WORLD SCENE ===")
    print(fresh_session.world.scene_context())
    for text in [
        "Оглядываюсь. Кто здесь и что видно?",
        "Подхожу к Иве и спрашиваю: что ты знаешь о «Северной свече»?",
    ]:
        results["fresh_world"].append(play(fresh_session, text, n_ctx))

    OUT.write_text(json.dumps(results, ensure_ascii=False, indent=2), encoding="utf-8")
    print(f"\nSaved: {OUT}")


if __name__ == "__main__":
    main()
