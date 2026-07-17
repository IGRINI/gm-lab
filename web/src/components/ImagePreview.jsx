import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";

const MIN_SCALE = 0.35;
const MAX_SCALE = 8;

function clamp(value, min, max) {
  return Math.min(Math.max(value, min), max);
}

export function ImageViewer({ src, title, alt, onClose }) {
  const { t } = useTranslation("game");
  const overlayRef = useRef(null);
  const stageRef = useRef(null);
  const downOnBackdropRef = useRef(false);
  const [scale, setScale] = useState(1);
  const [offset, setOffset] = useState({ x: 0, y: 0 });
  const [drag, setDrag] = useState(null);
  const [fullscreen, setFullscreen] = useState(false);
  const [loadError, setLoadError] = useState(false);

  const zoomLabel = useMemo(() => `${Math.round(scale * 100)}%`, [scale]);
  const resetView = useCallback(() => {
    setDrag(null);
    setScale(1);
    setOffset({ x: 0, y: 0 });
  }, []);

  useEffect(() => {
    resetView();
    setLoadError(false);
  }, [resetView, src]);

  useEffect(() => {
    const onKeyDown = (event) => {
      if (event.key === "Escape") {
        event.preventDefault();
        onClose?.();
      } else if (event.key === "+" || event.key === "=") {
        event.preventDefault();
        setScale((current) => clamp(current * 1.2, MIN_SCALE, MAX_SCALE));
      } else if (event.key === "-") {
        event.preventDefault();
        setScale((current) => clamp(current / 1.2, MIN_SCALE, MAX_SCALE));
      } else if (event.key === "0") {
        event.preventDefault();
        resetView();
      }
    };
    const onFullscreenChange = () => {
      setFullscreen(document.fullscreenElement === overlayRef.current);
    };
    document.addEventListener("keydown", onKeyDown);
    document.addEventListener("fullscreenchange", onFullscreenChange);
    return () => {
      document.removeEventListener("keydown", onKeyDown);
      document.removeEventListener("fullscreenchange", onFullscreenChange);
    };
  }, [onClose, resetView]);

  const zoomBy = (multiplier) => {
    setScale((current) => clamp(current * multiplier, MIN_SCALE, MAX_SCALE));
  };

  const handleWheel = (event) => {
    event.preventDefault();
    const currentScale = scale;
    const nextScale = clamp(currentScale * Math.exp(-event.deltaY * 0.0015), MIN_SCALE, MAX_SCALE);
    const rect = stageRef.current?.getBoundingClientRect();
    if (rect && currentScale > 0) {
      const cursorX = event.clientX - rect.left - rect.width / 2;
      const cursorY = event.clientY - rect.top - rect.height / 2;
      const factor = nextScale / currentScale;
      setOffset((current) => ({
        x: cursorX - (cursorX - current.x) * factor,
        y: cursorY - (cursorY - current.y) * factor,
      }));
    }
    setScale(nextScale);
  };

  const handlePointerDown = (event) => {
    if (!stageRef.current || event.button !== 0) return;
    downOnBackdropRef.current = event.target === stageRef.current;
    event.preventDefault();
    stageRef.current.setPointerCapture(event.pointerId);
    setDrag({
      pointerId: event.pointerId,
      startX: event.clientX,
      startY: event.clientY,
      offsetX: offset.x,
      offsetY: offset.y,
    });
  };

  const handlePointerMove = (event) => {
    if (!drag || drag.pointerId !== event.pointerId) return;
    setOffset({
      x: drag.offsetX + event.clientX - drag.startX,
      y: drag.offsetY + event.clientY - drag.startY,
    });
  };

  const handlePointerUp = (event) => {
    if (!drag || drag.pointerId !== event.pointerId) return;
    if (stageRef.current?.hasPointerCapture(event.pointerId)) {
      stageRef.current.releasePointerCapture(event.pointerId);
    }
    const moved = Math.hypot(event.clientX - drag.startX, event.clientY - drag.startY);
    setDrag(null);
    if (downOnBackdropRef.current && moved < 5) onClose?.();
  };

  const toggleFullscreen = async () => {
    if (!overlayRef.current) return;
    if (document.fullscreenElement === overlayRef.current) {
      await document.exitFullscreen?.();
    } else {
      await overlayRef.current.requestFullscreen?.();
    }
  };

  if (!src) return null;

  return (
    <div ref={overlayRef} className="image-viewer-overlay" role="dialog" aria-modal="true">
      <div className="image-viewer-toolbar">
        <div className="image-viewer-title">
          <strong>{title || alt || t("image.defaultAlt")}</strong>
          <span>{zoomLabel}</span>
        </div>
      </div>
      <div
        ref={stageRef}
        className={`image-viewer-stage${drag ? " dragging" : ""}`}
        onWheel={handleWheel}
        onPointerDown={handlePointerDown}
        onPointerMove={handlePointerMove}
        onPointerUp={handlePointerUp}
        onPointerCancel={handlePointerUp}
        onLostPointerCapture={() => setDrag(null)}
        onDoubleClick={resetView}
      >
        <div
          className="image-viewer-canvas"
          style={{ "--preview-scale": String(scale), "--preview-x": `${offset.x}px`, "--preview-y": `${offset.y}px` }}
        >
          {loadError ? (
            <div className="image-viewer-empty">{t("image.loadFailed")}</div>
          ) : (
            <img src={src} alt={alt || title || t("image.defaultAlt")} draggable={false} onError={() => setLoadError(true)} />
          )}
        </div>
        <div className="image-viewer-dock" onPointerDown={(event) => event.stopPropagation()}>
          <button type="button" title={t("image.zoomOut")} aria-label={t("image.zoomOut")} onClick={() => zoomBy(1 / 1.2)}>
            -
          </button>
          <span className="image-viewer-zoom">{zoomLabel}</span>
          <button type="button" title={t("image.zoomIn")} aria-label={t("image.zoomIn")} onClick={() => zoomBy(1.2)}>
            +
          </button>
          <button type="button" title={t("image.reset")} aria-label={t("image.reset")} onClick={resetView}>
            100%
          </button>
          <button
            type="button"
            title={fullscreen ? t("image.exitFullscreen") : t("image.fullscreen")}
            aria-label={fullscreen ? t("image.exitFullscreen") : t("image.fullscreen")}
            onClick={() => void toggleFullscreen()}
          >
            {fullscreen ? t("image.fitShort") : t("image.fullShort")}
          </button>
          <button type="button" className="image-viewer-close" title={t("common.closeAria")} aria-label={t("common.closeAria")} onClick={onClose}>
            x
          </button>
        </div>
      </div>
    </div>
  );
}

