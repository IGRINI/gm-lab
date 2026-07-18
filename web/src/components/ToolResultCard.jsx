import Icon from "./Icon.jsx";
import { useTranslation } from "react-i18next";
import MarkdownText, { MarkdownInline } from "./MarkdownText.jsx";
import Spoiler from "./Spoiler.jsx";
import Tooltip from "./Tooltip.jsx";
import { NpcRef, Field, Badge, TextBlock, nonEmpty, ActorRef, ParticipantChips } from "./ToolCard.jsx";
import { localizeServerMessage } from "../serverMessages.js";
import { characterChangeFieldLabel } from "../characterChanges.js";

// Presentation-only maps mirroring the request-card vocabulary in ToolCard.jsx.
const FACT_STATUS = {
  known: "ok",
  likely: "warn",
  rumored: "warn",
  confirmed: "ok",
  unknown: "muted",
};
const WS_TYPE = {
  fact: "ok",
  rumor: "warn",
  npc_memory: "",
  relationship: "",
  goal: "",
  public_lookup: "ok",
};
const WS_OP = {
  add: "ok",
  update: "warn",
  delete: "redo",
};
// query_world_state rows carry varied kinds across scopes (state_* prefixes plus
// canon/public/hidden). Normalise to a friendly Russian label + tone.
const QUERY_KIND = {
  public_lookup: "ok",
  public_intro: "ok",
  gm_canon: "",
  hidden_event: "warn",
};
function queryKind(kind) {
  const k = String(kind || "");
  if (Object.hasOwn(QUERY_KIND, k)) return { kind: k, tone: QUERY_KIND[k] };
  const bare = k.replace(/^state_/, "");
  if (Object.hasOwn(WS_TYPE, bare)) return { kind: bare, tone: WS_TYPE[bare] };
  return { kind: bare || "record", tone: "" };
}

function resultError(value, t, fallbackKey) {
  return localizeServerMessage(value, t, { fallbackText: t(fallbackKey) });
}

// id: stable concurrency key the model uses; short and monospace for the eye.
function ShortId({ id }) {
  const { t } = useTranslation("developer");
  if (!nonEmpty(id)) return null;
  const s = String(id);
  return (
    <Tooltip className="tc-id" tipClassName="tool-tip" content={t("results.recordId", { id: s })}>
      {s.length > 14 ? s.slice(0, 13) + "…" : s}
    </Tooltip>
  );
}

function Target({ id }) {
  return <span className="tc-arrow-to">→ <ActorRef id={id} /></span>;
}

