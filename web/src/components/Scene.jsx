import { useContext } from "react";
import { StatusLabelsContext } from "../statusContext.js";
import Tooltip from "./Tooltip.jsx";

function SceneTipRow({ label, value }) {
  if (!value) return null;
  return (
    <div className="scene-tip-row">
      <span>{label}</span>
      <b>{value}</b>
    </div>
  );
}

function SceneNpcTip({ npc, label, status, place }) {
  const role = npc?.role || "";
  const type = npc?.physical_type || "";
  const features = npc?.distinctive_features || "";
  const condition = npc?.condition || "";
  return (
    <div className="scene-tip">
      <div className="scene-tip-head">
        <span>{place ? "где искать" : "в сцене"}</span>
        <b>{label}</b>
      </div>
      <div className="scene-tip-rows">
        <SceneTipRow label="роль" value={role} />
        <SceneTipRow label="тип" value={type} />
        <SceneTipRow label="приметы" value={features} />
        <SceneTipRow label="состояние" value={condition} />
        <SceneTipRow label="статус" value={status} />
        <SceneTipRow label="ориентир" value={place} />
      </div>
    </div>
  );
}

function textValue(value) {
  return typeof value === "string" ? value.trim() : "";
}

function normalizeStoryBrief(storyBrief) {
  if (!storyBrief) return null;
  if (typeof storyBrief === "string") {
    const text = textValue(storyBrief);
    return text ? { title: "", text } : null;
  }
  if (typeof storyBrief !== "object") return null;
  const text = textValue(storyBrief.text) || textValue(storyBrief.description) || textValue(storyBrief.brief);
  if (!text) return null;
  return {
    title: textValue(storyBrief.title),
    text,
  };
}

export default function Scene({ storyBrief, scene, npcs }) {
  const statusLabels = useContext(StatusLabelsContext);
  const brief = normalizeStoryBrief(storyBrief);
  if (brief) {
    return (
      <div className="scene story-brief-card">
        <div className="lead">История</div>
        <div className="scene-title">{brief.title || "Вступление"}</div>
        <div className="scene-desc">{brief.text}</div>
      </div>
    );
  }

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

  return (
    <div className="scene">
      <div className="lead">Сцена</div>
      <div className="scene-title">{title}</div>
      {description && <div className="scene-desc">{description}</div>}
      <div className="legend">
        <span className="legend-label">В сцене:</span>
        {present.length ? present.map((n) => (
          <Tooltip
            key={n.id || npcLabel(n)}
            className="scene-person-chip"
            tipClassName="scene-tip-wrap"
            content={<SceneNpcTip npc={n} label={npcLabel(n)} />}
          >
            <span className="dot" style={{ "--c": n.color || "var(--entity-unknown)" }} />
            <span style={{ color: n.color || "var(--entity-unknown)" }}>{npcLabel(n)}</span>
          </Tooltip>
        )) : <span>нет именованных персонажей</span>}
      </div>
      {offscreen.length > 0 && (
        <div className="whereabouts-list">
          <div className="legend-label">Где искать:</div>
          {offscreen.map((n) => {
            const w = whereabouts[n.id] || {};
            const place = w.location_name || w.location_id || "место не установлено";
            return (
              <Tooltip
                as="div"
                className="whereabouts-row"
                tipClassName="scene-tip-wrap"
                key={n.id || npcLabel(n)}
                content={<SceneNpcTip npc={n} label={npcLabel(n)} status={statusText(w.status)} place={place} />}
              >
                <span className="dot" style={{ "--c": n.color || "var(--entity-unknown)" }} />
                <b style={{ color: n.color || "var(--entity-unknown)" }}>{npcLabel(n)}</b>
                <span>{statusText(w.status)} · {place}</span>
              </Tooltip>
            );
          })}
        </div>
      )}
    </div>
  );
}
