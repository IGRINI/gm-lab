const DEFAULT_LAYOUT = Object.freeze({
  nodeWidth: 248,
  nodeHeight: 178,
  exitWidth: 188,
  exitHeight: 52,
  columnGap: 156,
  rowGap: 72,
  componentGap: 176,
  padding: 180,
});

export const LOCATION_MAP_MIN_SCALE = 0.04;

function asArray(value) {
  return Array.isArray(value) ? value : [];
}

function asId(value) {
  const candidate = value && typeof value === "object"
    ? value.id ?? value.location_id ?? value.locationId
    : value;
  if (candidate === null || candidate === undefined) return null;
  const id = String(candidate).trim();
  return id || null;
}

function asText(value, fallback = "") {
  if (value === null || value === undefined) return fallback;
  const text = String(value).trim();
  return text || fallback;
}

function firstText(...values) {
  for (const value of values) {
    const text = asText(value);
    if (text) return text;
  }
  return "";
}

function asFiniteNumber(...values) {
  for (const value of values) {
    if (value === null || value === undefined || value === "") continue;
    const number = Number(value);
    if (Number.isFinite(number)) return number;
  }
  return 0;
}

function normalizeNodeDetails(rawNode, fallback) {
  const supplied = rawNode.scene && typeof rawNode.scene === "object"
    ? rawNode.scene
    : rawNode.details && typeof rawNode.details === "object"
      ? rawNode.details
      : {};
  const details = {
    ...supplied,
    location_id: asId(supplied.location_id ?? supplied.locationId) ?? fallback.id,
    title: firstText(supplied.title, supplied.name, fallback.title),
    description: firstText(supplied.description, fallback.description),
  };
  const imageUrl = firstText(supplied.image_url, supplied.imageUrl, supplied.image, fallback.imageUrl);
  if (imageUrl) details.image_url = imageUrl;
  return details;
}

function unwrapGraph(value) {
  if (!value || typeof value !== "object") return {};
  if (Array.isArray(value.nodes) || Array.isArray(value.edges)) return value;
  if (value.location_graph && typeof value.location_graph === "object") {
    return value.location_graph;
  }
  if (value.graph && typeof value.graph === "object") return value.graph;
  return value;
}

function uniqueId(preferred, used, prefix) {
  const base = preferred || prefix;
  let id = base;
  let suffix = 2;
  while (used.has(id)) {
    id = `${base}:${suffix}`;
    suffix += 1;
  }
  used.add(id);
  return id;
}

/**
 * Converts both the backend location_graph contract and older UI-shaped data
 * into a small, presentation-independent graph model.
 */
