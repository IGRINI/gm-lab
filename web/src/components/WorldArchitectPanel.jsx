import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api } from "../api.js";
import ImageThumbnail from "./ImagePreview.jsx";
import {
  EMPTY_ARCHITECT_USAGE,
  textValue,
  normalizeVisibleMessage,
  AutoTextarea,
  useLiveSegments,
  ArchitectChatPane,
  ArchitectDebugModal,
  accumulateUsage,
  debugFromDone,
} from "./architectShared.jsx";

const WORLD_PRESETS = [
  {
    id: "machine",
    label: "Машинный постапокалипсис",
    description: "Руины, автономные узлы, вода, энергия, дроны и выжившие общины.",
    values: {
      title: "Пепельный Узел",
      genre: "postapocalyptic machine world",
      tone: "bleak",
      worldSize: "Один большой регион вокруг цепочки старых машинных узлов; за его пределами мир шире, но связь и дороги ненадежны.",
      population: "Сотни тысяч выживших, разбитых на общины, караванные союзы и технокульты.",
      publicPremise: "Люди живут вокруг старых узлов воды, энергии и ремонта. Машины не исчезли: одни служат, другие охраняют протоколы, которые никто уже не понимает.",
      worldLore: {
        world_laws: [
          "старые машины следуют протоколам, а не морали",
          "вода, энергия, детали и доступ к узлам важнее монет",
        ],
        inhabitants: ["выжившие общины", "технокульты", "караванщики", "автономные дроны"],
        regions: ["сеть узлов в пепельной зоне", "сухие трассы между поселениями", "закрытые машинные сектора"],
        power_centers: ["советы водных общин", "ремонтные братства", "узловые культы"],
        location_rules: ["каждая локация должна показывать цену воды, энергии, деталей, доступа или сигнала"],
        prohibited_elements: ["классическая магия без объяснения как сбой, культ или чужеродный артефакт"],
      },
    },
  },
  {
    id: "isekai",
    label: "Фентезийный иссекай",
    description: "Клятвы, духи мест, цена магии, призванные чужаки и местные долги.",
    values: {
      title: "Порог Второго Неба",
      genre: "fantasy isekai",
      tone: "tense hopeful",
      worldSize: "Континент с несколькими королевствами, духами дорог и местными святилищами; игра может начинаться где угодно внутри него.",
      population: "Десятки миллионов людей, духовных родов, призванных чужаков и малых народов.",
      publicPremise: "Имя, клятва и долг имеют силу закона и магии. Призванные чужаки появляются редко, но почти всегда становятся частью старого договора.",
      worldLore: {
        dogmas: ["имя и клятва имеют юридическую и мистическую силу", "духи мест помнят долги лучше людей"],
        world_laws: ["магия требует имени, цены или признанного права", "дальняя дорога меняет слухи и баланс сил"],
        inhabitants: ["родовые дома", "духи мест", "призванные чужаки", "гильдии рунников"],
        regions: ["Семь земель под Осколочной Луной", "живые дороги", "пороговые святилища"],
        religions: ["культ дорожных духов", "официальная вера клятв"],
        gods: ["Старшие Духи Порогов"],
        location_rules: ["каждая новая локация должна иметь связь с долгом, властью, дорогой или духом места"],
        prohibited_elements: ["бесплатное воскрешение", "магия без имени, цены или договора"],
      },
    },
  },
  {
    id: "frontier",
    label: "Пограничье",
    description: "Дороги, фракции, слухи, старые места и поселения с реальной функцией.",
    values: {
      title: "Край Старых Дорог",
      genre: "frontier fantasy",
      tone: "tense",
      worldSize: "Один обжитой континент с королевствами, пограничными трактами, старыми местами и дальними землями за картой.",
      population: "Несколько миллионов жителей: города-государства, деревни, кочевые семьи, дорожные ордена и старые народы.",
      publicPremise: "Дороги важнее стен: слухи, караваны, старые договоры и опасные места связывают поселения сильнее любой короны.",
      worldLore: {
        conflicts: ["корона пытается подчинить дороги", "старые места снова просыпаются", "пограничные поселения спорят за пошлины"],
        regions: ["королевские тракты", "старые леса", "пограничные города", "забытые святилища"],
        power_centers: ["дорожные ордена", "городские советы", "караванные дома", "пограничная стража"],
        economy: ["пошлины", "караваны", "долги за охрану", "право прохода"],
        location_rules: ["каждая дорога должна иметь характер, цену, риск и тех, кто о ней знает"],
        prohibited_elements: ["случайные данжи без связи с дорогами, долгами или старым правом"],
      },
    },
  },
];

const DEFAULT_WORLD_DRAFT = {
  title: "",
  genre: "fantasy",
  tone: "tense",
  worldSize: "",
  population: "",
  publicPremise: "",
  worldLore: null,
};

