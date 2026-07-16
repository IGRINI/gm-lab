let translate = (key, options = {}) => options.defaultValue ?? key;

// Install the initialized i18next instance without forcing framework-neutral
// modules to import the Vite locale loader. Node tests therefore keep working
// while browser-side errors and catalog fallbacks use the active UI language.
export function installRuntimeTranslator(i18n) {
  translate = (key, options = {}) => i18n.t(key, options);
}

export function runtimeText(key, options = {}) {
  return translate(key, options);
}
