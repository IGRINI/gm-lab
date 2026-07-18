const VALUE_FIELDS = Object.freeze([
  "title",
  "genre",
  "tone",
  "worldSize",
  "population",
  "publicPremise",
]);

export const WORLD_PRESETS = Object.freeze([
  Object.freeze({
    id: "machine",
    loreFields: Object.freeze([
      "world_laws",
      "inhabitants",
      "regions",
      "power_centers",
      "location_rules",
      "prohibited_elements",
    ]),
  }),
  Object.freeze({
    id: "isekai",
    loreFields: Object.freeze([
      "dogmas",
      "world_laws",
      "inhabitants",
      "regions",
      "religions",
      "gods",
      "location_rules",
      "prohibited_elements",
    ]),
  }),
  Object.freeze({
    id: "frontier",
    loreFields: Object.freeze([
      "conflicts",
      "regions",
      "power_centers",
      "economy",
      "location_rules",
      "prohibited_elements",
    ]),
  }),
]);

function translatedText(t, key, responseLanguage) {
  const value = t(key, { lng: responseLanguage });
  return typeof value === "string" ? value.trim() : "";
}

function translatedList(t, key, responseLanguage) {
  return translatedText(t, key, responseLanguage)
    .split(/\r?\n/)
    .map((value) => value.trim())
    .filter(Boolean);
}

export function localizedWorldPresetValues(preset, t, responseLanguage) {
  const baseKey = `world.presets.${preset.id}.values`;
  const values = Object.fromEntries(
    VALUE_FIELDS.map((field) => [
      field,
      translatedText(t, `${baseKey}.${field}`, responseLanguage),
    ])
  );
  values.worldLore = Object.fromEntries(
    preset.loreFields.map((field) => [
      field,
      translatedList(t, `${baseKey}.worldLore.${field}`, responseLanguage),
    ])
  );
  return values;
}
