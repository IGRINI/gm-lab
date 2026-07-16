import { assertLocaleParity, buildLocaleCatalog } from "./catalog.js";

const localeModules = import.meta.glob("../locales/*/*.json", {
  eager: true,
  import: "default",
});

export const localeCatalog = buildLocaleCatalog(localeModules);
assertLocaleParity(localeCatalog);

export const availableLanguages = localeCatalog.languages;
