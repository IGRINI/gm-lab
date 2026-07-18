import assert from "node:assert/strict";
import test from "node:test";

import { localizeStatusLabel } from "../src/statusContext.js";

test("known whereabouts statuses use the active UI translation", () => {
  const t = (key, options = {}) =>
    key === "status.labels.known" ? "known" : options.defaultValue ?? key;

  assert.equal(localizeStatusLabel(t, "known", { known: "известно" }), "known");
});

test("custom whereabouts statuses keep the backend compatibility label", () => {
  const t = (_key, options = {}) => options.defaultValue;

  assert.equal(localizeStatusLabel(t, "missing", { missing: "off map" }), "off map");
});
