import assert from "node:assert/strict";
import test from "node:test";

import { createTimeline } from "../src/timelineStore.js";
import { historicalFailedTurn, isTerminalTurnError } from "../src/turnRetry.js";

globalThis.requestAnimationFrame = (callback) => {
  callback();
  return 1;
};

const player = (id, text = "Осматриваюсь") => ({ id, type: "player", text });
const error = (id, text, agent = "ГМ") => ({ id, type: "error", agent, text });
const usage = (id, overrides = {}) => ({
  id,
  type: "meta_total",
  data: {
    calls: [],
    in: 0,
    out: 0,
    cached: 0,
    tokens: 0,
    peak_context: 0,
    secs: 0.77,
    ...overrides,
  },
});
const safeTail = () => [
  player(1),
  error(2, "Ошибка вызова модели: SuperGrok API error 503: Connection refused"),
  usage(3),
];

test("restores a legacy resume only for the exact persisted model-error tail", () => {
  assert.deepEqual(historicalFailedTurn(safeTail(), "chat-1"), {
    chatId: "chat-1",
    text: "Осматриваюсь",
    requestId: "",
    errorId: 2,
    legacyResume: true,
  });
});

test("recognizes only a GM model-call failure", () => {
  assert.equal(isTerminalTurnError(error(1, "Ошибка вызова модели: timeout")), true);
  assert.equal(
    isTerminalTurnError(error(1, "Превышен лимит вызовов инструментов за ход: 12.")),
    false
  );
  assert.equal(
    isTerminalTurnError(
      error(1, "Модель завершила ход без ask_player, хотя варианты игрока включены.")
    ),
    false
  );
  assert.equal(isTerminalTurnError(error(1, "Ошибка вызова модели: timeout", "NPC")), false);
});

test("rejects tails with activity between or after the three legacy rows", () => {
  const [playerRow, errorRow, usageRow] = safeTail();
  assert.equal(
    historicalFailedTurn(
      [playerRow, { id: 4, type: "tool", name: "roll_dice" }, errorRow, usageRow],
      "chat-1"
    ),
    null
  );
  assert.equal(
    historicalFailedTurn([...safeTail(), { id: 4, type: "meta", data: {} }], "chat-1"),
    null
  );
  assert.equal(
    historicalFailedTurn([playerRow, errorRow, { id: 4, type: "narration", text: "Дальше" }], "chat-1"),
    null
  );
});

test("rejects any legacy tail that may have consumed a model call or tokens", () => {
  const unsafeOverrides = [
    { calls: [{ scope: "gm" }] },
    { in: 1 },
    { out: 1 },
    { cached: 1 },
    { tokens: 1 },
    { peak_context: 1 },
    { tokens: "0" },
  ];

  for (const overrides of unsafeOverrides) {
    const [playerRow, errorRow] = safeTail();
    assert.equal(historicalFailedTurn([playerRow, errorRow, usage(3, overrides)], "chat-1"), null);
  }

  for (const field of ["in", "out", "cached", "tokens", "peak_context"]) {
    const data = usage(3).data;
    delete data[field];
    const [playerRow, errorRow] = safeTail();
    assert.equal(
      historicalFailedTurn([playerRow, errorRow, { id: 3, type: "meta_total", data }], "chat-1"),
      null
    );
  }
});

test("rejects a blank action, absent chat, wrong metadata, and non-model errors", () => {
  assert.equal(historicalFailedTurn(safeTail(), ""), null);
  assert.equal(historicalFailedTurn([player(1, "  "), safeTail()[1], usage(3)], "chat-1"), null);
  assert.equal(
    historicalFailedTurn([player(1), error(2, "Ошибка NPC: timeout"), usage(3)], "chat-1"),
    null
  );
  assert.equal(
    historicalFailedTurn([player(1), safeTail()[1], { id: 3, type: "meta_total", data: {} }], "chat-1"),
    null
  );
});

test("a failed resume rolls back to one historical tail with a working retry", () => {
  const store = createTimeline();
  const persisted = [
    { kind: "player", agent: "Игрок", data: "Осматриваюсь" },
    { kind: "error", agent: "ГМ", data: "Ошибка вызова модели: old failure" },
    { kind: "meta_total", data: usage(3).data },
  ];
  store.dispatchMany(persisted);
  const before = store.getSnapshot();

  store.beginTurn();
  store.dispatch({ kind: "error", agent: "ГМ", data: "Ошибка вызова модели: retry failure" });
  store.dispatch({ kind: "meta_total", data: usage(4).data });
  assert.equal(store.rollbackTurn(), true);

  assert.deepEqual(store.getSnapshot(), before);
  assert.equal(historicalFailedTurn(store.getSnapshot(), "chat-1")?.legacyResume, true);
});

test("a successful resume replaces the old tail with one canonical player row", () => {
  const store = createTimeline();
  store.dispatchMany([
    { kind: "player", agent: "Игрок", data: "Осматриваюсь" },
    { kind: "error", agent: "ГМ", data: "Ошибка вызова модели: old failure" },
    { kind: "meta_total", data: usage(3).data },
  ]);
  store.beginTurn();
  store.dispatch({ kind: "gm_narration", agent: "ГМ", data: "Временный поток" });

  store.clear();
  store.dispatchMany([
    { kind: "player", agent: "Игрок", data: "Осматриваюсь" },
    { kind: "gm_narration", agent: "ГМ", data: "Комната пуста" },
    { kind: "meta_total", data: { ...usage(3).data, out: 7, tokens: 7 } },
  ]);

  const messages = store.getSnapshot();
  assert.equal(messages.filter((message) => message.type === "player").length, 1);
  assert.equal(messages.some((message) => message.type === "error"), false);
  assert.equal(historicalFailedTurn(messages, "chat-1"), null);
});