export function normalizeLocationGraph(value) {
  const source = unwrapGraph(value);
  const rawNodes = asArray(source.nodes ?? source.locations);
  const allNodesById = new Map();
  const nodes = [];

  rawNodes.forEach((rawNode, sourceIndex) => {
    if (!rawNode || typeof rawNode !== "object") return;
    const id = asId(rawNode);
    if (!id || allNodesById.has(id)) return;

    const title = firstText(rawNode.name, rawNode.title, rawNode.label, id);
    const description = firstText(rawNode.description, rawNode.summary, rawNode.hint);
    const imageUrl = firstText(rawNode.image_url, rawNode.imageUrl, rawNode.image);
    const details = normalizeNodeDetails(rawNode, { id, title, description, imageUrl });
    const node = {
      id,
      title,
      description,
      imageUrl,
      kind: firstText(rawNode.kind, rawNode.type),
      visited: rawNode.visited !== false && rawNode.generated !== false && rawNode.unresolved !== true,
      sourceIndex,
      current: rawNode.current === true,
      scene: details,
      details,
    };
    allNodesById.set(id, node);
    if (node.visited) nodes.push(node);
  });

  const visitedIds = new Set(nodes.map((node) => node.id));
  const usedEdgeIds = new Set();
  const edges = [];

  asArray(source.edges ?? source.exits).forEach((rawEdge, sourceIndex) => {
    if (!rawEdge || typeof rawEdge !== "object") return;
    const fromId = asId(rawEdge.from ?? rawEdge.from_id ?? rawEdge.fromId ?? rawEdge.source);
    if (!fromId || !visitedIds.has(fromId)) return;

    const requestedToId = asId(rawEdge.to ?? rawEdge.to_id ?? rawEdge.toId ?? rawEdge.target);
    const isResolved = rawEdge.unresolved !== true && requestedToId && visitedIds.has(requestedToId);
    const destinationNode = requestedToId ? allNodesById.get(requestedToId) : null;
    const placeholder = rawEdge.placeholder && typeof rawEdge.placeholder === "object"
      ? rawEdge.placeholder
      : rawEdge.exit && typeof rawEdge.exit === "object"
        ? rawEdge.exit
        : rawEdge.destination && typeof rawEdge.destination === "object"
          ? rawEdge.destination
          : {};
    const rawDestinationText = typeof rawEdge.destination === "string" ? rawEdge.destination : "";
    const fallbackEdgeId = `edge:${fromId}:${requestedToId || sourceIndex + 1}`;
    const id = uniqueId(asId(rawEdge) || fallbackEdgeId, usedEdgeIds, fallbackEdgeId);
    const label = firstText(
      rawEdge.label,
      rawEdge.name,
      placeholder.name,
      placeholder.title,
      rawDestinationText,
      destinationNode?.title,
      "Выход"
    );

    let exit = null;
    if (!isResolved) {
      const placeholderId = asId(placeholder) || `exit:${id}`;
      exit = {
        id: placeholderId,
        title: firstText(placeholder.name, placeholder.title, label, "Неизведанный выход"),
        description: firstText(placeholder.hint, placeholder.description, rawEdge.hint),
        destinationId: requestedToId,
      };
    }

    const blockedBy = firstText(rawEdge.blocked_by, rawEdge.blockedBy);
    edges.push({
      id,
      passageId: asId(rawEdge.passage_id ?? rawEdge.passageId),
      directionality: firstText(rawEdge.directionality),
      fromId,
      toId: isResolved ? requestedToId : null,
      label,
      kind: firstText(rawEdge.kind, rawEdge.type),
      risk: firstText(rawEdge.risk),
      passable: rawEdge.passable !== false && rawEdge.locked !== true && !blockedBy,
      description: firstText(rawEdge.description, rawEdge.summary, rawEdge.hint),
      timeCost: asFiniteNumber(
        rawEdge.time_cost_minutes,
        rawEdge.timeCostMinutes,
        rawEdge.time_cost,
        rawEdge.timeCost,
        rawEdge.travel_minutes,
        rawEdge.travelMinutes,
        rawEdge.duration_minutes,
        rawEdge.durationMinutes
      ),
      blockedBy,
      unresolved: !isResolved,
      exit,
      sourceIndex,
    });
  });

  const requestedCurrentId = asId(
    source.current ?? source.current_location_id ?? source.currentLocationId
  );
  const currentLocationId = visitedIds.has(requestedCurrentId)
    ? requestedCurrentId
    : nodes.find((node) => node.current)?.id ?? null;
  const requestedRootId = asId(source.root ?? source.root_location_id ?? source.rootLocationId);
  const rootLocationId = visitedIds.has(requestedRootId)
    ? requestedRootId
    : nodes[0]?.id ?? null;

  return {
    nodes,
    edges,
    currentLocationId,
    rootLocationId,
  };
}

function visualEdgeLabel(directions) {
  const labels = directions
    .map((direction) => direction.label)
    .filter(Boolean)
    .filter((label, index, items) =>
      items.findIndex((candidate) => candidate.localeCompare(label, undefined, { sensitivity: "base" }) === 0) === index
    );
  return labels.length === 1 ? labels[0] : "";
}

function buildVisualEdge(directions) {
  const ordered = [...directions].sort(compareBySourceIndex);
  const first = ordered[0];
  const bidirectional = ordered.length > 1
    && ordered.some((edge) => edge.fromId === first.toId && edge.toId === first.fromId);
  return {
    ...first,
    id: ordered.length > 1
      ? `edge-group:${ordered.map((edge) => edge.id).sort().join("|")}`
      : first.id,
    label: visualEdgeLabel(ordered),
    passable: ordered.some((edge) => edge.passable),
    sourceIndex: Math.min(...ordered.map((edge) => edge.sourceIndex)),
    directions: ordered,
    directionIds: ordered.map((edge) => edge.id),
    bidirectional,
  };
}

/**
 * Combines a reciprocal passage only when exactly two opposite directed edges
 * explicitly share one non-empty passage id and both declare bidirectional
 * traversal. Endpoint coincidence alone never establishes route identity.
 */
