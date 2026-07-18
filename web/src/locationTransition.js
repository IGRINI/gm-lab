function cleanText(value) {
  return typeof value === "string" ? value.trim() : "";
}

export function sceneLocationId(scene) {
  return scene && typeof scene === "object" ? cleanText(scene.location_id) : "";
}

export function hasLocationGraph(graph) {
  return !!graph && Array.isArray(graph.nodes) && graph.nodes.length > 0;
}

export function locationTravelIntent(node, formatIntent) {
  const destination = cleanText(node?.title || node?.name);
  if (!destination) return "";
  const locationId = cleanText(node?.id || node?.location_id);
  const safeId = /[\]|\n]/.test(locationId) ? "" : locationId;
  const safeLabel = destination.replace(/[\]|\n]/g, " ").trim();
  const destinationReference = safeId && safeLabel
    ? `[[loc:${safeId}|${safeLabel}]]`
    : destination;
  const localized = typeof formatIntent === "function"
    ? cleanText(formatIntent(destinationReference))
    : "";
  return localized;
}

export function createLocationTransition(previousState, nextState, enabled) {
  if (!enabled || !hasLocationGraph(nextState?.locationGraph)) return null;

  const fromLocationId = sceneLocationId(previousState?.scene);
  const toLocationId = sceneLocationId(nextState?.scene);
  if (!fromLocationId || !toLocationId || fromLocationId === toLocationId) return null;

  const nodeIds = new Set(
    nextState.locationGraph.nodes
      .map((node) => cleanText(node?.id))
      .filter(Boolean)
  );
  if (!nodeIds.has(fromLocationId) || !nodeIds.has(toLocationId)) return null;

  return {
    graph: nextState.locationGraph,
    fromLocationId,
    toLocationId,
    fromScene: previousState.scene,
    toScene: nextState.scene,
  };
}
