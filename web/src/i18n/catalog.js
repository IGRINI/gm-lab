export const DEFAULT_LANGUAGE = "en";

const LOCALE_PATH_RE = /(?:^|\/)locales\/([^/]+)\/([^/]+)\.json$/;
const LANGUAGE_CODE_RE = /^[a-z]{2,8}(?:-[A-Za-z0-9]{1,8})*$/;
const NAMESPACE_RE = /^[a-z][a-z0-9_-]*$/;
const PLURAL_SUFFIX_RE = /^(.*)_(zero|one|two|few|many|other)$/;

function isPlainObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function isValidLanguageCode(value) {
  if (!LANGUAGE_CODE_RE.test(value)) return false;
  try {
    return Intl.getCanonicalLocales(value).length === 1;
  } catch {
    return false;
  }
}

function localeFile(path) {
  const normalized = String(path || "").replaceAll("\\", "/");
  const match = normalized.match(LOCALE_PATH_RE);
  if (!match) {
    throw new Error(`Invalid locale path: ${path}`);
  }
  return { language: match[1], namespace: match[2] };
}

function translationEntries(value, prefix = "", out = new Map()) {
  if (!isPlainObject(value)) {
    if (prefix) out.set(prefix, value);
    return out;
  }
  for (const [key, child] of Object.entries(value)) {
    translationEntries(child, prefix ? `${prefix}.${key}` : key, out);
  }
  return out;
}

function translationGroups(value) {
  const entries = translationEntries(value);
  const pluralBases = new Set();
  for (const key of entries.keys()) {
    const match = key.match(PLURAL_SUFFIX_RE);
    if (match?.[2] === "other") pluralBases.add(match[1]);
  }

  const groups = new Map();
  for (const [key, translation] of entries) {
    const match = key.match(PLURAL_SUFFIX_RE);
    const plural = Boolean(match && pluralBases.has(match[1]));
    const groupKey = plural ? match[1] : key;
    const category = plural ? match[2] : "";
    if (!groups.has(groupKey)) groups.set(groupKey, { plural, values: new Map() });
    groups.get(groupKey).values.set(category, translation);
  }
  return groups;
}

function pluralCategories(language) {
  return new Intl.PluralRules(language).resolvedOptions().pluralCategories;
}

function interpolationVariables(value) {
  if (typeof value !== "string") return [];
  const variables = new Set();
  for (const match of value.matchAll(/{{\s*([^},\s]+)[^}]*}}/g)) {
    variables.add(match[1]);
  }
  return [...variables].sort();
}

function localeSort(a, b) {
  if (a.code === DEFAULT_LANGUAGE) return -1;
  if (b.code === DEFAULT_LANGUAGE) return 1;
  return a.name.localeCompare(b.name, undefined, { sensitivity: "base" });
}

export function buildLocaleCatalog(modules) {
  if (!isPlainObject(modules) || Object.keys(modules).length === 0) {
    throw new Error("No locale files found");
  }

  const resources = {};
  const metadata = {};
  const namespaces = new Set();

  for (const [path, payload] of Object.entries(modules)) {
    const { language, namespace } = localeFile(path);
    if (!isValidLanguageCode(language)) {
      throw new Error(`Invalid language code '${language}' in ${path}`);
    }
    if (!NAMESPACE_RE.test(namespace)) {
      throw new Error(`Invalid locale namespace '${namespace}' in ${path}`);
    }
    if (!isPlainObject(payload)) {
      throw new Error(`Locale file must contain a JSON object: ${path}`);
    }

    if (namespace === "meta") {
      if (metadata[language]) throw new Error(`Duplicate metadata for '${language}'`);
      if (payload.code !== language) {
        throw new Error(`Locale metadata code '${payload.code}' does not match folder '${language}'`);
      }
      if (typeof payload.name !== "string" || !payload.name.trim()) {
        throw new Error(`Locale '${language}' has no display name`);
      }
      if (payload.dir !== "ltr" && payload.dir !== "rtl") {
        throw new Error(`Locale '${language}' must declare dir as 'ltr' or 'rtl'`);
      }
      metadata[language] = Object.freeze({
        code: language,
        name: payload.name.trim(),
        dir: payload.dir,
      });
      continue;
    }

    resources[language] ||= {};
    if (resources[language][namespace]) {
      throw new Error(`Duplicate namespace '${namespace}' for '${language}'`);
    }
    resources[language][namespace] = payload;
    namespaces.add(namespace);
  }

  const resourceLanguages = Object.keys(resources);
  for (const language of resourceLanguages) {
    if (!metadata[language]) throw new Error(`Locale '${language}' has no meta.json`);
  }
  for (const language of Object.keys(metadata)) {
    if (!resources[language] || Object.keys(resources[language]).length === 0) {
      throw new Error(`Locale '${language}' has no translation namespaces`);
    }
  }
  if (!metadata[DEFAULT_LANGUAGE] || !resources[DEFAULT_LANGUAGE]) {
    throw new Error(`Default locale '${DEFAULT_LANGUAGE}' is missing`);
  }

  return Object.freeze({
    resources,
    languages: Object.values(metadata).sort(localeSort),
    namespaces: [...namespaces].sort(),
  });
}