const DEFAULT_ARCHITECT_MESSAGES = [
  {
    role: "assistant",
    content:
      "Опиши мир свободно — или дай направление, а детали я соберу сам.\n\nЧто особенно полезно:\n\n1. Жанр и настроение.\n2. Насколько большой мир и сколько в нём жителей примерно.\n3. Кто его населяет: люди, расы, виды, культуры, фракции.\n4. Какие законы реальности работают: магия, технологии, боги, смерть, дороги.\n5. Что в мире точно должно быть.\n6. Чего нельзя добавлять без причины.\n7. Какие скрытые истины должен знать только GM.\n\nСтартовую сцену, роль игрока и квест сейчас не придумываем — это будет отдельный шаг истории.",
  },
];

const LORE_PREVIEW_FIELDS = [
  ["dogmas", "Догматы"],
  ["world_laws", "Законы мира"],
  ["inhabitants", "Народы/виды"],
  ["creatures", "Существа/угрозы"],
  ["power_sources", "Силы/магия/технологии"],
  ["technologies", "Материальная культура"],
  ["taboos", "Табу/законы"],
  ["conflicts", "Конфликты"],
  ["inspirations", "Референсы"],
  ["regions", "Регионы"],
  ["power_centers", "Власть"],
  ["religions", "Вера"],
  ["gods", "Боги/силы"],
  ["cultures", "Культуры"],
  ["history", "История"],
  ["economy", "Экономика"],
  ["daily_life", "Быт"],
  ["story_hooks", "Напряжения для будущих историй"],
  ["hidden_secrets", "Секреты GM"],
  ["location_rules", "Правила локаций"],
  ["prohibited_elements", "Нельзя без причины"],
];

const VISUAL_PROMPT_FIELDS = [
  [
    "world_image_prompt_en",
    "Prompt изображения мира (EN)",
    "English prompt for a world overview image: how the world looks, key landscapes, settlements, peoples, magic/technology cues, mood.",
    "world_image_url",
    "Изображение мира",
  ],
  [
    "world_map_prompt_en",
    "Prompt карты мира (EN)",
    "English prompt for a readable world map: geography, regions, borders, routes, settlements, labels, cartography style.",
    "world_map_url",
    "Карта мира",
  ],
];
const VISUAL_OUTPUT_FIELDS = VISUAL_PROMPT_FIELDS.map(([, , , outputField, outputLabel]) => [
  outputField,
  outputLabel,
]);

function cleanWorldDraft(draft) {
  return {
    title: textValue(draft.title),
    genre: textValue(draft.genre),
    tone: textValue(draft.tone),
    worldSize: textValue(draft.worldSize),
    population: textValue(draft.population),
    publicPremise: textValue(draft.publicPremise),
    worldLore: draft.worldLore && typeof draft.worldLore === "object" ? draft.worldLore : null,
  };
}

function worldDraftFromSaved(world) {
  if (!world || typeof world !== "object") return { ...DEFAULT_WORLD_DRAFT };
  const isDraft = textValue(world.status) === "draft";
  const savedTitle = textValue(world.title);
  const title = isDraft && savedTitle === "Новый мир" ? "" : savedTitle;
  return {
    title: title || textValue(world.world_lore?.name),
    genre: textValue(world.genre) || DEFAULT_WORLD_DRAFT.genre,
    tone: textValue(world.tone) || DEFAULT_WORLD_DRAFT.tone,
    worldSize: textValue(world.world_size),
    population: textValue(world.population),
    publicPremise: textValue(world.public_premise) || textValue(world.world_lore?.public_premise),
    worldLore: world.world_lore && typeof world.world_lore === "object" ? world.world_lore : null,
  };
}

// Restore the visible conversation from the server's architect block
// (`GET /worlds/{id}/architect` → `{architect: {messages}}`). The chat lives in
// the package's architect.json now — never inside the world row.
function architectMessagesFromChat(architect) {
  const raw = Array.isArray(architect?.messages) ? architect.messages : [];
  const messages = raw.map(normalizeVisibleMessage).filter(Boolean);
  return messages.length > 0 ? messages : DEFAULT_ARCHITECT_MESSAGES;
}

function mergeArchitectDraft(current, draft) {
  if (!draft || typeof draft !== "object") return current;
  const lore = draft.world_lore && typeof draft.world_lore === "object" ? draft.world_lore : null;
  return {
    ...current,
    title: textValue(draft.title) || current.title,
    genre: textValue(draft.genre) || current.genre,
    tone: textValue(draft.tone) || current.tone,
    worldSize: textValue(draft.world_size) || current.worldSize,
    population: textValue(draft.population) || current.population,
    publicPremise: textValue(draft.public_premise) || current.publicPremise,
    worldLore: lore ? normalizeWorldLore(lore, draft) : current.worldLore,
  };
}

function normalizeWorldLore(lore, draft) {
  const next = { ...lore };
  if (!textValue(next.name)) next.name = textValue(draft.title);
  if (!textValue(next.genre)) next.genre = textValue(draft.genre);
  if (!textValue(next.tone)) next.tone = textValue(draft.tone);
  if (!textValue(next.world_size)) next.world_size = textValue(draft.world_size);
  if (!textValue(next.population)) next.population = textValue(draft.population);
  if (!textValue(next.public_premise)) next.public_premise = textValue(draft.public_premise);
  return next;
}

