import assert from "node:assert/strict";
import test from "node:test";

import {
  clampLocationMapScale,
  createLocationMapLayout,
  getLocationMapFitCamera,
  getLocationMapFocusCamera,
  LOCATION_MAP_MIN_SCALE,
  createLocationMapVisualEdges,
  normalizeLocationGraph,
} from "../src/components/locationMapGraph.js";

const backendGraph = {
  current: "yard",
  root: "kitchen",
  nodes: [
    {
      id: "kitchen",
      name: "Кухня",
      description: "Тёплая кухня старого дома.",
      kind: "interior",
      image_url: "/image-files/kitchen.webp",
    },
    { id: "yard", name: "Задний двор", description: "Мокрая трава.", kind: "exterior" },
  ],
  edges: [
    {
      id: "door-to-yard",
      passage_id: "kitchen-yard-door",
      directionality: "one_way",
      from: "kitchen",
      to: "yard",
      label: "Через заднюю дверь",
      kind: "door",
      passable: true,
      time_cost_minutes: 3,
    },
    {
      id: "gate-to-street",
      from: "yard",
      to: null,
      label: "Калитка",
      kind: "gate",
      passable: false,
      blocked_by: "ржавая цепь",
      placeholder: {
        id: "exit:street",
        name: "Выход на улицу",
        hint: "За калиткой пока ничего не создано.",
      },
    },
  ],
};

test("normalizes the backend location_graph contract without losing map metadata", () => {
  const graph = normalizeLocationGraph(backendGraph);

  assert.equal(graph.currentLocationId, "yard");
  assert.equal(graph.rootLocationId, "kitchen");
  assert.deepEqual(
    graph.nodes.map(({ id, title, imageUrl, kind }) => ({ id, title, imageUrl, kind })),
    [
      { id: "kitchen", title: "Кухня", imageUrl: "/image-files/kitchen.webp", kind: "interior" },
      { id: "yard", title: "Задний двор", imageUrl: "", kind: "exterior" },
    ]
  );
  assert.deepEqual(
    {
      timeCost: graph.edges[0].timeCost,
      blockedBy: graph.edges[0].blockedBy,
      passageId: graph.edges[0].passageId,
      directionality: graph.edges[0].directionality,
    },
    {
      timeCost: 3,
      blockedBy: "",
      passageId: "kitchen-yard-door",
      directionality: "one_way",
    }
  );
  assert.deepEqual(graph.nodes[0].scene, {
    location_id: "kitchen",
    title: "Кухня",
    description: "Тёплая кухня старого дома.",
    image_url: "/image-files/kitchen.webp",
  });
  assert.equal(graph.nodes[0].details, graph.nodes[0].scene);
  assert.deepEqual(
    graph.edges.map(({ id, fromId, toId, unresolved, passable }) => ({ id, fromId, toId, unresolved, passable })),
    [
      { id: "door-to-yard", fromId: "kitchen", toId: "yard", unresolved: false, passable: true },
      { id: "gate-to-street", fromId: "yard", toId: null, unresolved: true, passable: false },
    ]
  );
  assert.deepEqual(graph.edges[1].exit, {
    id: "exit:street",
    title: "Выход на улицу",
    description: "За калиткой пока ничего не создано.",
    destinationId: null,
  });
  assert.equal(graph.edges[1].blockedBy, "ржавая цепь");
  assert.deepEqual(normalizeLocationGraph(graph), graph);
});

test("accepts wrapped and legacy graph aliases", () => {
  const graph = normalizeLocationGraph({
    location_graph: {
      current_location_id: "1",
      nodes: [
        { location_id: 1, title: "Холл", current: true },
        { location_id: 2, title: "Скрытая комната", visited: false },
      ],
      edges: [
        { from_id: 1, to_id: 2, destination: "Скрытая комната" },
        { from_id: "missing", to_id: 1, label: "Некорректное ребро" },
      ],
    },
  });

  assert.equal(graph.nodes.length, 1);
  assert.equal(graph.currentLocationId, "1");
  assert.equal(graph.edges.length, 1);
  assert.equal(graph.edges[0].unresolved, true);
  assert.equal(graph.edges[0].exit.title, "Скрытая комната");
  assert.equal(graph.edges[0].exit.destinationId, "2");
});

