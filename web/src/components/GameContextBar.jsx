import Icon from "./Icon.jsx";
import { useEffect, useRef, useState } from "react";
import Modal from "./Modal.jsx";
import WorldDetailModal from "./WorldDetailModal.jsx";
import { useTranslation } from "react-i18next";

// The game context bar (UI_REDESIGN_TZ §Игра). It sits at the top of the chat
// pane and shows the launched game's FIXED, read-only context — История (or
// «Процедурная кампания»), Мир, Персонаж — plus a «⋯» menu that carries the two
// actions moved out of the header: «Скачать JSON» and «Сброс партии» (the latter
// behind a confirm dialog). Nothing here is editable or switchable after launch;
// each badge only opens a read-only info modal.

function txt(value) {
  return typeof value === "string" ? value.trim() : "";
}

function firstText(...values) {
  for (const v of values) {
    const t = txt(v);
    if (t) return t;
  }
  return "";
}

// A read-only info sheet reused for the История / Мир badges (the character badge
// reuses WorldDetailModal's full sheet instead). `rows` are label/value pairs;
// `body` is a free paragraph.
function InfoModal({ title, subtitle, rows = [], body = "", empty, onClose }) {
  const { t } = useTranslation("game");
  useEffect(() => {
    const onKey = (event) => {
      if (event.key === "Escape") onClose?.();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);
  const visibleRows = rows.filter(([, value]) => txt(value));
  const hasBody = !!txt(body);
  return (
    <Modal title={title} subtitle={subtitle} onClose={onClose} className="wd-modal">
      <div className="wd">
        {visibleRows.length > 0 && (
          <div className="wd-fields">
            {visibleRows.map(([label, value]) => (
              <div className="wd-field wd-field--wide" key={label}>
                <span className="wd-field-k">{label}</span>
                <b className="wd-field-v">{txt(value)}</b>
              </div>
            ))}
          </div>
        )}
        {hasBody && <p className="wd-desc">{txt(body)}</p>}
        {visibleRows.length === 0 && !hasBody && (
          <p className="wd-empty">{empty || t("context.noDetails")}</p>
        )}
      </div>
    </Modal>
  );
}

// One read-only context badge (История / Мир / Персонаж). Clickable badges open
// their info modal; static ones render as a plain box.
function Badge({ kicker, value, onClick, clickable }) {
  const body = (
    <>
      <span className="game-badge-k">{kicker}</span>
      <b className="game-badge-v">{value}</b>
    </>
  );
  if (clickable) {
    return (
      <button type="button" className="game-badge game-badge-btn" onClick={onClick}>
        {body}
      </button>
    );
  }
  return <div className="game-badge">{body}</div>;
}

// The «Сброс партии» confirm dialog (reuses the shared .confirm-* styling).
function ResetConfirm({ busy, onConfirm, onCancel }) {
  const { t } = useTranslation("game");
  useEffect(() => {
    const onKey = (event) => {
      if (event.key === "Escape" && !busy) onCancel?.();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [busy, onCancel]);
  return (
    <div className="confirm-backdrop" role="presentation" onMouseDown={() => !busy && onCancel?.()}>
      <div
        className="confirm-card"
        role="dialog"
        aria-modal="true"
        onMouseDown={(event) => event.stopPropagation()}
      >
        <div className="confirm-icon" aria-hidden="true"><Icon name="refresh" size={20} /></div>
        <h3>{t("context.reset.title")}</h3>
        <p className="confirm-note">{t("context.reset.note")}</p>
        <div className="confirm-actions">
          <button type="button" className="btn" onClick={onCancel} disabled={busy}>
            {t("actions.cancel")}
          </button>
          <button
            type="button"
            className="btn confirm-danger"
            onClick={onConfirm}
            disabled={busy}
          >
            {busy ? t("context.reset.busy") : t("context.reset.action")}
          </button>
        </div>
      </div>
    </div>
  );
}

export default function GameContextBar({
  story = null,
  world = null,
  procedural = false,
  playerCharacter = null,
  scene = null,
  npcs = [],
  statusLabels = {},
  mapAvailable = false,
  onOpenMap,
  onExportJson,
  onReset,
  locked = false,
}) {
  const { t } = useTranslation("game");
  // `detail` = which read-only modal is open: "story" | "world" | "character".
  const [detail, setDetail] = useState(null);
  const [menuOpen, setMenuOpen] = useState(false);
  const [confirmReset, setConfirmReset] = useState(false);
  const [resetBusy, setResetBusy] = useState(false);
  const menuRef = useRef(null);

  // Close the «⋯» menu on an outside click / Escape.
  useEffect(() => {
    if (!menuOpen) return undefined;
    const onDown = (event) => {
      if (menuRef.current && !menuRef.current.contains(event.target)) setMenuOpen(false);
    };
    const onKey = (event) => {
      if (event.key === "Escape") setMenuOpen(false);
    };
    window.addEventListener("mousedown", onDown);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("mousedown", onDown);
      window.removeEventListener("keydown", onKey);
    };
  }, [menuOpen]);

  const storyTitle = procedural
    ? t("context.proceduralCampaign")
    : firstText(story?.title, story?.name) || t("context.story");
  const worldName = firstText(world?.title, world?.name, world?.world_lore?.name) || t("context.world");
  const pcName = firstText(playerCharacter?.name) || t("context.character");

  const storyBody = firstText(story?.story_brief, story?.description);
  const storyInfoAvailable = !procedural && !!storyBody;
  const worldBody = firstText(
    world?.public_premise,
    world?.premise,
    world?.world_lore?.premise,
    world?.preview
  );
  const worldRows = [
    [t("context.genre"), world?.genre],
    [t("context.tone"), world?.tone],
  ];
  const worldInfoAvailable = !!worldName && (!!worldBody || worldRows.some(([, v]) => txt(v)));
  const pcInfoAvailable = !!playerCharacter && !!firstText(playerCharacter?.name);

  const runReset = async () => {
    if (resetBusy || !onReset) return;
    setResetBusy(true);
    try {
      await onReset();
      setConfirmReset(false);
      setMenuOpen(false);
    } finally {
      setResetBusy(false);
    }
  };

  return (
    <div className="game-context-bar">
      <div className="game-badges">
        <Badge
          kicker={t("context.story")}
          value={storyTitle}
          clickable={storyInfoAvailable}
          onClick={() => setDetail("story")}
        />
        <Badge
          kicker={t("context.world")}
          value={worldName}
          clickable={worldInfoAvailable}
          onClick={() => setDetail("world")}
        />
        <Badge
          kicker={t("context.character")}
          value={pcName}
          clickable={pcInfoAvailable}
          onClick={() => setDetail("character")}
        />
      </div>

      <div className="game-context-actions">
        <button
          type="button"
          className="btn btn-icon game-context-map"
          aria-label={t("context.openMap")}
          title={t("context.openMap")}
          disabled={!mapAvailable}
          onClick={onOpenMap}
        >
          <Icon name="map" size={18} />
        </button>
        <div className="game-context-menu" ref={menuRef}>
          <button
            type="button"
            className="btn btn-icon game-context-more"
            aria-label={t("context.actionsAria")}
            aria-haspopup="true"
            aria-expanded={menuOpen}
            onClick={() => setMenuOpen((open) => !open)}
          >
            <Icon name="dots" size={18} />
          </button>
          {menuOpen && (
            <div className="game-context-dropdown" role="menu">
              <button
                type="button"
                className="game-context-item"
                role="menuitem"
                onClick={() => {
                  setMenuOpen(false);
                  onExportJson?.();
                }}
              >
                {t("context.downloadJson")}
              </button>
              <button
                type="button"
                className="game-context-item danger"
                role="menuitem"
                disabled={locked}
                onClick={() => {
                  setMenuOpen(false);
                  setConfirmReset(true);
                }}
              >
                {t("context.reset.menu")}
              </button>
            </div>
          )}
        </div>
      </div>

      {detail === "story" && (
        <InfoModal
          title={storyTitle}
          subtitle={t("context.story")}
          body={storyBody}
          empty={t("context.storyUnavailable")}
          onClose={() => setDetail(null)}
        />
      )}
      {detail === "world" && (
        <InfoModal
          title={worldName}
          subtitle={t("context.world")}
          rows={worldRows}
          body={worldBody}
          empty={t("context.worldUnavailable")}
          onClose={() => setDetail(null)}
        />
      )}
      {detail === "character" && (
        <WorldDetailModal
          kind="character"
          playerCharacter={playerCharacter}
          scene={scene}
          npcs={npcs}
          statusLabels={statusLabels}
          onClose={() => setDetail(null)}
        />
      )}
      {confirmReset && (
        <ResetConfirm
          busy={resetBusy}
          onConfirm={runReset}
          onCancel={() => setConfirmReset(false)}
        />
      )}
    </div>
  );
}
