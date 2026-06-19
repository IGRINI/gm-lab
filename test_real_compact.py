"""Реальная модель: компакт истории срабатывает и даёт связную сводку."""
import os
os.environ.setdefault("GM_BACKEND", "llamacpp")
os.environ["GM_HISTORY_TOKENS"] = "400"   # низкий порог -> компакт за пару ходов
os.environ["GM_KEEP_TURNS"] = "2"
from llm_client import make_client
from orchestrator import Session, run_turn

s = Session(make_client())
turns = [
    "Подхожу к Борину и спрашиваю про убийство Алдрика.",
    "Иду к Лизе и спрашиваю, видела ли она что-нибудь прошлой ночью.",
    "Возвращаюсь к Борину и говорю, что Лиза кое-что видела у лавки.",
    "Спрашиваю Лизу, не боится ли она говорить такое при Борине.",
]
for t in turns:
    narr = ""
    for e in run_turn(s, t):
        if e["kind"] == "gm_narration":
            narr = e["data"]
    print(f"\n### {t}")
    print(f"   ГМ: {narr[:180]}")
    print(f"   [история={len(s.gm_messages)} сообщ., саммари={'ЕСТЬ' if s.gm_summary else 'нет'}]")

print("\n" + "=" * 60)
print("СЖАТАЯ СВОДКА (gm_summary):")
print(s.gm_summary or "(пусто — компакт не сработал)")