function loreArray(value) {
  return Array.isArray(value) ? value.map(textValue).filter(Boolean) : [];
}

function lorePreviewRows(lore) {
  if (!lore || typeof lore !== "object") return [];
  const rows = [];
  if (textValue(lore.public_premise)) rows.push(["Публично", textValue(lore.public_premise)]);
  if (textValue(lore.hidden_premise)) rows.push(["Скрыто", textValue(lore.hidden_premise)]);
  for (const [field, label] of VISUAL_PROMPT_FIELDS) {
    const prompt = textValue(lore[field]);
    if (prompt) rows.push([label, prompt]);
  }
  for (const [field, label] of LORE_PREVIEW_FIELDS) {
    const values = loreArray(lore[field]);
    if (values.length > 0) rows.push([label, values.join("; ")]);
  }
  return rows;
}

function applyPresetValues(current, preset) {
  return {
    ...current,
    ...preset.values,
  };
}

// A world is "creatable" once it has any real lore — a public/hidden premise or
// at least one filled list field. Used both for the gate and the readiness chip.
function loreHasContent(lore) {
  if (!lore || typeof lore !== "object") return false;
  if (textValue(lore.public_premise) || textValue(lore.hidden_premise)) return true;
  return LORE_PREVIEW_FIELDS.some(([field]) => loreArray(lore[field]).length > 0);
}

// Render a list lore field as newline-separated text for the manual textareas.
function loreFieldText(lore, field) {
  if (Array.isArray(lore?.[field])) return lore[field].join("\n");
  if (typeof lore?.[field] === "string") return lore[field];
  return "";
}

// Build the final world_lore object on submit: clean list fields (trim + drop
// empties), keep a non-empty hidden premise, and backfill name/genre/tone/
// world-size/public premise from the top-level draft so manual worlds are valid.
function finalizeWorldLore(payload) {
  const source = payload.worldLore && typeof payload.worldLore === "object" ? payload.worldLore : {};
  const lore = { ...source };
  for (const [field] of LORE_PREVIEW_FIELDS) {
    if (field in lore) {
      const items = loreArray(lore[field]);
      if (items.length) lore[field] = items;
      else delete lore[field];
    }
  }
  // Open questions are conversational (the architect asks them in chat), never a
  // stored bible field.
  delete lore.open_questions;
  const hidden = textValue(lore.hidden_premise);
  if (hidden) lore.hidden_premise = hidden;
  else delete lore.hidden_premise;
  for (const [field] of VISUAL_PROMPT_FIELDS) {
    const prompt = textValue(lore[field]);
    if (prompt) lore[field] = prompt;
    else delete lore[field];
  }
  for (const [field] of VISUAL_OUTPUT_FIELDS) {
    const url = textValue(lore[field]);
    if (url) lore[field] = url;
    else delete lore[field];
  }
  if (!textValue(lore.public_premise) && textValue(payload.publicPremise)) {
    lore.public_premise = textValue(payload.publicPremise);
  }
  if (!textValue(lore.name)) lore.name = textValue(payload.title);
  if (!textValue(lore.genre)) lore.genre = textValue(payload.genre);
  if (!textValue(lore.tone)) lore.tone = textValue(payload.tone);
  if (!textValue(lore.world_size)) lore.world_size = textValue(payload.worldSize);
  if (!textValue(lore.population)) lore.population = textValue(payload.population);
  return lore;
}

function visualPromptSnapshot(lore) {
  return VISUAL_PROMPT_FIELDS.map(([promptField, , , outputField]) => ({
    promptField,
    outputField,
    prompt: textValue(lore?.[promptField]),
    imageUrl: textValue(lore?.[outputField]),
  }));
}

function visualJobLabel(job, prompt, imageUrl) {
  if (job?.loading) return "Генерация...";
  if (job?.queued) return "В очереди...";
  if (imageUrl) return "Готово";
  if (prompt) return "Ожидает генерации";
  return "Нет prompt";
}

