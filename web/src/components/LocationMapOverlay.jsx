import {
  useCallback,
  useEffect,
  useId,
  useMemo,
  useRef,
  useState,
} from "react";
import { useTranslation } from "react-i18next";
import Icon from "./Icon.jsx";
import WorldDetailModal from "./WorldDetailModal.jsx";
import {
  clampLocationMapScale,
  createLocationMapLayout,
  getLocationMapFitCamera,
  getLocationMapFocusCamera,
  LOCATION_MAP_MIN_SCALE,
} from "./locationMapGraph.js";
import "../styles-location-map.css";

// A full campaign graph can grow far beyond one viewport. Keep the overview
// floor low enough for "fit map" to remain truthful, then let the user zoom in.
const MIN_SCALE = LOCATION_MAP_MIN_SCALE;
const MAX_SCALE = 1.8;
const DEFAULT_TRANSITION_DURATION = 1800;
const TRANSITION_LEAD_IN = 260;
const TRANSITION_HOLD = 440;

function usePrefersReducedMotion() {
  const [reduced, setReduced] = useState(() =>
    typeof window !== "undefined" &&
    typeof window.matchMedia === "function" &&
    window.matchMedia("(prefers-reduced-motion: reduce)").matches
  );

  useEffect(() => {
    if (typeof window === "undefined" || typeof window.matchMedia !== "function") return undefined;
    const query = window.matchMedia("(prefers-reduced-motion: reduce)");
    const update = () => setReduced(query.matches);
    update();
    query.addEventListener?.("change", update);
    return () => query.removeEventListener?.("change", update);
  }, []);

  return reduced;
}

function useElementSize(ref) {
  const [size, setSize] = useState({ width: 0, height: 0 });

  useEffect(() => {
    const element = ref.current;
    if (!element) return undefined;
    const update = () => {
      const rect = element.getBoundingClientRect();
      setSize((previous) => {
        const width = Math.round(rect.width);
        const height = Math.round(rect.height);
        return previous.width === width && previous.height === height
          ? previous
          : { width, height };
      });
    };
    update();

    if (typeof ResizeObserver === "function") {
      const observer = new ResizeObserver(update);
      observer.observe(element);
      return () => observer.disconnect();
    }
    window.addEventListener("resize", update);
    return () => window.removeEventListener("resize", update);
  }, [ref]);

  return size;
}

function normalizeId(value) {
  if (value === null || value === undefined) return null;
  const id = String(value).trim();
  return id || null;
}

function mix(from, to, progress) {
  return from + (to - from) * progress;
}

function easeInOut(progress) {
  const bounded = Math.min(1, Math.max(0, progress));
  return bounded < 0.5
    ? 4 * bounded * bounded * bounded
    : 1 - Math.pow(-2 * bounded + 2, 3) / 2;
}

function cameraForNode(node, viewport, requestedScale = 1) {
  if (!node) return null;
  const safeWidth = Math.max(1, viewport.width - 64);
  const safeHeight = Math.max(1, viewport.height - 132);
  const scale = clampLocationMapScale(
    Math.min(requestedScale, safeWidth / node.width, safeHeight / node.height),
    MIN_SCALE,
    MAX_SCALE
  );
  return getLocationMapFocusCamera(node, viewport, scale);
}

function markerAnchor(node) {
  if (!node) return null;
  return {
    x: node.x + node.width / 2,
    y: node.y + node.height + 26,
  };
}

function firstText(...values) {
  for (const value of values) {
    if (typeof value !== "string") continue;
    const text = value.trim();
    if (text) return text;
  }
  return "";
}

function hasOpenImageViewer() {
  return typeof document !== "undefined"
    && document.querySelector(".image-viewer-overlay") !== null;
}

function locationSceneForNode(node, currentLocationId, currentScene) {
  if (!node) return null;
  const storedScene = node.scene && typeof node.scene === "object" ? node.scene : {};
  const liveScene = node.id === currentLocationId
    && currentScene
    && typeof currentScene === "object"
    ? currentScene
    : {};
  const scene = { ...storedScene, ...liveScene };
  const imageUrl = firstText(scene.image_url, scene.imageUrl, node.imageUrl);

  return {
    ...scene,
    scene_id: normalizeId(scene.scene_id) ?? node.id,
    location_id: normalizeId(scene.location_id ?? scene.locationId) ?? node.id,
    title: firstText(scene.title, scene.name, node.title),
    description: firstText(scene.description, node.description),
    ...(imageUrl ? { image_url: imageUrl } : {}),
  };
}

function stopMapGesture(event) {
  event.stopPropagation();
}

