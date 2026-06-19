"""CLI-прогон каркаса БЕЗ модели (мок-бэкенд).

Печатает поток событий хода: мысли ГМ -> вызов тула -> субагент NPC -> критик ->
доп. раунд -> финальный нарратив. Проверяет, что вся логика и доп. раунд работают.

Запуск:  python smoke.py        (использует мок, модель не нужна)
"""
import os
import sys

os.environ.setdefault("GM_BACKEND", "mock")  # форсируем мок до импорта config

import config
from llm_client import make_client
from orchestrator import Session, run_turn


def fmt(e: dict):
    k, a, d = e["kind"], e["agent"], e["data"]
    if k in ("delta", "npc_start"):   return None   # токен-стрим — в консоли не нужен
    if k == "player":        return f"\n🧑 {a}: {d}"
    if k == "gm_thinking":   return f"   🧠 ГМ [мысли]: {d}"
    if k == "gm_tool_call":  return f"   🔧 ГМ → tool {d['name']}({d['arguments']})"
    if k == "npc_history":   return f"      🧾 история {a}: {d['messages']} сообщ., summary={d['has_summary']}"
    if k == "dice":          return f"   🎲 {d}"
    if k == "world_fact":    return f"   📖 факт мира: {d}"
    if k == "scene_update":
        if d.get("title") or d.get("scene_id"):
            return f"   🧭 новая сцена: {d.get('title') or d.get('scene_id')} roster={d.get('present_npcs')}"
        state = "в сцене" if d.get("present") else "вне сцены"
        return f"   🧭 сцена: {d.get('name')} теперь {state}; roster={d.get('present_npcs')}"
    if k == "tool_result":   return f"   ↩  tool {a} → ГМ: {d}"
    if k == "npc_thinking":  return f"      🎭 [{a}] мысли: {d}"
    if k == "npc_speech":
        act = f" [{d['action']}]" if d["action"] else ""
        return f"      💬 {a}: «{d['speech']}»{act}  claims={d['claims']}"
    if k == "gm_reject":     return f"      ♻  ГМ вернул действие [{a}] на переделку: {d}"
    if k == "meta":          return f"      ⏱ [{d['label']}] {d['secs']}s · {d['tps']} tok/s · {d['in']}↑ {d['out']}↓ ток"
    if k == "meta_total":    return (f"   Σ ход: {d['secs']}s · {d['tokens']} ток "
                                     f"({d['in']}↑/{d['out']}↓) · {len(d['calls'])} вызовов"
                                     + (f" · ран: {d['run']['tokens']} ток"
                                        if d.get("run") else ""))
    if k == "gm_narration":  return f"   📜 ГМ: {d}"
    if k == "error":         return f"   ❗ {a}: {d}"
    return f"   ? {k}: {d}"


def main():
    print(f"=== SMOKE (backend={config.BACKEND}) ===")
    session = Session(make_client())
    default = "Громко, на весь зал, заявляю Борину: «Я знаю, что ты связан с убийством Алдрика!»"
    turn = sys.argv[1] if len(sys.argv) > 1 else default
    for e in run_turn(session, turn):
        line = fmt(e)
        if line:
            print(line)
    print("\n=== OK ===")


if __name__ == "__main__":
    main()
