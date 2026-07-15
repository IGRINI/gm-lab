import Icon from "./Icon.jsx";
import { useState, useRef, useLayoutEffect, useContext } from "react";
import { ChatScrollContext } from "../chatScrollContext.js";

// Disclosure tuned for the virtualized chat:
//
//  • EXPAND  — keep the bottom anchored, so older content slides up and the
//    reading position / composer stay put ("chat shifts up, not down").
//  • COLLAPSE — keep the SUMMARY put; the body just tucks away below it and the
//    content beneath rises to fill the gap. No upward scroll.
//
// followOutput is intentionally OFF on the list (streaming auto-scroll is handled
// in Chat), so Virtuoso doesn't force-anchor the bottom on item resize. That
// leaves the expand correction below as the sole writer of scrollTop, and lets
// collapse fall through to natural flow (summary stays put). Done in
// useLayoutEffect (pre-paint) so there's no flicker.
export default function Spoiler({ label, children }) {
  const [open, setOpen] = useState(false);
  const scroll = useContext(ChatScrollContext);
  const expandAnchor = useRef(null); // bottom-relative distance, captured on expand

  const toggle = () => {
    if (!open) {
      const sc = scroll.getScroller();
      expandAnchor.current = sc ? sc.scrollHeight - sc.scrollTop : null;
    }
    setOpen((v) => !v);
  };

  useLayoutEffect(() => {
    if (!open) return; // collapse: natural flow keeps the summary in place
    const before = expandAnchor.current;
    expandAnchor.current = null;
    if (before == null) return;
    const sc = scroll.getScroller();
    if (sc) sc.scrollTop = sc.scrollHeight - before;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  const onKey = (e) => {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      toggle();
    }
  };

  return (
    <div className={"spoiler" + (open ? " open" : "")}>
      <div
        className="spoiler-summary"
        onClick={toggle}
        onKeyDown={onKey}
        role="button"
        tabIndex={0}
        aria-expanded={open}
      >
        <span className="mark"><Icon name={open ? "chevron-down" : "chevron-right"} size={11} /></span>
        <span>{label}</span>
      </div>
      {open && <div className="spoiler-body">{children}</div>}
    </div>
  );
}
