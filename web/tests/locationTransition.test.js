import test from "node:test";
import assert from "node:assert/strict";

import {
  createLocationTransition,
  hasLocationGraph,
  locationTravelIntent,
  sceneLocationId,
} from "../src/locationTransition.js";

const graph = {
  current: "hall",
  nodes: [
    { id: "kitchen", name: "Kitchen" },
    { id: "hall", name: "Hall" },
  ],
  edges: [{ id: "door", from: "kitchen", to: "hall", label: "Door" }],
};

test("sceneLocationId accepts only a canonical location id", () => {
  assert.equal(sceneLocationId({ location_id: " kitchen ", scene_id: "scene-1" }), "kitchen");
  assert.equal(sceneLocationId({ scene_id: "scene-1", title: "Kitchen" }), "");
  assert.equal(sceneLocationId("Kitchen"), "");
});

test("location graph availability requires at least one node", () => {
  assert.equal(hasLocationGraph(graph), true);
  assert.equal(hasLocationGraph({ nodes: [] }), false);
  assert.equal(hasLocationGraph(null), false);
});

test("map selection creates only a player intent for the GM", () => {
  assert.equal(
    locationTravelIntent({ id: "market", title: " Рынок " }),
    "Я хочу перейти в [[loc:market|Рынок]]."
  );
  assert.equal(
    locationTravelIntent({ id: "market", name: "Рынок" }),
    "Я хочу перейти в [[loc:market|Рынок]]."
  );
  assert.equal(
    locationTravelIntent({ title: "Market" }, (destination) => `I want to go to “${destination}”.`),
    "I want to go to “Market”."
  );
  assert.equal(
    locationTravelIntent({ id: "bad]id", title: "Рынок" }),
    "Я хочу перейти в Рынок."
  );
  assert.equal(locationTravelIntent({ id: "market" }), "");
});

test("transition is created only for an explicitly enabled location change", () => {
  const previous = { scene: { location_id: "kitchen", title: "Kitchen" } };
  const next = {
    scene: { location_id: "hall", title: "Hall" },
    locationGraph: graph,
  };

  assert.equal(createLocationTransition(previous, next, false), null);
  assert.deepEqual(createLocationTransition(previous, next, true), {
    graph,
    fromLocationId: "kitchen",
    toLocationId: "hall",
    fromScene: previous.scene,
    toScene: next.scene,
  });
});

test("same location, missing ids, and incomplete graphs do not animate", () => {
  assert.equal(
    createLocationTransition(
      { scene: { location_id: "hall", scene_id: "old" } },
      { scene: { location_id: "hall", scene_id: "new" }, locationGraph: graph },
      true
    ),
    null
  );
  assert.equal(
    createLocationTransition(
      { scene: { location_id: "" } },
      { scene: { location_id: "hall" }, locationGraph: graph },
      true
    ),
    null
  );
  assert.equal(
    createLocationTransition(
      { scene: { location_id: "kitchen" } },
      { scene: { location_id: "attic" }, locationGraph: graph },
      true
    ),
    null
  );
});
