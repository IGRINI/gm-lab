import { useContext } from "react";
import { StatusLabelsContext } from "../statusContext.js";
import NpcTooltip from "./NpcTooltip.jsx";
import { useTranslation } from "react-i18next";

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
  const { t } = useTranslation("game");
  const statusLabels = useContext(StatusLabelsContext);
  const brief = normalizeStoryBrief(storyBrief);
  if (brief) {
    return (
      <div className="scene story-brief-card">
        <div className="lead">{t("scene.story")}</div>
        <div className="scene-title">{brief.title || t("scene.introduction")}</div>
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
  const statusText = (status) => ["present", "known", "likely", "rumored", "unknown", "left_scene"].includes(status)
    ? t(`scene.statuses.${status}`)
    : statusLabels[status] || status || t("scene.unknown");
  const npcLabel = (npc) => npc?.label || npc?.name || npc?.public_label || npc?.id
    || t("scene.characterFallback");

  return (
    <div className="scene">
      <div className="lead">{t("scene.title")}</div>
      <div className="scene-title">{title}</div>
      {description && <div className="scene-desc">{description}</div>}
      <div className="legend">
        <span className="legend-label">{t("scene.presentLabel")}</span>
        {present.length ? present.map((n) => (
          <NpcTooltip
            key={n.id || npcLabel(n)}
            className="scene-person-chip"
            npc={n}
            label={npcLabel(n)}
          >
            <span className="dot" style={{ "--c": n.color || "var(--entity-unknown)" }} />
            <span style={{ color: n.color || "var(--entity-unknown)" }}>{npcLabel(n)}</span>
          </NpcTooltip>
        )) : <span>{t("scene.noNamedCharacters")}</span>}
      </div>
      {offscreen.length > 0 && (
        <div className="whereabouts-list">
          <div className="legend-label">{t("scene.whereToFindLabel")}</div>
          {offscreen.map((n) => {
            const w = whereabouts[n.id] || {};
            const place = w.location_name || w.location_id || t("scene.placeUnknown");
            return (
              <NpcTooltip
                as="div"
                className="whereabouts-row"
                key={n.id || npcLabel(n)}
                npc={n}
                label={npcLabel(n)}
                status={statusText(w.status)}
                place={place}
              >
                <span className="dot" style={{ "--c": n.color || "var(--entity-unknown)" }} />
                <b style={{ color: n.color || "var(--entity-unknown)" }}>{npcLabel(n)}</b>
                <span>{statusText(w.status)} · {place}</span>
              </NpcTooltip>
            );
          })}
        </div>
      )}
    </div>
  );
}
