// Shared primitives for the architect panels (world + story). The two panels are
// NOT forks: they share the chat/SSE machinery, the live-segment folding, the
// message normalizers and the auto-growing textarea here, and differ only in
// their draft shape + form fields (the world bible vs the story plot). See
// docs/CHARACTERS_AND_STORY_TZ.md §С1.3.
import {
  useCallback,
  useContext,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import Tooltip, { TipContent } from "./Tooltip.jsx";
import Modal from "./Modal.jsx";
import ToolCard from "./ToolCard.jsx";
import Spoiler from "./Spoiler.jsx";
import MarkdownText from "./MarkdownText.jsx";
import { fmtK } from "../util.js";
import { VisibilityContext } from "../devSettings.js";

export const EMPTY_ARCHITECT_USAGE = { in: 0, out: 0, cached: 0, tokens: 0, calls: 0 };

export function textValue(value) {
  return typeof value === "string" ? value.trim() : "";
}

// Controlled-input value binding: the RAW string, no trim — a trimmed binding
// eats the trailing space the user just typed (the keystroke round-trips through
// state and comes back without it). Trim at the save/draft boundary instead.
export function rawText(value) {
  if (value == null) return "";
  return typeof value === "string" ? value : String(value);
}

export function normalizeVisibleMessage(value) {
  if (!value || typeof value !== "object") return null;
  const role = textValue(value.role);
  if (role === "tool") {
    const name = textValue(value.name);
    if (!name) return null;
    return {
      role: "tool",
      name,
      args: value.args && typeof value.args === "object" ? value.args : {},
    };
  }
  // `think` = a reasoning segment, rendered as a collapsible spoiler (like the
  // main chat's "ГМ думает").
  if (role !== "user" && role !== "assistant" && role !== "think") return null;
  const content = textValue(value.content);
  if (!content) return null;
  return { role, content };
}

// A textarea that grows to fit its content (no fixed rows, no inner scroll) so
// the whole field is readable. Height is recomputed on every value change (incl.
// the architect filling fields) and on viewport resize (wrapping changes).
export function AutoTextarea({ value, onChange, className = "", placeholder, disabled, minRows = 2 }) {
  const ref = useRef(null);
  const fit = () => {
    const el = ref.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${el.scrollHeight}px`;
  };
  useLayoutEffect(fit, [value]);
  useEffect(() => {
    window.addEventListener("resize", fit);
    return () => window.removeEventListener("resize", fit);
  }, []);
  return (
    <textarea
      ref={ref}
      className={className}
      value={value}
      onChange={onChange}
      placeholder={placeholder}
      disabled={disabled}
      rows={minRows}
    />
  );
}

// The live in-flight segment state + folding helpers, shared verbatim by both
// panels: deltas fold into the latest same-hop (sid)+role segment, tool calls
// append a card. Returns the state plus the three mutators + a ref for recovery.
export function useLiveSegments() {
  const [liveSegments, setLiveSegments] = useState([]);
  const liveSegmentsRef = useRef([]);

  const appendLiveDelta = useCallback((sid, role, text) => {
    if (!text) return;
    setLiveSegments((segs) => {
      const next = segs.slice();
      for (let i = next.length - 1; i >= 0; i--) {
        if (next[i].sid === sid && next[i].role === role) {
          next[i] = { ...next[i], content: (next[i].content || "") + text };
          liveSegmentsRef.current = next;
          return next;
        }
      }
      next.push({ role, sid, content: text });
      liveSegmentsRef.current = next;
      return next;
    });
  }, []);

  const pushLiveTool = useCallback((sid, name, args) => {
    setLiveSegments((segs) => {
      const next = [...segs, { role: "tool", sid, name, args }];
      liveSegmentsRef.current = next;
      return next;
    });
  }, []);

  const clearLive = useCallback(() => {
    liveSegmentsRef.current = [];
    setLiveSegments([]);
  }, []);

  return { liveSegments, liveSegmentsRef, appendLiveDelta, pushLiveTool, clearLive };
}

// One renderer for both the committed log and the in-flight segments, so the
// live view and the reloaded history look identical: reasoning → spoiler, tool →
// detailed card, user/assistant → bubble (with a caret while streaming).
export function ArchitectSegment({ message, live = false, thinkLabel = "🧠 Архитектор рассуждает" }) {
  const vis = useContext(VisibilityContext);
  if (message.role === "think") {
    return (
      <div className="world-architect-step">
        <Spoiler label={thinkLabel}>
          <MarkdownText>{textValue(message.content) || "—"}</MarkdownText>
        </Spoiler>
      </div>
    );
  }
  if (message.role === "tool") {
    return <ToolCard name={message.name} args={message.args} mode={vis.toolCalls ? "full" : "detail"} />;
  }
  const isLiveAssistant = live && message.role === "assistant";
  return (
    <div className={`world-architect-msg ${message.role}${isLiveAssistant ? " world-architect-live" : ""}`}>
      <MarkdownText>{message.content}</MarkdownText>
      {isLiveAssistant && <span className="world-architect-caret" aria-hidden="true" />}
    </div>
  );
}

// The full architect chat pane: header (usage/debug/help), scrolling log with
// live segments + typing indicator, and the composer input row. Both panels
// render this identically; the surrounding form differs. Sending is delegated to
// `onSend(text)` so each panel owns its SSE turn.
export function ArchitectChatPane({
  headKicker,
  headTitle,
  helpTitle,
  helpSubtitle,
  helpNote,
  usageTitle = "Токены архитектора",
  thinkLabel,
  placeholder,
  messages,
  liveSegments,
  busy,
  elapsed,
  error,
  usage,
  debug,
  onOpenDebug,
  input,
  onInputChange,
  onSend,
  onRetry,
  locked,
}) {
  const vis = useContext(VisibilityContext);
  const logRef = useRef(null);
  const inputRef = useRef(null);

  // Auto-grow the input with its content; reset to one line on send.
  useEffect(() => {
    const el = inputRef.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${el.scrollHeight}px`;
  }, [input]);

  // Keep the log pinned to the newest message / the typing indicator.
  useEffect(() => {
    const el = logRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [messages, busy, liveSegments]);

  const liveReplyStreaming = liveSegments.some(
    (segment) => segment.role === "assistant" && textValue(segment.content)
  );

  return (
    <section className="world-studio-pane world-architect" aria-label={headTitle}>
      <div className="world-architect-head">
        <div className="world-architect-head-id">
          <span>{headKicker}</span>
          <b>{headTitle}</b>
        </div>
        <div className="world-architect-tools">
          {vis.tokenCards && usage.calls > 0 && (
            <Tooltip
              tipClassName="ui-tip-wrap"
              content={
                <TipContent
                  title={usageTitle}
                  subtitle={`вызовов: ${usage.calls}`}
                  rows={[
                    ["ввод", `${usage.in}`],
                    ["вывод", `${usage.out}`],
                    ["кэш", `${usage.cached}`],
                    ["всего", `${usage.tokens}`],
                  ]}
                />
              }
            >
              <span className="world-architect-usage">
                {fmtK(usage.tokens)} ток · кэш {fmtK(usage.cached)}
              </span>
            </Tooltip>
          )}
          {vis.historyDebug && (
            <button
              type="button"
              className="world-architect-debug"
              onClick={onOpenDebug}
              disabled={!debug}
            >
              debug
            </button>
          )}
          <Tooltip
            tipClassName="ui-tip-wrap"
            focusable={false}
            content={<TipContent title={helpTitle} subtitle={helpSubtitle} note={helpNote} />}
          >
            <span className="world-architect-help" aria-hidden="true">?</span>
          </Tooltip>
        </div>
      </div>
      <div ref={logRef} className="world-architect-log" aria-live="polite">
        {messages.map((message, index) => (
          <ArchitectSegment key={`msg-${index}`} message={message} thinkLabel={thinkLabel} />
        ))}
        {liveSegments.map((segment, index) => (
          <ArchitectSegment
            key={`live-${segment.sid}-${segment.role}-${index}`}
            message={segment}
            live
            thinkLabel={thinkLabel}
          />
        ))}
        {busy && !liveReplyStreaming && (
          <div className="world-architect-msg assistant world-architect-typing">
            <span className="world-architect-dots" aria-hidden="true">
              <i />
              <i />
              <i />
            </span>
            <span>Архитектор печатает…{elapsed > 0 ? ` ${elapsed} с` : ""}</span>
          </div>
        )}
      </div>
      <div className="world-architect-input-row">
        <textarea
          ref={inputRef}
          value={input}
          onChange={(event) => onInputChange(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter" && !event.shiftKey && !event.nativeEvent.isComposing) {
              event.preventDefault();
              onSend();
            }
          }}
          placeholder={placeholder}
          rows={2}
          disabled={locked}
        />
        <button type="button" className="btn" onClick={onSend} disabled={locked || !input.trim()}>
          Спросить
        </button>
      </div>
      {error && (
        <div className="chat-sidebar-error inline world-architect-error">
          <span>{error}</span>
          {onRetry && (
            <button
              type="button"
              className="btn world-architect-retry"
              onClick={onRetry}
              disabled={locked}
            >
              Повторить
            </button>
          )}
        </div>
      )}
    </section>
  );
}

