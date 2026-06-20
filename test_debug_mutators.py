"""Contract tests for the debug-panel author mutators + token-cache discipline.

Run: python test_debug_mutators.py   (exit 0 = pass)

These guard two things the debug panel relies on:
1. Every editable field actually lands where the model reads it (cached prefix for
   public_intro; world.canon + 'hidden_truth' truth-fact for hidden_truth; world.*
   fields that orchestrator._gm_query_rows reads for events/rumors; world.constraints
   for scene constraints).
2. CACHE DISCIPLINE: editing per-turn data (scene/npc) must NOT mutate the cached
   system prefix (agents._gm_world_setup), so the long prompt-cache prefix stays valid.
"""
import agents
import world as world_mod

w = world_mod.World.from_story("turnvale-murder")

# --- public_intro: editable and lands in the cacheable prefix builder ---
assert "Тёрнвейл" in agents._gm_world_setup(w)
assert w.set_public_intro("НОВОЕ ПУБЛИЧНОЕ ИНТРО для теста.")
assert w.public == "НОВОЕ ПУБЛИЧНОЕ ИНТРО для теста."
assert "НОВОЕ ПУБЛИЧНОЕ ИНТРО" in agents._gm_world_setup(w)
assert not w.set_public_intro("   ")  # empty rejected, keeps previous
assert w.public == "НОВОЕ ПУБЛИЧНОЕ ИНТРО для теста."

# --- CACHE DISCIPLINE: scene/npc edits must not move the cached prefix one byte ---
prefix_ref = agents._gm_world_setup(w)
w.patch_scene({"description": "совсем другое описание сцены", "tension": "очень напряжённо"})
w.update_npc("borin", {"persona": "совсем другая личность"})
w.add_fact("новый публичный факт", "public")
assert agents._gm_world_setup(w) == prefix_ref, "scene/npc/fact edits must NOT touch the cached prefix"

# --- hidden_truth: canon + the 'hidden_truth' truth FactRecord stay in sync ---
w.set_hidden_truth("Новая скрытая правда для теста.")
assert w.canon == "Новая скрытая правда для теста."
truth = [r for r in w.fact_records if r.fact_id == "hidden_truth" and r.kind == "truth"]
assert len(truth) == 1 and truth[0].text == "Новая скрытая правда для теста.", "truth fact not synced"
w.set_hidden_truth("")  # emptying removes the duplicate truth record
assert w.canon == ""
assert not [r for r in w.fact_records if r.fact_id == "hidden_truth"], "empty canon must drop truth fact"

# --- hidden_events (GM-only, reach the model via query_world_state) ---
assert w.set_hidden_events(["a", "b"]) == ["a", "b"]
assert w.add_hidden_event("c") and w.hidden_events == ["a", "b", "c"]
assert w.remove_hidden_event(1) and w.hidden_events == ["a", "c"]
assert not w.remove_hidden_event(99)

# --- rumors (reach the model via query_world_state) ---
r = w.add_debug_rumor("Лиза", "видела фигуру в капюшоне")
assert r and any(x.seq == r.seq and x.text == "видела фигуру в капюшоне" for x in w.rumors)
assert w.set_rumor_confirmed(r.seq, True)
assert next(x for x in w.rumors if x.seq == r.seq).confirmed is True
assert w.remove_rumor(r.seq) and not any(x.seq == r.seq for x in w.rumors)

# --- patch_scene: re-points world.constraints (read by the GM turn context) ---
w.patch_scene({"constraints": ["ограничение раз", "ограничение два"]})
assert list(w.constraints) == ["ограничение раз", "ограничение два"]
assert w.constraints is w.scene.constraints, "world.constraints must alias the live scene, not a stale list"

# --- patch_scene: untouched items/exits/present round-trip verbatim ---
n_items, n_exits = len(w.scene.items), len(w.scene.exits)
present_before = set(w.scene.present_npcs)
w.patch_scene({"title": "Новый зал"})
assert w.scene.title == "Новый зал"
assert len(w.scene.items) == n_items and len(w.scene.exits) == n_exits, "untouched items/exits must survive"
assert set(w.scene.present_npcs) == present_before, "untouched present_npcs must survive"

print("OK: debug mutators reach the model + cache prefix stays stable")
