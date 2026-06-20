import { useContext } from "react";
import { StatusLabelsContext } from "../statusContext.js";

export default function Scene({ scene, npcs }) {
  const statusLabels = useContext(StatusLabelsContext);
  const sceneObj = scene && typeof scene === "object" ? scene : null;
  const title = sceneObj?.title || scene || "…";
  const description = sceneObj?.description || "";
  const presentIds = new Set(sceneObj?.present_npcs || []);
  const roster = npcs || [];
  const present = sceneObj ? roster.filter((n) => presentIds.has(n.id)) : roster;
  const whereabouts = sceneObj?.npc_whereabouts || {};
  const offscreen = sceneObj
    ? roster.filter((n) => {
        if (presentIds.has(n.id)) return false;
        const w = whereabouts[n.id] || {};
        return (w.status && w.status !== "unknown") || w.location_name || w.details;
      })
    : [];
  const statusText = (status) => statusLabels[status] || status || "неизвестно";
  const npcLabel = (npc) => npc?.label || npc?.name || npc?.public_label || npc?.id || "персонаж";
  const npcHint = (npc) => [
    npc?.role,
    npc?.physical_type,
    npc?.distinctive_features,
    npc?.condition,
  ].filter(Boolean).join(" · ");

  return (
    <div className="scene">
      <div className="lead">Сцена</div>
      <div className="scene-title">{title}</div>
      {description && <div className="scene-desc">{description}</div>}
      <div className="legend">
        <span className="legend-label">В сцене:</span>
        {present.length ? present.map((n) => (
          <span key={n.id || npcLabel(n)} title={npcHint(n) || undefined}>
            <span className="dot" style={{ "--c": n.color || "var(--entity-unknown)" }} />
            <span style={{ color: n.color || "var(--entity-unknown)" }}>{npcLabel(n)}</span>
          </span>
        )) : <span>нет именованных персонажей</span>}
      </div>
      {offscreen.length > 0 && (
        <div className="whereabouts-list">
          <div className="legend-label">Где искать:</div>
          {offscreen.map((n) => {
            const w = whereabouts[n.id] || {};
            const place = w.location_name || w.location_id || "место не установлено";
            return (
              <div className="whereabouts-row" key={n.id || npcLabel(n)} title={npcHint(n) || undefined}>
                <span className="dot" style={{ "--c": n.color || "var(--entity-unknown)" }} />
                <b style={{ color: n.color || "var(--entity-unknown)" }}>{npcLabel(n)}</b>
                <span>{statusText(w.status)} · {place}</span>
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
