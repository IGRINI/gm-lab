import { useState } from "react";
import Tooltip, { TipContent } from "./Tooltip.jsx";
import WorldDetailModal from "./WorldDetailModal.jsx";
import Icon from "./Icon.jsx";
import { useTranslation } from "react-i18next";

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

function npcName(n, fallback) {
  return txt(n?.label) || txt(n?.name) || txt(n?.public_label) || txt(n?.id) || fallback;
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
  const { t } = useTranslation("game");
  // `detail` = which read-only modal is open: "scene" | "character".
  const [detail, setDetail] = useState(null);
  // Save-hero control state: null (idle), true (choice row open), "busy" (in flight).
  const [saveState, setSaveState] = useState(null);
  // Узкие экраны: панель свёрнута в одну строку-саммари; тап раскрывает.
  // На десктопе саммари скрыта CSS'ом и состояние ни на что не влияет.
  const [expanded, setExpanded] = useState(false);

  const dateLabel =
    txt(time?.current_date_label)
    || (time?.day_number ? t("scenePanel.day", { count: time.day_number }) : "");
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
  const sourceTitle = txt(source?.title) || txt(source?.preview) || pcName
    || t("scenePanel.sourceFallback");
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
      <aside className={"scene-panel" + (expanded ? " is-open" : "")} aria-label={t("scenePanel.stateAria")}>
        <button
          type="button"
          className="scene-panel-summary"
          onClick={() => setExpanded((v) => !v)}
          aria-expanded={expanded}
          aria-label={expanded ? t("scenePanel.collapseAria") : t("scenePanel.expandAria")}
        >
          <Icon name="pin" size={13} />
          <span className="scene-summary-main">{sceneTitle || t("scene.title")}</span>
          {timeOfDay && <span className="scene-summary-meta">{timeOfDay}</span>}
          {pcName && (
            <span className="scene-summary-meta">
              {pcName}
              {hpText ? ` · ${hpText}` : ""}
            </span>
          )}
          <Icon name={expanded ? "chevron-up" : "chevron-down"} size={14} className="scene-summary-caret" />
        </button>
        <section className="scene-group">
          <div className="scene-group-kicker">{t("scenePanel.scene")}</div>
          <div className="scene-row">
            <span>{t("scenePanel.location")}</span>
            {sceneClickable ? (
              <Tooltip
                className="tooltip-block"
                tipClassName="ui-tip-wrap"
                focusable={false}
                content={
                  <TipContent
                    title={t("scenePanel.locationTitle")}
                    subtitle={sceneTitle || t("scenePanel.currentScene")}
                    note={t("scenePanel.locationNote")}
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
            <span>{t("scenePanel.date")}</span>
            <b>{calendar ? `${calendar}, ${dateLabel || "—"}` : dateLabel || "—"}</b>
          </div>
          <div className="scene-row">
            <span>{t("scenePanel.time")}</span>
            <b>{timeOfDay || "—"}</b>
          </div>
        </section>

        <section className="scene-group">
          <div className="scene-group-kicker">{t("scenePanel.nearby")}</div>
          {present.length > 0 ? (
            <div className="scene-npcs">
              {present.map((n) => (
                <div className="scene-npc" key={n.id || npcName(n, t("scene.characterFallback"))}>
                  <span className="dot" style={{ "--c": npcColor(n) }} />
                  <b style={{ color: npcColor(n) }}>{npcName(n, t("scene.characterFallback"))}</b>
                </div>
              ))}
            </div>
          ) : (
            <p className="scene-empty">{t("scenePanel.nobodyNearby")}</p>
          )}
        </section>

        {(pcName || canSaveCharacter) && (
          <section className="scene-group">
            <div className="scene-group-kicker">{t("scenePanel.hero")}</div>
            {pcName && (
              <div className="scene-row">
                <span>{t("scenePanel.name")}</span>
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
                <span>{t("scenePanel.hp")}</span>
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
                  <Icon name="user" size={13} /> {t("scenePanel.characterSheet")}
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
                      title={t("scenePanel.overwriteTitle", { title: sourceTitle })}
                    >
                      {t("scenePanel.updateSource", { title: sourceTitle })}
                    </button>
                    <button
                      type="button"
                      className="btn small scene-hero-btn"
                      onClick={() => runSave("")}
                      disabled={saveBusy}
                    >
                      {t("scenePanel.saveAsNew")}
                    </button>
                    <button
                      type="button"
                      className="btn small scene-save-cancel"
                      onClick={() => setSaveState(null)}
                      disabled={saveBusy}
                    >
                      {t("actions.cancel")}
                    </button>
                  </div>
                ) : source ? (
                  <button
                    type="button"
                    className="btn small scene-hero-btn"
                    onClick={() => setSaveState(true)}
                    disabled={saveBusy}
                  >
                    <Icon name="download" size={13} /> {saveBusy ? t("scenePanel.saving") : t("scenePanel.saveToLibrary")}
                  </button>
                ) : (
                  <button
                    type="button"
                    className="btn small scene-hero-btn"
                    onClick={() => runSave("")}
                    disabled={saveBusy}
                  >
                    <Icon name="download" size={13} /> {saveBusy ? t("scenePanel.saving") : t("scenePanel.saveToLibrary")}
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
