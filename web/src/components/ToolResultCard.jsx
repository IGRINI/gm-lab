import MarkdownText, { MarkdownInline } from "./MarkdownText.jsx";
import Spoiler from "./Spoiler.jsx";
import Tooltip from "./Tooltip.jsx";
import { NpcRef, Field, Badge, TextBlock, nonEmpty, ActorRef, ParticipantChips } from "./ToolCard.jsx";

// Presentation-only maps mirroring the request-card vocabulary in ToolCard.jsx.
const FACT_STATUS = {
  known: { label: "известно", tone: "ok" },
  likely: { label: "вероятно", tone: "warn" },
  rumored: { label: "слух", tone: "warn" },
  confirmed: { label: "подтверждено", tone: "ok" },
  unknown: { label: "неизвестно", tone: "muted" },
};
const WS_TYPE = {
  fact: { label: "факт", tone: "ok" },
  rumor: { label: "слух", tone: "warn" },
  npc_memory: { label: "память NPC", tone: "" },
  relationship: { label: "отношение", tone: "" },
  goal: { label: "цель", tone: "" },
  public_lookup: { label: "публичный факт", tone: "ok" },
};
const WS_OP = {
  add: { label: "добавлено", tone: "ok" },
  update: { label: "изменено", tone: "warn" },
  delete: { label: "удалено", tone: "redo" },
};
const WS_SCOPE = {
  public: "публично",
  gm: "ГМ",
  npc: "только NPC",
  shared: "общее",
  player: "игрок",
};
const PROFILE_PRESET = {
  visible: "видимое",
  social: "социальное",
  mechanics: "механика",
  status: "состояние",
  identity: "личность",
};
// query_world_state rows carry varied kinds across scopes (state_* prefixes plus
// canon/public/hidden). Normalise to a friendly Russian label + tone.
const QUERY_KIND = {
  public_lookup: { label: "публичный факт", tone: "ok" },
  public_intro: { label: "публичный факт", tone: "ok" },
  gm_canon: { label: "канон ГМ", tone: "" },
  hidden_event: { label: "скрытое событие", tone: "warn" },
};
function queryKind(kind) {
  const k = String(kind || "");
  if (QUERY_KIND[k]) return QUERY_KIND[k];
  const bare = k.replace(/^state_/, "");
  if (WS_TYPE[bare]) return WS_TYPE[bare];
  return { label: bare || "запись", tone: "" };
}

const RESULT_HELP = {
  get_world_fact: "Что память мира вернула на запрос ГМ.",
  query_world_state: "Что нашёл поиск ГМ по памяти мира.",
  update_world_state: "Итог записи в память мира: что сохранено и что отклонено.",
  get_npc_profile: "Поля карточки NPC, которые вернул запрос.",
  advance_time: "Новое состояние часов мира после сдвига.",
  update_player_character: "Какие поля листа персонажа изменились.",
  tool_search: "Какие скрытые инструменты загрузил ГМ.",
};

// id: stable concurrency key the model uses; short and monospace for the eye.
function ShortId({ id }) {
  if (!nonEmpty(id)) return null;
  const s = String(id);
  return (
    <Tooltip className="tc-id" tipClassName="tool-tip" content={`id записи: ${s}`}>
      {s.length > 14 ? s.slice(0, 13) + "…" : s}
    </Tooltip>
  );
}

function Target({ id }) {
  return <span className="tc-arrow-to">→ <ActorRef id={id} /></span>;
}

