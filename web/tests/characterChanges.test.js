import assert from "node:assert/strict";
import test from "node:test";

import {
  formatCharacterChangeValue,
  normalizeCharacterChanges,
} from "../src/characterChanges.js";

const t = (_key, options = {}) => options.defaultValue || _key;

test("character changes preserve explicit before/after/add/remove", () => {
  const [change] = normalizeCharacterChanges({
    changes: [{
      field: "inventory",
      before: ["Факел"],
      after: ["Верёвка"],
      added: ["Верёвка"],
      removed: ["Факел"],
    }],
  });
  assert.deepEqual(change.added, ["Верёвка"]);
  assert.deepEqual(change.removed, ["Факел"]);
  assert.deepEqual(change.before, ["Факел"]);
  assert.deepEqual(change.after, ["Верёвка"]);
});

test("array add/remove is derived from before and after when omitted", () => {
  const [change] = normalizeCharacterChanges({
    changes: [{
      field: "inventory",
      before: ["Факел", "Факел", "Мел"],
      after: ["Факел", "Мел", "Верёвка"],
    }],
  });
  assert.deepEqual(change.added, ["Верёвка"]);
  assert.deepEqual(change.removed, ["Факел"]);
});

test("change values format scalars, hp and lists for a compact card", () => {
  assert.equal(formatCharacterChangeValue({ current: 7, max: 16 }, t), "7 / 16");
  assert.equal(formatCharacterChangeValue(["Факел", "Верёвка"], t), "Факел, Верёвка");
  assert.equal(formatCharacterChangeValue(null, t), "—");
});

test("internal card revision is never shown as a character change", () => {
  assert.deepEqual(normalizeCharacterChanges({
    changes: [{ field: "card_revision", before: 1, after: 2 }],
  }), []);
});