function useImageViewerPortal(src, title, alt) {
  const [open, setOpen] = useState(false);

  useEffect(() => {
    setOpen(false);
  }, [src]);

  const viewer =
    open && typeof document !== "undefined"
      ? createPortal(<ImageViewer src={src} title={title} alt={alt} onClose={() => setOpen(false)} />, document.body)
      : null;

  return { openViewer: () => setOpen(true), viewer };
}

export function ZoomableImage({
  src,
  title = "",
  alt = "",
  className = "",
  imageClassName = "",
  loading,
  decoding = "async",
}) {
  const { t } = useTranslation("game");
  const label = title || alt || t("image.defaultAlt");
  const { openViewer, viewer } = useImageViewerPortal(src, label, alt);

  if (!src) return null;

  return (
    <>
      <button
        type="button"
        className={["zoomable-image", className].filter(Boolean).join(" ")}
        onClick={openViewer}
        aria-label={label}
        title={title || undefined}
      >
        <img
          className={imageClassName || undefined}
          src={src}
          alt={alt || label}
          loading={loading}
          decoding={decoding}
          draggable={false}
        />
      </button>
      {viewer}
    </>
  );
}

export default function ImageThumbnail({ src, alt = "", caption = "", className = "" }) {
  const { t } = useTranslation("game");
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState(false);
  const title = caption || alt;
  const label = title || t("image.defaultAlt");
  const { openViewer, viewer } = useImageViewerPortal(src, label, alt);

  useEffect(() => {
    setLoaded(false);
    setError(false);
  }, [src]);

  if (!src) return null;

  return (
    <>
      <button
        type="button"
        className={["image-thumb", loaded ? "loaded" : "", error ? "error" : "", className].filter(Boolean).join(" ")}
        onClick={() => {
          if (!error) openViewer();
        }}
        disabled={error}
        title={title || undefined}
        aria-label={label}
      >
        {!loaded && (
          <span className={`image-thumb-placeholder${error ? " error" : ""}`}>
            {error ? t("image.unavailable") : t("image.loading")}
          </span>
        )}
        <img
          src={src}
          alt={alt || label}
          loading="lazy"
          draggable={false}
          onLoad={() => setLoaded(true)}
          onError={() => setError(true)}
        />
        {title && <span className="image-thumb-caption">{title}</span>}
      </button>
      {viewer}
    </>
  );
}
