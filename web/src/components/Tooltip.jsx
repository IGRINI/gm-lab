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
  pinnable = false,
}) {
  const [hovered, setHovered] = useState(false);
  const [focused, setFocused] = useState(false);
  const [pinned, setPinned] = useState(false);
  const refEl = useRef(null);
  const floatEl = useRef(null);
  const arrowEl = useRef(null);
  const pinnedRef = useRef(false);
  const tipId = useId();
  const open = !disabled && (hovered || focused || pinned);

  // WCAG 1.4.13 (dismissible): while a tip is open, Escape closes IT — captured
  // on document ahead of the modals' bubble-phase listeners, so Esc does not
  // also discard the whole wizard/panel underneath.
  useEffect(() => {
    if (!open) return undefined;
    const onKey = (event) => {
      if (event.key !== "Escape") return;
      event.stopPropagation();
      pinnedRef.current = false;
      setPinned(false);
      setHovered(false);
      setFocused(false);
      refEl.current?.blur?.();
    };
    document.addEventListener("keydown", onKey, true);
    return () => document.removeEventListener("keydown", onKey, true);
  }, [open]);

  useEffect(() => {
    if (!pinned) return undefined;
    const onPointerDown = (event) => {
      if (refEl.current?.contains(event.target) || floatEl.current?.contains(event.target)) return;
      pinnedRef.current = false;
      setPinned(false);
      setHovered(false);
      setFocused(false);
    };
    document.addEventListener("pointerdown", onPointerDown, true);
    return () => document.removeEventListener("pointerdown", onPointerDown, true);
  }, [pinned]);

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
    if (!disabled) return;
    pinnedRef.current = false;
    setPinned(false);
    setHovered(false);
    setFocused(false);
  }, [disabled]);

  if (disabled || content == null || content === "") return children;

  const togglePinned = (event) => {
    if (!pinnable) return;
    event.stopPropagation();
    const next = !pinnedRef.current;
    pinnedRef.current = next;
    setPinned(next);
    if (!next) {
      setHovered(false);
      setFocused(false);
      refEl.current?.blur?.();
    }
  };

  const triggerProps = {
    ref: refEl,
    className: [className, pinnable ? "tooltip-pinnable" : ""].filter(Boolean).join(" "),
    style,
    onMouseEnter: () => setHovered(true),
    onMouseLeave: () => setHovered(false),
    onFocus: () => setFocused(true),
    onBlur: () => setFocused(false),
    // Screen readers only announce the tip when it is programmatically linked.
    "aria-describedby": open ? tipId : undefined,
  };
  if (focusable) triggerProps.tabIndex = 0;
  if (pinnable) {
    triggerProps.role = "button";
    triggerProps.onClick = togglePinned;
    triggerProps.onKeyDown = (event) => {
      if (event.key !== "Enter" && event.key !== " ") return;
      event.preventDefault();
      togglePinned(event);
    };
    triggerProps["aria-expanded"] = pinned;
    triggerProps["aria-controls"] = open ? tipId : undefined;
  } else {
    // Action controls keep focus after a click. Dismiss their transient tooltip
    // immediately so it cannot remain floating until the next unrelated click.
    triggerProps.onClick = () => {
      setHovered(false);
      setFocused(false);
    };
  }

  const trigger = createElement(as, triggerProps, children);

  return (
    <>
      {trigger}
      {open &&
        createPortal(
          <div
            ref={floatEl}
            id={tipId}
            className={["tip", "show", pinnable ? "interactive" : "", tipClassName].filter(Boolean).join(" ")}
            role={pinnable ? "dialog" : "tooltip"}
            style={{ visibility: "hidden" }}
            onPointerDown={(event) => event.stopPropagation()}
            onMouseDown={(event) => event.stopPropagation()}
            onClick={(event) => event.stopPropagation()}
          >
            {content}
            <div ref={arrowEl} className="tip-arrow" />
          </div>,
          document.body
        )}
    </>
  );
}
