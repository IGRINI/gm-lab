import { normalizeEntities } from "./entityContext.js";

function objectOf(value) {
  return value && typeof value === "object" && !Array.isArray(value) ? value : null;
}

function hasOwn(value, key) {
  return Object.prototype.hasOwnProperty.call(value, key);
}

function text(value) {
  return value == null ? "" : String(value).trim();
}

function stableId(value) {
  return text(value?.id || value?.npc_id || value?.key || value?.name || value?.label);
}

function visualValue(value, key) {
  const candidate = text(value?.[key]);
  return candidate || "";
}

function preserveVisual(previous, incoming, key) {
  if (!objectOf(incoming)) return incoming;
  if (visualValue(incoming, key) || !visualValue(previous, key)) return incoming;
  return { ...incoming, [key]: previous[key] };
}

function sameScene(previous, incoming) {
  const previousLocation = text(previous?.location_id);
  const incomingLocation = text(incoming?.location_id);
  if (previousLocation || incomingLocation) {
    return Boolean(previousLocation && incomingLocation && previousLocation === incomingLocation);
  }
  const previousScene = text(previous?.scene_id);
  const incomingScene = text(incoming?.scene_id);
  return Boolean(previousScene && incomingScene && previousScene === incomingScene);
}

function mergeSceneVisual(previous, incoming) {
  if (!objectOf(incoming)) return incoming;
  if (!sameScene(previous, incoming)) return incoming;
  return preserveVisual(previous, incoming, "image_url");
}

function mergeNpcVisuals(previous, incoming) {
  if (!Array.isArray(incoming)) return incoming;
  const previousById = new Map(
    (Array.isArray(previous) ? previous : [])
      .map((npc) => [stableId(npc), npc])
      .filter(([id]) => id)
  );
  return incoming.map((npc) => {
    const oldNpc = previousById.get(stableId(npc));
    return oldNpc ? preserveVisual(oldNpc, npc, "portrait_url") : npc;
  });
}

function mergeGraphVisuals(previous, incoming) {
  if (!objectOf(incoming)) return incoming;
  const previousNodes = new Map(
    (Array.isArray(previous?.nodes) ? previous.nodes : [])
      .map((node) => [text(node?.id), node])
      .filter(([id]) => id)
  );
  if (!Array.isArray(incoming.nodes)) return incoming;

  const nodes = incoming.nodes.map((node) => {
    const oldNode = previousNodes.get(text(node?.id));
    if (!oldNode || !objectOf(node)) return node;
    let nextNode = preserveVisual(oldNode, node, "image_url");
    if (objectOf(nextNode.scene) && objectOf(oldNode.scene)) {
      nextNode = {
        ...nextNode,
        scene: preserveVisual(oldNode.scene, nextNode.scene, "image_url"),
      };
    }
    return nextNode;
  });
  return { ...incoming, nodes };
}

function rawStateOf(event) {
  if (event?.kind !== "state_sync") return null;
  const data = objectOf(event.data);
  return objectOf(data?.state);
}

export function stateSyncSequence(event) {
  if (event?.kind !== "state_sync") return null;
  const value = Number(event?.data?.seq);
  return Number.isSafeInteger(value) && value > 0 ? value : null;
}

export function acceptStateSyncEvent(seenSequences, event) {
  const sequence = stateSyncSequence(event);
  if (sequence == null) return event?.kind === "state_sync";
  const lastSequence = seenSequences.values().next().value;
  if (Number.isSafeInteger(lastSequence) && sequence <= lastSequence) return false;
  seenSequences.clear();
  seenSequences.add(sequence);
  return true;
}

// Fold one authoritative post-tool projection into the currently rendered
// server state. Missing keys mean "not part of this sync", never "clear it".
// Visual URLs are owned by the server's asset layer and may be absent from a
// transient projection, so they survive only for the same entity/location.
export function applyStateSyncEvent(current, event) {
  const state = rawStateOf(event);
  if (!state) return current;

  let changed = false;
  const next = { ...current };

  if (hasOwn(state, "time")) {
    next.time = state.time;
    changed = true;
  }
  if (hasOwn(state, "player_character")) {
    next.playerCharacter = preserveVisual(
      current?.playerCharacter,
      state.player_character,
      "portrait_url"
    );
    changed = true;
  }
  if (hasOwn(state, "scene")) {
    next.scene = mergeSceneVisual(current?.scene, state.scene);
    changed = true;
  }
  if (hasOwn(state, "npcs")) {
    next.npcs = mergeNpcVisuals(current?.npcs, state.npcs);
    changed = true;
  }
  if (hasOwn(state, "location_graph")) {
    next.locationGraph = mergeGraphVisuals(current?.locationGraph, state.location_graph);
    changed = true;
  }
  if (hasOwn(state, "entities")) {
    next.entities = normalizeEntities(state.entities, next.npcs || current?.npcs || []);
    changed = true;
  }

  return changed ? next : current;
}