export function createLocationMapVisualEdges(edges) {
  const passageBuckets = new Map();
  const visualEdges = [];

  edges.forEach((edge) => {
    if (
      !edge.toId
      || edge.fromId === edge.toId
      || edge.directionality !== "bidirectional"
      || !edge.passageId
    ) {
      visualEdges.push(buildVisualEdge([edge]));
      return;
    }
    if (!passageBuckets.has(edge.passageId)) passageBuckets.set(edge.passageId, []);
    passageBuckets.get(edge.passageId).push(edge);
  });

  passageBuckets.forEach((bucket) => {
    if (
      bucket.length === 2
      && bucket[0].fromId === bucket[1].toId
      && bucket[0].toId === bucket[1].fromId
    ) {
      visualEdges.push(buildVisualEdge(bucket));
      return;
    }
    bucket.forEach((edge) => visualEdges.push(buildVisualEdge([edge])));
  });

  return visualEdges.sort(compareBySourceIndex);
}

function compareBySourceIndex(left, right) {
  return left.sourceIndex - right.sourceIndex || left.id.localeCompare(right.id);
}

function collectComponents(nodes, edges, rootLocationId) {
  const nodeById = new Map(nodes.map((node) => [node.id, node]));
  const neighbors = new Map(nodes.map((node) => [node.id, new Set()]));
  edges.forEach((edge) => {
    if (!edge.toId || !nodeById.has(edge.toId)) return;
    neighbors.get(edge.fromId)?.add(edge.toId);
    neighbors.get(edge.toId)?.add(edge.fromId);
  });

  const orderedStarts = [...nodes].sort((left, right) => {
    if (left.id === rootLocationId) return -1;
    if (right.id === rootLocationId) return 1;
    return compareBySourceIndex(left, right);
  });
  const visited = new Set();
  const components = [];

  orderedStarts.forEach((start) => {
    if (visited.has(start.id)) return;
    const queue = [start.id];
    const component = [];
    visited.add(start.id);
    for (let cursor = 0; cursor < queue.length; cursor += 1) {
      const id = queue[cursor];
      component.push(nodeById.get(id));
      const nextIds = [...(neighbors.get(id) ?? [])]
        .map((nextId) => nodeById.get(nextId))
        .filter(Boolean)
        .sort(compareBySourceIndex);
      nextIds.forEach((node) => {
        if (visited.has(node.id)) return;
        visited.add(node.id);
        queue.push(node.id);
      });
    }
    components.push(component);
  });

  return { components, neighbors };
}

function rankComponent(component, edges, neighbors, preferredRootId) {
  const componentIds = new Set(component.map((node) => node.id));
  const rootId = componentIds.has(preferredRootId)
    ? preferredRootId
    : [...component].sort(compareBySourceIndex)[0]?.id;
  const outgoing = new Map(component.map((node) => [node.id, []]));
  edges.forEach((edge) => {
    if (!edge.toId || !componentIds.has(edge.fromId) || !componentIds.has(edge.toId)) return;
    outgoing.get(edge.fromId).push(edge);
  });
  outgoing.forEach((items) => items.sort(compareBySourceIndex));

  const ranks = new Map();
  const queue = rootId ? [rootId] : [];
  if (rootId) ranks.set(rootId, 0);
  for (let cursor = 0; cursor < queue.length; cursor += 1) {
    const id = queue[cursor];
    const rank = ranks.get(id);
    outgoing.get(id)?.forEach((edge) => {
      if (ranks.has(edge.toId)) return;
      ranks.set(edge.toId, rank + 1);
      queue.push(edge.toId);
    });
  }

  // Directed data can point toward the root. Fill those nodes through the
  // undirected topology so every connected location still gets a stable rank.
  while (ranks.size < component.length) {
    let changed = false;
    [...component].sort(compareBySourceIndex).forEach((node) => {
      if (ranks.has(node.id)) return;
      const rankedNeighbor = [...(neighbors.get(node.id) ?? [])]
        .filter((id) => componentIds.has(id) && ranks.has(id))
        .sort((left, right) => ranks.get(left) - ranks.get(right) || left.localeCompare(right))[0];
      if (!rankedNeighbor) return;
      ranks.set(node.id, ranks.get(rankedNeighbor) + 1);
      changed = true;
    });
    if (!changed) {
      const next = component.find((node) => !ranks.has(node.id));
      if (!next) break;
      ranks.set(next.id, Math.max(0, ...ranks.values()) + 1);
    }
  }
  return ranks;
}

function createExitItems(edges, nodeRanks) {
  const exitsById = new Map();
  edges.forEach((edge) => {
    if (!edge.unresolved || !edge.exit || !nodeRanks.has(edge.fromId)) return;
    const rank = nodeRanks.get(edge.fromId) + 1;
    const existing = exitsById.get(edge.exit.id);
    if (existing) {
      existing.rank = Math.min(existing.rank, rank);
      existing.edgeIds.push(edge.id);
      return;
    }
    exitsById.set(edge.exit.id, {
      ...edge.exit,
      kind: edge.kind,
      passable: edge.passable,
      rank,
      sourceIndex: edge.sourceIndex,
      edgeIds: [edge.id],
    });
  });
  return [...exitsById.values()].sort(compareBySourceIndex);
}

