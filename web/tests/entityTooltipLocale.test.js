import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import { localizeEntityMetaRow, localizeEntitySubtitle } from "../src/entityTooltipLocale.js";

async function gameTranslator(language) {
  const path = new URL(`../src/locales/${language}/game.json`, import.meta.url);
  const catalog = JSON.parse(await readFile(path, "utf8"));
  return (key, options = {}) => {
    const value = key.split(".").reduce((current, part) => current?.[part], catalog);
    return typeof value === "string" ? value : options.defaultValue;
  };
}

test("entity tooltip chrome follows the UI locale while content stays authored", async () => {
  const english = await gameTranslator("en");
  const russian = await gameTranslator("ru");
  const entity = {
    subtitle: "персонаж · трактирщик",
    subtitle_key: "entity.kind.npc",
    subtitle_detail: "трактирщик",
  };
  const status = {
    label: "статус",
    label_key: "entity.meta.status",
    value: "в текущей сцене",
    value_key: "entity.status.present",
  };
  const location = {
    label: "где",
    label_key: "entity.meta.where",
    value: "Таверна",
  };
  const gender = {
    label: "род",
    label_key: "entity.meta.gender",
    value: "мужской род",
    value_key: "entity.gender.masculine",
  };
  const source = {
    label: "источник",
    label_key: "entity.meta.source",
    value: "текущая сцена",
    value_key: "entity.source.current_scene",
  };

  assert.equal(localizeEntitySubtitle(english, entity), "character · трактирщик");
  assert.equal(localizeEntitySubtitle(russian, entity), "персонаж · трактирщик");
  assert.deepEqual(localizeEntityMetaRow(english, status), {
    ...status,
    label: "status",
    value: "in the current scene",
  });
  assert.deepEqual(localizeEntityMetaRow(english, location), {
    ...location,
    label: "where",
    value: "Таверна",
  });
  assert.equal(localizeEntityMetaRow(english, gender).value, "masculine");
  assert.equal(localizeEntityMetaRow(english, source).value, "current scene");
  assert.equal(
    localizeEntityMetaRow(english, {
      value: "персонаж",
      value_key: "entity.fallback.character",
    }).value,
    "character"
  );
});

test("legacy and unknown entity semantics retain their payload fallback", async () => {
  const english = await gameTranslator("en");

  assert.equal(localizeEntitySubtitle(english, { subtitle: "старый подзаголовок" }), "старый подзаголовок");
  assert.deepEqual(
    localizeEntityMetaRow(english, {
      label: "особое поле",
      label_key: "extension.meta.custom",
      value: "авторское значение",
      value_key: "extension.value.custom",
    }),
    {
      label: "особое поле",
      label_key: "extension.meta.custom",
      value: "авторское значение",
      value_key: "extension.value.custom",
    }
  );
});