export function assertLocaleParity(catalog, referenceLanguage = DEFAULT_LANGUAGE) {
  const reference = catalog?.resources?.[referenceLanguage];
  if (!reference) throw new Error(`Reference locale '${referenceLanguage}' is missing`);

  const referenceNamespaces = Object.keys(reference).sort();
  for (const language of catalog.languages.map((locale) => locale.code)) {
    if (language === referenceLanguage) continue;
    const candidate = catalog.resources[language] || {};
    const candidateNamespaces = Object.keys(candidate).sort();
    const missingNamespaces = referenceNamespaces.filter((name) => !candidateNamespaces.includes(name));
    const extraNamespaces = candidateNamespaces.filter((name) => !referenceNamespaces.includes(name));
    const problems = [];
    if (missingNamespaces.length) problems.push(`missing namespaces: ${missingNamespaces.join(", ")}`);
    if (extraNamespaces.length) problems.push(`extra namespaces: ${extraNamespaces.join(", ")}`);

    for (const namespace of referenceNamespaces) {
      if (!candidate[namespace]) continue;
      const expectedGroups = translationGroups(reference[namespace]);
      const actualGroups = translationGroups(candidate[namespace]);
      const expected = new Set(expectedGroups.keys());
      const actual = new Set(actualGroups.keys());
      const missing = [...expected].filter((key) => !actual.has(key));
      const extra = [...actual].filter((key) => !expected.has(key));
      if (missing.length) problems.push(`${namespace} missing keys: ${missing.join(", ")}`);
      if (extra.length) problems.push(`${namespace} extra keys: ${extra.join(", ")}`);

      for (const key of expected) {
        if (!actual.has(key)) continue;
        const expectedGroup = expectedGroups.get(key);
        const actualGroup = actualGroups.get(key);
        if (expectedGroup.plural !== actualGroup.plural) {
          problems.push(`${namespace}.${key} plural structure differs`);
          continue;
        }

        if (actualGroup.plural) {
          const missingForms = pluralCategories(language).filter(
            (category) => !actualGroup.values.has(category)
          );
          if (missingForms.length) {
            problems.push(`${namespace}.${key} missing plural forms: ${missingForms.join(", ")}`);
          }
        }

        const expectedValue = expectedGroup.values.get(expectedGroup.plural ? "other" : "");
        const expectedVariables = interpolationVariables(expectedValue);
        for (const [category, actualValue] of actualGroup.values) {
          const actualVariables = interpolationVariables(actualValue);
          if (expectedVariables.join("\0") !== actualVariables.join("\0")) {
            const variant = category ? `_${category}` : "";
            problems.push(
              `${namespace}.${key}${variant} interpolation differs: ` +
                `expected [${expectedVariables.join(", ")}], got [${actualVariables.join(", ")}]`
            );
          }
        }
      }
    }

    if (problems.length) {
      throw new Error(`Locale '${language}' differs from '${referenceLanguage}': ${problems.join("; ")}`);
    }
  }
  return true;
}
