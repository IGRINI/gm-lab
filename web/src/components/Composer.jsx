import { useRef, useState, useEffect, useCallback } from "react";
import Tooltip from "./Tooltip.jsx";
import { fmtK } from "../util.js";

const PLACEHOLDER =
  "Действие игрока…  или /new ледяной порт, пропал корабль, рядом Ива и Рун\nEnter — отправить · Shift+Enter — новая строка";
const PLACEHOLDER_COMPACT = "Действие игрока…  или /new <сцена>";

function useCompact() {
  const [compact, setCompact] = useState(
    () => typeof window !== "undefined" && window.matchMedia("(max-width:700px)").matches
  );
  useEffect(() => {
    if (typeof window === "undefined") return;
    const mq = window.matchMedia("(max-width:700px)");
    const on = (e) => setCompact(e.matches);
    mq.addEventListener?.("change", on);
    return () => mq.removeEventListener?.("change", on);
  }, []);
  return compact;
}

function usageTitle(run) {
  return [
    "Весь ран",
    `ходов: ${run.turns || 0}`,
    `вызовов модели: ${run.calls || 0}`,
    `ввод: ${run.in || 0} ток`,
    `вывод: ${run.out || 0} ток`,
    `кэш: ${run.cached || 0} ток`,
    `ГМ: ${run.gm_tokens || 0} ток / ${run.gm_calls || 0} выз.`,
    `персонажи: ${run.npc_tokens || 0} ток / ${run.npc_calls || 0} выз.`,
    `пик контекста: ${run.peak_context || 0} ток`,
    `время: ${run.secs || 0}s`,
  ].join("\n");
}

function RunUsage({ run }) {
  const data = run || {};
  return (
    <Tooltip as="aside" className="run-usage" content={usageTitle(data)}>
      <div className="run-usage-label">За ран</div>
      <div className="run-usage-main">
        <b>{fmtK(data.tokens || 0)}</b>
        <span>токенов</span>
      </div>
      <div className="run-usage-grid">
        <span>Кэш</span>
        <b>{fmtK(data.cached || 0)}</b>
        <span>Персонажи</span>
        <b>{fmtK(data.npc_tokens || 0)}</b>
      </div>
    </Tooltip>
  );
}

function percent(used, limit) {
  const u = Number(used || 0);
  const l = Number(limit || 0);
  if (!l || l <= 0) return 0;
  return Math.max(0, Math.min(100, Math.round((u / l) * 100)));
}

function Meter({ value }) {
  return (
    <div className="context-detail-meter" aria-hidden="true">
      <span style={{ width: `${Math.max(0, Math.min(100, value || 0))}%` }} />
    </div>
  );
}

function DetailRow({ label, value, pct }) {
  return (
    <div className="context-detail-row">
      <span>{label}</span>
      <b>{value}</b>
      {pct != null ? <em>{pct}%</em> : null}
    </div>
  );
}

function ContextDetails({ context, modelWindow }) {
  const data = context || {};
  const gm = data.gm || {};
  const npcs = (data.npcs && data.npcs.length ? data.npcs : data.npc?.name ? [data.npc] : []);
  const gmContextPct = percent(gm.active, gm.limit);
  const gmCompactPct = percent(gm.history, gm.limit);
  const gmLimit = gm.limit || 0;

  return (
    <div className="context-tip-panel">
      <div className="context-tip-head">
        <b>Контекст модели</b>
        <span>
          компакт {gmLimit ? fmtK(gmLimit) : "?"}
          {modelWindow ? ` · окно ${fmtK(modelWindow)}` : ""}
        </span>
      </div>

      <div className="context-detail-card">
        <div className="context-detail-title">
          <b style={{ color: "var(--gm)" }}>ГМ</b>
          <span>{fmtK(gm.active || 0)} / {gmLimit ? fmtK(gmLimit) : "?"}</span>
        </div>
        <Meter value={gmContextPct} />
        <DetailRow label="активно" value={`${fmtK(gm.active || 0)} / ${gmLimit ? fmtK(gmLimit) : "?"}`} pct={gmContextPct} />
        <DetailRow label="история" value={`${fmtK(gm.history || 0)} / ${fmtK(gm.limit || 0)}`} pct={gmCompactPct} />
        <DetailRow label="до компакта" value={`${fmtK(gm.remaining || 0)} ток`} />
        <DetailRow label="сводка" value={`${fmtK(gm.summary || 0)} ток`} />
      </div>

      <div className="context-detail-section">
        <span>Сессии персонажей</span>
        <b>{npcs.filter((npc) => npc.has_session).length}/{npcs.length}</b>
      </div>

      {npcs.length ? (
        npcs.map((npc) => {
          const contextPct = percent(npc.active, npc.limit);
          const compactPct = percent(npc.history, npc.limit);
          const npcLimit = npc.limit || 0;
          return (
            <div className={"context-detail-card npc" + (npc.has_session ? "" : " inactive")} key={npc.id || npc.name}>
              <div className="context-detail-title">
                <b style={{ color: npc.color || "var(--entity-unknown)" }}>{npc.name || npc.id || "персонаж"}</b>
                <span>{npc.has_session ? "есть история" : "ещё не вызывался"}</span>
              </div>
              <Meter value={contextPct} />
              <DetailRow label="активно" value={`${fmtK(npc.active || 0)} / ${npcLimit ? fmtK(npcLimit) : "?"}`} pct={contextPct} />
              <Meter value={compactPct} />
              <DetailRow label="история" value={`${fmtK(npc.history || 0)} / ${fmtK(npc.limit || 0)}`} pct={compactPct} />
              <DetailRow label="до компакта" value={`${fmtK(npc.remaining || 0)} ток`} />
              {npc.summary ? <DetailRow label="сводка" value={`${fmtK(npc.summary)} ток`} /> : null}
            </div>
          );
        })
      ) : (
        <div className="context-detail-empty">Сессий персонажей пока нет.</div>
      )}

      <div className="context-detail-foot">мир/сцена: {fmtK(data.world || 0)} ток</div>
    </div>
  );
}

