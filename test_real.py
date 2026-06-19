"""Реальная модель: память + осведомлённость через 2 хода в одной сессии."""
import os
os.environ.setdefault("GM_BACKEND", "llamacpp")
from llm_client import make_client
from orchestrator import Session, run_turn

s = Session(make_client())


def play(text):
    print(f"\n### ИГРОК: {text}")
    for e in run_turn(s, text):
        k, a, d = e["kind"], e["agent"], e["data"]
        if k == "gm_tool_call":
            args = {kk: vv for kk, vv in d["arguments"].items() if kk != "situation"}
            print(f"   → {d['name']}({args})")
        elif k == "npc_speech":
            act = f" [{d['action']}]" if d["action"] else ""
            print(f"   {a}: «{d['speech']}»{act}")
        elif k == "gm_narration":
            print(f"   ГМ: {d[:220]}")


play("Подхожу к Борину и в лоб спрашиваю: это правда, что Алдрика убила гильдия?")

print("\n" + "=" * 60)
print("ЧТО ЛИЗА УВИДИТ/УСЛЫШИТ НА СЛЕД. ХОДУ (её observations):")
print(s.observations("lysa") or "(пусто)")
print("=" * 60)

play("Поворачиваюсь к Лизе: ты слышала, о чём я говорил с Борином? Что скажешь?")

print("\n" + "=" * 60)
print("ПАМЯТЬ БОРИНА (commitments — держит консистентность):")
print(s.commit_text("borin") or "(пусто)")
print("\nЛОГ СОБЫТИЙ:")
for ev_ in s.events:
    print(f"  seq{ev_.seq} {ev_.actor}/{ev_.kind}: «{ev_.speech}»"
          + (f" [{ev_.action}]" if ev_.action else ""))
print("\nИЗОЛЯЦИЯ: в observations Лизы нет reasoning/secret-маркеров?")
obs_lysa_now = " ".join(e.speech + e.action for e in s.events)
leak = [w for w in ("осведомитель", "reasoning", "claims") if w in obs_lysa_now.lower()]
print("  утечки:", leak or "нет")