// The shared architect debug modal (last model call). Both panels open it from
// the chat pane's "debug" button.
export function ArchitectDebugModal({ debug, onClose }) {
  if (!debug) return null;
  return (
    <Modal title="Debug · архитектор" subtitle="последний вызов модели" wide onClose={onClose}>
      <div className="arch-debug">
        <section className="arch-debug-sec">
          <h4>Токены</h4>
          <div className="arch-debug-usage">
            <span>ввод <b>{debug.usage?.in ?? "—"}</b></span>
            <span>вывод <b>{debug.usage?.out ?? "—"}</b></span>
            <span>кэш <b>{debug.usage?.cached ?? "—"}</b></span>
            <span>всего <b>{debug.usage?.tokens ?? "—"}</b></span>
          </div>
        </section>
        {debug.thinking && (
          <section className="arch-debug-sec">
            <h4>Рассуждение</h4>
            <pre>{debug.thinking}</pre>
          </section>
        )}
        <section className="arch-debug-sec">
          <h4>Ответ модели</h4>
          <pre>{JSON.stringify(debug.response, null, 2)}</pre>
        </section>
        {debug.calls?.length > 0 && (
          <section className="arch-debug-sec">
            <h4>Tool calls</h4>
            <pre>{JSON.stringify(debug.calls, null, 2)}</pre>
          </section>
        )}
        <section className="arch-debug-sec">
          <h4>Запрос (messages)</h4>
          <pre>{JSON.stringify(debug.request, null, 2)}</pre>
        </section>
        <section className="arch-debug-sec">
          <h4>Stats (raw _meta)</h4>
          <pre>{JSON.stringify(debug.stats, null, 2)}</pre>
        </section>
      </div>
    </Modal>
  );
}

// Fold the architect_done usage into a running total (shared reducer used by both
// panels' done handlers).
export function accumulateUsage(current, usage) {
  return {
    in: current.in + (Number(usage.in) || 0),
    out: current.out + (Number(usage.out) || 0),
    cached: current.cached + (Number(usage.cached) || 0),
    tokens: current.tokens + (Number(usage.tokens) || 0),
    calls: current.calls + 1,
  };
}

// Build the debug snapshot from an architect_done payload (shared shape).
export function debugFromDone(data, usage) {
  return {
    request: data.request_messages ?? null,
    response: data.assistant_message ?? null,
    thinking: textValue(data.thinking),
    stats: data.stats ?? null,
    calls: Array.isArray(data.calls) ? data.calls : [],
    usage,
  };
}
