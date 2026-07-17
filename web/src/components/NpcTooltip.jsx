import { useTranslation } from "react-i18next";
import Tooltip from "./Tooltip.jsx";
import { ZoomableImage } from "./ImagePreview.jsx";

function text(value) {
  return typeof value === "string" ? value.trim() : "";
}

function NpcTipRow({ label, value }) {
  if (!value) return null;
  return (
    <div className="scene-tip-row">
      <span>{label}</span>
      <b>{value}</b>
    </div>
  );
}

export function NpcTooltipContent({ npc, label, status = "", place = "", eyebrow = "" }) {
  const { t } = useTranslation("game");
  const portraitUrl = text(npc?.portrait_url);
  const title = label || text(npc?.label) || text(npc?.name) || text(npc?.id)
    || t("scene.characterFallback");
  const heading = eyebrow || (place ? t("scene.whereToFind") : t("scene.inScene"));

  return (
    <div className="scene-tip">
      <div className={"scene-tip-top" + (portraitUrl ? " has-portrait" : "")}>
        {portraitUrl && (
          <ZoomableImage
            className="scene-tip-portrait"
            src={portraitUrl}
            alt={title}
            title={title}
            loading="lazy"
          />
        )}
        <div className="scene-tip-head">
          <span>{heading}</span>
          <b>{title}</b>
        </div>
      </div>
      <div className="scene-tip-rows">
        <NpcTipRow label={t("scene.fields.role")} value={text(npc?.role)} />
        <NpcTipRow label={t("scene.fields.type")} value={text(npc?.physical_type)} />
        <NpcTipRow label={t("scene.fields.appearance")} value={text(npc?.current_appearance)} />
        <NpcTipRow label={t("scene.fields.features")} value={text(npc?.distinctive_features)} />
        <NpcTipRow label={t("scene.fields.condition")} value={text(npc?.condition)} />
        <NpcTipRow label={t("scene.fields.status")} value={status} />
        <NpcTipRow label={t("scene.fields.landmark")} value={place} />
      </div>
    </div>
  );
}

export default function NpcTooltip({
  npc,
  label,
  status = "",
  place = "",
  eyebrow = "",
  children,
  className = "",
  as = "span",
  style,
}) {
  return (
    <Tooltip
      as={as}
      className={className}
      style={style}
      tipClassName="scene-tip-wrap"
      pinnable
      content={(
        <NpcTooltipContent
          npc={npc}
          label={label}
          status={status}
          place={place}
          eyebrow={eyebrow}
        />
      )}
    >
      {children}
    </Tooltip>
  );
}
