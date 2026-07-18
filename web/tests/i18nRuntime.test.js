import assert from "node:assert/strict";
import test from "node:test";

import { installRuntimeTranslator, runtimeText } from "../src/i18n/runtime.js";

test("runtime translations are safe before the browser catalog is initialized", () => {
  assert.equal(
    runtimeText("app:api.turnFailed", { defaultValue: "The turn failed" }),
    "The turn failed"
  );
});

test("runtime translations delegate to the installed i18next instance", () => {
  installRuntimeTranslator({
    t(key, options) {
      return `${key}:${options.status}`;
    },
  });

  assert.equal(
    runtimeText("app:api.exportFailed", { defaultValue: "fallback", status: 503 }),
    "app:api.exportFailed:503"
  );
});
