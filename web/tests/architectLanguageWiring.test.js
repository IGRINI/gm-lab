import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const APP_PATH = new URL("../src/App.jsx", import.meta.url);

test("every architect receives the model response language independently of UI locale", async () => {
  const source = await readFile(APP_PATH, "utf8");
  for (const component of [
    "WorldArchitectPanel",
    "StoryArchitectPanel",
    "CharacterArchitectPanel",
  ]) {
    const openingTag = source.match(new RegExp(`<${component}\\b[\\s\\S]*?\\/>`))?.[0] || "";
    assert.match(openingTag, /responseLanguage=\{settings\.response_language\}/, component);
  }
});
