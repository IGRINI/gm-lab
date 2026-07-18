import assert from "node:assert/strict";
import { readdir, readFile } from "node:fs/promises";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { assertLocaleParity, buildLocaleCatalog } from "../src/i18n/catalog.js";

const WEB_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const LOCALES_ROOT = path.join(WEB_ROOT, "src", "locales");

async function readLocaleModules() {
  const modules = {};
  const languages = await readdir(LOCALES_ROOT, { withFileTypes: true });
  for (const language of languages.filter((entry) => entry.isDirectory())) {
    const localeRoot = path.join(LOCALES_ROOT, language.name);
    const files = await readdir(localeRoot, { withFileTypes: true });
    for (const file of files.filter((entry) => entry.isFile() && entry.name.endsWith(".json"))) {
      const content = await readFile(path.join(localeRoot, file.name), "utf8");
      modules[`../locales/${language.name}/${file.name}`] = JSON.parse(content);
    }
  }
  return modules;
}

test("locale folders build an ordered catalog with matching keys", async () => {
  const catalog = buildLocaleCatalog(await readLocaleModules());

  const languageCodes = catalog.languages.map((locale) => locale.code);
  assert.equal(languageCodes[0], "en");
  assert.ok(languageCodes.includes("ru"));
  assert.equal(new Set(languageCodes).size, languageCodes.length);
  assert.deepEqual(catalog.namespaces, Object.keys(catalog.resources.en).sort());
  assert.ok(catalog.namespaces.includes("common"));
  assert.ok(catalog.namespaces.includes("server"));
  assert.ok(catalog.namespaces.includes("settings"));
  assert.equal(assertLocaleParity(catalog), true);
});

test("locale parity reports a missing translation key", () => {
  const modules = {
    "../locales/en/meta.json": { code: "en", name: "English", dir: "ltr" },
    "../locales/en/common.json": { action: { save: "Save" } },
    "../locales/ru/meta.json": { code: "ru", name: "Русский", dir: "ltr" },
    "../locales/ru/common.json": { action: {} },
  };

  assert.throws(
    () => assertLocaleParity(buildLocaleCatalog(modules)),
    /common missing keys: action\.save/
  );
});

test("locale folder and metadata codes must match", () => {
  const modules = {
    "../locales/ru/meta.json": { code: "en", name: "Русский", dir: "ltr" },
    "../locales/ru/common.json": { action: "Сохранить" },
  };

  assert.throws(() => buildLocaleCatalog(modules), /does not match folder 'ru'/);
});

test("locale folders must use valid BCP-47 language tags", () => {
  const modules = {
    "../locales/en-a/meta.json": { code: "en-a", name: "Invalid", dir: "ltr" },
    "../locales/en-a/common.json": { action: "Save" },
  };

  assert.throws(() => buildLocaleCatalog(modules), /Invalid language code 'en-a'/);
});

test("locale parity accepts and validates language-specific plural forms", () => {
  const modules = {
    "../locales/en/meta.json": { code: "en", name: "English", dir: "ltr" },
    "../locales/en/common.json": {
      turns_one: "{{count}} turn",
      turns_other: "{{count}} turns",
    },
    "../locales/ar/meta.json": { code: "ar", name: "العربية", dir: "rtl" },
    "../locales/ar/common.json": {
      turns_zero: "{{count}} دور",
      turns_one: "{{count}} دور",
      turns_two: "{{count}} دوران",
      turns_few: "{{count}} أدوار",
      turns_many: "{{count}} دورًا",
      turns_other: "{{count}} دور",
    },
  };

  const catalog = buildLocaleCatalog(modules);
  assert.equal(assertLocaleParity(catalog), true);

  delete modules["../locales/ar/common.json"].turns_two;
  assert.throws(
    () => assertLocaleParity(buildLocaleCatalog(modules)),
    /common\.turns missing plural forms: two/
  );
});

test("locale parity reports changed interpolation variables", () => {
  const modules = {
    "../locales/ru/meta.json": { code: "ru", name: "Русский", dir: "ltr" },
    "../locales/ru/common.json": { hello: "Привет, {{name}}" },
    "../locales/en/meta.json": { code: "en", name: "English", dir: "ltr" },
    "../locales/en/common.json": { hello: "Hello, {{person}}" },
  };

  assert.throws(
    () => assertLocaleParity(buildLocaleCatalog(modules)),
    /common\.hello interpolation differs/
  );
});