function LocationCard({
  node,
  current,
  routeFrom,
  routeTo,
  onSelect,
  selectAriaLabel,
  onTravel,
  travelBusy,
  travelLabel,
  travelAriaLabel,
  busyLabel,
  currentLabel,
}) {
  const className = [
    "location-map-card",
    current ? "is-current" : "",
    routeFrom ? "is-route-from" : "",
    routeTo ? "is-route-to" : "",
    onSelect ? "is-selectable" : "",
  ].filter(Boolean).join(" ");
  const content = (
    <>
      <div className="location-map-card-image" aria-hidden="true">
        {node.imageUrl ? (
          <img
            src={node.imageUrl}
            alt=""
            draggable="false"
            loading={routeFrom || routeTo ? "eager" : "lazy"}
            onError={(event) => { event.currentTarget.hidden = true; }}
          />
        ) : null}
        <span className="location-map-card-placeholder"><Icon name="map" size={30} /></span>
        <span className="location-map-card-shade" />
      </div>
      <div className="location-map-card-copy">
        <strong>{node.title}</strong>
        {node.description ? <span>{node.description}</span> : null}
      </div>
    </>
  );

  return (
    <article
      className={className}
      style={{ left: node.x, top: node.y, width: node.width, height: node.height }}
      data-location-id={node.id}
    >
      {onSelect ? (
        <button
          type="button"
          className="location-map-card-open"
          onClick={() => onSelect(node)}
          aria-label={selectAriaLabel}
          onPointerDown={stopMapGesture}
          onPointerUp={stopMapGesture}
          onPointerCancel={stopMapGesture}
          onDoubleClick={stopMapGesture}
        >
          {content}
        </button>
      ) : content}
      {current && currentLabel ? (
        <span className="location-map-card-status is-current" aria-label={currentLabel}>
          <Icon name="target" size={13} />
          {currentLabel}
        </span>
      ) : null}
      {onTravel ? (
        <button
          type="button"
          className="location-map-card-status is-travel"
          onClick={(event) => {
            event.stopPropagation();
            onTravel(node);
          }}
          disabled={travelBusy}
          aria-busy={travelBusy || undefined}
          aria-label={travelAriaLabel}
          onPointerDown={stopMapGesture}
          onPointerUp={stopMapGesture}
          onPointerCancel={stopMapGesture}
          onDoubleClick={stopMapGesture}
        >
          <Icon name="walk" size={13} />
          {travelBusy ? busyLabel : travelLabel}
        </button>
      ) : null}
    </article>
  );
}

function ExitCard({ exit, unexploredLabel }) {
  return (
    <article
      className={["location-map-exit", exit.passable ? "" : "is-blocked"].filter(Boolean).join(" ")}
      style={{ left: exit.x, top: exit.y, width: exit.width, minHeight: exit.height }}
      aria-label={`${unexploredLabel}: ${exit.title}`}
    >
      <span className="location-map-exit-dot" aria-hidden="true" />
      <span className="location-map-exit-copy">
        <small>{unexploredLabel}</small>
        <strong>{exit.title}</strong>
        {exit.description ? <span>{exit.description}</span> : null}
      </span>
    </article>
  );
}

function directionMatchesRoute(direction, routeFromId, routeToId) {
  return direction.toId
    && direction.fromId === routeFromId
    && direction.toId === routeToId;
}

function edgeDirections(edge) {
  return edge.directions?.length ? edge.directions : [edge];
}

function MapEdges({
  edges,
  markerId,
  routeFromId,
  routeToId,
  interactive,
  activeEdgeId,
  focusedEdgeId,
  tooltipId,
  edgeAriaLabel,
  onHover,
  onFocus,
}) {
  return (
    <g className="location-map-edges" aria-hidden={interactive ? undefined : "true"}>
      {edges.map((edge) => {
        const isRoute = edgeDirections(edge)
          .some((direction) => directionMatchesRoute(direction, routeFromId, routeToId));
        const isActive = edge.id === activeEdgeId;
        return (
          <g
            key={edge.id}
            className={[
              "location-map-edge",
              edge.unresolved ? "is-unresolved" : "",
              edge.passable ? "" : "is-blocked",
              isRoute ? "is-route" : "",
              edge.bidirectional ? "is-bidirectional" : "",
              isActive ? "is-active" : "",
            ].filter(Boolean).join(" ")}
          >
            <path className="location-map-edge-halo" d={edge.d} />
            <path
              className="location-map-edge-line"
              d={edge.d}
              markerStart={edge.bidirectional ? `url(#${markerId})` : undefined}
              markerEnd={`url(#${markerId})`}
            />
            {interactive ? (
              <path
                className="location-map-edge-hit"
                d={edge.d}
                tabIndex={0}
                aria-label={edgeAriaLabel(edge)}
                aria-describedby={focusedEdgeId === edge.id ? tooltipId : undefined}
                onPointerEnter={() => onHover(edge.id)}
                onPointerLeave={() => onHover(null)}
                onFocus={() => onFocus(edge.id)}
                onBlur={() => onFocus(null)}
                onPointerDown={(event) => event.stopPropagation()}
              />
            ) : null}
          </g>
        );
      })}
    </g>
  );
}