test("uses caller-provided localized labels for presentation fallbacks", () => {
  const source = {
    nodes: [{ id: "room", name: "Room" }],
    edges: [{ id: "unknown", from: "room", to: null }],
  };

  const english = normalizeLocationGraph(source, {
    exitLabel: "Exit",
    unexploredExitLabel: "Unexplored exit",
  });
  assert.equal(english.edges[0].label, "Exit");
  assert.equal(english.edges[0].exit.title, "Unexplored exit");

  const russian = normalizeLocationGraph(source, {
    exitLabel: "Выход",
    unexploredExitLabel: "Неизведанный выход",
  });
  assert.equal(russian.edges[0].label, "Выход");
  assert.equal(russian.edges[0].exit.title, "Неизведанный выход");

  const presentationIndependent = normalizeLocationGraph(source);
  assert.equal(presentationIndependent.edges[0].label, "");
  assert.equal(presentationIndependent.edges[0].exit.title, "");
});

test("creates a deterministic rooted layout and a separate placeholder node", () => {
  const first = createLocationMapLayout(backendGraph);
  const second = createLocationMapLayout(backendGraph);

  assert.deepEqual(first, second);
  assert.equal(first.exits.length, 1);
  assert.equal(first.exits[0].id, "exit:street");
  assert.equal(first.edges[1].targetType, "exit");
  assert.equal(first.edges[1].targetId, "exit:street");

  const kitchen = first.nodes.find((node) => node.id === "kitchen");
  const yard = first.nodes.find((node) => node.id === "yard");
  assert.ok(yard.x > kitchen.x);
  assert.ok(first.exits[0].x > yard.x);
  assert.match(first.edges[0].d, /^M /);
  assert.ok(first.bounds.width > first.exits[0].x + first.exits[0].width);
});

test("keeps disconnected visited locations in the same finite map", () => {
  const layout = createLocationMapLayout({
    root: "a",
    nodes: [{ id: "a", name: "A" }, { id: "b", name: "B" }, { id: "c", name: "C" }],
    edges: [{ id: "ab", from: "a", to: "b", label: "AB" }],
  });

  const a = layout.nodes.find((node) => node.id === "a");
  const c = layout.nodes.find((node) => node.id === "c");
  assert.ok(c.y > a.y);
  layout.nodes.forEach((node) => {
    assert.equal(Number.isFinite(node.x), true);
    assert.equal(Number.isFinite(node.y), true);
  });
});

test("keeps ambiguous parallel passages separate instead of inventing reciprocal pairs", () => {
  const graph = normalizeLocationGraph({
    root: "a",
    nodes: [{ id: "a", name: "A" }, { id: "b", name: "B" }, { id: "c", name: "C" }],
    edges: [
      { id: "a-b-door", from: "a", to: "b", label: "Через дверь", time_cost_minutes: 2 },
      { id: "a-b-window", from: "a", to: "b", label: "Через окно" },
      { id: "b-a-window", from: "b", to: "a", label: "Обратно через окно", time_cost_minutes: 3 },
      { id: "a-c-alley", from: "a", to: "c", label: "По переулку" },
    ],
  });

  const visualEdges = createLocationMapVisualEdges(graph.edges);
  assert.equal(visualEdges.length, 4);
  assert.equal(visualEdges.some((edge) => edge.bidirectional), false);
  assert.deepEqual(
    visualEdges.map((edge) => edge.directionIds),
    [["a-b-door"], ["a-b-window"], ["b-a-window"], ["a-c-alley"]]
  );

  const layout = createLocationMapLayout(graph);
  assert.equal(layout.edges.length, 4);
  assert.equal(layout.edges.filter((edge) => edge.bidirectional).length, 0);
  assert.equal(new Set(layout.edges.map((edge) => edge.d)).size, 4);
});

test("keeps a shared reciprocal label only once", () => {
  const edges = normalizeLocationGraph({
    nodes: [{ id: "a", name: "A" }, { id: "b", name: "B" }],
    edges: [
      {
        id: "out",
        passage_id: "stone-bridge",
        directionality: "bidirectional",
        from: "a",
        to: "b",
        label: "Каменный мост",
      },
      {
        id: "back",
        passage_id: "stone-bridge",
        directionality: "bidirectional",
        from: "b",
        to: "a",
        label: "каменный мост",
      },
    ],
  }).edges;

  const [visualEdge] = createLocationMapVisualEdges(edges);
  assert.equal(visualEdge.bidirectional, true);
  assert.equal(visualEdge.label, "Каменный мост");
});

