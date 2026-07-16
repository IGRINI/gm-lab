import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import Modal from "./Modal.jsx";
import ConnectorModelPicker from "./ConnectorModelPicker.jsx";
import WizCard, {
  characterMeta,
  characterTip,
  characterTitle,
  pcMeta,
  protagonistTip,
  storyDescription,
  storyPc,
  storyTip,
  storyTitle,
  worldMeta,
  worldPreview,
  worldTip,
  worldTitle,
} from "./WizCard.jsx";
import {
  bindingReady,
  normalizeModelBinding,
  resolveModelBinding,
} from "../connectorCatalog.js";

// NewGameWizard — the ONLY path to start a game (§Мастер «Новая игра»). Three
// steps in the approved order: Мир → История → Персонаж, then a name + «Начать».
// Connector/model selection sits above those content steps and becomes the
// immutable provider binding of the new chat (the model may change later).
// State lives entirely here; the integrator just mounts it and reacts to onLaunch
// / onOpenStudio / onClose.

function textValue(value) {
  return typeof value === "string" ? value.trim() : "";
}

function idOf(value) {
  return value == null ? "" : String(value).trim();
}

function sameId(a, b) {
  return a != null && b != null && String(a) === String(b);
}

// A self-contained "builtin bundle": a story that ships its own world (no
// world_ref). In step 1 these render as world cards labeled «встроенная классика»
// — picking one fixes BOTH the world and the single story it carries.
function isBuiltinBundle(story) {
  return (
    !!story &&
    idOf(story.id) !== "procedural" &&
    !story.procedural &&
    !idOf(story?.world_ref?.id)
  );
}

// Whether a story carries an authored protagonist the player can take as-is. The
// PLAYER-facing catalog does not expose the seed, so trust an explicit `has_pc`
// flag when the integrator supplies it, else derive: any non-procedural authored
// story (incl. self-contained builtins) ships a protagonist.
function storyHasProtagonist(story, procedural) {
  if (procedural || !story) return false;
  if (typeof story.has_pc === "boolean") return story.has_pc;
  const pc = story.player_character;
  if (pc && typeof pc === "object" && !Array.isArray(pc) && Object.keys(pc).length > 0) return true;
  return story.kind === "authored" || !idOf(story?.world_ref?.id);
}

// Resolve the initial step/selection from a preselect hint (Library «Играть»):
// a story jumps straight to Персонаж, a world to История. A lone character
// follows its OWN base refs — a hero built «под историю» jumps to Персонаж with
// that story picked, a world-based hero to История of his world; only a
// standalone hero (or one whose base was deleted) starts at Мир. The hero is
// pre-picked in every branch.
function deriveInitial(preselect, worlds, stories, characters) {
  const pre = preselect || {};
  const preChar = idOf(pre.characterId) || null;

  // A story pick (explicit, or implied by the preselected character's base).
  // A world-bound story whose world was DELETED is unresolvable: the wizard
  // would show «Без названия» and the launch would 400 — degrade instead.
  // `allowProcedural`: an EXPLICIT «Играть» on a saved procedural story is a
  // legitimate launch; a hero's story_ref never legitimately points at one.
  const storyChoiceFor = (storyId, allowProcedural) => {
    if (!storyId) return null;
    const story = (stories || []).find((s) => sameId(s.id, storyId));
    if (!story || (!allowProcedural && story.kind === "procedural")) return null;
    const bound = idOf(story?.world_ref?.id);
    if (bound && !(worlds || []).find((w) => sameId(w.id, bound))) return null;
    const worldChoice = bound
      ? { type: "world", id: bound }
      : { type: "builtin", id: idOf(story.id), storyId: idOf(story.id) };
    return { step: 3, worldChoice, storyId: idOf(story.id), characterChoice: preChar };
  };

  const fromStory = storyChoiceFor(idOf(pre.storyId), true);
  if (fromStory) return fromStory;

  const worldChoiceFor = (worldId) => {
    if (!worldId) return null;
    const world = (worlds || []).find((w) => sameId(w.id, worldId));
    if (!world) return null;
    return {
      step: 2,
      worldChoice: { type: "world", id: idOf(worldId) },
      storyId: "",
      characterChoice: preChar,
    };
  };

  const fromWorld = worldChoiceFor(idOf(pre.worldId));
  if (fromWorld) return fromWorld;

  // A lone character: follow its base refs (story first — it implies the
  // world), degrading gracefully when the base package no longer exists.
  if (preChar) {
    const hero = (characters || []).find((c) => sameId(c.id, preChar));
    const fromHeroStory = storyChoiceFor(idOf(hero?.story_ref?.id), false);
    if (fromHeroStory) return fromHeroStory;
    const fromHeroWorld = worldChoiceFor(idOf(hero?.world_ref?.id));
    if (fromHeroWorld) return fromHeroWorld;
  }

  return { step: 1, worldChoice: null, storyId: "", characterChoice: preChar };
}