function EdgeTooltip({ edge, nodesById, exitsById, t, id, position }) {
  const directions = edgeDirections(edge);
  const targetTitle = (direction) => direction.toId
    ? nodesById.get(direction.toId)?.title ?? direction.toId
    : exitsById.get(direction.exit?.id)?.title ?? direction.exit?.title ?? direction.label;
  const title = edge.bidirectional
    ? t("locationMap.edgeBidirectional", { defaultValue: "Переход в обе стороны" })
    : edge.label;

  return (
    <aside
      id={id}
      className={`location-map-edge-tooltip ${position.above ? "is-above" : "is-below"}`}
      style={{ left: position.x, top: position.y }}
      role="tooltip"
    >
      <strong className="location-map-edge-tooltip-title">{title}</strong>
      {directions.map((direction) => {
        const from = nodesById.get(direction.fromId)?.title ?? direction.fromId;
        const to = targetTitle(direction);
        return (
          <section key={direction.id} className="location-map-edge-direction">
            <div className="location-map-edge-direction-route">
              <span>{from}</span><span aria-hidden="true">→</span><span>{to}</span>
            </div>
            {direction.label && (edge.bidirectional || direction.label !== title) ? (
              <b>{direction.label}</b>
            ) : null}
            {direction.description ? (
              <p className="location-map-edge-description">{direction.description}</p>
            ) : null}
            <dl>
              {direction.timeCost > 0 ? (
                <div>
                  <dt>{t("locationMap.edgeTime", { defaultValue: "Время" })}</dt>
                  <dd>{t("locationMap.edgeMinutes", {
                    count: direction.timeCost,
                    defaultValue: "{{count}} мин.",
                  })}</dd>
                </div>
              ) : null}
              {direction.kind ? (
                <div>
                  <dt>{t("locationMap.edgeKind", { defaultValue: "Тип" })}</dt>
                  <dd>{direction.kind}</dd>
                </div>
              ) : null}
              {direction.risk ? (
                <div>
                  <dt>{t("locationMap.edgeRisk", { defaultValue: "Риск" })}</dt>
                  <dd>{t(`locationMap.risk_${direction.risk}`, { defaultValue: direction.risk })}</dd>
                </div>
              ) : null}
              {direction.blockedBy ? (
                <div>
                  <dt>{t("locationMap.edgeBlockedBy", { defaultValue: "Преграда" })}</dt>
                  <dd>{direction.blockedBy}</dd>
                </div>
              ) : null}
            </dl>
            <span className={direction.passable ? "is-passable" : "is-blocked"}>
              {direction.passable
                ? t("locationMap.edgeAvailable", { defaultValue: "Переход доступен" })
                : t("locationMap.edgeUnavailable", { defaultValue: "Переход недоступен" })}
            </span>
          </section>
        );
      })}
    </aside>
  );
}

