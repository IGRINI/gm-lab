import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import {
  createServerMessageError,
  localizeServerMessage,
  serverMessageDetail,
} from "../src/serverMessages.js";

async function localeTranslator(language) {
  const url = new URL(`../src/locales/${language}/server.json`, import.meta.url);
  const messages = JSON.parse(await readFile(url, "utf8"));
  return (key, params = {}) => {
    const path = key.replace(/^server:/, "").split(".");
    const template = path.reduce((value, part) => value?.[part], messages);
    assert.equal(typeof template, "string", `missing translation: ${key}`);
    return template.replace(/{{\s*([^},\s]+)[^}]*}}/g, (_, name) => String(params[name] ?? ""));
  };
}

test("structured server messages use codes and top-level interpolation params", async () => {
  const t = await localeTranslator("en");
  const message = localizeServerMessage(
    { code: "world_version_drift", authored_version: 2, live_version: 5 },
    t
  );

  assert.match(message, /v2/);
  assert.match(message, /v5/);
});

test("structured server messages support nested payloads and params", async () => {
  const t = await localeTranslator("ru");
  const error = createServerMessageError({
    error: {
      code: "world_version_drift",
      params: { authored_version: 3, live_version: 4 },
      detail: "database internals",
    },
  });

  assert.match(localizeServerMessage(error, t), /v3/);
  assert.match(localizeServerMessage(error, t), /v4/);
  assert.equal(error.message, "Server request failed");
  assert.equal(serverMessageDetail(error), "database internals");
});

test("legacy Russian server errors are localized without exposing raw detail", async () => {
  const t = await localeTranslator("en");
  const raw = "Ошибка вызова модели: provider secret detail";
  const message = localizeServerMessage(raw, t);

  assert.equal(message, "The model did not return a response.");
  assert.doesNotMatch(message, /provider secret detail/);
  assert.equal(serverMessageDetail(raw), raw);
});

test("legacy architect errors map to their user-safe category", async () => {
  const t = await localeTranslator("en");

  assert.equal(
    localizeServerMessage("не удалось сохранить черновик мира", t),
    "The world could not be saved."
  );
  assert.equal(
    localizeServerMessage("не удалось загрузить переписку архитектора: sqlite detail", t),
    "The architect conversation could not be loaded."
  );
  assert.equal(
    localizeServerMessage("ход выполнен, но переписка не сохранилась: disk detail", t),
    "The architect conversation could not be saved."
  );
});

test("legacy media and credential errors map to localized codes", async () => {
  const t = await localeTranslator("en");

  assert.equal(
    localizeServerMessage("Сначала сохрани OpenAI API-ключ.", t),
    "This model requires an API key."
  );
  assert.equal(
    localizeServerMessage("TTS-сервис недоступен: connection refused", t),
    "Text-to-speech is currently unavailable."
  );
});

test("architect wildcard codes resolve to a stable translated category", async () => {
  const t = await localeTranslator("en");

  assert.equal(
    localizeServerMessage({ code: "architect_world_failed" }, t),
    "The world could not be saved."
  );
  assert.equal(
    localizeServerMessage({ code: "architect_provider_timeout_failed" }, t),
    "The architect could not complete the request."
  );
});

test("unknown server details use a localized fallback", async () => {
  const t = await localeTranslator("ru");
  const raw = "driver failed with private filesystem path";

  assert.equal(localizeServerMessage(raw, t), "Не удалось выполнить действие.");
  assert.equal(
    localizeServerMessage(raw, t, { fallbackCode: "architect_turn_failed" }),
    "Архитектор не смог выполнить запрос."
  );
  assert.equal(serverMessageDetail(raw), raw);
});