export function resultView(name, p) {
  const payload = p || {};
  switch (name) {
    case "get_world_fact": {
      const st = FACT_STATUS[payload.status] || { label: payload.status || "—", tone: "muted" };
      const sources = Array.isArray(payload.sources) ? payload.sources : [];
      return {
        icon: "📖",
        accent: "var(--md-link)",
        title: "Память мира — ответ",
        body: (
          <>
            <div className="tc-chips">
              <Badge tone={st.tone}>{st.label}</Badge>
            </div>
            {nonEmpty(payload.text) && <TextBlock>{payload.text}</TextBlock>}
            {sources.length > 0 && (
              <Field label="Источники">
                <div className="tc-list">
                  {sources.map((s, i) => (
                    <Tooltip
                      as="div"
                      className="tc-source"
                      tipClassName="tool-tip"
                      key={s.n ?? i}
                      content={[
                        s.kind ? `тип: ${s.kind}` : "",
                        s.status ? `статус: ${s.status}` : "",
                        s.source ? `источник: ${s.source}` : "",
                        s.score != null ? `релевантность: ${s.score}` : "",
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
      return {
        icon: "🔍",
        accent: "var(--md-link)",
        title: "Память мира — найдено",
        body: (
          <>
            <div className="tc-chips">
              <Badge tone={results.length ? "ok" : "muted"}>{results.length ? `найдено: ${results.length}` : "ничего не найдено"}</Badge>
              {nonEmpty(payload.scope) && <Badge tone="muted">{WS_SCOPE[payload.scope] || payload.scope}</Badge>}
            </div>
            {nonEmpty(payload.text) && <TextBlock>{payload.text}</TextBlock>}
            {results.length > 0 && (
              <div className="tc-ws-list">
                {results.map((r, i) => {
                  const typ = queryKind(r.kind);
                  return (
                    <div className="tc-ws-item" key={r.id || i}>
                      <div className="tc-chips">
                        <Badge tone={typ.tone}>{typ.label}</Badge>
                        {nonEmpty(r.scope) && <Badge tone="muted">{WS_SCOPE[r.scope] || r.scope}</Badge>}
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
        icon: "🧠",
        accent: "var(--entity-note)",
        title: "Память мира — записано",
        body: (
          <>
            <div className="tc-chips">
              <Badge tone={applied.length ? "ok" : "muted"}>{`сохранено: ${applied.length}`}</Badge>
              {errors.length > 0 && <Badge tone="redo">{`отклонено: ${errors.length}`}</Badge>}
            </div>
            {applied.length > 0 && (
              <div className="tc-ws-list">
                {applied.map((row, i) => {
                  const op = WS_OP[row.op || "add"] || WS_OP.add;
                  const typ = WS_TYPE[row.type] || { label: row.type || "", tone: "" };
                  return (
                    <div className="tc-result-row" key={row.id || i}>
                      <Badge tone={op.tone}>{op.label}</Badge>
                      {nonEmpty(row.type) && <Badge tone={typ.tone}>{typ.label}</Badge>}
                      {nonEmpty(row.scope) && <Badge tone="muted">{WS_SCOPE[row.scope] || row.scope}</Badge>}
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
              <Field label="Не сохранено">
                <div className="tc-list">
                  {errors.map((e, i) => (
                    <div className="tc-text redo" key={i}>
                      <MarkdownInline>{e.error || "ошибка записи"}</MarkdownInline>
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
        icon: "🪪",
        accent: "var(--acc)",
        title: (
          <>
            Карточка — <NpcRef id={payload.npc_id} />
          </>
        ),
        body: (
          <>
            <div className="tc-chips">
              {nonEmpty(payload.preset) && <Badge tone="muted">{PROFILE_PRESET[payload.preset] || payload.preset}</Badge>}
              {payload.card_revision != null && <Badge tone="muted">{`ревизия ${payload.card_revision}`}</Badge>}
            </div>
            {nonEmpty(payload.error) ? (
              <div className="tc-text redo"><MarkdownInline>{payload.error}</MarkdownInline></div>
            ) : keys.length ? (
              keys.map((k) => (
                <Field key={k} label={k}>
                  {typeof profile[k] === "object"
                    ? <code>{JSON.stringify(profile[k])}</code>
                    : <MarkdownInline>{String(profile[k])}</MarkdownInline>}
                </Field>
              ))
            ) : (
              <div className="tc-text">нет полей</div>
            )}
          </>
        ),
      };
    }

    case "advance_time": {
      const current = (payload.current && typeof payload.current === "object") ? payload.current : {};
      const now = [current.current_date_label, current.time_of_day].filter(nonEmpty).join(" · ");
      return {
        icon: "⏳",
        accent: "var(--md-em)",
        title: "Время мира",
        body: (
          <>
            <div className="tc-chips">
              {payload.elapsed_minutes != null && <Badge tone="warn">{`+${payload.elapsed_minutes} мин`}</Badge>}
              {nonEmpty(now) && <Badge tone="ok">{now}</Badge>}
            </div>
            {nonEmpty(payload.summary) && <TextBlock>{payload.summary}</TextBlock>}
            {nonEmpty(payload.error) && <div className="tc-text redo"><MarkdownInline>{payload.error}</MarkdownInline></div>}
          </>
        ),
      };
    }

    case "update_player_character": {
      const updated = Array.isArray(payload.updated) ? payload.updated : [];
      return {
        icon: "🛡",
        accent: "var(--player)",
        title: "Лист персонажа — обновлён",
        body: (
          <>
            <div className="tc-chips">
              {updated.length
                ? updated.map((f) => <Badge key={f} tone="ok">{f}</Badge>)
                : <Badge tone="muted">без изменений</Badge>}
              {payload.card_revision != null && <Badge tone="muted">{`ревизия ${payload.card_revision}`}</Badge>}
            </div>
            {nonEmpty(payload.reason) && <TextBlock>{payload.reason}</TextBlock>}
            {nonEmpty(payload.error) && <div className="tc-text redo"><MarkdownInline>{payload.error}</MarkdownInline></div>}
          </>
        ),
      };
    }

    case "tool_search": {
      return {
        icon: "🧰",
        accent: "var(--mut)",
        title: "Инструменты ГМ",
        body: nonEmpty(payload.text) ? (
          <TextBlock>{payload.text}</TextBlock>
        ) : (
          <div className="tc-text">—</div>
        ),
      };
    }

    default:
      return {
        icon: "✅",
        accent: "var(--entity-unknown)",
        title: <>Результат <code>{name}</code></>,
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
  return resultView(name, payload).body;
}

export default function ToolResultCard({ name, payload = {} }) {
  const { icon, accent, title, body } = resultView(name, payload);
  return (
    <div className="tool-card result" style={{ "--tc": accent }}>
      <div className="tc-hd">
        <Tooltip className="tc-ico" tipClassName="tool-tip" content={RESULT_HELP[name] || "Результат инструмента ГМ."}>
          {icon}
        </Tooltip>
        <span className="tc-title">{title}</span>
        <span className="tc-result-tag">результат</span>
      </div>
      <div className="tc-body">{body}</div>
      <Spoiler label="сырой результат (JSON)">
        <MarkdownText>{"```json\n" + JSON.stringify(payload, null, 2) + "\n```"}</MarkdownText>
      </Spoiler>
    </div>
  );
}
