import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import {
  localizedWorldPresetValues,
  WORLD_PRESETS,
} from "../src/components/worldPresets.js";

async function loadStudioLocale(language) {
  const url = new URL(`../src/locales/${language}/studio.json`, import.meta.url);
  return JSON.parse(await readFile(url, "utf8"));
}

function valueAtPath(value, path) {
  return path.split(".").reduce((current, key) => current?.[key], value);
}

function translator(resources, interfaceLanguage) {
  return (key, options = {}) => valueAtPath(
    resources[options.lng || interfaceLanguage],
    key
  );
}

test("world preset contents follow the response language, not the interface language", async () => {
  const [ru, en] = await Promise.all([loadStudioLocale("ru"), loadStudioLocale("en")]);
  const t = translator({ ru, en }, "ru");

  assert.deepEqual(en.world.defaults, {
    title: "New world",
    genre: "fantasy",
    tone: "tense",
  });
  assert.deepEqual(ru.world.defaults, {
    title: "Новый мир",
    genre: "фэнтези",
    tone: "напряжённый",
  });

  for (const preset of WORLD_PRESETS) {
    const english = localizedWorldPresetValues(preset, t, "en");
    const russian = localizedWorldPresetValues(preset, t, "ru");

    assert.doesNotMatch(JSON.stringify(english), /[А-Яа-яЁё]/, `${preset.id} must be English`);
    assert.match(JSON.stringify(russian), /[А-Яа-яЁё]/, `${preset.id} must be Russian`);
    assert.notDeepEqual(english, russian);
    assert.ok(english.title);
    assert.ok(english.genre);
    assert.ok(english.tone);
    assert.ok(english.worldSize);
    assert.ok(english.population);
    assert.ok(english.publicPremise);
    assert.deepEqual(Object.keys(english.worldLore), [...preset.loreFields]);
    assert.equal(
      Object.values(english.worldLore).every(
        (items) => Array.isArray(items) && items.length > 0 && items.every(Boolean)
      ),
      true
    );
  }
});

test("world preset lists are restored from localized newline-separated values", async () => {
  const en = await loadStudioLocale("en");
  const t = translator({ en }, "en");
  const machine = WORLD_PRESETS.find((preset) => preset.id === "machine");
  const values = localizedWorldPresetValues(machine, t, "en");

  assert.deepEqual(values.worldLore.world_laws, [
    "Old machines follow protocols, not morality.",
    "Water, energy, spare parts, and hub access matter more than coins.",
  ]);
});