export default function NewGameWizard({
  worlds = [],
  stories = [],
  characters = [],
  connectors = [],
  models = [],
  connectorModelsLoadingIds = [],
  onEnsureConnectorModels,
  initialModelBinding = null,
  connectorAuthBusyIds = [],
  connectorAuthCancellingIds = [],
  connectorAuthPrompts = {},
  onConnectorAuthStart,
  onConnectorAuthCancel,
  preselect = null,
  onLaunch,
  onOpenStudio,
  onClose,
  busy = false,
}) {
  const { t } = useTranslation("library");
  // Seed the selection once, from the preselect hint. The integrator mounts a
  // fresh wizard per open, so a one-shot initializer is enough (no re-sync).
  const [init] = useState(() => deriveInitial(preselect, worlds, stories, characters));
  const [step, setStep] = useState(init.step);
  const [worldChoice, setWorldChoice] = useState(init.worldChoice);
  const [storyId, setStoryId] = useState(init.storyId);
  const [characterChoice, setCharacterChoice] = useState(init.characterChoice);
  const [title, setTitle] = useState("");
  const [titleDirty, setTitleDirty] = useState(false);
  const [modelBinding, setModelBinding] = useState(() => normalizeModelBinding(initialModelBinding));

  useEffect(() => {
    setModelBinding((current) => resolveModelBinding(
      current.connector_id ? current : initialModelBinding,
      connectors,
      models
    ));
  }, [initialModelBinding, connectors, models]);

  const locked = !!busy;

  const libraryWorlds = useMemo(() => (Array.isArray(worlds) ? worlds : []), [worlds]);
  const builtinBundles = useMemo(
    () => (Array.isArray(stories) ? stories.filter(isBuiltinBundle) : []),
    [stories],
  );

  const selectedWorld = useMemo(
    () =>
      worldChoice?.type === "world"
        ? libraryWorlds.find((w) => sameId(w.id, worldChoice.id)) || null
        : null,
    [libraryWorlds, worldChoice],
  );

  const worldBoundStories = useMemo(
    () =>
      worldChoice?.type === "world"
        ? (stories || []).filter(
            (s) =>
              sameId(s?.world_ref?.id, worldChoice.id) &&
              idOf(s.id) !== "procedural" &&
              !s.procedural,
          )
        : [],
    [stories, worldChoice],
  );

  const selectedStoryRow = useMemo(
    () =>
      storyId && storyId !== "procedural"
        ? (stories || []).find((s) => sameId(s.id, storyId)) || null
        : null,
    [stories, storyId],
  );

  const isSyntheticProcedural = storyId === "procedural";
  const isProcedural = isSyntheticProcedural || selectedStoryRow?.kind === "procedural";
  const hasProtagonist = storyHasProtagonist(selectedStoryRow, isProcedural);
  // The selected story's PUBLIC protagonist summary (catalog `pc`, whitelisted
  // server-side) — names the «Протагонист истории» card after the actual hero.
  const protagonistPc = storyPc(selectedStoryRow);
  const protagonistName = textValue(protagonistPc?.name);

  // The synthetic procedural card is hardcoded, but the catalog ships a real
  // "procedural" row whose story_brief makes a fuller hover tip.
  const proceduralRow = useMemo(
    () => (stories || []).find((s) => idOf(s.id) === "procedural") || null,
    [stories],
  );

  const selectedCharacter = useMemo(
    () =>
      characterChoice && characterChoice !== "protagonist"
        ? (characters || []).find((c) => sameId(c.id, characterChoice)) || null
        : null,
    [characters, characterChoice],
  );

  // Match rank of a character against the CURRENT world/story pick, from its
  // base refs (world_ref/story_ref provenance): 2 = built for this story,
  // 1 = built for this world, 0 = neutral. Powers the step-3 badge + ordering —
  // matching heroes float first, but NOTHING is filtered out (any hero can play
  // any story; a mismatch only warns at launch).
  const characterMatchRank = (c) => {
    if (!isProcedural && storyId && sameId(c?.story_ref?.id, storyId)) return 2;
    if (worldChoice?.type === "world" && sameId(c?.world_ref?.id, worldChoice.id)) return 1;
    return 0;
  };
  const sortedCharacters = useMemo(() => {
    const list = Array.isArray(characters) ? [...characters] : [];
    return list
      .map((c, i) => ({ c, i, rank: characterMatchRank(c) }))
      .sort((a, b) => b.rank - a.rank || a.i - b.i)
      .map((x) => x.c);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [characters, worldChoice, storyId, isProcedural]);
  const characterBaseBadge = (c) => {
    const rank = characterMatchRank(c);
    return rank === 2
      ? t("badges.forThisStory")
      : rank === 1
        ? t("badges.fromThisWorld")
        : undefined;
  };

  // On entering Персонаж with nothing picked yet, default to the story's own
  // protagonist when it has one (procedural has none → stays empty → a package
  // is required to launch).
  useEffect(() => {
    if (step !== 3 || characterChoice != null) return;
    if (hasProtagonist) setCharacterChoice("protagonist");
  }, [step, characterChoice, hasProtagonist]);

  const storyNameForTitle = isSyntheticProcedural
    ? t("wizard.proceduralCampaign")
    : storyTitle(selectedStoryRow, t);
  const characterNameForTitle =
    characterChoice === "protagonist"
      ? protagonistName
      : selectedCharacter
        ? characterTitle(selectedCharacter, t)
        : "";
  const defaultTitle = useMemo(() => {
    const base = storyNameForTitle || t("wizard.newGame");
    return characterNameForTitle ? `${base} — ${characterNameForTitle}` : base;
  }, [storyNameForTitle, characterNameForTitle, t]);

  // Название auto-fills «{история} — {персонаж}» until the player edits it.
  useEffect(() => {
    if (!titleDirty) setTitle(defaultTitle);
  }, [defaultTitle, titleDirty]);

  // ESC closes (unless a launch is in flight).
  useEffect(() => {
    if (typeof document === "undefined") return undefined;
    const onKey = (event) => {
      if (event.key !== "Escape" || locked) return;
      event.preventDefault();
      onClose?.();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [locked, onClose]);

  const characterOk =
    (characterChoice === "protagonist" && hasProtagonist) ||
    (typeof characterChoice === "string" &&
      characterChoice !== "protagonist" &&
      characterChoice.length > 0 &&
      !!selectedCharacter);
  const canAdvance = step === 1 ? !!worldChoice : step === 2 ? !!storyId : true;
  const canLaunch =
    !locked &&
    step === 3 &&
    !!storyId &&
    characterOk &&
    bindingReady(modelBinding, connectors, models);

  const chooseWorld = (choice) => {
    if (locked) return;
    setWorldChoice(choice);
    setStoryId(choice.type === "builtin" ? choice.storyId : "");
    // Drop the "story protagonist" pick — it belongs to the previous story.
    setCharacterChoice((prev) => (prev === "protagonist" ? null : prev));
  };

  const chooseStory = (id) => {
    if (locked) return;
    setStoryId(idOf(id) === "procedural" ? "procedural" : idOf(id));
    setCharacterChoice((prev) => (prev === "protagonist" ? null : prev));
  };

  const chooseCharacter = (choice) => {
    if (locked) return;
    setCharacterChoice(choice);
  };

  const goBack = () => {
    if (step > 1) setStep(step - 1);
  };
  const goNext = () => {
    if (canAdvance && step < 3) setStep(step + 1);
  };
  const gotoStep = (n) => {
    if (locked) return;
    if (n === 1) setStep(1);
    else if (n === 2 && worldChoice) setStep(2);
    else if (n === 3 && storyId) setStep(3);
  };

  const openStudio = (kind, ctx) => {
    onOpenStudio?.(kind, ctx);
    onClose?.();
  };

  const launch = () => {
    if (!canLaunch || !onLaunch) return;
    const resolvedCharacterId = characterChoice === "protagonist" ? "" : idOf(characterChoice);
    const body = {
      title: textValue(title) || defaultTitle,
      connectorId: modelBinding.connector_id,
      modelId: modelBinding.model_id,
    };
    if (isSyntheticProcedural) {
      body.storyId = "procedural";
      body.worldId = idOf(worldChoice?.id);
    } else {
      body.storyId = idOf(storyId);
    }
    if (resolvedCharacterId) body.characterId = resolvedCharacterId;
    onLaunch(body);
  };

  const worldLabel = worldChoice
    ? worldChoice.type === "builtin"
      ? storyTitle((stories || []).find((s) => sameId(s.id, worldChoice.storyId)), t)
      : worldTitle(selectedWorld, t)
    : "";
  const storyLabel = storyId
    ? isSyntheticProcedural
      ? t("wizard.proceduralCampaign")
      : storyTitle(selectedStoryRow, t)
    : "";
  const characterLabel =
    characterChoice === "protagonist"
      ? protagonistName || t("wizard.storyProtagonist")
      : selectedCharacter
        ? characterTitle(selectedCharacter, t)
        : "";

  const steps = [
    { n: 1, name: t("entities.world"), value: worldLabel, reachable: true },
    { n: 2, name: t("entities.story"), value: storyLabel, reachable: !!worldChoice },
    { n: 3, name: t("entities.character"), value: characterLabel, reachable: !!storyId },
  ];

  const bundleStory =
    worldChoice?.type === "builtin"
      ? (stories || []).find((s) => sameId(s.id, worldChoice.storyId)) || null
      : null;

  const footer = (
    <div className="wiz-foot">
      <button
        type="button"
        className="btn"
        onClick={step === 1 ? onClose : goBack}
        disabled={locked}
      >
        {step === 1 ? t("actions.cancel") : t("actions.back")}
      </button>
      <div className="wiz-foot-right">
        {step === 3 && (
          <label className="wiz-name">
            <span>{t("wizard.titleLabel")}</span>
            <input
              value={title}
              placeholder={defaultTitle}
              onChange={(event) => {
                setTitle(event.target.value);
                setTitleDirty(true);
              }}
              disabled={locked}
            />
          </label>
        )}
        {step < 3 ? (
          <button
            type="button"
            className="btn primary"
            onClick={goNext}
            disabled={locked || !canAdvance}
          >
            {t("actions.next")}
          </button>
        ) : (
          <button type="button" className="btn primary" onClick={launch} disabled={!canLaunch}>
            {locked ? t("wizard.launching") : t("wizard.start")}
          </button>
        )}
      </div>
    </div>
  );

  return (
    <Modal
      title={t("wizard.newGame")}
      subtitle={t("wizard.step", { step, total: 3 })}
      onClose={onClose}
      className="wiz-modal"
      footer={footer}
    >
      <div className="wiz">
        <ConnectorModelPicker
          connectors={connectors}
          models={models}
          connectorModelsLoadingIds={connectorModelsLoadingIds}
          onEnsureConnectorModels={onEnsureConnectorModels}
          value={modelBinding}
          onChange={setModelBinding}
          disabled={locked}
          compact
          authBusyConnectorIds={connectorAuthBusyIds}
          authCancellingConnectorIds={connectorAuthCancellingIds}
          authPrompts={connectorAuthPrompts}
          onAuthStart={onConnectorAuthStart}
          onAuthCancel={onConnectorAuthCancel}
          ariaLabel={t("wizard.connectorModelAria")}
        />
        <ol className="wiz-steps">
          {steps.map((s) => (
            <li
              key={s.n}
              className={
                "wiz-step" +
                (step === s.n ? " is-active" : "") +
                (s.value ? " is-done" : "")
              }
            >
              <button
                type="button"
                className="wiz-step-btn"
                onClick={() => gotoStep(s.n)}
                disabled={!s.reachable || locked}
              >
                <span className="wiz-step-n">{s.n}</span>
                <span className="wiz-step-text">
                  <span className="wiz-step-name">{s.name}</span>
                  {s.value && <span className="wiz-step-value">{s.value}</span>}
                </span>
              </button>
            </li>
          ))}
        </ol>

        {step === 1 && (
          <div className="wiz-panel">
            <p className="wiz-lead">{t("wizard.chooseWorldLead")}</p>
            <div className="wiz-grid">
              {libraryWorlds.map((w) => (
                <WizCard
                  key={`w-${w.id}`}
                  selected={worldChoice?.type === "world" && sameId(worldChoice.id, w.id)}
                  disabled={locked}
                  onClick={() => chooseWorld({ type: "world", id: idOf(w.id) })}
                  kicker={t("entities.worldLower")}
                  title={worldTitle(w, t)}
                  meta={worldMeta(w)}
                  desc={worldPreview(w)}
                  tip={worldTip(w, t)}
                />
              ))}
              {builtinBundles.map((s) => (
                <WizCard
                  key={`b-${s.id}`}
                  selected={worldChoice?.type === "builtin" && sameId(worldChoice.storyId, s.id)}
                  disabled={locked}
                  onClick={() =>
                    chooseWorld({ type: "builtin", id: idOf(s.id), storyId: idOf(s.id) })
                  }
                  kicker={t("entities.worldLower")}
                  badge={t("badges.builtinClassic")}
                  title={storyTitle(s, t)}
                  desc={storyDescription(s)}
                  tip={storyTip(s, t, { kicker: t("badges.builtinClassic") })}
                />
              ))}
              <WizCard
                add
                title={t("actions.createWorldBare")}
                disabled={locked}
                onClick={() => openStudio("world", null)}
              />
            </div>
            {libraryWorlds.length === 0 && builtinBundles.length === 0 && (
              <p className="wiz-empty">{t("wizard.noWorlds")}</p>
            )}
          </div>
        )}

        {step === 2 &&
          (worldChoice?.type === "builtin" ? (
            <div className="wiz-panel">
              <p className="wiz-lead">{t("wizard.builtinStoryLead")}</p>
              <div className="wiz-grid">
                <WizCard
                  selected
                  disabled={locked}
                  onClick={() => chooseStory(worldChoice.storyId)}
                  kicker={t("entities.storyLower")}
                  title={storyTitle(bundleStory, t)}
                  desc={storyDescription(bundleStory)}
                  tip={storyTip(bundleStory, t)}
                />
              </div>
            </div>
          ) : (
            <div className="wiz-panel">
              <p className="wiz-lead">
                {t("wizard.worldStoriesLead", { world: worldTitle(selectedWorld, t) })}
              </p>
              <div className="wiz-grid">
                {worldBoundStories.map((s) => (
                  <WizCard
                    key={`s-${s.id}`}
                    selected={sameId(storyId, s.id)}
                    disabled={locked}
                    onClick={() => chooseStory(s.id)}
                    kicker={t("entities.storyLower")}
                    meta={s.kind === "procedural" ? t("badges.procedural") : undefined}
                    title={storyTitle(s, t)}
                    desc={storyDescription(s)}
                    tip={storyTip(s, t)}
                  />
                ))}
                <WizCard
                  selected={isSyntheticProcedural}
                  disabled={locked}
                  onClick={() => chooseStory("procedural")}
                  kicker={t("entities.storyLower")}
                  badge={t("badges.procedural")}
                  title={t("wizard.proceduralCampaign")}
                  desc={t("wizard.proceduralDescription")}
                  tip={storyTip(proceduralRow, t)}
                />
                <WizCard
                  add
                  title={t("actions.createStoryBare")}
                  disabled={locked}
                  onClick={() => openStudio("story", { worldId: idOf(worldChoice?.id) })}
                />
              </div>
            </div>
          ))}

        {step === 3 && (
          <div className="wiz-panel">
            <p className="wiz-lead">
              {isProcedural
                ? t("wizard.characterLeadProcedural")
                : hasProtagonist
                  ? t("wizard.characterLeadWithProtagonist")
                  : t("wizard.characterLeadWithoutProtagonist")}
            </p>
            <div className="wiz-grid">
              {hasProtagonist && (
                <WizCard
                  selected={characterChoice === "protagonist"}
                  disabled={locked}
                  onClick={() => chooseCharacter("protagonist")}
                  kicker={t("entities.characterLower")}
                  badge={t("badges.fromStory")}
                  title={protagonistName || t("wizard.storyProtagonist")}
                  meta={pcMeta(protagonistPc, t)}
                  desc={
                    textValue(protagonistPc?.background) ||
                    textValue(protagonistPc?.physical_type) ||
                    t("wizard.protagonistDescription")
                  }
                  tip={protagonistTip(selectedStoryRow, t)}
                />
              )}
              {sortedCharacters.map((c) => {
                const preview = textValue(c.preview);
                return (
                  <WizCard
                    key={`c-${c.id}`}
                    selected={sameId(characterChoice, c.id)}
                    disabled={locked}
                    onClick={() => chooseCharacter(idOf(c.id))}
                    kicker={t("entities.characterLower")}
                    badge={characterBaseBadge(c)}
                    title={characterTitle(c, t)}
                    meta={characterMeta(c, t)}
                    desc={preview && preview !== characterTitle(c, t) ? preview : ""}
                    tip={characterTip(c, t)}
                  />
                );
              })}
              <WizCard
                add
                title={t("actions.createCharacterBare")}
                disabled={locked}
                onClick={() =>
                  openStudio("character", {
                    worldId: worldChoice?.type === "world" ? idOf(worldChoice.id) : "",
                    // A procedural pick (synthetic OR a saved procedural-kind
                    // story) carries no plot to base a hero on — same gate as
                    // BasePickerModal; the base degrades to world-only.
                    storyId: isProcedural ? "" : idOf(storyId),
                  })
                }
              />
            </div>
            {!hasProtagonist && (characters || []).length === 0 && (
              <p className="wiz-hint">
                {t("wizard.noCharacters")}
              </p>
            )}
            {!characterOk && (characters || []).length > 0 && (
              <p className="wiz-hint">{t("wizard.chooseCharacter")}</p>
            )}
          </div>
        )}
      </div>
    </Modal>
  );
}
