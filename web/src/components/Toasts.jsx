import Icon from "./Icon.jsx";
import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";

// Bottom-right toast stack for the redesigned shell (§Игра/Ошибки in the TZ).
// Replaces the old `store.dispatch({kind:"error"})`-into-the-transcript channel.
//
// This module is presentational + a tiny local-state hook — it introduces NO
// global store. App owns the toast array (via `useToasts`) and renders
// `<Toasts toasts onDismiss />`; the timers that auto-dismiss transient toasts
// live inside each item so the parent only ever holds plain data.
//
// A toast is a plain object:
//   { id?, kind?: "error"|"success"|"info"|"warning", code?, message?, detail?,
//     title?, sticky?, ttl? }
// `code` is a stable machine string the server surfaces (protagonist_required,
// world_lore_required, …); it maps to a human headline. Raw server text rides in
// `message`/`detail` and is tucked behind the "детали" expander so the headline
// stays clean. Errors are sticky by default; other kinds auto-dismiss after 6s.

const DEFAULT_TTL = 6000;

// Machine error-code -> human, player-facing headline. Anything not listed falls
// back to the human `message`, then a generic line (see `toastHeadline`).
const ERROR_CODES = new Set([
  "protagonist_required",
  "world_lore_required",
  "world_required",
  "story_required",
  "world_not_found",
  "story_not_found",
  "character_not_found",
  "import_collision",
]);

// Resolve an error `code` to its human headline, or "" when unmapped.
export function mapErrorCode(code, t) {
  if (!ERROR_CODES.has(code) || typeof t !== "function") return "";
  return t(`toasts.codes.${code}`);
}

const KIND_ICON = {
  error: <Icon name="alert" size={15} />,
  success: <Icon name="check" size={15} />,
  info: <Icon name="info" size={15} />,
  // warn-but-allow launch notices (character_world_mismatch, story_pc_override,
  // world_version_drift…) — visually distinct from neutral info.
  warning: <Icon name="alert" size={15} />,
};

// The one-line headline shown in the toast body. Prefers an explicit title, then
// the mapped error code, then the raw human message, then a kind-generic line.
export function toastHeadline(toast, t) {
  if (!toast) return "";
  if (toast.title && toast.title.trim()) return toast.title.trim();
  const mapped = mapErrorCode(toast.code, t);
  if (mapped) return mapped;
  const message = typeof toast.message === "string" ? toast.message.trim() : "";
  if (message) return message;
  return toast.kind === "error" ? t("toasts.genericError") : t("toasts.done");
}

// The collapsible detail text (raw server output). Only surfaced when it differs
// from the headline — a mapped code pushes its raw `message` down here.
export function toastDetail(toast, t) {
  if (!toast) return "";
  const headline = toastHeadline(toast, t);
  const parts = [];
  const message = typeof toast.message === "string" ? toast.message.trim() : "";
  const detail = typeof toast.detail === "string" ? toast.detail.trim() : "";
  if (message && message !== headline) parts.push(message);
  if (detail && detail !== headline && detail !== message) parts.push(detail);
  return parts.join("\n\n");
}

function ToastItem({ toast, onDismiss }) {
  const { t } = useTranslation("game");
  const [expanded, setExpanded] = useState(false);
  const kind = toast.kind || "info";
  const sticky = toast.sticky ?? kind === "error";
  const ttl = Number.isFinite(toast.ttl) ? toast.ttl : DEFAULT_TTL;

  // Auto-dismiss transient toasts. Errors (sticky) sit until the user closes them.
  useEffect(() => {
    if (sticky || ttl <= 0) return undefined;
    const timer = setTimeout(() => onDismiss?.(toast.id), ttl);
    return () => clearTimeout(timer);
  }, [toast.id, sticky, ttl, onDismiss]);

  const headline = toastHeadline(toast, t);
  const detail = toastDetail(toast, t);

  return (
    <div className={"toast toast--" + kind} role={kind === "error" ? "alert" : "status"}>
      <div className="toast-row">
        <span className="toast-icon" aria-hidden="true">{KIND_ICON[kind] || KIND_ICON.info}</span>
        <span className="toast-msg">{headline}</span>
        <button
          type="button"
          className="toast-close"
          onClick={() => onDismiss?.(toast.id)}
          aria-label={t("toasts.closeAria")}
        >
          <Icon name="x" size={13} />
        </button>
      </div>
      {detail && (
        <div className="toast-detail">
          <button
            type="button"
            className="toast-detail-toggle"
            aria-expanded={expanded}
            onClick={() => setExpanded((value) => !value)}
          >
            {expanded ? t("toasts.hideDetails") : t("toasts.showDetails")}
          </button>
          {expanded && <pre className="toast-detail-text">{detail}</pre>}
        </div>
      )}
    </div>
  );
}

export default function Toasts({ toasts, onDismiss }) {
  const { t } = useTranslation("game");
  const list = Array.isArray(toasts) ? toasts : [];
  if (list.length === 0) return null;
  return (
    <div className="toast-stack" role="region" aria-label={t("toasts.regionAria")} aria-live="polite">
      {list.map((toast) => (
        <ToastItem key={toast.id} toast={toast} onDismiss={onDismiss} />
      ))}
    </div>
  );
}

// Component-local toast state for the shell to own (App calls this once). No
// global store — just a list the shell holds and feeds back into <Toasts/>.
let TOAST_SEQ = 0;
export function useToasts() {
  const [toasts, setToasts] = useState([]);

  const dismissToast = useCallback((id) => {
    setToasts((list) => list.filter((toast) => toast.id !== id));
  }, []);

  const pushToast = useCallback((toast) => {
    const next = toast && typeof toast === "object" ? toast : { message: String(toast ?? "") };
    const id = next.id ?? `toast_${Date.now()}_${TOAST_SEQ++}`;
    setToasts((list) => [...list, { ...next, id }]);
    return id;
  }, []);

  const clearToasts = useCallback(() => setToasts([]), []);

  return { toasts, pushToast, dismissToast, clearToasts };
}