export function resultView(name, p, t) {
  const payload = p || {};
  switch (name) {
    case "get_world_fact": {
      const statusName = payload.status || "unknown";
      const statusTone = FACT_STATUS[payload.status] || "muted";
      const sources = Array.isArray(payload.sources) ? payload.sources : [];
      const delivered = Number(payload.already_delivered || 0);
      return {
        icon: <Icon name="book" size={14} />,
        accent: "var(--md-link)",
        title: t("results.worldFact.title"),
        body: (
          <>
            <div className="tc-chips">
              <Badge tone={payload.status === "already_delivered" ? "muted" : statusTone}>
                {payload.status === "already_delivered" ? t("results.noNew") : t(`factStatus.${statusName}`, { defaultValue: payload.status || "—" })}
              </Badge>
              {delivered > 0 && <Badge tone="muted">{t("results.alreadyDelivered", { count: delivered })}</Badge>}
            </div>
            {nonEmpty(payload.text) && <TextBlock>{payload.text}</TextBlock>}
            {sources.length > 0 && (
              <Field label={t("results.sources.label")}>
                <div className="tc-list">
                  {sources.map((s, i) => (
                    <Tooltip
                      as="div"
                      className="tc-source"
                      tipClassName="tool-tip"
                      key={s.n ?? i}
                      content={[
                        s.kind ? t("results.sources.kind", { value: s.kind }) : "",
                        s.status ? t("results.sources.status", { value: s.status }) : "",
                        s.source ? t("results.sources.source", { value: s.source }) : "",
                        s.score != null ? t("results.sources.relevance", { value: s.score }) : "",
                      ].filter(Boolean).join("\n")}
                    >
                      <span className="tc-source-n">[{s.n ?? i + 1}]</span>
                      <span>{s.kind || "—"}</span>
                      {nonEmpty(s.status) && <span className="tc-source-meta">· {s.status}</span>}
                      {nonEmpty(s.source) && <span className="tc-source-meta">· {s.source}</span>}
                    </Tooltip>
                  ))}
                </div>
              </Field>
            )}
          </>
        ),
      };
    }

    case "query_world_state": {
      const results = Array.isArray(payload.results) ? payload.results : [];
      const delivered = Number(payload.already_delivered || 0);
      const foundLabel = results.length
        ? t("results.found", { count: results.length })
        : delivered > 0
          ? t("results.noNew")
          : t("results.nothingFound");
      return {
        icon: <Icon name="search" size={14} />,
        accent: "var(--md-link)",
        title: t("results.worldQuery.title"),
        body: (
          <>
            <div className="tc-chips">
              <Badge tone={results.length ? "ok" : "muted"}>{foundLabel}</Badge>
              {delivered > 0 && <Badge tone="muted">{t("results.alreadyDelivered", { count: delivered })}</Badge>}
              {nonEmpty(payload.scope) && <Badge tone="muted">{t(`worldState.scopes.${payload.scope}`, { defaultValue: payload.scope })}</Badge>}
            </div>
            {nonEmpty(payload.text) && <TextBlock>{payload.text}</TextBlock>}
            {results.length > 0 && (
              <div className="tc-ws-list">
                {results.map((r, i) => {
                  const typ = queryKind(r.kind);
                  return (
                    <div className="tc-ws-item" key={r.id || i}>
                      <div className="tc-chips">
                        <Badge tone={typ.tone}>{t(`queryKinds.${typ.kind}`, { defaultValue: t(`worldState.types.${typ.kind}`, { defaultValue: typ.kind }) })}</Badge>
                        {nonEmpty(r.scope) && <Badge tone="muted">{t(`worldState.scopes.${r.scope}`, { defaultValue: r.scope })}</Badge>}
                        {nonEmpty(r.npc_id) && <NpcRef id={r.npc_id} />}
                        {nonEmpty(r.target) && <Target id={r.target} />}
                        <ParticipantChips ids={r.participants} />
                        <ShortId id={r.id} />
                      </div>
                      {nonEmpty(r.text) && <TextBlock>{r.text}</TextBlock>}
                    </div>
                  );
                })}
              </div>
            )}
          </>
        ),
      };
    }

    case "update_world_state": {
      const applied = Array.isArray(payload.applied) ? payload.applied : [];
      const errors = Array.isArray(payload.errors) ? payload.errors : [];
      return {
        icon: <Icon name="sparkles" size={14} />,
        accent: "var(--entity-note)",
        title: t("results.worldUpdate.title"),
        body: (
          <>
            <div className="tc-chips">
              <Badge tone={applied.length ? "ok" : "muted"}>{t("results.saved", { count: applied.length })}</Badge>
              {errors.length > 0 && <Badge tone="redo">{t("results.rejected", { count: errors.length })}</Badge>}
            </div>
            {applied.length > 0 && (
              <div className="tc-ws-list">
                {applied.map((row, i) => {
                  const opName = row.op || "add";
                  const opTone = WS_OP[opName] || WS_OP.add;
                  const typeTone = WS_TYPE[row.type] || "";
                  return (
                    <div className="tc-result-row" key={row.id || i}>
                      <Badge tone={opTone}>{t(`worldState.operationResults.${opName}`, { defaultValue: opName })}</Badge>
                      {nonEmpty(row.type) && <Badge tone={typeTone}>{t(`worldState.types.${row.type}`, { defaultValue: row.type })}</Badge>}
                      {nonEmpty(row.scope) && <Badge tone="muted">{t(`worldState.scopes.${row.scope}`, { defaultValue: row.scope })}</Badge>}
                      {nonEmpty(row.npc_id) && <NpcRef id={row.npc_id} />}
                      {nonEmpty(row.target) && <Target id={row.target} />}
                      <ParticipantChips ids={row.participants} />
                      <ShortId id={row.id} />
                    </div>
                  );
                })}
              </div>
            )}
            {errors.length > 0 && (
              <Field label={t("results.notSaved")}>
                <div className="tc-list">
                  {errors.map((e, i) => (
                    <div className="tc-text redo" key={i}>
                      <MarkdownInline>{resultError(e, t, "results.writeError")}</MarkdownInline>
                    </div>
                  ))}
                </div>
              </Field>
            )}
          </>
        ),
      };
    }

    case "get_npc_profile": {
      const profile = (payload.profile && typeof payload.profile === "object") ? payload.profile : {};
      const keys = Object.keys(profile);
      return {
        icon: <Icon name="user" size={14} />,
        accent: "var(--brand-text)",
        title: (
          <>
            {t("results.npcProfile.title")}<NpcRef id={payload.npc_id} />
          </>
        ),
        body: (
          <>
            <div className="tc-chips">
              {nonEmpty(payload.preset) && <Badge tone="muted">{t(`profilePresets.${payload.preset}`, { defaultValue: payload.preset })}</Badge>}
              {payload.card_revision != null && <Badge tone="muted">{t("results.revision", { value: payload.card_revision })}</Badge>}
            </div>
            {nonEmpty(payload.error) ? (
              <div className="tc-text redo">
                <MarkdownInline>{resultError(payload, t, "results.profileError")}</MarkdownInline>
              </div>
            ) : keys.length ? (
              keys.map((k) => (
                <Field key={k} label={characterChangeFieldLabel(k, t)}>
                  {typeof profile[k] === "object"
                    ? <code>{JSON.stringify(profile[k])}</code>
                    : <MarkdownInline>{String(profile[k])}</MarkdownInline>}
                </Field>
              ))
            ) : (
              <div className="tc-text">{t("results.noFields")}</div>
            )}
          </>
        ),
      };
    }

    case "advance_time": {
      const current = (payload.current && typeof payload.current === "object") ? payload.current : {};
      const now = [current.current_date_label, current.time_of_day].filter(nonEmpty).join(" · ");
      return {
        icon: <Icon name="clock" size={14} />,
        accent: "var(--md-em)",
        title: t("results.worldTime.title"),
        body: (
          <>
            <div className="tc-chips">
              {payload.elapsed_minutes != null && <Badge tone="warn">{t("time.plusMinutes", { count: payload.elapsed_minutes })}</Badge>}
              {nonEmpty(now) && <Badge tone="ok">{now}</Badge>}
            </div>
            {nonEmpty(payload.summary) && <TextBlock>{payload.summary}</TextBlock>}
            {nonEmpty(payload.error) && (
              <div className="tc-text redo">
                <MarkdownInline>{resultError(payload, t, "results.timeError")}</MarkdownInline>
              </div>
            )}
          </>
        ),
      };
    }

    case "update_character":
    case "update_player_character": {
      const updated = Array.isArray(payload.updated) ? payload.updated : [];
      const isNpc = payload.target === "npc";
      return {
        icon: <Icon name="shield" size={14} />,
        accent: "var(--player)",
        title: isNpc ? (
          <>
            {t("results.character.npcTitle")}
            {nonEmpty(payload.npc_id)
              ? <NpcRef id={payload.npc_id} />
              : nonEmpty(payload.label) && <Badge tone="muted">{payload.label}</Badge>}
          </>
        ) : t("results.character.playerTitle"),
        body: (
          <>
            <div className="tc-chips">
              {updated.length
                ? updated.map((f) => <Badge key={f} tone="ok">{characterChangeFieldLabel(f, t)}</Badge>)
                : <Badge tone="muted">{t("common.noChanges")}</Badge>}
              {payload.card_revision != null && <Badge tone="muted">{t("results.revision", { value: payload.card_revision })}</Badge>}
            </div>
            {nonEmpty(payload.reason) && <TextBlock>{payload.reason}</TextBlock>}
            {nonEmpty(payload.error) && (
              <div className="tc-text redo">
                <MarkdownInline>{resultError(payload, t, "results.characterError")}</MarkdownInline>
              </div>
            )}
          </>
        ),
      };
    }

    case "tool_search":
    case "load_tool_schema": {
      const matches = Array.isArray(payload.matches) ? payload.matches : [];
      const missing = Array.isArray(payload.missing) ? payload.missing.filter(nonEmpty) : [];
      const alreadyLoaded = Array.isArray(payload.already_loaded) ? payload.already_loaded.filter(nonEmpty) : [];
      const status = nonEmpty(payload.status) ? String(payload.status) : "";
      const schemaName = nonEmpty(payload.loaded_schema) ? payload.loaded_schema : payload.name;
      const resultLabel = status
        ? t(`results.toolSearch.statuses.${status}`, { defaultValue: t("results.toolSearch.statuses.unknown") })
        : matches.length
          ? t("results.found", { count: matches.length })
          : alreadyLoaded.length
            ? t("results.toolSearch.alreadyLoaded", { count: alreadyLoaded.length })
            : payload.legacy
              ? t("results.toolSearch.completed")
              : t("results.nothingFound");
      return {
        icon: <Icon name="sliders" size={14} />,
        accent: "var(--text-3)",
        title: t(name === "load_tool_schema" ? "results.toolSearch.schemaTitle" : "results.toolSearch.title"),
        body: (
          <>
            <div className="tc-chips"><Badge tone={status === "missing" || status === "invalid" ? "warn" : "ok"}>{resultLabel}</Badge></div>
            {matches.length > 0 && (
              <Field label={t("results.toolSearch.matches")}>
                <div className="tc-chips">
                  {matches.map((match, index) => {
                    const toolName = typeof match === "string" ? match : match?.name;
                    return nonEmpty(toolName) ? <Badge key={`${toolName}:${index}`} tone="muted"><code>{toolName}</code></Badge> : null;
                  })}
                </div>
              </Field>
            )}
            {nonEmpty(schemaName) && (
              <Field label={t("results.toolSearch.schema")}><code>{schemaName}</code></Field>
            )}
            {alreadyLoaded.length > 0 && (
              <Field label={t("results.toolSearch.available")}><code>{alreadyLoaded.join(", ")}</code></Field>
            )}
            {missing.length > 0 && (
              <Field label={t("results.toolSearch.missing")}><code>{missing.join(", ")}</code></Field>
            )}
          </>
        ),
      };
    }

    default:
      return {
        icon: <Icon name="check" size={14} />,
        accent: "var(--entity-unknown)",
        title: <>{t("results.fallbackTitle")} <code>{name}</code></>,
        body: (
          <div className="tc-text">
            <MarkdownText>{"```json\n" + JSON.stringify(payload, null, 2) + "\n```"}</MarkdownText>
          </div>
        ),
      };
  }
}

// Just the result body (no card chrome) — for embedding under a tool call's request
// inside one merged ToolCard.
export function ToolResultBody({ name, payload = {} }) {
  const { t } = useTranslation("developer");
  return resultView(name, payload, t).body;
}

export default function ToolResultCard({ name, payload = {}, showRaw = true }) {
  const { t } = useTranslation("developer");
  const { icon, accent, title, body } = resultView(name, payload, t);
  return (
    <div className="tool-card result" style={{ "--tc": accent }}>
      <div className="tc-hd">
        <Tooltip className="tc-ico" tipClassName="tool-tip" content={t(`results.help.${name}`, { defaultValue: t("results.help.default") })}>
          {icon}
        </Tooltip>
        <span className="tc-title">{title}</span>
        <span className="tc-result-tag">{t("common.result")}</span>
      </div>
      <div className="tc-body">{body}</div>
      {showRaw && (
        <Spoiler label={t("raw.resultJson")}>
          <MarkdownText>{"```json\n" + JSON.stringify(payload, null, 2) + "\n```"}</MarkdownText>
        </Spoiler>
      )}
    </div>
  );
}
