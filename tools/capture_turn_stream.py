"""Capture golden /turn event streams from the Python orchestrator + in-process MockClient.

This is the gold-standard fixture for the Rust gml-orchestrator port: the exact sequence
of turn events (kind/agent/data/sid) the engine emits for a player action, using the same
deterministic mock backend (llm_client.MockClient) the Rust port reproduces in gml-llm.

The campaign RNG is pinned to a fixed seed so any dice rolls are reproducible.

Run:  cd E:\\gemma\\gm-lab ; python ..\\gm-lab-rs\\tools\\capture_turn_stream.py
"""
from __future__ import annotations

import json
import os
import random
from pathlib import Path

os.environ.setdefault("GM_BACKEND", "mock")  # force mock before importing config

import config  # noqa: E402
from llm_client import make_client  # noqa: E402
from orchestrator import Session, run_turn  # noqa: E402

OUT = (Path(__file__).resolve().parent.parent / "tests" / "reference" / "turns")
OUT.mkdir(parents=True, exist_ok=True)

FIXED_DICE_SEED = 20260622

PLAYER_ACTIONS = {
    "accuse_borin": "Громко, на весь зал, заявляю Борину: «Я знаю, что ты связан с убийством Алдрика!»",
    "look_around": "Я осматриваю зал трактира и прислушиваюсь к разговорам.",
    "ask_innkeeper": "Подхожу к стойке и тихо спрашиваю трактирщика, что он видел прошлой ночью.",
}


def _pin_rng(session) -> None:
    """Pin the campaign RNG so dice rolls are reproducible across runs."""
    world = getattr(session, "world", None)
    if world is None:
        return
    try:
        world.dice_seed = FIXED_DICE_SEED
        world._rng = random.Random(FIXED_DICE_SEED)
        world.forced_die_next = None
        world.forced_die_all = None
    except Exception as ex:
        print(f"  WARN could not pin rng: {ex}")


def capture(name: str, action: str) -> None:
    session = Session(make_client())
    _pin_rng(session)
    events = []
    for e in run_turn(session, action):
        # Normalize to a plain JSON-able dict with stable key order.
        events.append({
            "kind": e.get("kind"),
            "agent": e.get("agent"),
            "data": e.get("data"),
            "sid": e.get("sid"),
        })
    full_path = OUT / f"{name}.full.json"
    full_path.write_text(json.dumps(events, ensure_ascii=False, indent=2), encoding="utf-8", newline="\n")

    # Skeleton: non-delta event sequence (kind, agent) — the order contract.
    skeleton = [
        {"kind": e["kind"], "agent": e["agent"]}
        for e in events if e["kind"] != "delta"
    ]
    skel_path = OUT / f"{name}.skeleton.json"
    skel_path.write_text(json.dumps(skeleton, ensure_ascii=False, indent=2), encoding="utf-8", newline="\n")

    kinds = {}
    for e in events:
        kinds[e["kind"]] = kinds.get(e["kind"], 0) + 1
    print(f"  {name}: {len(events)} events ({len(skeleton)} non-delta) -> {full_path.name}")
    print(f"    kinds: {kinds}")


def main() -> None:
    print(f"=== capture turn streams (backend={config.BACKEND}, dice_seed={FIXED_DICE_SEED}) ===")
    for name, action in PLAYER_ACTIONS.items():
        try:
            capture(name, action)
        except Exception as ex:
            import traceback
            print(f"  {name}: ERROR {ex}")
            traceback.print_exc()
    print("Done.")


if __name__ == "__main__":
    main()
