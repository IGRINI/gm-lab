import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

async function loadLocale(language) {
  const url = new URL(`../src/locales/${language}/game.json`, import.meta.url);
  return JSON.parse(await readFile(url, "utf8"));
}

function flatten(value, prefix = "", out = new Map()) {
  for (const [key, child] of Object.entries(value)) {
    const path = prefix ? `${prefix}.${key}` : key;
    if (child && typeof child === "object" && !Array.isArray(child)) flatten(child, path, out);
    else out.set(path, child);
  }
  return out;
}

function interpolationNames(value) {
  return [...String(value).matchAll(/{{\s*([^},\s]+)[^}]*}}/g)]
    .map((match) => match[1])
    .sort();
}

test("game locale files have matching keys and interpolation contracts", async () => {
  const [ru, en] = await Promise.all([loadLocale("ru"), loadLocale("en")]);
  const ruFlat = flatten(ru);
  const enFlat = flatten(en);

  assert.deepEqual([...enFlat.keys()].sort(), [...ruFlat.keys()].sort());
  for (const [key, ruValue] of ruFlat) {
    assert.equal(typeof ruValue, "string", `${key} must be a string`);
    assert.equal(typeof enFlat.get(key), "string", `${key} must be translated`);
    assert.deepEqual(
      interpolationNames(enFlat.get(key)),
      interpolationNames(ruValue),
      `${key} must use the same interpolation variables`
    );
  }
});