function positionComponent(component, componentEdges, neighbors, preferredRootId, top, config) {
  const ranks = rankComponent(component, componentEdges, neighbors, preferredRootId);
  const exits = createExitItems(componentEdges, ranks);
  const layers = new Map();
  const pushLayerItem = (rank, item) => {
    if (!layers.has(rank)) layers.set(rank, []);
    layers.get(rank).push(item);
  };

  component.forEach((node) => pushLayerItem(ranks.get(node.id) ?? 0, {
    type: "node",
    sourceIndex: node.sourceIndex,
    data: node,
    width: config.nodeWidth,
    height: config.nodeHeight,
  }));
  exits.forEach((exit) => pushLayerItem(exit.rank, {
    type: "exit",
    sourceIndex: exit.sourceIndex,
    data: exit,
    width: config.exitWidth,
    height: config.exitHeight,
  }));

  layers.forEach((items) => items.sort((left, right) =>
    left.sourceIndex - right.sourceIndex || left.data.id.localeCompare(right.data.id)
  ));
  const layerHeights = new Map();
  layers.forEach((items, rank) => {
    const contentHeight = items.reduce((height, item) => height + item.height, 0);
    layerHeights.set(rank, contentHeight + Math.max(0, items.length - 1) * config.rowGap);
  });
  const componentHeight = Math.max(config.nodeHeight, ...layerHeights.values());
  const positionedNodes = [];
  const positionedExits = [];

  [...layers.entries()].sort(([left], [right]) => left - right).forEach(([rank, items]) => {
    let y = top + (componentHeight - layerHeights.get(rank)) / 2;
    items.forEach((item) => {
      const x = config.padding + rank * (config.nodeWidth + config.columnGap);
      const positioned = { ...item.data, x, y, width: item.width, height: item.height, rank };
      if (item.type === "node") positionedNodes.push(positioned);
      else positionedExits.push(positioned);
      y += item.height + config.rowGap;
    });
  });

  return { nodes: positionedNodes, exits: positionedExits, height: componentHeight, ranks };
}

function edgePath(source, target, offset = 0) {
  const sourceCenterX = source.x + source.width / 2;
  const sourceCenterY = source.y + source.height / 2;
  const targetCenterX = target.x + target.width / 2;
  const targetCenterY = target.y + target.height / 2;
  const deltaX = targetCenterX - sourceCenterX;
  const deltaY = targetCenterY - sourceCenterY;

  if (source === target || (deltaX === 0 && deltaY === 0)) {
    const startX = source.x + source.width;
    const startY = sourceCenterY - 24;
    const radius = 58 + Math.abs(offset);
    return {
      d: `M ${startX} ${startY} C ${startX + radius} ${startY - radius}, ${startX + radius} ${startY + radius}, ${startX} ${startY + 48}`,
      labelX: startX + radius * 0.82,
      labelY: sourceCenterY,
    };
  }

  if (Math.abs(deltaX) >= Math.abs(deltaY) * 0.65) {
    const direction = deltaX >= 0 ? 1 : -1;
    const startX = direction > 0 ? source.x + source.width : source.x;
    const endX = direction > 0 ? target.x : target.x + target.width;
    const startY = sourceCenterY;
    const endY = targetCenterY;
    const control = Math.max(58, Math.abs(endX - startX) * 0.48);
    return {
      d: `M ${startX} ${startY} C ${startX + direction * control} ${startY + offset}, ${endX - direction * control} ${endY + offset}, ${endX} ${endY}`,
      labelX: (startX + endX) / 2,
      labelY: (startY + endY) / 2 + offset - 10,
    };
  }

  const direction = deltaY >= 0 ? 1 : -1;
  const startX = sourceCenterX;
  const endX = targetCenterX;
  const startY = direction > 0 ? source.y + source.height : source.y;
  const endY = direction > 0 ? target.y : target.y + target.height;
  const control = Math.max(52, Math.abs(endY - startY) * 0.48);
  return {
    d: `M ${startX} ${startY} C ${startX + offset} ${startY + direction * control}, ${endX + offset} ${endY - direction * control}, ${endX} ${endY}`,
    labelX: (startX + endX) / 2 + offset,
    labelY: (startY + endY) / 2 - 10,
  };
}