function ContextUsage({ context, modelWindow }) {
  const data = context || {};
  const gm = data.gm || {};
  const gmActive = gm.active || data.current || 0;
  const contextPct = percent(gmActive, gm.limit);
  const compactPct = percent(gm.history, gm.limit);
  return (
    <Tooltip
      as="aside"
      className="context-usage"
      tipClassName="context-tip"
      content={<ContextDetails context={data} modelWindow={modelWindow} />}
    >
      <div className="context-usage-label">ГМ контекст</div>
      <div className="context-usage-main">
        <b>{fmtK(gmActive)}</b>
        <span>токенов</span>
        {gm.limit ? <em>{contextPct}%</em> : null}
      </div>
      <div className="context-meter" aria-hidden="true">
        <span style={{ width: `${contextPct}%` }} />
      </div>
      <div className="context-usage-grid">
        <span>История</span>
        <b>{fmtK(gm.history || 0)} / {fmtK(gm.limit || 0)} · {compactPct}%</b>
        <span>До компакта</span>
        <b>{fmtK(gm.remaining || 0)}</b>
      </div>
    </Tooltip>
  );
}

function QuickReplies({ playerOptions, busy, onPick }) {
  const options = Array.isArray(playerOptions?.options) ? playerOptions.options : [];
  if (!options.length) return null;
  return (
    <section className="quick-replies" aria-label="Варианты действий">
      <div className="quick-replies-head">{playerOptions.question || "Что ты делаешь дальше?"}</div>
      <div className="quick-replies-list">
        {options.map((option, index) => (
          <button
            type="button"
            className="quick-reply"
            key={`${option.label}:${index}`}
            disabled={busy}
            title={option.message}
            onClick={() => onPick(option.message)}
          >
            <span>{option.label}</span>
            <small>{option.message}</small>
          </button>
        ))}
      </div>
    </section>
  );
}

export default function Composer({
  onSend,
  busy,
  status,
  playerOptions,
  runUsage,
  contextUsage,
  modelWindow,
}) {
  const [value, setValue] = useState("");
  const ref = useRef(null);
  const compact = useCompact();

  // Auto-grow: reset to content height; CSS max-height caps it and switches to
  // an inner scroll once the limit is reached.
  const resize = useCallback(() => {
    const el = ref.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = el.scrollHeight + "px";
  }, []);

  useEffect(() => {
    resize();
  }, [value, resize]);

  const submit = () => {
    const t = value.trim();
    if (!t || busy) return;
    onSend(t);
    setValue("");
    requestAnimationFrame(() => {
      resize();
      ref.current?.focus();
    });
  };

  const sendQuickReply = (message) => {
    const text = String(message || "").trim();
    if (!text || busy) return;
    onSend(text);
    setValue("");
    requestAnimationFrame(() => {
      resize();
      ref.current?.focus();
    });
  };

  const onKeyDown = (e) => {
    if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
      e.preventDefault();
      submit();
    }
  };

  return (
    <footer>
      <div className="footer-main">
        <ContextUsage context={contextUsage} modelWindow={modelWindow} />
        <div className="composer-zone">
          <QuickReplies playerOptions={playerOptions} busy={busy} onPick={sendQuickReply} />
          <div className="composer">
            <div className="inp-wrap">
              <textarea
                id="inp"
                ref={ref}
                rows={1}
                value={value}
                placeholder={compact ? PLACEHOLDER_COMPACT : PLACEHOLDER}
                autoComplete="off"
                onChange={(e) => setValue(e.target.value)}
                onKeyDown={onKeyDown}
              />
            </div>
            <button id="send" onClick={submit} disabled={busy || !value.trim()} aria-label="Отправить">
              <span className="send-label">Отправить</span>
              <span className="send-ico" aria-hidden="true">➤</span>
            </button>
          </div>
        </div>
        <RunUsage run={runUsage} />
      </div>
      <div id="status">{status ? <><span className="pulse" />{status}</> : null}</div>
    </footer>
  );
}