export default function WorldArchitectPanel({
  world,
  locked,
  onCreateWorld,
  onArchitectStream,
  onGenerateImage,
  onPlayWorld,
  onCreateStory,
  className = "",
}) {
  // The model history and prompt-cache ids are SERVER-side (the package's
  // architect.json); the panel holds only the visible conversation.
  const [worldDraft, setWorldDraft] = useState(() => worldDraftFromSaved(world));
  const [messages, setMessages] = useState(() => architectMessagesFromChat(null));
  const [input, setInput] = useState("");
  const [architectBusy, setArchitectBusy] = useState(false);
  const [architectError, setArchitectError] = useState("");
  // The last message whose turn FAILED — powers the «Повторить» button.
  const [retryText, setRetryText] = useState("");
  const [bibleOpen, setBibleOpen] = useState(false);
  const [architectUsage, setArchitectUsage] = useState(EMPTY_ARCHITECT_USAGE);
  const [architectDebug, setArchitectDebug] = useState(null);
  const [debugOpen, setDebugOpen] = useState(false);
  const [imageJobs, setImageJobs] = useState({});
  const imageAutoRequestsRef = useRef({});
  const imagePromptLatestRef = useRef({});
  const imageQueueRef = useRef([]);
  const imageQueueRunningRef = useRef(false);
  const imageScopeRef = useRef(0);
  const [architectElapsed, setArchitectElapsed] = useState(0);
  // In-flight segments for the current turn (think / reply text / tool), folded
  // from the SSE stream in production order. Mirrors the main chat's live view.
  const { liveSegments, liveSegmentsRef, appendLiveDelta, pushLiveTool, clearLive } =
    useLiveSegments();
  // Start as `null` (not the mount id) so the load effect ALWAYS runs on mount —
  // for an existing world that means fetching its architect conversation on open.
  const loadedWorldIdRef = useRef(null);
  const worldPayload = useMemo(() => cleanWorldDraft(worldDraft), [worldDraft]);
  // "Filled" for the bible label / auto-open = real DETAIL (hidden premise or any
  // list field), not just a public premise mirrored from the top-level field —
  // otherwise the label reads "заполнена" while every detail field is empty.
  const loreFilled = useMemo(() => {
    const lore = worldPayload.worldLore;
    if (!lore || typeof lore !== "object") return false;
    if (textValue(lore.hidden_premise)) return true;
    return LORE_PREVIEW_FIELDS.some(([field]) => loreArray(lore[field]).length > 0);
  }, [worldPayload.worldLore]);
  // Creatable manually too: the basics plus either a public premise or any lore.
  const loreReady = !!textValue(worldPayload.publicPremise) || loreFilled;
  const worldCreateLocked =
    locked ||
    !worldPayload.title ||
    !worldPayload.genre ||
    !worldPayload.tone ||
    !worldPayload.worldSize ||
    !worldPayload.population ||
    !loreReady;
  const architectLocked = locked || architectBusy;

  useEffect(() => {
    const id = world?.id ?? null;
    // Only reload when the user switches to a DIFFERENT world. The world our own
    // turn just created/updated (App syncs selectedWorldId) is already ours —
    // reloading it would wipe the live conversation.
    if (id === loadedWorldIdRef.current) return undefined;
    loadedWorldIdRef.current = id;
    const nextDraft = worldDraftFromSaved(world);
    setWorldDraft(nextDraft);
    setMessages(architectMessagesFromChat(null));
    clearLive();
    setInput("");
    setArchitectError("");
    setRetryText("");
    setArchitectUsage(EMPTY_ARCHITECT_USAGE);
    setArchitectDebug(null);
    setDebugOpen(false);
    setImageJobs({});
    imageScopeRef.current += 1;
    imageAutoRequestsRef.current = {};
    imagePromptLatestRef.current = {};
    imageQueueRef.current = [];
    setBibleOpen(loreHasContent(nextDraft.worldLore));
    if (!id) return undefined;
    // Restore the conversation from the server. A failed fetch is a VISIBLE
    // error (a silently-default intro would look like the chat never existed).
    // `cancelled` guards a stale response when the user switches worlds
    // mid-flight.
    let cancelled = false;
    api
      .worldArchitect(id)
      .then((data) => {
        if (cancelled || loadedWorldIdRef.current !== id) return;
        if (!data?.ok) {
          throw new Error(data?.error || "не удалось загрузить переписку архитектора");
        }
        setMessages(architectMessagesFromChat(data.architect));
      })
      .catch((error) => {
        if (cancelled || loadedWorldIdRef.current !== id) return;
        setArchitectError(error?.message || "не удалось загрузить переписку архитектора");
      });
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [world?.id]);

  // Reveal the bible editor the first time real lore appears (architect draft or
  // manual entry); the user can still collapse it afterwards.
  useEffect(() => {
    if (loreFilled) setBibleOpen(true);
  }, [loreFilled]);

  // Tick an elapsed-seconds counter while the architect works, so a slow model
  // still shows visible progress instead of a frozen-looking screen.
  useEffect(() => {
    if (!architectBusy) {
      setArchitectElapsed(0);
      return undefined;
    }
    const startedAt = Date.now();
    const id = window.setInterval(() => {
      setArchitectElapsed(Math.floor((Date.now() - startedAt) / 1000));
    }, 1000);
    return () => window.clearInterval(id);
  }, [architectBusy]);

  useEffect(() => {
    const latest = {};
    for (const { promptField, prompt } of visualPromptSnapshot(worldDraft.worldLore)) {
      latest[promptField] = prompt;
    }
    imagePromptLatestRef.current = latest;
  }, [worldDraft.worldLore]);

  const updateWorldDraft = (field, value) => {
    setWorldDraft((current) => ({ ...current, [field]: value }));
  };

  const applyPreset = (preset) => {
    setWorldDraft((current) => applyPresetValues(current, preset));
  };

  const updateWorldLore = useCallback((field, value) => {
    setWorldDraft((current) => {
      const lore = current.worldLore && typeof current.worldLore === "object" ? { ...current.worldLore } : {};
      lore[field] = value;
      return { ...current, worldLore: lore };
    });
  }, []);
  const updateLoreText = (field, text) => updateWorldLore(field, text);
  const updateLoreList = (field, text) => updateWorldLore(field, text.split("\n"));

  const setImageJob = useCallback((field, patch) => {
    setImageJobs((current) => ({ ...current, [field]: { ...(current[field] || {}), ...patch } }));
  }, []);

  const runVisualGeneration = useCallback(async (promptField, outputField, prompt, scope) => {
    const isCurrentScope = () => scope === imageScopeRef.current;
    const setScopedImageJob = (patch) => {
      if (isCurrentScope()) setImageJob(promptField, patch);
    };
    const releaseAutoRequest = () => {
      if (imageAutoRequestsRef.current[promptField] === prompt) delete imageAutoRequestsRef.current[promptField];
    };
    if (!isCurrentScope() || imagePromptLatestRef.current[promptField] !== prompt) {
      releaseAutoRequest();
      setScopedImageJob({ queued: false, loading: false });
      return;
    }
    setScopedImageJob({ queued: false, loading: true, error: "" });
    try {
      if (typeof onGenerateImage !== "function") {
        throw new Error("генерация картинок недоступна");
      }
      const isMap = outputField === "world_map_url";
      const data = await onGenerateImage({
        prompt,
        model: "nvfp4",
        width: isMap ? 1536 : 1024,
        height: 1024,
      });
      if (!data.ok) throw new Error(data.error || "картинка не сгенерирована");
      const image = Array.isArray(data.images) ? data.images.find((item) => textValue(item?.url)) : null;
      const url = textValue(image?.url);
      if (!url) throw new Error("sidecar не вернул URL картинки");
      if (!isCurrentScope() || imagePromptLatestRef.current[promptField] !== prompt) {
        releaseAutoRequest();
        setScopedImageJob({ queued: false, loading: false });
        return;
      }
      updateWorldLore(outputField, url);
      setScopedImageJob({ queued: false, loading: false, error: "", seed: data.seed, url });
    } catch (error) {
      setScopedImageJob({
        queued: false,
        loading: false,
        error: error?.message || "не удалось сгенерировать картинку",
      });
    }
  }, [onGenerateImage, setImageJob, updateWorldLore]);

  const drainVisualQueue = useCallback(async () => {
    if (imageQueueRunningRef.current) return;
    const next = imageQueueRef.current.shift();
    if (!next) return;
    imageQueueRunningRef.current = true;
    try {
      await runVisualGeneration(next.promptField, next.outputField, next.prompt, next.scope);
    } finally {
      imageQueueRunningRef.current = false;
      if (imageQueueRef.current.length > 0) {
        window.setTimeout(() => {
          void drainVisualQueue();
        }, 0);
      }
    }
  }, [runVisualGeneration]);

  const enqueueVisualGeneration = useCallback((job) => {
    const duplicate = imageQueueRef.current.some(
      (queued) => queued.promptField === job.promptField && queued.prompt === job.prompt
    );
    if (duplicate) return;
    imageQueueRef.current = imageQueueRef.current.filter((queued) => queued.promptField !== job.promptField);
    imageQueueRef.current.push({ ...job, scope: imageScopeRef.current });
    setImageJob(job.promptField, { queued: true, loading: false, error: "" });
    void drainVisualQueue();
  }, [drainVisualQueue, setImageJob]);

  useEffect(() => {
    if (locked || architectBusy || typeof onGenerateImage !== "function") return undefined;
    const runnable = visualPromptSnapshot(worldDraft.worldLore).filter(({ promptField, prompt, imageUrl }) => {
      if (!prompt || imageUrl || imageJobs[promptField]?.loading) return false;
      return imageAutoRequestsRef.current[promptField] !== prompt;
    });
    if (!runnable.length) return undefined;

    const timer = window.setTimeout(() => {
      for (const { promptField, outputField, prompt } of runnable) {
        imageAutoRequestsRef.current[promptField] = prompt;
        enqueueVisualGeneration({ promptField, outputField, prompt });
      }
    }, 900);
    return () => window.clearTimeout(timer);
  }, [worldDraft.worldLore, imageJobs, locked, architectBusy, onGenerateImage, enqueueVisualGeneration]);

  const submitWorld = async (event) => {
    event.preventDefault();
    if (worldCreateLocked) return;
    const saved = await onCreateWorld?.({ ...worldPayload, worldLore: finalizeWorldLore(worldPayload) });
    // Adopt the server-rewritten image URLs (/world-assets/<id>/<file>) so the
    // preview points at the package asset instead of the volatile sidecar URL.
    adoptPersistedImageUrls(saved);
  };

  // Overlay the persisted world's image fields onto the live draft. The server
  // copies generated images into the package and returns same-origin
  // /world-assets URLs; keeping the old sidecar URL would 404 once the sidecar
  // run dir clears. Empty fields stay empty (a valid "no image" state).
  const adoptPersistedImageUrls = useCallback((savedWorld) => {
    const lore = savedWorld?.world_lore;
    if (!lore || typeof lore !== "object") return;
    setWorldDraft((current) => {
      const currentLore =
        current.worldLore && typeof current.worldLore === "object" ? current.worldLore : {};
      const nextLore = { ...currentLore };
      let changed = false;
      for (const [outputField] of VISUAL_OUTPUT_FIELDS) {
        const persisted = textValue(lore[outputField]);
        if (persisted && persisted !== textValue(currentLore[outputField])) {
          nextLore[outputField] = persisted;
          changed = true;
        }
      }
      return changed ? { ...current, worldLore: nextLore } : current;
    });
  }, []);

  // One architect turn. `appendUser=false` is the RETRY path: the visible chat
  // already carries the user message (and the failure note) from the failed
  // attempt, so only the request is repeated.
  const runArchitectTurn = async (text, appendUser) => {
    const visibleMessages = appendUser
      ? [...messages, { role: "user", content: text }]
      : [...messages];
    setArchitectError("");
    setArchitectBusy(true);
    clearLive();
    setMessages(visibleMessages);
    let adopted = false;
    let failure = "";
    try {
      // The server owns the conversation (model history + cache ids live in the
      // package's architect.json). The body carries only the message and the
      // form's CONTENT draft — the server applies it as a normal world update
      // before the turn, so hand-edited fields are never lost. App injects the
      // selected world_id.
      await onArchitectStream?.(
        {
          message: text,
          draft: worldPayload,
        },
        (ev) => {
          if (ev.kind === "architect_delta") {
            // Per-hop content/thinking delta. Reasoning streams into a collapsed
            // spoiler; reply text streams into its own bubble — like the main chat.
            const d = ev.data || {};
            const sid = textValue(d.sid) || "arch";
            const role = d.channel === "thinking" ? "think" : "assistant";
            appendLiveDelta(sid, role, String(d.text || ""));
          } else if (ev.kind === "architect_tool") {
            // Surface each tool call inline, in order, and fill the inspector live.
            const call = ev.data || {};
            const name = textValue(call.name);
            if (!name) return;
            const args = call.arguments && typeof call.arguments === "object" ? call.arguments : {};
            const sid = textValue(call.sid) || "arch";
            pushLiveTool(sid, name, args);
            if (name === "draft_world_bible") {
              setWorldDraft((current) => mergeArchitectDraft(current, args));
            }
          } else if (ev.kind === "architect_error") {
            failure = textValue(ev.data) || "Архитектор не ответил";
          } else if (ev.kind === "architect_done") {
            adopted = true;
            const data = ev.data || {};
            const usage = data.usage && typeof data.usage === "object" ? data.usage : null;
            if (usage) setArchitectUsage((current) => accumulateUsage(current, usage));
            setArchitectDebug(debugFromDone(data, usage));
            if (data.draft && typeof data.draft === "object") {
              setWorldDraft((current) => mergeArchitectDraft(current, data.draft));
            }
            // The architect draft carries volatile sidecar image URLs; the
            // persisted world (data.world) carries the package /world-assets
            // URLs the server rewrote to. Adopt those last so the preview is
            // stable across sidecar restarts and image-gen toggles.
            adoptPersistedImageUrls(data.world);
            // The world we just created/updated is ours — keep the `world` prop
            // sync (App.setSelectedWorldId) from wiping this live conversation.
            if (data.world?.id) loadedWorldIdRef.current = data.world.id;
            // Fold this turn's live segments into the visible chat — the same
            // shape the server just persisted to architect.json.
            setMessages([...visibleMessages, ...liveSegmentsRef.current]);
            clearLive();
          }
        }
      );
      if (failure) throw new Error(failure);
      setRetryText("");
    } catch (error) {
      const message = error?.message || "Не удалось вызвать архитектора";
      setArchitectError(message);
      setRetryText(text);
      if (!adopted) {
        // Keep whatever streamed before the failure, then append the error note.
        setMessages((current) => [
          ...current,
          ...liveSegmentsRef.current,
          { role: "assistant", content: `Не получилось обновить мир: ${message}` },
        ]);
        clearLive();
      }
    } finally {
      setArchitectBusy(false);
    }
  };

  const sendArchitectMessage = async () => {
    const text = input.trim();
    if (!text || architectLocked) return;
    setInput("");
    await runArchitectTurn(text, true);
  };

  const retryArchitectTurn = async () => {
    if (!retryText || architectLocked) return;
    await runArchitectTurn(retryText, false);
  };

  // One renderer for both the committed log and the in-flight segments, so the
  // live view and the reloaded history look identical: reasoning → spoiler,
  // tool → detailed card, user/assistant → bubble (with a caret while streaming).
  return (
    <form className={`world-studio${className ? ` ${className}` : ""}`} onSubmit={submitWorld}>
      <header className="world-studio-head">
        <div className="world-studio-id">
          <span className="world-studio-emblem" aria-hidden="true">✦</span>
          <div className="world-studio-title">
            <span className="world-studio-kicker">создание мира</span>
            <b>Студия миров</b>
            <p className="world-studio-sub">
              Соберите лор и правила мира с архитектором или заполните вручную — без старта игрового чата.
            </p>
          </div>
        </div>
        <span className={`world-studio-chip${worldCreateLocked ? "" : " ready"}`}>
          {worldCreateLocked ? "черновик не готов" : "готово к сохранению"}
        </span>
      </header>

      <div className="world-studio-body">
        <ArchitectChatPane
          headKicker="архитектор"
          headTitle="Собрать лор мира"
          helpTitle="Архитектор мира"
          helpSubtitle="Отдельный AI-контур до старта игры."
          helpNote="Он задаёт вопросы и собирает библию мира: законы, веру, историю, регионы, власти, секреты и правила генерации локаций."
          thinkLabel="🧠 Архитектор рассуждает"
          placeholder="Например: хочу тёмный иссекай про клятвы, богов-должников и живые дороги… (Enter — отправить)"
          messages={messages}
          liveSegments={liveSegments}
          busy={architectBusy}
          elapsed={architectElapsed}
          error={architectError}
          usage={architectUsage}
          debug={architectDebug}
          onOpenDebug={() => setDebugOpen(true)}
          input={input}
          onInputChange={setInput}
          onSend={sendArchitectMessage}
          onRetry={retryText ? retryArchitectTurn : undefined}
          locked={architectLocked}
        />

        <section
          className={`world-studio-pane world-inspector${loreReady ? " is-live" : ""}`}
          aria-label="Параметры мира"
        >
          <div className="world-inspector-head">
            <span className="world-inspector-kicker">параметры</span>
            <b>{textValue(worldPayload.worldLore?.name) || worldDraft.title || "Без названия"}</b>
          </div>

          <div className="world-inspector-body">
            <div className="world-inspector-section">
              <span className="world-inspector-label">Пресеты мира</span>
              <div className="world-manager-presets" aria-label="Быстрые пресеты мира">
                {WORLD_PRESETS.map((preset) => (
                  <button
                    key={preset.id}
                    type="button"
                    className="world-preset"
                    onClick={() => applyPreset(preset)}
                    disabled={locked}
                  >
                    <b>{preset.label}</b>
                    <span>{preset.description}</span>
                  </button>
                ))}
              </div>
            </div>

            <label className="world-field">
              <span>Название мира</span>
              <input
                value={worldDraft.title}
                onChange={(event) => updateWorldDraft("title", event.target.value)}
                placeholder="Например: Порог Второго Неба"
                disabled={locked}
              />
            </label>

            <div className="world-field-grid">
              <label className="world-field">
                <span>Жанр</span>
                <input
                  value={worldDraft.genre}
                  onChange={(event) => updateWorldDraft("genre", event.target.value)}
                  placeholder="fantasy isekai"
                  disabled={locked}
                />
              </label>
              <label className="world-field">
                <span>Тон</span>
                <input
                  value={worldDraft.tone}
                  onChange={(event) => updateWorldDraft("tone", event.target.value)}
                  placeholder="tense"
                  disabled={locked}
                />
              </label>
            </div>

            <label className="world-field">
              <span>Размер мира</span>
              <AutoTextarea
                value={worldDraft.worldSize}
                onChange={(event) => updateWorldDraft("worldSize", event.target.value)}
                placeholder="Например: один континент; школа внутри большого магического общества; сектор галактики с десятками планет."
                disabled={locked}
              />
            </label>

            <label className="world-field">
              <span>Население</span>
              <AutoTextarea
                value={worldDraft.population}
                onChange={(event) => updateWorldDraft("population", event.target.value)}
                placeholder="Например: десятки миллионов, 5 разумных видов, сотни культур."
                disabled={locked}
              />
            </label>

            <label className="world-field">
              <span>Публичное описание мира</span>
              <AutoTextarea
                value={worldDraft.publicPremise}
                onChange={(event) => updateWorldDraft("publicPremise", event.target.value)}
                placeholder="Что можно безопасно рассказать игроку о мире без стартового квеста и скрытых секретов GM."
                disabled={locked}
              />
            </label>

            {visualPromptSnapshot(worldDraft.worldLore).some(({ prompt, imageUrl }) => prompt || imageUrl) && (
              <div className="world-visual-gallery" aria-label="Изображения мира">
                <div className="world-visual-gallery-head">
                  <span className="world-inspector-label">Изображения мира</span>
                </div>
                <div className="world-visual-gallery-grid">
                  {VISUAL_PROMPT_FIELDS.map(([field, , , outputField, outputLabel]) => {
                    const prompt = textValue(worldDraft.worldLore?.[field]);
                    const imageUrl = textValue(worldDraft.worldLore?.[outputField]);
                    const job = imageJobs[field] || {};
                    if (!prompt && !imageUrl && !job.loading && !job.error) return null;
                    return (
                      <div key={field} className="world-visual-card">
                        <div className="world-visual-card-head">
                          <b>{outputLabel}</b>
                          <span className="world-visual-state">{visualJobLabel(job, prompt, imageUrl)}</span>
                        </div>
                        {imageUrl ? (
                          <ImageThumbnail
                            src={imageUrl}
                            alt={outputLabel}
                            caption={outputLabel}
                            className="world-visual-thumb"
                          />
                        ) : (
                          <div className="world-visual-pending">
                            {visualJobLabel(job, prompt, imageUrl)}
                          </div>
                        )}
                        {job.seed != null && <span className="world-visual-seed">seed {job.seed}</span>}
                        {job.error && <div className="world-visual-error">{job.error}</div>}
                      </div>
                    );
                  })}
                </div>
              </div>
            )}

            <div className="world-bible">
              <button
                type="button"
                className="world-bible-toggle"
                onClick={() => setBibleOpen((open) => !open)}
                aria-expanded={bibleOpen}
                disabled={locked}
              >
                <span className="world-bible-toggle-label">
                  <b>Библия мира</b>
                  <small>{loreFilled ? "заполнена — можно править" : "вручную или через архитектора"}</small>
                </span>
                <span className="world-bible-caret" aria-hidden="true">{bibleOpen ? "▾" : "▸"}</span>
              </button>
              {bibleOpen && (
                <div className="world-bible-fields">
                  <p className="world-bible-hint">
                    Заполни сам или дождись архитектора. Каждый пункт — с новой строки.
                  </p>
                  <label className="world-field">
                    <span>Скрытая предпосылка (секрет GM)</span>
                    <AutoTextarea
                      value={worldDraft.worldLore?.hidden_premise || ""}
                      onChange={(event) => updateLoreText("hidden_premise", event.target.value)}
                      placeholder="То, что знает только GM и чего не должен знать игрок."
                      disabled={locked}
                    />
                  </label>
                  {VISUAL_PROMPT_FIELDS.map(([field, label, placeholder, outputField, outputLabel]) => {
                    const prompt = textValue(worldDraft.worldLore?.[field]);
                    const imageUrl = textValue(worldDraft.worldLore?.[outputField]);
                    const job = imageJobs[field] || {};
                    return (
                      <div key={field} className="world-visual-field">
                        <label className="world-field">
                          <span>{label}</span>
                          <AutoTextarea
                            value={worldDraft.worldLore?.[field] || ""}
                            onChange={(event) => updateLoreText(field, event.target.value)}
                            placeholder={placeholder}
                            disabled={locked}
                          />
                        </label>
                        <div className="world-visual-actions">
                          <span className="world-visual-state">{visualJobLabel(job, prompt, imageUrl)}</span>
                          {job.seed != null && <span className="world-visual-seed">seed {job.seed}</span>}
                        </div>
                        {job.error && <div className="world-visual-error">{job.error}</div>}
                        {imageUrl && (
                          <ImageThumbnail
                            src={imageUrl}
                            alt={outputLabel}
                            caption={outputLabel}
                            className="world-visual-thumb"
                          />
                        )}
                      </div>
                    );
                  })}
                  {LORE_PREVIEW_FIELDS.map(([field, label]) => (
                    <label key={field} className="world-field">
                      <span>{label}</span>
                      <AutoTextarea
                        value={loreFieldText(worldDraft.worldLore, field)}
                        onChange={(event) => updateLoreList(field, event.target.value)}
                        placeholder="по пункту на строку"
                        disabled={locked}
                      />
                    </label>
                  ))}
                </div>
              )}
            </div>
          </div>

          <div className="world-inspector-foot">
            <button type="submit" className="btn primary world-create-btn" disabled={worldCreateLocked}>
              Сохранить мир
            </button>
            {world?.id && (
              <div className="world-inspector-launch">
                <button
                  type="button"
                  className="btn"
                  onClick={() => onPlayWorld?.(world.id)}
                  disabled={locked}
                >
                  ▶ Играть в этом мире
                </button>
                <button
                  type="button"
                  className="btn"
                  onClick={() => onCreateStory?.(world.id)}
                  disabled={locked}
                >
                  + Создать историю
                </button>
              </div>
            )}
            <p className="world-manager-note">
              Нужны название, жанр, тон, размер мира, население и публичное описание или библия мира. Сохранение не запускает чат.
            </p>
          </div>
        </section>
      </div>

      <ArchitectDebugModal
        debug={debugOpen ? architectDebug : null}
        onClose={() => setDebugOpen(false)}
      />
    </form>
  );
}
