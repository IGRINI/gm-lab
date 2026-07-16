import i18n from "i18next";
import LanguageDetector from "i18next-browser-languagedetector";
import { initReactI18next } from "react-i18next";
import { DEFAULT_LANGUAGE } from "./catalog.js";
import { availableLanguages, localeCatalog } from "./localeCatalog.js";
import { installRuntimeTranslator } from "./runtime.js";

export const UI_LANGUAGE_STORAGE_KEY = "gmlab.uiLanguage";

const supportedLanguages = availableLanguages.map((locale) => locale.code);

export function resolveUiLanguage(rawLanguage) {
  const value = String(rawLanguage || "").trim();
  if (supportedLanguages.includes(value)) return value;
  const valueLower = value.toLowerCase();
  const exact = supportedLanguages.find((language) => language.toLowerCase() === valueLower);
  if (exact) return exact;
  const base = valueLower.split("-")[0];
  return supportedLanguages.find((language) => language.toLowerCase() === base) || DEFAULT_LANGUAGE;
}

function updateDocumentLocale(rawLanguage) {
  if (typeof document === "undefined") return;
  const language = resolveUiLanguage(rawLanguage);
  const locale = availableLanguages.find((item) => item.code === language);
  document.documentElement.lang = language;
  document.documentElement.dir = locale?.dir || "ltr";
}

i18n
  .use(LanguageDetector)
  .use(initReactI18next)
  .init({
    resources: localeCatalog.resources,
    supportedLngs: supportedLanguages,
    fallbackLng: DEFAULT_LANGUAGE,
    defaultNS: "common",
    ns: localeCatalog.namespaces,
    nonExplicitSupportedLngs: true,
    load: "all",
    cleanCode: true,
    initImmediate: false,
    interpolation: { escapeValue: false },
    react: { useSuspense: false },
    detection: {
      order: ["localStorage", "navigator"],
      lookupLocalStorage: UI_LANGUAGE_STORAGE_KEY,
      caches: ["localStorage"],
    },
  });

installRuntimeTranslator(i18n);

updateDocumentLocale(i18n.resolvedLanguage || i18n.language);
i18n.on("languageChanged", updateDocumentLocale);

export async function setUiLanguage(language) {
  return i18n.changeLanguage(resolveUiLanguage(language));
}

export { availableLanguages, DEFAULT_LANGUAGE };
export default i18n;
