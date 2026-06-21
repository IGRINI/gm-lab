"""Capture golden fixtures from the reference Python GM-Lab implementation.

Run from the Python project dir (E:\\gemma\\gm-lab) with that dir on sys.path.
Writes byte-exact reference fixtures into ../gm-lab-rs/tests/reference/ so the Rust
port can assert bit/byte parity for the load-bearing invariants:
  * prompt strings (GM_SYSTEM, NPC_*, compact systems) — byte identity
  * CPython MT19937 (random.Random) getstate + randint sequences — RNG fidelity
  * state_record_hash — sha256 canonical-JSON identity
  * dice grade ladder + roll() detail strings
  * a world snapshot payload (rng_state round-trip shape)

Usage:
    cd E:\\gemma\\gm-lab
    python ..\\gm-lab-rs\\tools\\capture_fixtures.py
"""
from __future__ import annotations

import hashlib
import json
import os
import random
import sys
from pathlib import Path

# Resolve the reference output dir relative to this script.
OUT = (Path(__file__).resolve().parent.parent / "tests" / "reference")
OUT.mkdir(parents=True, exist_ok=True)
PROMPTS_DIR = OUT / "prompts"
PROMPTS_DIR.mkdir(parents=True, exist_ok=True)


def _sha(text: str) -> str:
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def _write(path: Path, data) -> None:
    if isinstance(data, str):
        path.write_text(data, encoding="utf-8", newline="")
    else:
        path.write_text(
            json.dumps(data, ensure_ascii=False, indent=2), encoding="utf-8", newline="\n"
        )
    print(f"  wrote {path.relative_to(OUT.parent.parent)}  ({path.stat().st_size} bytes)")


def capture_prompts() -> None:
    print("[prompts]")
    import prompts

    names = [
        "GM_SYSTEM",
        "NPC_SYSTEM_STATIC",
        "NPC_SYSTEM_TEMPLATE",
        "NPC_CARD_TEMPLATE",
        "NPC_COMPACT_SYSTEM",
        "GM_COMPACT_SYSTEM",
    ]
    index = {}
    for name in names:
        val = getattr(prompts, name, None)
        if not isinstance(val, str):
            print(f"  SKIP {name}: not a str ({type(val).__name__})")
            continue
        # raw byte-exact text file
        _write(PROMPTS_DIR / f"{name}.txt", val)
        index[name] = {
            "sha256": _sha(val),
            "chars": len(val),
            "bytes": len(val.encode("utf-8")),
        }
    _write(OUT / "prompts_index.json", index)


def capture_rng() -> None:
    print("[rng]")
    # CPython random.Random(int_seed): seed -> init_by_array, getstate() ->
    # (version=3, internal=tuple(625 ints: 624 MT words + index), gauss_next).
    cases = []
    for seed in [0, 1, 12345, 2**63 + 7, 0xDEADBEEFCAFEBABE]:
        r = random.Random(seed)
        state0 = r.getstate()
        seq = {}
        for sides in [2, 4, 6, 8, 10, 12, 20, 100]:
            seq[str(sides)] = [r.randint(1, sides) for _ in range(64)]
        state1 = r.getstate()
        cases.append(
            {
                "seed": str(seed),  # str: seeds exceed JS/JSON safe int range
                "state_after_seed": {
                    "version": int(state0[0]),
                    "internal": [int(x) for x in state0[1]],
                    "gauss": state0[2],
                },
                "randint_sequences": seq,
                "state_after_rolls": {
                    "version": int(state1[0]),
                    "internal": [int(x) for x in state1[1]],
                    "gauss": state1[2],
                },
            }
        )
    _write(OUT / "rng_vectors.json", cases)

    # setstate round-trip: restore a known state, then the sequence must reproduce.
    r = random.Random(999)
    for _ in range(37):  # advance to a non-trivial point
        r.randint(1, 20)
    saved = r.getstate()
    after = [r.randint(1, 20) for _ in range(48)]
    r2 = random.Random(0)
    r2.setstate(saved)
    after2 = [r2.randint(1, 20) for _ in range(48)]
    assert after == after2, "setstate round-trip mismatch in reference!"
    _write(
        OUT / "rng_setstate_roundtrip.json",
        {
            "saved_state": {
                "version": int(saved[0]),
                "internal": [int(x) for x in saved[1]],
                "gauss": saved[2],
            },
            "next_d20": after,
        },
    )


