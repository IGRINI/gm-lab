import assert from "node:assert/strict";
import test from "node:test";

import {
  acceptStateSyncEvent,
  applyStateSyncEvent,
  stateSyncSequence,
} from "../src/liveTurnState.js";

function sync(seq, state) {
  return { kind: "state_sync", data: { seq, call_id: `call-${seq}`, state } };
}

test("state_sync applies every supplied public state slice immediately", () => {
  const current = {
    time: { time_of_day: "Утро" },
    playerCharacter: { name: "Ингвар", portrait_url: "/hero.png", hp: { current: 10, max: 10 } },
    scene: { scene_id: "market", location_id: "market", title: "Рынок", image_url: "/market.png" },
    npcs: [{ id: "liza", name: "Лиза", portrait_url: "/liza.png" }],
    entities: { byKey: {} },
    locationGraph: {
      current: "market",
      nodes: [{ id: "market", image_url: "/market.png", scene: { image_url: "/market.png" } }],
      edges: [],
    },
  };
  const event = sync(1, {
    time: { time_of_day: "Полдень" },
    player_character: { name: "Ингвар", hp: { current: 7, max: 10 } },
    scene: { scene_id: "market", location_id: "market", title: "Рынок после драки" },
    npcs: [{ id: "liza", name: "Лиза", condition: "испугана" }],
    entities: { entities: [{ kind: "npc", id: "liza", label: "Лиза" }] },
    location_graph: {
      current: "market",
      nodes: [{ id: "market", scene: { title: "Рынок после драки" } }],
      edges: [{ id: "door", from: "market", to: "tavern" }],
    },
  });

  const next = applyStateSyncEvent(current, event);

  assert.equal(next.time.time_of_day, "Полдень");
  assert.equal(next.playerCharacter.hp.current, 7);
  assert.equal(next.playerCharacter.portrait_url, "/hero.png");
  assert.equal(next.scene.title, "Рынок после драки");
  assert.equal(next.scene.image_url, "/market.png");
  assert.equal(next.npcs[0].portrait_url, "/liza.png");
  assert.equal(next.entities.byKey["npc:liza"].condition, "испугана");
  assert.equal(next.locationGraph.edges[0].id, "door");
  assert.equal(next.locationGraph.nodes[0].image_url, "/market.png");
  assert.equal(next.locationGraph.nodes[0].scene.image_url, "/market.png");
});

test("a new location never inherits the previous location image", () => {
  const current = {
    scene: { scene_id: "market", location_id: "market", image_url: "/market.png" },
  };
  const next = applyStateSyncEvent(
    current,
    sync(2, { scene: { scene_id: "castle", location_id: "castle", title: "Замок" } })
  );
  assert.equal(next.scene.image_url, undefined);
});

test("missing state_sync fields are not treated as clears", () => {
  const current = { time: { time_of_day: "Утро" }, scene: { title: "Рынок" } };
  const next = applyStateSyncEvent(current, sync(3, { time: { time_of_day: "Вечер" } }));
  assert.equal(next.scene, current.scene);
  assert.equal(next.time.time_of_day, "Вечер");
});

test("non-sync events and empty syncs are no-ops", () => {
  const current = { scene: { title: "Рынок" } };
  assert.equal(applyStateSyncEvent(current, { kind: "scene_update", data: {} }), current);
  assert.equal(applyStateSyncEvent(current, { kind: "state_sync", data: {} }), current);
});

test("state sync sequence accepts only positive safe integers", () => {
  assert.equal(stateSyncSequence(sync(4, {})), 4);
  assert.equal(stateSyncSequence(sync(0, {})), null);
  assert.equal(stateSyncSequence(sync("bad", {})), null);
  assert.equal(stateSyncSequence({ kind: "scene_update", data: { seq: 1 } }), null);
});

test("state sync sequence deduplicates a replay and can be reset for a retry", () => {
  const seen = new Set();
  const event = sync(5, { scene: { title: "Рынок" } });
  assert.equal(acceptStateSyncEvent(seen, event), true);
  assert.equal(acceptStateSyncEvent(seen, event), false);
  seen.clear();
  assert.equal(acceptStateSyncEvent(seen, event), true);
});

test("an older state sync cannot roll the UI back after a newer sequence", () => {
  const seen = new Set();
  assert.equal(acceptStateSyncEvent(seen, sync(2, {})), true);
  assert.equal(acceptStateSyncEvent(seen, sync(1, {})), false);
  assert.equal(acceptStateSyncEvent(seen, sync(3, {})), true);
});
