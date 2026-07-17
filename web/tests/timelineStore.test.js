import assert from "node:assert/strict";
import test from "node:test";

import { createTimeline } from "../src/timelineStore.js";

globalThis.requestAnimationFrame = (callback) => {
  callback();
  return 1;
};

test("rollbackTurn restores the exact pre-turn timeline and id sequence", () => {
  const store = createTimeline();
  store.dispatchMany([{ kind: "player", data: "Старый ход" }]);
  const before = store.getSnapshot();

  store.beginTurn();
  store.dispatch({ kind: "player", data: "Незавершённый ход" });
  store.dispatch({ kind: "error", agent: "ГМ", data: "Временная ошибка" });

  assert.equal(store.rollbackTurn(), true);
  assert.deepEqual(store.getSnapshot(), before);

  store.pushLocal({ type: "command", text: "После отката" });
  assert.equal(store.getSnapshot().at(-1).id, 2);
});

test("commitTurn keeps streamed events and closes the checkpoint", () => {
  const store = createTimeline();
  store.beginTurn();
  store.dispatch({ kind: "player", data: "Успешный ход" });

  assert.equal(store.commitTurn(), true);
  assert.equal(store.rollbackTurn(), false);
  assert.equal(store.getSnapshot().at(-1).text, "Успешный ход");
});

test("beginTurn refuses to replace an active checkpoint", () => {
  const store = createTimeline();
  store.beginTurn();

  assert.throws(() => store.beginTurn(), /already active/);
  assert.equal(store.rollbackTurn(), true);
});

test("clear removes an active turn and resets message ids", () => {
  const store = createTimeline();
  store.pushLocal({ type: "command", text: "До очистки" });
  store.beginTurn();
  store.dispatch({ kind: "player", data: "Черновик" });

  store.clear();

  assert.deepEqual(store.getSnapshot(), []);
  assert.equal(store.rollbackTurn(), false);
  store.pushLocal({ type: "command", text: "После очистки" });
  assert.equal(store.getSnapshot()[0].id, 1);
});

test("canonical restore operations discard an active checkpoint", () => {
  const cleared = createTimeline();
  cleared.beginTurn();
  cleared.dispatch({ kind: "player", data: "Черновик" });
  cleared.clear();
  assert.equal(cleared.rollbackTurn(), false);

  const restored = createTimeline();
  restored.beginTurn();
  restored.dispatch({ kind: "player", data: "Черновик" });
  restored.dispatchMany([{ kind: "player", data: "Канон" }]);
  assert.equal(restored.rollbackTurn(), false);
});

test("restored player rows preserve turn rollback metadata", () => {
  const store = createTimeline();
  store.dispatchMany([
    { kind: "player", data: "Старый ход", turn: 4, rewindable: false },
    { kind: "player", data: "Доступный ход", turn: 5, rewindable: true },
  ]);

  assert.deepEqual(
    store.getSnapshot().map(({ text, turn, rewindable }) => ({ text, turn, rewindable })),
    [
      { text: "Старый ход", turn: 4, rewindable: false },
      { text: "Доступный ход", turn: 5, rewindable: true },
    ]
  );
});

test("a terminal receipt can enable the latest streamed player row", () => {
  const store = createTimeline();
  store.beginTurn();
  store.dispatch({ kind: "player", data: "Новый ход" });

  assert.equal(store.markLatestPlayerRewindable(7), true);
  assert.deepEqual(
    (({ text, turn, rewindable }) => ({ text, turn, rewindable }))(store.getSnapshot()[0]),
    { text: "Новый ход", turn: 7, rewindable: true }
  );
});

test("staged history mutation shows only the prefix before the selected turn", () => {
  const store = createTimeline();
  store.dispatchMany([
    { kind: "player", data: "Первый ход", turn: 1, rewindable: true },
    { kind: "gm_narration", data: "Первый ответ", sid: "gm-1" },
    { kind: "player", data: "Второй ход", turn: 2, rewindable: true },
    { kind: "gm_narration", data: "Второй ответ", sid: "gm-2" },
  ]);

  assert.equal(store.truncateFromPlayerTurn(2), true);
  assert.deepEqual(
    store.getSnapshot().map((message) => message.text),
    ["Первый ход", "Первый ответ"]
  );
  assert.equal(store.truncateFromPlayerTurn(99), false);
});

test("generic character update attaches to its tool call", () => {
  const store = createTimeline();
  const payload = {
    target: "npc",
    npc_id: "liza",
    label: "Лиза",
    updated: ["current_appearance"],
  };

  store.dispatchMany([
    {
      kind: "gm_tool_call",
      data: {
        name: "update_character",
        arguments: { target: "npc", npc_id: "liza", fields: { current_appearance: "Фартук в муке" } },
      },
    },
    { kind: "character_update", data: payload },
  ]);

  const messages = store.getSnapshot();
  assert.equal(messages.length, 1);
  assert.equal(messages[0].name, "update_character");
  assert.deepEqual(messages[0].result, payload);
});

test("legacy player character update remains compatible", () => {
  const store = createTimeline();
  const payload = { updated: ["condition"] };

  store.dispatchMany([
    {
      kind: "gm_tool_call",
      data: {
        name: "update_player_character",
        arguments: { fields: { condition: "Усталость" } },
      },
    },
    { kind: "player_character_update", data: payload },
  ]);

  const messages = store.getSnapshot();
  assert.equal(messages.length, 1);
  assert.equal(messages[0].name, "update_player_character");
  assert.deepEqual(messages[0].result, payload);
});

test("new character event can finish a restored legacy tool call", () => {
  const store = createTimeline();
  const payload = { target: "player", updated: ["current_appearance"] };

  store.dispatchMany([
    {
      kind: "gm_tool_call",
      data: {
        name: "update_player_character",
        arguments: { fields: { current_appearance: "Дорожный плащ" } },
      },
    },
    { kind: "character_update", data: payload },
  ]);

  const messages = store.getSnapshot();
  assert.equal(messages.length, 1);
  assert.equal(messages[0].name, "update_player_character");
  assert.deepEqual(messages[0].result, payload);
});

test("scene updates preserve the location snapshot and image for the timeline card", () => {
  const store = createTimeline();
  const scene = {
    scene_id: "scene-1",
    location_id: "place-1",
    title: "Сырая келья",
    description: "Тесная влажная келья.",
    image_url: "/image-files/run/location.png",
    present_npcs: ["liza"],
    items: [{ item_id: "chain", name: "Цепь" }],
    exits: [{ exit_id: "deeper", name: "Дальний проём" }],
  };

  store.dispatch({ kind: "scene_update", data: scene });

  const [message] = store.getSnapshot();
  assert.equal(message.type, "scene_update");
  assert.equal(message.location_id, "place-1");
  assert.equal(message.image_url, "/image-files/run/location.png");
  assert.deepEqual(message.scene, scene);
});