test("keeps a closed bidirectional passage as one edge with its blocker", () => {
  const closedGraph = normalizeLocationGraph({
    nodes: [{ id: "cave", name: "Пещера" }, { id: "ledge", name: "Уступ" }],
    edges: [
      {
        id: "down",
        passage_id: "cave-rope",
        directionality: "bidirectional",
        from: "cave",
        to: "ledge",
        label: "По верёвке вниз",
        passable: false,
        blocked_by: "верёвку убрали",
      },
      {
        id: "up",
        passage_id: "cave-rope",
        directionality: "bidirectional",
        from: "ledge",
        to: "cave",
        label: "По верёвке наверх",
        // Legacy saves can contain this inconsistent pair of fields.
        passable: true,
        blocked_by: "верёвку убрали",
      },
    ],
  });

  const [closedEdge] = createLocationMapVisualEdges(closedGraph.edges);
  assert.equal(closedGraph.edges.length, 2);
  assert.equal(closedEdge.bidirectional, true);
  assert.equal(closedEdge.passable, false);
  assert.deepEqual(closedEdge.directionIds, ["down", "up"]);
  assert.deepEqual(
    closedEdge.directions.map(({ passable, blockedBy }) => ({ passable, blockedBy })),
    [
      { passable: false, blockedBy: "верёвку убрали" },
      { passable: false, blockedBy: "верёвку убрали" },
    ]
  );

  const reopenedGraph = normalizeLocationGraph({
    nodes: closedGraph.nodes,
    edges: closedGraph.edges.map((edge) => ({
      id: edge.id,
      passage_id: edge.passageId,
      directionality: edge.directionality,
      from: edge.fromId,
      to: edge.toId,
      label: edge.label,
      passable: true,
      blocked_by: "",
    })),
  });
  const [reopenedEdge] = createLocationMapVisualEdges(reopenedGraph.edges);
  assert.equal(reopenedGraph.edges.length, 2);
  assert.equal(reopenedEdge.bidirectional, true);
  assert.equal(reopenedEdge.passable, true);
  assert.equal(reopenedEdge.directions.every((edge) => edge.blockedBy === ""), true);
});

test("never merges a fall and a separate climb just because their endpoints are opposite", () => {
  const edges = normalizeLocationGraph({
    nodes: [{ id: "cave", name: "Пещера" }, { id: "chasm", name: "Дно обрыва" }],
    edges: [
      {
        id: "fall",
        passage_id: "cave-fall",
        directionality: "one_way",
        from: "cave",
        to: "chasm",
        label: "Прыгнуть в обрыв",
      },
      {
        id: "climb",
        passage_id: "chasm-climb",
        directionality: "one_way",
        from: "chasm",
        to: "cave",
        label: "Долгий обход наверх",
      },
    ],
  }).edges;

  const visualEdges = createLocationMapVisualEdges(edges);
  assert.equal(visualEdges.length, 2);
  assert.equal(visualEdges.some((edge) => edge.bidirectional), false);
  assert.deepEqual(visualEdges.map((edge) => edge.directionIds), [["fall"], ["climb"]]);
});

test("does not infer a reciprocal passage for legacy edges without passage ids", () => {
  const edges = normalizeLocationGraph({
    nodes: [{ id: "a", name: "A" }, { id: "b", name: "B" }],
    edges: [
      { id: "legacy-out", from: "a", to: "b", label: "Туда" },
      { id: "legacy-back", from: "b", to: "a", label: "Обратно" },
    ],
  }).edges;

  const visualEdges = createLocationMapVisualEdges(edges);
  assert.equal(visualEdges.length, 2);
  assert.equal(visualEdges.some((edge) => edge.bidirectional), false);
});

test("camera helpers center targets, fit bounds and clamp unsafe scales", () => {
  assert.deepEqual(
    getLocationMapFocusCamera({ x: 100, y: 50, width: 200, height: 100 }, { width: 800, height: 600 }, 1),
    { x: 200, y: 200, scale: 1 }
  );
  assert.deepEqual(
    getLocationMapFitCamera(
      { minX: 0, minY: 0, width: 1000, height: 500 },
      { width: 600, height: 400 },
      { padding: 50, minScale: 0.2, maxScale: 1 }
    ),
    { x: 50, y: 75, scale: 0.5 }
  );
  assert.equal(clampLocationMapScale(Number.NaN, 0.4, 2), 0.4);
  assert.equal(clampLocationMapScale(0.1, 0.4, 2), 0.4);
  assert.equal(clampLocationMapScale(4, 0.4, 2), 2);

  const largeOverview = getLocationMapFitCamera(
    { minX: 0, minY: 0, width: 5000, height: 900 },
    { width: 390, height: 844 },
    { padding: 72, minScale: LOCATION_MAP_MIN_SCALE, maxScale: 1 }
  );
  assert.ok(largeOverview.scale < 0.35);
  assert.ok(5000 * largeOverview.scale <= 390 - 144);
});
