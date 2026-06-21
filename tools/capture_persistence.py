"""Capture a realistic DialogStore chat payload after a turn, for the gml-persistence
round-trip golden test: load the Python-written payload into Rust structs, re-serialize
with compact UTF-8-raw JSON, and assert byte-identity (validates field names, key order,
serde compact separators, rng_state shape, ids, gm/npc messages).

Run:  cd E:\\gemma\\gm-lab ; python ..\\gm-lab-rs\\tools\\capture_persistence.py
"""
from __future__ import annotations

import json
import os
import random
from pathlib import Path

os.environ.setdefault("GM_BACKEND", "mock")

import config  # noqa: E402
from llm_client import make_client  # noqa: E402
from orchestrator import Session, run_turn  # noqa: E402
import dialog_store  # noqa: E402

OUT = (Path(__file__).resolve().parent.parent / "tests" / "reference" / "persistence")
OUT.mkdir(parents=True, exist_ok=True)

FIXED_DICE_SEED = 20260622


def main() -> None:
    session = Session(make_client())
    w = getattr(session, "world", None)
    if w is not None:
        w.dice_seed = FIXED_DICE_SEED
        w._rng = random.Random(FIXED_DICE_SEED)

    transcript = []
    action = "Громко заявляю Борину: «Я знаю про убийство Алдрика!»"
    for e in run_turn(session, action):
        if e.get("kind") != "delta":
            transcript.append({"kind": e.get("kind"), "agent": e.get("agent"),
                               "data": e.get("data"), "sid": e.get("sid")})

    runtime = dialog_store.DialogRuntime(
        guest_id="local",
        chat_id="chat_fixture",
        session=session,
        transcript=transcript,
        turn_count=1,
    )
    payload = dialog_store._runtime_to_payload(runtime)

    # Pretty for human diff
    (OUT / "chat_payload.json").write_text(
        json.dumps(payload, ensure_ascii=False, indent=2), encoding="utf-8", newline="\n"
    )
    # Compact = the exact bytes DialogStore.save() writes (ensure_ascii=False, separators=(',',':'))
    compact = json.dumps(payload, ensure_ascii=False, separators=(",", ":"))
    (OUT / "chat_payload.compact.json").write_text(compact, encoding="utf-8", newline="")
    print(f"wrote chat_payload.json + chat_payload.compact.json ({len(compact.encode('utf-8'))} bytes compact)")
    # Top-level keys for quick reference
    print("top-level keys:", list(payload.keys()))
    print("session keys:", list(payload["session"].keys()))
    print("world keys:", list(payload["session"]["world"].keys()))


if __name__ == "__main__":
    main()