export default function LocationMapOverlay({
  graph,
  mode = "map",
  fromLocationId = null,
  toLocationId = null,
  onClose,
  onLocationSelect,
  onTravelRequest,
  travelBusy = false,
  currentScene = null,
  npcs = [],
  statusLabels = {},
  onTransitionComplete,
  transitionDuration = DEFAULT_TRANSITION_DURATION,
  className = "",
}) {
  const { t } = useTranslation("game");
  const isTransition = mode === "transition";
  const reducedMotion = usePrefersReducedMotion();
  const layout = useMemo(() => createLocationMapLayout(graph), [graph]);
  const nodesById = useMemo(
    () => new Map(layout.nodes.map((node) => [node.id, node])),
    [layout.nodes]
  );
  const exitsById = useMemo(
    () => new Map(layout.exits.map((exit) => [exit.id, exit])),
    [layout.exits]
  );
  const requestedFromId = normalizeId(fromLocationId);
  const requestedToId = normalizeId(toLocationId);
  const fromNode = nodesById.get(requestedFromId)
    ?? nodesById.get(layout.rootLocationId)
    ?? layout.nodes[0]
    ?? null;
  const toNode = nodesById.get(requestedToId)
    ?? nodesById.get(layout.currentLocationId)
    ?? fromNode;
  const currentNode = nodesById.get(layout.currentLocationId) ?? toNode ?? fromNode;
  const accessibleConnections = useMemo(() => {
    return layout.edges.flatMap((edge) => edgeDirections(edge).map((direction) => ({
      id: direction.id,
      from: nodesById.get(direction.fromId)?.title ?? direction.fromId,
      label: direction.label,
      to: direction.toId
        ? nodesById.get(direction.toId)?.title ?? direction.toId
        : exitsById.get(direction.exit?.id)?.title ?? direction.exit?.title ?? direction.label,
    })));
  }, [exitsById, layout.edges, nodesById]);

  const overlayRef = useRef(null);
  const viewportRef = useRef(null);
  const closeButtonRef = useRef(null);
  const viewport = useElementSize(viewportRef);
  const [camera, setCamera] = useState({ x: 0, y: 0, scale: 1 });
  const [hoveredEdgeId, setHoveredEdgeId] = useState(null);
  const [focusedEdgeId, setFocusedEdgeId] = useState(null);
  const [selectedLocationId, setSelectedLocationId] = useState(null);
  const cameraRef = useRef(camera);
  const [routeProgress, setRouteProgress] = useState(isTransition ? 0 : 1);
  const mapFocusRef = useRef(null);
  const pointersRef = useRef(new Map());
  const gestureRef = useRef(null);
  const completionRef = useRef(null);
  const transitionCallbackRef = useRef(onTransitionComplete);
  const svgMarkerId = `location-map-arrow-${useId().replace(/:/g, "")}`;
  const edgeTooltipId = `location-map-edge-tooltip-${useId().replace(/:/g, "")}`;
  const activeEdgeId = focusedEdgeId ?? hoveredEdgeId;
  const activeEdge = layout.edges.find((edge) => edge.id === activeEdgeId) ?? null;
  const selectedLocation = selectedLocationId
    ? nodesById.get(selectedLocationId) ?? null
    : null;
  const selectedLocationScene = useMemo(
    () => locationSceneForNode(
      selectedLocation,
      layout.currentLocationId,
      currentScene
    ),
    [currentScene, layout.currentLocationId, selectedLocation]
  );
  const travelEnabled = !isTransition && typeof onTravelRequest === "function";
  const selectedLocationIsCurrent = selectedLocation?.id === layout.currentLocationId;
  const selectedLocationCanTravel = travelEnabled
    && selectedLocation
    && selectedLocation.visited !== false
    && !selectedLocationIsCurrent;

  useEffect(() => {
    if (selectedLocationId && !nodesById.has(selectedLocationId)) {
      setSelectedLocationId(null);
    }
  }, [nodesById, selectedLocationId]);

  const edgeAriaLabel = useCallback((edge) => edgeDirections(edge)
    .map((direction) => t("locationMap.connection", {
      defaultValue: "{{from}} — {{label}} — {{to}}",
      from: nodesById.get(direction.fromId)?.title ?? direction.fromId,
      label: direction.label,
      to: direction.toId
        ? nodesById.get(direction.toId)?.title ?? direction.toId
        : exitsById.get(direction.exit?.id)?.title ?? direction.exit?.title ?? direction.label,
    }))
    .join(". "), [exitsById, nodesById, t]);

  const edgeTooltipPosition = activeEdge ? (() => {
    const screenX = camera.x + activeEdge.labelX * camera.scale;
    const screenY = camera.y + activeEdge.labelY * camera.scale;
    const sidePadding = Math.min(154, Math.max(16, viewport.width / 2));
    return {
      x: Math.min(Math.max(screenX, sidePadding), Math.max(sidePadding, viewport.width - sidePadding)),
      y: Math.min(Math.max(screenY, 104), Math.max(104, viewport.height - 24)),
      above: screenY > viewport.height / 2,
    };
  })() : null;

  useEffect(() => {
    transitionCallbackRef.current = onTransitionComplete;
  }, [onTransitionComplete]);

  const updateCamera = useCallback((next) => {
    const resolved = typeof next === "function" ? next(cameraRef.current) : next;
    if (!resolved) return;
    const bounded = { ...resolved, scale: clampLocationMapScale(resolved.scale, MIN_SCALE, MAX_SCALE) };
    cameraRef.current = bounded;
    setCamera(bounded);
  }, []);

  const fitMap = useCallback(() => {
    updateCamera(getLocationMapFitCamera(layout.bounds, viewport, {
      minScale: MIN_SCALE,
      maxScale: 1,
      padding: 72,
    }));
  }, [layout.bounds, updateCamera, viewport]);

  const focusCurrent = useCallback(() => {
    updateCamera(cameraForNode(currentNode, viewport, Math.min(1, cameraRef.current.scale || 1)));
  }, [currentNode, updateCamera, viewport]);

  const zoomAt = useCallback((factor, point = null) => {
    updateCamera((previous) => {
      const scale = clampLocationMapScale(previous.scale * factor, MIN_SCALE, MAX_SCALE);
      const anchor = point ?? { x: viewport.width / 2, y: viewport.height / 2 };
      const ratio = scale / previous.scale;
      return {
        x: anchor.x - (anchor.x - previous.x) * ratio,
        y: anchor.y - (anchor.y - previous.y) * ratio,
        scale,
      };
    });
  }, [updateCamera, viewport.height, viewport.width]);

  useEffect(() => {
    if (isTransition || !viewport.width || !viewport.height) return;
    const focusKey = currentNode?.id ?? "empty";
    if (mapFocusRef.current === focusKey) return;
    mapFocusRef.current = focusKey;
    if (currentNode) focusCurrent();
    else fitMap();
  }, [currentNode, fitMap, focusCurrent, isTransition, viewport.height, viewport.width]);

  useEffect(() => {
    const previousFocus = document.activeElement;
    const focusTimer = window.setTimeout(() => {
      if (isTransition) overlayRef.current?.focus({ preventScroll: true });
      else (closeButtonRef.current ?? overlayRef.current)?.focus({ preventScroll: true });
    }, 0);
    return () => {
      window.clearTimeout(focusTimer);
      if (previousFocus instanceof HTMLElement && previousFocus.isConnected) {
        previousFocus.focus({ preventScroll: true });
      }
    };
  }, [isTransition]);

  useEffect(() => {
    if (!selectedLocationId) return undefined;
    const previousFocus = document.activeElement;
    const focusTimer = window.setTimeout(() => {
      overlayRef.current
        ?.querySelector(".dbg-backdrop .icon-btn")
        ?.focus({ preventScroll: true });
    }, 0);
    return () => {
      window.clearTimeout(focusTimer);
      if (previousFocus instanceof HTMLElement && previousFocus.isConnected) {
        previousFocus.focus({ preventScroll: true });
      }
    };
  }, [selectedLocationId]);

  useEffect(() => {
    const handleEscape = (event) => {
      if (event.key !== "Escape" || isTransition) return;
      if (hasOpenImageViewer()) return;
      event.preventDefault();
      if (selectedLocationId) {
        setSelectedLocationId(null);
        return;
      }
      onClose?.();
    };
    document.addEventListener("keydown", handleEscape);
    return () => document.removeEventListener("keydown", handleEscape);
  }, [isTransition, onClose, selectedLocationId]);

  useEffect(() => {
    if (!isTransition) {
      completionRef.current = null;
      setRouteProgress(1);
      return undefined;
    }
    if (!viewport.width || !viewport.height) return undefined;

    const transitionKey = `${fromNode?.id ?? "none"}->${toNode?.id ?? "none"}`;
    const startCamera = cameraForNode(fromNode, viewport, 0.96)
      ?? getLocationMapFitCamera(layout.bounds, viewport, { minScale: MIN_SCALE, maxScale: 0.9 });
    const endCamera = cameraForNode(toNode, viewport, 1)
      ?? startCamera;
    updateCamera(reducedMotion ? endCamera : startCamera);
    setRouteProgress(reducedMotion ? 1 : 0);

    let animationFrame = 0;
    let completionTimer = 0;
    let cancelled = false;
    const complete = () => {
      if (cancelled || completionRef.current === transitionKey) return;
      completionRef.current = transitionKey;
      transitionCallbackRef.current?.({
        fromLocationId: fromNode?.id ?? requestedFromId,
        toLocationId: toNode?.id ?? requestedToId,
      });
    };

    if (reducedMotion || !startCamera || !endCamera || fromNode?.id === toNode?.id) {
      completionTimer = window.setTimeout(complete, reducedMotion ? 360 : 700);
      return () => {
        cancelled = true;
        window.clearTimeout(completionTimer);
      };
    }

    const duration = Math.max(500, Number(transitionDuration) || DEFAULT_TRANSITION_DURATION);
    const startedAt = performance.now();
    const animate = (now) => {
      const elapsed = now - startedAt;
      const rawProgress = Math.min(1, Math.max(0, (elapsed - TRANSITION_LEAD_IN) / duration));
      const markerProgress = easeInOut(rawProgress);
      const cameraProgress = easeInOut(Math.max(0, (rawProgress - 0.12) / 0.88));
      setRouteProgress(markerProgress);
      updateCamera({
        x: mix(startCamera.x, endCamera.x, cameraProgress),
        y: mix(startCamera.y, endCamera.y, cameraProgress),
        scale: mix(startCamera.scale, endCamera.scale, cameraProgress),
      });
      if (rawProgress < 1) {
        animationFrame = requestAnimationFrame(animate);
      } else {
        completionTimer = window.setTimeout(complete, TRANSITION_HOLD);
      }
    };
    animationFrame = requestAnimationFrame(animate);

    return () => {
      cancelled = true;
      cancelAnimationFrame(animationFrame);
      window.clearTimeout(completionTimer);
    };
  }, [
    fromNode,
    isTransition,
    layout.bounds,
    reducedMotion,
    requestedFromId,
    requestedToId,
    toNode,
    transitionDuration,
    updateCamera,
    viewport,
  ]);

  const startGesture = useCallback(() => {
    const points = [...pointersRef.current.values()];
    if (points.length >= 2) {
      const [first, second] = points;
      const center = { x: (first.x + second.x) / 2, y: (first.y + second.y) / 2 };
      const distance = Math.hypot(second.x - first.x, second.y - first.y) || 1;
      const current = cameraRef.current;
      gestureRef.current = {
        type: "pinch",
        distance,
        scale: current.scale,
        worldX: (center.x - current.x) / current.scale,
        worldY: (center.y - current.y) / current.scale,
      };
    } else if (points.length === 1) {
      const [point] = points;
      gestureRef.current = {
        type: "pan",
        x: point.x,
        y: point.y,
        cameraX: cameraRef.current.x,
        cameraY: cameraRef.current.y,
      };
    } else {
      gestureRef.current = null;
    }
  }, []);

  const localPointer = useCallback((event) => {
    const rect = viewportRef.current?.getBoundingClientRect();
    return { x: event.clientX - (rect?.left ?? 0), y: event.clientY - (rect?.top ?? 0) };
  }, []);

  const handlePointerDown = useCallback((event) => {
    if (isTransition || event.button > 0) return;
    event.currentTarget.setPointerCapture?.(event.pointerId);
    pointersRef.current.set(event.pointerId, localPointer(event));
    startGesture();
  }, [isTransition, localPointer, startGesture]);

  const handlePointerMove = useCallback((event) => {
    if (isTransition || !pointersRef.current.has(event.pointerId)) return;
    pointersRef.current.set(event.pointerId, localPointer(event));
    const points = [...pointersRef.current.values()];
    const gesture = gestureRef.current;
    if (points.length >= 2) {
      if (gesture?.type !== "pinch") {
        startGesture();
        return;
      }
      const [first, second] = points;
      const center = { x: (first.x + second.x) / 2, y: (first.y + second.y) / 2 };
      const distance = Math.hypot(second.x - first.x, second.y - first.y) || 1;
      const scale = clampLocationMapScale(
        gesture.scale * distance / gesture.distance,
        MIN_SCALE,
        MAX_SCALE
      );
      updateCamera({
        x: center.x - gesture.worldX * scale,
        y: center.y - gesture.worldY * scale,
        scale,
      });
    } else if (points.length === 1 && gesture?.type === "pan") {
      const [point] = points;
      updateCamera((previous) => ({
        x: gesture.cameraX + point.x - gesture.x,
        y: gesture.cameraY + point.y - gesture.y,
        scale: previous.scale,
      }));
    }
  }, [isTransition, localPointer, startGesture, updateCamera]);

  const handlePointerEnd = useCallback((event) => {
    const wasTracked = pointersRef.current.delete(event.pointerId);
    if (!wasTracked) return;
    if (event.currentTarget.hasPointerCapture?.(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId);
    }
    startGesture();
  }, [startGesture]);

  const handleWheel = useCallback((event) => {
    if (isTransition) return;
    event.preventDefault();
    zoomAt(Math.exp(-event.deltaY * 0.0012), localPointer(event));
  }, [isTransition, localPointer, zoomAt]);

  useEffect(() => {
    const element = viewportRef.current;
    if (!element || isTransition) return undefined;
    element.addEventListener("wheel", handleWheel, { passive: false });
    return () => element.removeEventListener("wheel", handleWheel);
  }, [handleWheel, isTransition]);

  const handleKeyDown = useCallback((event) => {
    if (hasOpenImageViewer()) return;
    if (event.key === "Tab") {
      const focusScope = selectedLocationId
        ? overlayRef.current?.querySelector(".dbg-backdrop")
        : overlayRef.current;
      const focusable = [...(focusScope?.querySelectorAll(
        "button:not([disabled]), [href], [tabindex]:not([tabindex='-1'])"
      ) ?? [])];
      if (!focusable.length) {
        event.preventDefault();
        focusScope?.focus?.();
      } else {
        const first = focusable[0];
        const last = focusable[focusable.length - 1];
        const active = document.activeElement;
        if (!focusScope?.contains(active) || active === focusScope) {
          event.preventDefault();
          (event.shiftKey ? last : first).focus();
        } else if (event.shiftKey && active === first) {
          event.preventDefault();
          last.focus();
        } else if (!event.shiftKey && active === last) {
          event.preventDefault();
          first.focus();
        }
      }
      return;
    }
    if (isTransition || selectedLocationId) return;
    if (event.key === "+" || event.key === "=") {
      event.preventDefault();
      zoomAt(1.2);
    } else if (event.key === "-" || event.key === "_") {
      event.preventDefault();
      zoomAt(1 / 1.2);
    } else if (event.key === "0") {
      event.preventDefault();
      fitMap();
    } else if (event.key === "Home") {
      event.preventDefault();
      focusCurrent();
    }
  }, [fitMap, focusCurrent, isTransition, selectedLocationId, zoomAt]);

  const selectLocation = useCallback((node) => {
    setHoveredEdgeId(null);
    setFocusedEdgeId(null);
    setSelectedLocationId(node.id);
    onLocationSelect?.(node);
  }, [onLocationSelect]);

  const requestTravel = useCallback((node) => {
    if (
      !travelEnabled
      || travelBusy
      || !node
      || node.visited === false
      || node.id === layout.currentLocationId
    ) return;
    onTravelRequest(node);
  }, [layout.currentLocationId, onTravelRequest, travelBusy, travelEnabled]);

  const fromAnchor = markerAnchor(fromNode);
  const toAnchor = markerAnchor(toNode);
  const staticAnchor = markerAnchor(currentNode);
  const marker = isTransition && fromAnchor && toAnchor
    ? {
        x: mix(fromAnchor.x, toAnchor.x, routeProgress),
        y: mix(fromAnchor.y, toAnchor.y, routeProgress),
      }
    : staticAnchor;

  const title = isTransition
    ? t("locationMap.transitionTitle", { defaultValue: "Переход между локациями" })
    : t("locationMap.title", { defaultValue: "Карта локаций" });
  const currentMarkerLabel = t("locationMap.currentMarker", { defaultValue: "Вы здесь" });
  const unexploredLabel = t("locationMap.unexploredExit", { defaultValue: "Неизведанный выход" });
  const travelLabel = t("locationMap.travelTo", { defaultValue: "Хочу сюда" });
  const travelBusyLabel = t("locationMap.travelBusy", { defaultValue: "Отправляю запрос…" });
  const overlayClassName = [
    "location-map-overlay",
    isTransition ? "is-transition" : "is-map",
    className,
  ].filter(Boolean).join(" ");

  return (
    <section
      ref={overlayRef}
      className={overlayClassName}
      role="dialog"
      aria-modal="true"
      aria-label={title}
      tabIndex={-1}
      onKeyDown={handleKeyDown}
    >
      <header className="location-map-header">
        <div className="location-map-heading">
          <span className="location-map-kicker"><Icon name={isTransition ? "walk" : "map"} size={15} />{title}</span>
          {isTransition && fromNode && toNode ? (
            <div className="location-map-route-title" aria-live="polite">
              <span><small>{t("locationMap.fromLabel", { defaultValue: "Откуда" })}</small>{fromNode.title}</span>
              <Icon name="arrow-right" size={18} />
              <span><small>{t("locationMap.toLabel", { defaultValue: "Куда" })}</small>{toNode.title}</span>
            </div>
          ) : null}
        </div>
        {!isTransition ? (
          <button
            ref={closeButtonRef}
            type="button"
            className="location-map-close"
            onClick={onClose}
            aria-label={t("locationMap.closeAria", { defaultValue: "Закрыть карту" })}
          >
            <Icon name="x" size={19} />
          </button>
        ) : null}
      </header>

      {accessibleConnections.length ? (
        <div className="location-map-sr-only">
          <h2>{t("locationMap.connectionsTitle", { defaultValue: "Связи локаций" })}</h2>
          <ul>
            {accessibleConnections.map((connection) => (
              <li key={connection.id}>
                {t("locationMap.connection", {
                  defaultValue: "{{from}} — {{label}} — {{to}}",
                  from: connection.from,
                  label: connection.label,
                  to: connection.to,
                })}
              </li>
            ))}
          </ul>
        </div>
      ) : null}

      <div
        ref={viewportRef}
        className="location-map-viewport"
        onPointerDown={handlePointerDown}
        onPointerMove={handlePointerMove}
        onPointerUp={handlePointerEnd}
        onPointerCancel={handlePointerEnd}
        onDoubleClick={(event) => {
          if (!isTransition) zoomAt(1.25, localPointer(event));
        }}
      >
        {layout.nodes.length ? (
          <div
            className="location-map-world"
            style={{
              width: layout.bounds.width,
              height: layout.bounds.height,
              transform: `translate3d(${camera.x}px, ${camera.y}px, 0) scale(${camera.scale})`,
            }}
          >
            <svg
              className="location-map-svg"
              width={layout.bounds.width}
              height={layout.bounds.height}
              viewBox={`0 0 ${layout.bounds.width} ${layout.bounds.height}`}
            >
              <defs>
                <marker
                  id={svgMarkerId}
                  viewBox="0 0 10 10"
                  refX="8"
                  refY="5"
                  markerWidth="7"
                  markerHeight="7"
                  orient="auto-start-reverse"
                >
                  <path d="M 0 1 L 9 5 L 0 9 z" className="location-map-arrowhead" />
                </marker>
              </defs>
              <MapEdges
                edges={layout.edges}
                markerId={svgMarkerId}
                routeFromId={isTransition ? fromNode?.id : null}
                routeToId={isTransition ? toNode?.id : null}
                interactive={!isTransition}
                activeEdgeId={activeEdgeId}
                focusedEdgeId={focusedEdgeId}
                tooltipId={edgeTooltipId}
                edgeAriaLabel={edgeAriaLabel}
                onHover={setHoveredEdgeId}
                onFocus={setFocusedEdgeId}
              />
            </svg>

            {layout.nodes.map((node) => (
              <LocationCard
                key={node.id}
                node={node}
                current={node.id === layout.currentLocationId}
                routeFrom={isTransition && node.id === fromNode?.id}
                routeTo={isTransition && node.id === toNode?.id}
                onSelect={!isTransition ? selectLocation : null}
                selectAriaLabel={t("locationMap.openLocationAria", {
                  defaultValue: "Открыть карточку локации «{{title}}»",
                  title: node.title,
                })}
                currentLabel={!isTransition && node.id === layout.currentLocationId
                  ? currentMarkerLabel
                  : ""}
                onTravel={travelEnabled
                  && node.visited !== false
                  && node.id !== layout.currentLocationId
                  ? requestTravel
                  : null}
                travelBusy={travelBusy}
                travelLabel={travelLabel}
                busyLabel={travelBusyLabel}
                travelAriaLabel={t("locationMap.travelToAria", {
                  defaultValue: "Попросить ГМ перейти в «{{title}}»",
                  title: node.title,
                })}
              />
            ))}
            {layout.exits.map((exit) => (
              <ExitCard key={exit.id} exit={exit} unexploredLabel={unexploredLabel} />
            ))}
            {marker ? (
              <div
                className="location-map-marker"
                style={{ left: marker.x, top: marker.y }}
                role="status"
                aria-live={isTransition ? "off" : "polite"}
              >
                <span className="location-map-marker-pulse" aria-hidden="true" />
                <span>{currentMarkerLabel}</span>
              </div>
            ) : null}
          </div>
        ) : (
          <div className="location-map-empty" role="status">
            <Icon name="map" size={34} />
            <strong>{t("locationMap.emptyTitle", { defaultValue: "Карта пока пуста" })}</strong>
            <span>{t("locationMap.emptyDescription", { defaultValue: "Открытые локации появятся здесь по мере путешествия." })}</span>
          </div>
        )}
        {!isTransition && activeEdge && edgeTooltipPosition ? (
          <EdgeTooltip
            edge={activeEdge}
            nodesById={nodesById}
            exitsById={exitsById}
            t={t}
            id={edgeTooltipId}
            position={edgeTooltipPosition}
          />
        ) : null}
      </div>

      {!isTransition && layout.nodes.length ? (
        <nav
          className="location-map-controls"
          aria-label={t("locationMap.controlsHint", { defaultValue: "Управление картой" })}
        >
          <button type="button" onClick={() => zoomAt(1.2)} aria-label={t("locationMap.zoomInAria", { defaultValue: "Приблизить" })}>
            <Icon name="plus" size={17} />
          </button>
          <button type="button" onClick={() => zoomAt(1 / 1.2)} aria-label={t("locationMap.zoomOutAria", { defaultValue: "Отдалить" })}>
            <Icon name="minus" size={17} />
          </button>
          <span aria-hidden="true" />
          <button type="button" onClick={focusCurrent} aria-label={t("locationMap.focusCurrentAria", { defaultValue: "Показать текущую локацию" })}>
            <Icon name="target" size={17} />
          </button>
          <button type="button" onClick={fitMap} aria-label={t("locationMap.fitAria", { defaultValue: "Показать всю карту" })}>
            <Icon name="map" size={17} />
          </button>
        </nav>
      ) : null}
      {selectedLocationScene ? (
        <WorldDetailModal
          kind="scene"
          scene={selectedLocationScene}
          npcs={selectedLocation?.id === layout.currentLocationId ? npcs : []}
          statusLabels={statusLabels}
          closeOnEscape={false}
          onClose={() => setSelectedLocationId(null)}
          footer={selectedLocationIsCurrent ? (
            <div className="location-map-detail-status is-current" role="status">
              <Icon name="target" size={16} />
              <span>{currentMarkerLabel}</span>
            </div>
          ) : selectedLocationCanTravel ? (
            <div className="location-map-detail-actions">
              <button
                type="button"
                className="btn primary location-map-travel-button"
                onClick={() => requestTravel(selectedLocation)}
                disabled={travelBusy}
                aria-busy={travelBusy || undefined}
              >
                <Icon name="walk" size={17} />
                <span>{travelBusy ? travelBusyLabel : travelLabel}</span>
              </button>
            </div>
          ) : null}
        />
      ) : null}
    </section>
  );
}