def capture_state_record_hash() -> None:
    print("[state_record_hash]")
    import world as world_mod

    samples = [
        world_mod.StateRecord(
            record_id="r1", kind="public", text="Городские ворота открыты на рассвете.",
            scope="public", status="known", tags=("город", "ворота"),
        ),
        world_mod.StateRecord(
            record_id="r2", kind="truth", text="Капитан стражи — оборотень.",
            scope="gm", owner="gm", status="known", subject="captain",
            participants=("captain", "player"), metadata={"weight": 5, "ru": "тайна"},
        ),
        world_mod.StateRecord(
            record_id="r3", kind="rumor", text="Говорят, в подвале прячут золото.",
            scope="shared", status="rumored", location_id="loc_inn",
            location_name="Таверна «Старый дуб»", aliases=("слух",),
        ),
    ]
    out = []
    for rec in samples:
        out.append(
            {
                "record": {
                    "record_id": rec.record_id, "kind": rec.kind, "text": rec.text,
                    "scope": rec.scope, "active": rec.active, "owner": rec.owner,
                    "subject": rec.subject, "source": rec.source, "status": rec.status,
                    "tags": list(rec.tags), "entity_id": rec.entity_id,
                    "source_npc": rec.source_npc, "participants": list(rec.participants),
                    "location_id": rec.location_id, "location_name": rec.location_name,
                    "region_id": rec.region_id, "region_name": rec.region_name,
                    "scene_id": rec.scene_id, "importance": rec.importance,
                    "aliases": list(rec.aliases), "metadata": rec.metadata,
                },
                "hash": world_mod.state_record_hash(rec),
            }
        )
    _write(OUT / "state_record_hash.json", out)


def capture_dice() -> None:
    print("[dice]")
    import world as world_mod

    # grade ladder (pure static fn)
    grades = {}
    for margin in range(-20, 21):
        grades[str(margin)] = world_mod.World._grade_from_margin(margin)
    _write(OUT / "dice_grades.json", grades)

    # roll() detail strings with a fixed seed + forced overrides
    w = world_mod.World.__new__(world_mod.World)
    w._rng = random.Random(424242)
    w.forced_die_next = None
    w.forced_die_all = None
    rolls = []
    for notation in ["1d20", "2d6+3", "4d6kh3", "1d20+5", "3d8kl1", "1d100", "2d20kh1-1"]:
        total, detail = w.roll(notation)
        rolls.append({"notation": notation, "total": total, "detail": detail})
    # forced one-shot
    w.forced_die_next = 20
    t, d = w.roll("1d20+5")
    rolls.append({"notation": "1d20+5", "forced_die_next": 20, "total": t, "detail": d})
    # forced_die_next consumed -> next roll random again
    t, d = w.roll("1d20")
    rolls.append({"notation": "1d20", "after_forced_consumed": True, "total": t, "detail": d})
    # sticky forced_all
    w.forced_die_all = 1
    for _ in range(3):
        t, d = w.roll("1d20")
        rolls.append({"notation": "1d20", "forced_die_all": 1, "total": t, "detail": d})
    _write(OUT / "dice_rolls.json", rolls)


def capture_snapshot() -> None:
    print("[snapshot]")
    try:
        import stories
        import dialog_store
        import world as world_mod
    except Exception as ex:
        print(f"  SKIP snapshot import: {ex}")
        return

    # Find a default story seed by trying a few likely API shapes.
    seed = None
    for getter in ("default_story_seed", "story_seed"):
        fn = getattr(stories, getter, None)
        if callable(fn):
            try:
                seed = fn() if getter == "default_story_seed" else fn(_default_story_id(stories))
                break
            except Exception as ex:
                print(f"  {getter} failed: {ex}")
    if seed is None:
        print("  SKIP snapshot: could not obtain a story seed")
        return
    try:
        w = world_mod.World(seed)
        payload = dialog_store._world_to_payload(w)
        raw = json.dumps(payload, ensure_ascii=False, separators=(",", ":"))
        _write(OUT / "world_payload_default.json", json.loads(raw))
        (OUT / "world_payload_default.compact.json").write_text(raw, encoding="utf-8", newline="")
        print(f"  wrote tests/reference/world_payload_default.compact.json ({len(raw.encode('utf-8'))} bytes)")
    except Exception as ex:
        print(f"  SKIP snapshot build: {ex}")


def _default_story_id(stories):
    for name in ("DEFAULT_STORY_ID", "DEFAULT_STORY", "default_story_id"):
        val = getattr(stories, name, None)
        if isinstance(val, str):
            return val
        if callable(val):
            try:
                return val()
            except Exception:
                pass
    ids = getattr(stories, "story_ids", None)
    if callable(ids):
        lst = ids()
        if lst:
            return lst[0]
    return ""


def main() -> None:
    print(f"Capturing reference fixtures -> {OUT}")
    capture_prompts()
    capture_rng()
    capture_state_record_hash()
    capture_dice()
    capture_snapshot()
    print("Done.")


if __name__ == "__main__":
    main()
