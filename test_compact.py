"""Тест компакта истории ГМ и капа событий (мок, низкие пороги)."""
import os
os.environ["GM_BACKEND"] = "mock"
os.environ["GM_HISTORY_TOKENS"] = "300"   # низкий порог -> компакт сработает быстро
os.environ["GM_KEEP_TURNS"] = "2"
os.environ["GM_EVENTS_CAP"] = "10"

import config
from llm_client import make_client
from orchestrator import Session, run_turn

s = Session(make_client())
sizes = []
for i in range(8):
    for _ in run_turn(s, f"Ход {i}: спрашиваю Борина что нового в городе?"):
        pass
    sizes.append(len(s.gm_messages))

print("gm_messages по ходам:", sizes)
print("gm_summary:", repr(s.gm_summary)[:90])
print("events:", len(s.events), "(cap", config.EVENTS_CAP, ")")

assert s.gm_summary, "саммари должно появиться после компакта"
assert max(sizes) < 20, ("история ГМ не должна расти линейно", sizes)
assert len(s.events) <= config.EVENTS_CAP, "события должны быть капнуты"
# граница хода в начале истории должна быть user-сообщением (целые ходы)
first = s.gm_messages[0]
role = first.get("role") if isinstance(first, dict) else getattr(first, "role", None)
assert role == "user", ("история должна начинаться с целого хода", role)
print("\nCOMPACT TEST PASSED")