function addParallelEdgeOffsets(edges) {
  const groups = new Map();
  edges.forEach((edge) => {
    const targetKey = edge.toId ? `node:${edge.toId}` : `exit:${edge.exit?.id ?? edge.id}`;
    const pair = [`node:${edge.fromId}`, targetKey].sort().join("|");
    if (!groups.has(pair)) groups.set(pair, []);
    groups.get(pair).push(edge);
  });
  const offsets = new Map();
  groups.forEach((group) => {
    group.sort(compareBySourceIndex).forEach((edge, index) => {
      offsets.set(edge.id, (index - (group.length - 1) / 2) * 22);
    });
  });
  return offsets;
}

/** Creates a deterministic, left-to-right layout with no random/physics step. */
export function createLocationMapLayout(value, options = {}) {
  const graph = normalizeLocationGraph(value);
  const config = { ...DEFAULT_LAYOUT, ...options };
  if (!graph.nodes.length) {
    return {
      ...graph,
      nodes: [],
      exits: [],
      edges: [],
      bounds: { minX: 0, minY: 0, maxX: 640, maxY: 420, width: 640, height: 420 },
      config,
    };
  }

  const { components, neighbors } = collectComponents(graph.nodes, graph.edges, graph.rootLocationId);
  const positionedNodes = [];
  const positionedExits = [];
  let componentTop = config.padding;

  components.forEach((component) => {
    const ids = new Set(component.map((node) => node.id));
    const componentEdges = graph.edges.filter((edge) => ids.has(edge.fromId) && (!edge.toId || ids.has(edge.toId)));
    const positioned = positionComponent(
      component,
      componentEdges,
      neighbors,
      graph.rootLocationId,
      componentTop,
      config
    );
    positionedNodes.push(...positioned.nodes);
    positionedExits.push(...positioned.exits);
    componentTop += positioned.height + config.componentGap;
  });

  const nodeById = new Map(positionedNodes.map((node) => [node.id, node]));
  const exitById = new Map(positionedExits.map((item) => [item.id, item]));
  const visualEdges = createLocationMapVisualEdges(graph.edges);
  const parallelOffsets = addParallelEdgeOffsets(visualEdges);
  const positionedEdges = visualEdges.flatMap((edge) => {
    const source = nodeById.get(edge.fromId);
    const target = edge.toId ? nodeById.get(edge.toId) : exitById.get(edge.exit?.id);
    if (!source || !target) return [];
    return [{
      ...edge,
      targetType: edge.toId ? "node" : "exit",
      targetId: edge.toId ?? edge.exit.id,
      ...edgePath(source, target, parallelOffsets.get(edge.id) ?? 0),
    }];
  });

  const items = [...positionedNodes, ...positionedExits];
  const maxX = Math.max(...items.map((item) => item.x + item.width)) + config.padding;
  const maxY = Math.max(...items.map((item) => item.y + item.height)) + config.padding;
  return {
    ...graph,
    nodes: positionedNodes,
    exits: positionedExits,
    edges: positionedEdges,
    bounds: { minX: 0, minY: 0, maxX, maxY, width: maxX, height: maxY },
    config,
  };
}

export function getLocationMapFocusCamera(item, viewport, scale = 1) {
  if (!item || !viewport?.width || !viewport?.height) return null;
  return {
    x: viewport.width / 2 - (item.x + item.width / 2) * scale,
    y: viewport.height / 2 - (item.y + item.height / 2) * scale,
    scale,
  };
}

export function getLocationMapFitCamera(bounds, viewport, options = {}) {
  if (!bounds || !viewport?.width || !viewport?.height) return null;
  const padding = Number.isFinite(options.padding) ? options.padding : 56;
  const minScale = Number.isFinite(options.minScale) ? options.minScale : LOCATION_MAP_MIN_SCALE;
  const maxScale = Number.isFinite(options.maxScale) ? options.maxScale : 1;
  const availableWidth = Math.max(1, viewport.width - padding * 2);
  const availableHeight = Math.max(1, viewport.height - padding * 2);
  const scale = Math.min(
    maxScale,
    Math.max(minScale, Math.min(availableWidth / bounds.width, availableHeight / bounds.height))
  );
  return {
    x: (viewport.width - bounds.width * scale) / 2 - bounds.minX * scale,
    y: (viewport.height - bounds.height * scale) / 2 - bounds.minY * scale,
    scale,
  };
}

export function clampLocationMapScale(value, min = LOCATION_MAP_MIN_SCALE, max = 1.8) {
  const scale = Number(value);
  if (!Number.isFinite(scale)) return min;
  return Math.min(max, Math.max(min, scale));
}

export { DEFAULT_LAYOUT as LOCATION_MAP_LAYOUT_DEFAULTS };
