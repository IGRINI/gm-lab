import { useState, useRef, useLayoutEffect, useEffect, useId, createElement } from "react";
import { createPortal } from "react-dom";
import {
  computePosition,
  offset,
  flip,
  shift,
  arrow,
  autoUpdate,
} from "@floating-ui/dom";

export function TipContent({ title, subtitle, rows = [], note }) {
  const cleanRows = rows.filter((row) => row && row[0] && row[1]);
  return (
    <div className="ui-tip">
      <div className="ui-tip-head">
        <b>{title}</b>
        {subtitle && <span>{subtitle}</span>}
      </div>
      {cleanRows.length > 0 && (
        <div className="ui-tip-rows">
          {cleanRows.map(([label, value]) => (
            <div className="ui-tip-row" key={label}>
              <span>{label}</span>
              <b>{value}</b>
            </div>
          ))}
        </div>
      )}
      {note && <div className="ui-tip-note">{note}</div>}
    </div>
  );
}

// Custom hover/focus tooltip — replaces the native title="" popup.
// `content` may be a string (newlines honored) or any React node.
// `as` lets the trigger render as a block element (e.g. the meta line).
export default function Tooltip({
  content,
  children,
  className,
  tipClassName = "",
  as = "span",
  style,
  focusable = true,
  disabled = false,
}) {
  const [open, setOpen] = useState(false);
  const refEl = useRef(null);
  const floatEl = useRef(null);
  const arrowEl = useRef(null);
  const tipId = useId();

  // WCAG 1.4.13 (dismissible): while a tip is open, Escape closes IT — captured
  // on document ahead of the modals' bubble-phase listeners, so Esc does not
  // also discard the whole wizard/panel underneath.
  useEffect(() => {
    if (!open) return undefined;
    const onKey = (event) => {
      if (event.key !== "Escape") return;
      event.stopPropagation();
      setOpen(false);
    };
    document.addEventListener("keydown", onKey, true);
    return () => document.removeEventListener("keydown", onKey, true);
  }, [open]);

  useLayoutEffect(() => {
    if (!open || !refEl.current || !floatEl.current) return;
    const ref = refEl.current;
    const tip = floatEl.current;
    const arr = arrowEl.current;
    const update = () =>
      computePosition(ref, tip, {
        placement: "top",
        middleware: [offset(9), flip({ padding: 8 }), shift({ padding: 8 }), arrow({ element: arr })],
      }).then(({ x, y, placement, middlewareData }) => {
        Object.assign(tip.style, { left: `${x}px`, top: `${y}px`, visibility: "visible" });
        const ad = middlewareData.arrow;
        if (ad && arr) {
          const side = placement.split("-")[0];
          const staticSide = { top: "bottom", right: "left", bottom: "top", left: "right" }[side];
          Object.assign(arr.style, {
            left: ad.x != null ? `${ad.x}px` : "",
            top: ad.y != null ? `${ad.y}px` : "",
            right: "",
            bottom: "",
            [staticSide]: "-5px",
          });
        }
      });
    const stop = autoUpdate(ref, tip, update);
    return stop;
  }, [open]);

  useLayoutEffect(() => {
    if (disabled) setOpen(false);
  }, [disabled]);

  if (disabled || content == null || content === "") return children;

  const show = () => setOpen(true);
  const hide = () => setOpen(false);

  const triggerProps = {
    ref: refEl,
    className,
    style,
    onMouseEnter: show,
    onMouseLeave: hide,
    onFocus: show,
    onBlur: hide,
    // Screen readers only announce the tip when it is programmatically linked.
    "aria-describedby": open ? tipId : undefined,
  };
  if (focusable) triggerProps.tabIndex = 0;

  const trigger = createElement(as, triggerProps, children);

  return (
    <>
      {trigger}
      {open &&
        createPortal(
          <div
            ref={floatEl}
            id={tipId}
            className={["tip", "show", tipClassName].filter(Boolean).join(" ")}
            role="tooltip"
            style={{ visibility: "hidden" }}
          >
            {content}
            <div ref={arrowEl} className="tip-arrow" />
          </div>,
          document.body
        )}
    </>
  );
}
