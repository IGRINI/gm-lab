"""Прямой тест логики памяти Session (без модели) — крайние случаи из ревью."""
import os
os.environ.setdefault("GM_BACKEND", "mock")
from orchestrator import Session


def fresh():
    return Session(None)


# A) Неадъяцентная коррекция (critical/major): player -> borin(черновик1) ->
#    lysa(видит борина) -> borin(коррекция->финал) -> конец хода.
s = fresh(); s.turn = 1
s.record_public("player", "speech", speech="ОБВИНЕНИЕ")
ob = s.observations("borin"); s.snapshot_shown("borin")
assert "ОБВИНЕНИЕ" not in ob, ("текущее действие игрока идёт через situation, не observations", ob)
s.draft("borin", "ЧЕРНОВИК1", "", ["claimX"])
ol = s.observations("lysa"); s.snapshot_shown("lysa")
assert "ЧЕРНОВИК1" in ol, ("lysa должна видеть pending борина", ol)
s.draft("lysa", "РЕПЛИКА-ЛИЗЫ", "", [])
ob2 = s.observations("borin"); s.snapshot_shown("borin")
assert "РЕПЛИКА-ЛИЗЫ" in ob2, ("borin на коррекции должен видеть лизу", ob2)
s.draft("borin", "ФИНАЛ-БОРИНА", "", [])      # перезапись того же seq
s.commit_turn()
sp = [e.speech for e in s.events]
assert "ЧЕРНОВИК1" not in sp, ("отвергнутый черновик должен исчезнуть", sp)
assert sp.count("ФИНАЛ-БОРИНА") == 1, ("без двойной записи", sp)
assert "РЕПЛИКА-ЛИЗЫ" in sp
assert all("claimX" not in b for b in s.commitments.get("borin", [])), "claim отвергнутого ушёл"
print("A OK (неадъяцентная коррекция):", sp)

# B) delivered не перескакивает: player -> borin -> dice -> конец; на след. ходу
#    borin ОБЯЗАН увидеть кубы (раньше они терялись).
s = fresh(); s.turn = 1
s.record_public("player", "speech", speech="P")
s.observations("borin"); s.snapshot_shown("borin")
s.draft("borin", "Б", "", [])
s.record_public("gm", "dice", action="1d20=7")
s.commit_turn()
s.turn = 2
ob = s.observations("borin")
assert "1d20=7" in ob, ("кубы после пробуждения должны прийти на след. ход", ob)
assert "Б" not in ob, ("своя реплика — не наблюдение", ob)
print("B OK (нет перескока delivered):", repr(ob))

# C) Изоляция: Event структурно без reasoning/claims/secret; claim одного NPC
#    не виден другому в наблюдениях.
s = fresh(); s.turn = 1
s.record_public("player", "speech", speech="P")
s.draft("borin", "речь", "действие", ["секретный_клейм"])
s.commit_turn()
for e in s.events:
    assert not any(hasattr(e, a) for a in ("reasoning", "claims", "secret"))
s.turn = 2
assert "секретный_клейм" not in s.observations("lysa"), "claim борина не должен течь лизе"
print("C OK (изоляция секретов)")

# D) Пустой черновик не пишется.
s = fresh(); s.turn = 1
s.draft("borin", "", "", [])
s.commit_turn()
assert s.events == [] and "borin" not in s.commitments, "пустой черновик не должен сохраняться"
print("D OK (пустой черновик игнорится)")

print("\nALL MEMORY LOGIC TESTS PASSED")
