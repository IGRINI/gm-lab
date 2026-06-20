import { useState, useRef, useLayoutEffect, createElement } from "react";
import { createPortal } from "react-dom";
import {
  computePosition,
  offset,
  flip,
  shift,
  arrow,
  autoUpdate,
} from "@floating-ui/dom";

// Custom hover/focus tooltip — replaces the native title="" popup.
// `content` may be a string (newlines honored) or any React node.
// `as` lets the trigger render as a block element (e.g. the meta line).
export default function Tooltip({ content, children, className, tipClassName = "", as = "span", style }) {
  const [open, setOpen] = useState(false);
  const refEl = useRef(null);
  const floatEl = useRef(null);
  const arrowEl = useRef(null);

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

  if (content == null || content === "") return children;

  const show = () => setOpen(true);
  const hide = () => setOpen(false);

  const trigger = createElement(
    as,
    {
      ref: refEl,
      className,
      style,
      onMouseEnter: show,
      onMouseLeave: hide,
      onFocus: show,
      onBlur: hide,
      tabIndex: 0,
    },
    children
  );

  return (
    <>
      {trigger}
      {open &&
        createPortal(
          <div
            ref={floatEl}
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
