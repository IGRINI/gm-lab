const ENTITY_UI_TRANSLATION_KEYS = Object.freeze({
  "entity.kind.npc": "markdown.entityKinds.npc",
  "entity.kind.loc": "markdown.entityKinds.loc",
  "entity.kind.item": "markdown.entityKinds.item",
  "entity.kind.note": "markdown.entityKinds.note",
  "entity.meta.role": "markdown.entityMeta.role",
  "entity.meta.status": "markdown.entityMeta.status",
  "entity.meta.gender": "markdown.entityMeta.gender",
  "entity.meta.where": "markdown.entityMeta.where",
  "entity.meta.activity": "markdown.entityMeta.activity",
  "entity.meta.present_characters": "markdown.entityMeta.presentCharacters",
  "entity.meta.exits": "markdown.entityMeta.exits",
  "entity.meta.via": "markdown.entityMeta.via",
  "entity.meta.source": "markdown.entityMeta.source",
  "entity.status.present": "scene.statuses.present",
  "entity.status.known": "scene.statuses.known",
  "entity.status.likely": "scene.statuses.likely",
  "entity.status.rumored": "scene.statuses.rumored",
  "entity.status.unknown": "scene.statuses.unknown",
  "entity.status.left_scene": "scene.statuses.left_scene",
  "entity.fallback.character": "scene.characterFallback",
  "entity.gender.masculine": "markdown.entityGenders.masculine",
  "entity.gender.feminine": "markdown.entityGenders.feminine",
  "entity.gender.neuter": "markdown.entityGenders.neuter",
  "entity.gender.plural": "markdown.entityGenders.plural",
  "entity.gender.other": "markdown.entityGenders.other",
  "entity.source.public_lore": "markdown.entitySources.publicLore",
  "entity.source.previous_scene": "markdown.entitySources.previousScene",
  "entity.source.current_scene": "markdown.entitySources.currentScene",
  "entity.source.character_roster": "markdown.entitySources.characterRoster",
  "entity.source.starting_data": "markdown.entitySources.startingData",
  "entity.source.character_movement": "markdown.entitySources.characterMovement",
  "entity.source.game_master": "markdown.entitySources.gameMaster",
});

function localizedUiText(t, semanticKey, legacyText) {
  const translationKey = ENTITY_UI_TRANSLATION_KEYS[String(semanticKey || "")];
  return translationKey ? t(translationKey, { defaultValue: legacyText }) : legacyText;
}

export function localizeEntitySubtitle(t, entity, fallback = "") {
  const legacyText = String(entity?.subtitle || fallback || "");
  const semanticKey = String(entity?.subtitle_key || "");
  if (!ENTITY_UI_TRANSLATION_KEYS[semanticKey]) return legacyText;

  const base = localizedUiText(t, semanticKey, legacyText);
  const detail = String(entity?.subtitle_detail || "").trim();
  return detail ? `${base} · ${detail}` : base;
}

export function localizeEntityMetaRow(t, row = {}) {
  return {
    ...row,
    label: localizedUiText(t, row.label_key, String(row.label || "")),
    value: localizedUiText(t, row.value_key, String(row.value || "")),
  };
}
