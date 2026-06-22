// Lightweight stacked-modal. A modal manager (DebugPanel) owns the stack and
// closes only the TOP layer on ESC / backdrop-click, so layering stays sane.
// Each deeper layer sits at a higher z-index and dims the one below it.
export default function Modal({ title, subtitle, onClose, depth = 0, wide = false, className = "", children, footer }) {
  return (
    <div
      className="dbg-backdrop"
      style={{ zIndex: 60 + depth * 2 }}
      role="presentation"
      onMouseDown={onClose}
    >
      <div
        className={["dbg-modal", wide ? "wide" : "", className].filter(Boolean).join(" ")}
        role="dialog"
        aria-modal="true"
        onMouseDown={(e) => e.stopPropagation()}
      >
        <div className="dbg-modal-head">
          <div className="dbg-modal-title">
            <h3>{title}</h3>
            {subtitle && <span>{subtitle}</span>}
          </div>
          <button type="button" className="icon-btn" onClick={onClose} aria-label="Закрыть">
            x
          </button>
        </div>
        <div className="dbg-modal-body">{children}</div>
        {footer && <div className="dbg-modal-foot">{footer}</div>}
      </div>
    </div>
  );
}
