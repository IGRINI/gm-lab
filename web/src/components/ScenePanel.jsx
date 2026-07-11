import { useState } from "react";
import Tooltip, { TipContent } from "./Tooltip.jsx";
import WorldDetailModal from "./WorldDetailModal.jsx";
import "../styles-studio.css";

// The right-side scene panel (UI_REDESIGN_TZ §Игра). It replaces the old inline
// WorldHud and regroups the live game state into three sections — СЦЕНА (title +
// date/time + location), РЯДОМ (present NPCs), ГЕРОЙ (name + hp + «Лист
// персонажа» + «Сохранить в библиотеку»). The save-hero flow (moved here from the
// «мир» card) and the scene/character detail modals are reused verbatim.

function txt(value) {
  return typeof value === "string" ? value.trim() : "";
}

function arr(value) {
  return Array.isArray(value) ? value : [];
}

function npcName(n) {
  return txt(n?.label) || txt(n?.name) || txt(n?.public_label) || txt(n?.id) || "персонаж";
}

function npcColor(n) {
  return txt(n?.color) || "var(--entity-unknown)";
}

export default function ScenePanel({
  time,
  scene,
  playerCharacter,
  npcs = [],
  statusLabels = {},
  charRef = null,
  characters = [],
  canSaveCharacter = false,
  onSaveCharacter,
}) {
  // `detail` = which read-only modal is open: "scene" | "character".
  const [detail, setDetail] = useState(null);
  // Save-hero control state: null (idle), true (choice row open), "busy" (in flight).
  const [saveState, setSaveState] = useState(null);

  const dateLabel =
    txt(time?.current_date_label) || (time?.day_number ? `День ${time.day_number}` : "");
  const timeOfDay = txt(time?.time_of_day);
  const calendar = txt(time?.calendar_name);
  const sceneTitle = txt(scene?.title);
  const pcName = txt(playerCharacter?.name);

  const sceneClickable = !!scene && !!(sceneTitle || txt(scene?.description));
  const pcClickable = !!playerCharacter && !!pcName;

  // Present NPCs: the roster entries whose id is in scene.present_npcs.
  const presentIds = new Set(arr(scene?.present_npcs).map((id) => String(id)));
  const present = arr(npcs).filter((n) => n && presentIds.has(String(n.id)));

  const hp = playerCharacter?.hp && typeof playerCharacter.hp === "object" ? playerCharacter.hp : null;
  const hpText = hp
    ? [hp.current, hp.max].filter((x) => x != null && String(x).trim() !== "").join(" / ")
    : "";

  // §К1.5: "update the source" is offered only when char_ref resolves to a
  // character still present in the loaded library; otherwise only "save as new".
  const sourceId = charRef && charRef.id != null ? String(charRef.id) : "";
  const source =
    sourceId && Array.isArray(characters)
      ? characters.find((c) => c != null && String(c.id) === sourceId) || null
      : null;
  const sourceTitle = txt(source?.title) || txt(source?.preview) || pcName || "исходный";
  const saveBusy = saveState === "busy";

  const runSave = async (characterId) => {
    if (saveBusy || !onSaveCharacter) return;
    setSaveState("busy");
    try {
      await onSaveCharacter(characterId);
    } finally {
      // The transcript notice reports the result; collapse the control either way.
      setSaveState(null);
    }
  };

  const nothingToShow = !dateLabel && !timeOfDay && !sceneTitle && !pcName && present.length === 0;
  if (nothingToShow && !canSaveCharacter) return null;

  return (
    <>
      <aside className="scene-panel" aria-label="Текущее состояние">
        <section className="scene-group">
          <div className="scene-group-kicker">сцена</div>
          <div className="scene-row">
            <span>локация</span>
            {sceneClickable ? (
              <Tooltip
                className="tooltip-block"
                tipClassName="ui-tip-wrap"
                focusable={false}
                content={
                  <TipContent
                    title="Локация"
                    subtitle={sceneTitle || "Текущая сцена"}
                    note="Открыть подробности: описание, персонажи, выходы и предметы."
                  />
                }
              >
                <button type="button" className="scene-link" onClick={() => setDetail("scene")}>
                  {sceneTitle || "—"}
                </button>
              </Tooltip>
            ) : (
              <b>{sceneTitle || "—"}</b>
            )}
          </div>
          <div className="scene-row">
            <span>дата</span>
            <b>{calendar ? `${calendar}, ${dateLabel || "—"}` : dateLabel || "—"}</b>
          </div>
          <div className="scene-row">
            <span>время</span>
            <b>{timeOfDay || "—"}</b>
          </div>
        </section>

        <section className="scene-group">
          <div className="scene-group-kicker">рядом</div>
          {present.length > 0 ? (
            <div className="scene-npcs">
              {present.map((n) => (
                <div className="scene-npc" key={n.id || npcName(n)}>
                  <span className="dot" style={{ "--c": npcColor(n) }} />
                  <b style={{ color: npcColor(n) }}>{npcName(n)}</b>
                </div>
              ))}
            </div>
          ) : (
            <p className="scene-empty">Рядом никого нет.</p>
          )}
        </section>

        {(pcName || canSaveCharacter) && (
          <section className="scene-group">
            <div className="scene-group-kicker">герой</div>
            {pcName && (
              <div className="scene-row">
                <span>имя</span>
                {pcClickable ? (
                  <button type="button" className="scene-link" onClick={() => setDetail("character")}>
                    {pcName}
                  </button>
                ) : (
                  <b>{pcName}</b>
                )}
              </div>
            )}
            {hpText && (
              <div className="scene-row">
                <span>ХП</span>
                <b>{hpText}</b>
              </div>
            )}
            <div className="scene-hero-actions">
              {pcClickable && (
                <button
                  type="button"
                  className="btn small scene-hero-btn"
                  onClick={() => setDetail("character")}
                >
                  Лист персонажа
                </button>
              )}
              {canSaveCharacter &&
                (saveState === true ? (
                  <div className="scene-save-choice">
                    <button
                      type="button"
                      className="btn small scene-hero-btn"
                      onClick={() => runSave(sourceId)}
                      disabled={saveBusy}
                      title={`Перезаписать пакет «${sourceTitle}» текущим состоянием`}
                    >
                      Обновить «{sourceTitle}»
                    </button>
                    <button
                      type="button"
                      className="btn small scene-hero-btn"
                      onClick={() => runSave("")}
                      disabled={saveBusy}
                    >
                      Сохранить как нового
                    </button>
                    <button
                      type="button"
                      className="btn small scene-save-cancel"
                      onClick={() => setSaveState(null)}
                      disabled={saveBusy}
                    >
                      Отмена
                    </button>
                  </div>
                ) : source ? (
                  <button
                    type="button"
                    className="btn small scene-hero-btn"
                    onClick={() => setSaveState(true)}
                    disabled={saveBusy}
                  >
                    {saveBusy ? "Сохраняю…" : "Сохранить в библиотеку"}
                  </button>
                ) : (
                  <button
                    type="button"
                    className="btn small scene-hero-btn"
                    onClick={() => runSave("")}
                    disabled={saveBusy}
                  >
                    {saveBusy ? "Сохраняю…" : "Сохранить в библиотеку"}
                  </button>
                ))}
            </div>
          </section>
        )}
      </aside>

      {detail && (
        <WorldDetailModal
          kind={detail}
          scene={scene}
          playerCharacter={playerCharacter}
          npcs={npcs}
          statusLabels={statusLabels}
          onClose={() => setDetail(null)}
        />
      )}
    </>
  );
}
