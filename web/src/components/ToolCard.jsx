import Icon from "./Icon.jsx";
import { useContext } from "react";
import { useTranslation } from "react-i18next";
import MarkdownText, { MarkdownInline } from "./MarkdownText.jsx";
import Spoiler from "./Spoiler.jsx";
import Tooltip from "./Tooltip.jsx";
import { ToolResultBody } from "./ToolResultCard.jsx";
import { DiceBody, gradeAccent } from "./DiceRoll.jsx";
import { NpcRosterContext } from "../npcContext.js";
import { localizeStatusLabel, StatusLabelsContext } from "../statusContext.js";
import {
  characterChangeFieldLabel,
  formatCharacterChangeValue,
  normalizeCharacterChanges,
} from "../characterChanges.js";

// Per-tool accent: references the centralized CSS palette tokens (styles.css :root)
// so the cards never carry raw hex that can drift from the theme.
const ACCENT = {
  ask_npc: "var(--player)",
  ask_npc_redo: "var(--md-del)",
  move_npc: "var(--brand-text)",
  set_npc_presence: "var(--brand-text)",
  set_npc_whereabouts: "var(--md-em)",
  set_scene: "var(--gm)",
  roll_dice: "var(--md-strong)",
  get_world_fact: "var(--md-link)",
  ask_player: "var(--player)",
  draft_world_bible: "var(--gm)",
  edit_world_bible: "var(--md-em)",
  query_world_state: "var(--md-link)",
  update_world_state: "var(--entity-note)",
  update_character: "var(--player)",
  update_player_character: "var(--player)",
  advance_time: "var(--md-em)",
  get_npc_profile: "var(--brand-text)",
  tool_search: "var(--text-3)",
  _: "var(--entity-unknown)",
};

// World-state record namespaces (update_world_state / query_world_state items).
// Russian labels + tone keyed by the backend type enum — presentation only.
const WS_TYPE = {
  fact: "ok",
  rumor: "warn",
  npc_memory: "",
  relationship: "",
  goal: "",
  public_lookup: "ok",
};
const WS_OP = {
  add: "ok",
  update: "warn",
  delete: "redo",
};

// Status keys come from the backend; labels and help follow the active UI locale.
// Tone is presentation-only and keyed by the same stable enum.
const STATUS_TONE = { known: "ok", likely: "warn", rumored: "muted", unknown: "muted" };

// Tooltip body for a bible-section chip: header + the actual entries.
function bibleTip(label, items) {
  return (
    <div className="tc-bible-tip">
      <span className="tc-bible-tip-h">{label} · {items.length}</span>
      <ul>
        {items.map((item, i) => (
          <li key={i}>{item}</li>
        ))}
      </ul>
    </div>
  );
}

// Lore sections shown as count chips in the draft_world_bible card (dev view).
const BIBLE_SECTIONS = [
  "dogmas",
  "world_laws",
  "inhabitants",
  "creatures",
  "regions",
  "power_centers",
  "religions",
  "gods",
  "cultures",
  "history",
  "economy",
  "daily_life",
  "story_hooks",
  "hidden_secrets",
  "location_rules",
  "prohibited_elements",
];
const BIBLE_VISUAL_PROMPTS = [
  "world_image_prompt_en",
  "world_map_prompt_en",
];

function toolHelp(t, name) {
  return t(`tools.help.${name}`, { defaultValue: t("tools.help.default") });
}

function fieldProps(t, key) {
  return {
    label: t(`fields.${key}.label`),
    tip: t(`fields.${key}.help`, { defaultValue: "" }),
  };
}

export function useNpcResolver() {
  const { t } = useTranslation("developer");
  const roster = useContext(NpcRosterContext);
  return (id) => {
    const n = (roster || []).find((x) => x.id === id);
    const name = n?.name || id || t("references.character");
    return { name, c: n?.color || "var(--entity-unknown)", role: n?.role || "", pronouns: n?.pronouns || "", id };
  };
}

export function NpcRef({ id }) {
  const { t } = useTranslation("developer");
  const resolve = useNpcResolver();
  const { name, c, role, pronouns } = resolve(id);
  return (
    <Tooltip
      className="tc-npc"
      tipClassName="tool-tip"
      content={[
        name,
        role ? t("references.role", { value: role }) : "",
        pronouns ? t("references.pronouns", { value: pronouns }) : "",
        id ? `id: ${id}` : "",
      ].filter(Boolean).join("\n")}
    >
      <span className="dot" style={{ "--c": c }} />
      <span style={{ color: c }}>{name}</span>
    </Tooltip>
  );
}

export function Field({ label, tip, children }) {
  const help = tip || "";
  return (
    <div className="tc-field">
      {help ? (
        <Tooltip className="tc-flabel has-tip" tipClassName="tool-tip" content={help}>
          {label}
        </Tooltip>
      ) : (
        <span className="tc-flabel">{label}</span>
      )}
      <div className="tc-fval">{children}</div>
    </div>
  );
}

export function Badge({ tone, tip, children }) {
  const hasTip = nonEmpty(tip);
  const className = "tc-badge" + (tone ? " " + tone : "") + (hasTip ? " has-tip" : "");
  if (!hasTip) return <span className={className}>{children}</span>;
  return (
    <Tooltip className={className} tipClassName="tool-tip" content={tip}>
      {children}
    </Tooltip>
  );
}

export function ActorRef({ id }) {
  const { t } = useTranslation("developer");
  if (id === "player") return <Badge tone="muted">{t("references.player")}</Badge>;
  return <NpcRef id={id} />;
}

export function ParticipantChips({ ids }) {
  const list = Array.isArray(ids) ? ids.filter(nonEmpty) : [];
  if (!list.length) return null;
  return (
    <>
      {list.map((id) => (
        <span className="tc-arrow-to" key={id}>+ <ActorRef id={id} /></span>
      ))}
    </>
  );
}

// A bordered text block for the "free-text" arguments (situation, reason, …).
export function TextBlock({ tone, children }) {
  return (
    <div className={"tc-text" + (tone ? " " + tone : "")}>
      <MarkdownText>{children}</MarkdownText>
    </div>
  );
}

export function nonEmpty(v) {
  return v != null && String(v).trim() !== "";
}

function diceTarget(args) {
  if (!nonEmpty(args.target_number)) return "";
  const rawKind = nonEmpty(args.target_kind) && args.target_kind !== "none" ? args.target_kind : "";
  const kind = rawKind || (args.roll_kind === "attack" ? "AC" : "DC");
  return `${kind} ${args.target_number}`;
}

// Builds { icon, accent, title, body } for one tool call. NpcRef is a component,
// so it can appear in the returned JSX without violating the rules of hooks.
function toolView(name, args, statusLabels, t) {
  switch (name) {
    case "ask_npc": {
      const redo = nonEmpty(args.correction);
      return {
        icon: redo ? <Icon name="refresh" size={14} /> : <Icon name="message" size={14} />,
        accent: redo ? ACCENT.ask_npc_redo : ACCENT.ask_npc,
        title: (
          <>
            {redo ? t("tools.askNpc.redoTitle") : t("tools.askNpc.title")}
            <NpcRef id={args.npc_id} />
          </>
        ),
        body: (
          <>
            {nonEmpty(args.situation) && (
              <Field {...fieldProps(t, "situation")}>
                <TextBlock>{args.situation}</TextBlock>
              </Field>
            )}
            {redo && (
              <Field {...fieldProps(t, "gmCorrection")}>
                <TextBlock tone="redo">{args.correction}</TextBlock>
              </Field>
            )}
          </>
        ),
      };
    }

    case "move_npc":
    case "set_npc_presence": {
      const present = args.present;
      return {
        icon: <Icon name="walk" size={14} />,
        accent: ACCENT.move_npc,
        title: (
          <>
            {t("tools.npcPresence.title")}<NpcRef id={args.npc_id} />
          </>
        ),
        body: (
          <>
            <div className="tc-chips">
              {present === true && <Badge tone="ok" tip={t("tools.npcPresence.presentHelp")}>{t("tools.npcPresence.present")}</Badge>}
              {present === false && <Badge tone="muted" tip={t("tools.npcPresence.absentHelp")}>{t("tools.npcPresence.absent")}</Badge>}
              {args.visible === true && <Badge tip={t("tools.npcPresence.visibleHelp")}>{t("tools.npcPresence.visible")}</Badge>}
              {args.visible === false && <Badge tone="muted" tip={t("tools.npcPresence.hiddenHelp")}>{t("tools.npcPresence.hidden")}</Badge>}
              {args.can_hear === true && <Badge tip={t("tools.npcPresence.hearsHelp")}>{t("tools.npcPresence.hears")}</Badge>}
              {args.can_hear === false && <Badge tone="muted" tip={t("tools.npcPresence.deafHelp")}>{t("tools.npcPresence.deaf")}</Badge>}
            </div>
            {nonEmpty(args.location) && <Field {...fieldProps(t, "location")}><MarkdownInline>{args.location}</MarkdownInline></Field>}
            {nonEmpty(args.activity) && <Field {...fieldProps(t, "activity")}><MarkdownInline>{args.activity}</MarkdownInline></Field>}
            {nonEmpty(args.attitude) && <Field {...fieldProps(t, "attitude")}><MarkdownInline>{args.attitude}</MarkdownInline></Field>}
            {nonEmpty(args.reason) && (
              <Field {...fieldProps(t, "reason")}>
                <TextBlock>{args.reason}</TextBlock>
              </Field>
            )}
          </>
        ),
      };
    }

    case "set_npc_whereabouts": {
      const place = args.location_name || args.location_id;
      return {
        icon: <Icon name="pin" size={14} />,
        accent: ACCENT.set_npc_whereabouts,
        title: (
          <>
            {t("tools.whereabouts.title")}<NpcRef id={args.npc_id} />
          </>
        ),
        body: (
          <>
            <div className="tc-chips">
              <Badge tone={STATUS_TONE[args.status] || "muted"} tip={t(`status.help.${args.status}`, { defaultValue: t("status.help.default") })}>
                {localizeStatusLabel(t, args.status, statusLabels)}
              </Badge>
              {nonEmpty(place) && <Badge tip={t("tools.whereabouts.placeHelp")}>{place}</Badge>}
            </div>
            {nonEmpty(args.source) && <Field {...fieldProps(t, "source")}><MarkdownInline>{args.source}</MarkdownInline></Field>}
            {nonEmpty(args.details) && (
              <Field {...fieldProps(t, "details")}>
                <TextBlock>{args.details}</TextBlock>
              </Field>
            )}
          </>
        ),
      };
    }

    case "set_scene": {
      const npcs = args.present_npcs || [];
      const exits = args.exits || [];
      const items = args.items || [];
      const constraints = args.constraints || [];
      return {
        icon: <Icon name="map" size={14} />,
        accent: ACCENT.set_scene,
        title: t("tools.setScene.title"),
        body: (
          <>
            {nonEmpty(args.title) && (
              <Tooltip className="tc-scene-title" tipClassName="tool-tip" content={t("tools.setScene.titleHelp")}>
                {args.title}
              </Tooltip>
            )}
            {nonEmpty(args.description) && <TextBlock>{args.description}</TextBlock>}
            {npcs.length > 0 && (
              <Field {...fieldProps(t, "inScene")}>
                <div className="tc-chips">
                  {npcs.map((id) => <NpcRef key={id} id={id} />)}
                </div>
              </Field>
            )}
            {exits.length > 0 && (
              <Field {...fieldProps(t, "exits")}>
                <div className="tc-list">
                  {exits.map((e, i) => (
                    <Tooltip
                      as="div"
                      className="tc-exit"
                      tipClassName="tool-tip"
                      content={[
                        e.id ? `id: ${e.id}` : "",
                        e.destination ? t("tools.setScene.exitDestination", { value: e.destination }) : "",
                        e.visible === false ? t("tools.setScene.exitHidden") : t("tools.setScene.exitVisible"),
                        e.blocked_by ? t("tools.setScene.exitBlocked", { value: e.blocked_by }) : "",
                      ].filter(Boolean).join("\n")}
                      key={e.id || i}
                    >
                      <span>{e.name || e.id || t("tools.setScene.exit")}</span>
                      {nonEmpty(e.destination) && (
                        <>
                          <span className="arr">→</span>
                          <span>{e.destination}</span>
                        </>
                      )}
                      {nonEmpty(e.blocked_by) && <Badge tone="redo" tip={t("tools.setScene.blockedHelp")}>{e.blocked_by}</Badge>}
                    </Tooltip>
                  ))}
                </div>
              </Field>
            )}
            {items.length > 0 && (
              <Field {...fieldProps(t, "items")}>
                <div className="tc-chips">
                  {items.map((it, i) => (
                    <Badge
                      key={it.id || i}
                      tip={[
                        it.id ? `id: ${it.id}` : "",
                        it.location ? t("tools.setScene.itemLocation", { value: it.location }) : "",
                        it.owner ? t("tools.setScene.itemOwner", { value: it.owner }) : "",
                        it.portable === true ? t("tools.setScene.itemPortable") : it.portable === false ? t("tools.setScene.itemFixed") : "",
                        it.details || "",
                      ].filter(Boolean).join("\n")}
                    >
                      {it.name || it.id || t("tools.setScene.item")}
                    </Badge>
                  ))}
                </div>
              </Field>
            )}
            {constraints.length > 0 && (
              <Field {...fieldProps(t, "constraints")}>
                <div className="tc-list">
                  {constraints.map((c, i) => (
                    <Tooltip
                      as="div"
                      className="tc-exit"
                      tipClassName="tool-tip"
                      content={t("tools.setScene.constraintHelp")}
                      key={i}
                    >
                      · <MarkdownInline>{c}</MarkdownInline>
                    </Tooltip>
                  ))}
                </div>
              </Field>
            )}
            {nonEmpty(args.tension) && <Field {...fieldProps(t, "tension")}><MarkdownInline>{args.tension}</MarkdownInline></Field>}
            {nonEmpty(args.reason) && (
              <Field {...fieldProps(t, "reason")}>
                <TextBlock>{args.reason}</TextBlock>
              </Field>
            )}
          </>
        ),
      };
    }

    case "roll_dice": {
      const target = diceTarget(args);
      return {
        icon: <Icon name="d20" size={14} />,
        accent: ACCENT.roll_dice,
        title: t("tools.rollDice.title"),
        body: (
          <>
            <div className="tc-dice">
              <Tooltip className="tc-notation" tipClassName="tool-tip" content={t("tools.rollDice.notationHelp")}>
                {args.notation || "—"}
              </Tooltip>
              {nonEmpty(args.roll_kind) && (
                <Tooltip className="tc-badge" tipClassName="tool-tip" content={t("tools.rollDice.kindHelp")}>
                  {args.roll_kind}
                </Tooltip>
              )}
              {nonEmpty(target) && (
                <Tooltip className="tc-badge warn" tipClassName="tool-tip" content={t("tools.rollDice.targetHelp")}>
                  {target}
                </Tooltip>
              )}
            </div>
            {nonEmpty(args.check_name) && (
              <Field label={t("fields.check.label")}>
                <MarkdownInline>{args.check_name}</MarkdownInline>
              </Field>
            )}
            {nonEmpty(args.reason) && (
              <Field {...fieldProps(t, "purpose")}>
                <TextBlock>{args.reason}</TextBlock>
              </Field>
            )}
          </>
        ),
      };
    }

    case "get_world_fact": {
      return {
        icon: <Icon name="book" size={14} />,
        accent: ACCENT.get_world_fact,
        title: t("tools.getWorldFact.title"),
        body: (
          <Tooltip className="tc-query" tipClassName="tool-tip" content={t("tools.getWorldFact.queryHelp")}>
            <MarkdownInline>{args.query || "—"}</MarkdownInline>
          </Tooltip>
        ),
      };
    }

    case "ask_player": {
      const options = Array.isArray(args.options) ? args.options : [];
      return {
        icon: <Icon name="target" size={14} />,
        accent: ACCENT.ask_player,
        title: t("tools.askPlayer.title"),
        body: (
          <>
            {nonEmpty(args.question) && (
              <Tooltip className="tc-ask-q" tipClassName="tool-tip" content={t("tools.askPlayer.questionHelp")}>
                <MarkdownInline>{args.question}</MarkdownInline>
              </Tooltip>
            )}
            {options.length > 0 && (
              <div className="tc-options">
                {options.map((o, i) => (
                  <div className="tc-option" key={i}>
                    <span className="tc-option-label">{nonEmpty(o.label) ? o.label : t("tools.askPlayer.option", { number: i + 1 })}</span>
                    {nonEmpty(o.message) && (
                      <span className="tc-option-msg"><MarkdownInline>{o.message}</MarkdownInline></span>
                    )}
                  </div>
                ))}
              </div>
            )}
          </>
        ),
      };
    }

    case "update_world_state": {
      const items = Array.isArray(args.items) ? args.items : [];
      return {
        icon: <Icon name="sparkles" size={14} />,
        accent: ACCENT.update_world_state,
        title: t("tools.updateWorldState.title"),
        body: items.length ? (
          <div className="tc-ws-list">
            {items.map((it, i) => {
              const opName = it.op || "add";
              const opTone = WS_OP[opName] || WS_OP.add;
              const typeName = it.type || "record";
              const typeTone = WS_TYPE[it.type] || "";
              return (
                <div className="tc-ws-item" key={i}>
                  <div className="tc-chips">
                    <Badge tone={opTone}>{t(`worldState.operations.${opName}`, { defaultValue: opName })}</Badge>
                    <Badge tone={typeTone}>{t(`worldState.types.${typeName}`, { defaultValue: typeName })}</Badge>
                    {nonEmpty(it.scope) && <Badge tone="muted">{t(`worldState.scopes.${it.scope}`, { defaultValue: it.scope })}</Badge>}
                    {nonEmpty(it.npc_id) && <NpcRef id={it.npc_id} />}
                    {nonEmpty(it.target) && (
                      <span className="tc-arrow-to">→ {it.target === "player" ? t("references.player") : <NpcRef id={it.target} />}</span>
                    )}
                    <ParticipantChips ids={it.participants} />
                    {nonEmpty(it.importance) && <Badge tone="warn">{it.importance}</Badge>}
                  </div>
                  {nonEmpty(it.text) && <TextBlock>{it.text}</TextBlock>}
                  {nonEmpty(it.known_name) && <Field label={t("fields.knownName.label")}><MarkdownInline>{it.known_name}</MarkdownInline></Field>}
                </div>
              );
            })}
          </div>
        ) : (
          <div className="tc-text">{t("tools.updateWorldState.empty")}</div>
        ),
      };
    }

    case "query_world_state": {
      return {
        icon: <Icon name="search" size={14} />,
        accent: ACCENT.query_world_state,
        title: t("tools.queryWorldState.title"),
        body: (
          <>
            <div className="tc-chips">
              {nonEmpty(args.scope) && <Badge tone="muted" tip={t("tools.queryWorldState.scopeHelp")}>{t(`worldState.scopes.${args.scope}`, { defaultValue: args.scope })}</Badge>}
              {nonEmpty(args.npc_id) && <NpcRef id={args.npc_id} />}
            </div>
            <Tooltip className="tc-query" tipClassName="tool-tip" content={t("tools.queryWorldState.queryHelp")}>
              <MarkdownInline>{args.query || "—"}</MarkdownInline>
            </Tooltip>
          </>
        ),
      };
    }

    case "update_character":
    case "update_player_character": {
      const target = name === "update_player_character" ? "player" : (args.target || "player");
      const isNpc = target === "npc";
      const fields = (args.fields && typeof args.fields === "object") ? args.fields : {};
      const keys = Object.keys(fields);
      return {
        icon: <Icon name="shield" size={14} />,
        accent: ACCENT.update_character,
        title: isNpc ? (
          <>
            {t("tools.updateCharacter.npcTitle")}
            {nonEmpty(args.npc_id) && <NpcRef id={args.npc_id} />}
          </>
        ) : t("tools.updateCharacter.playerTitle"),
        body: (
          <>
            {keys.length ? (
              keys.map((k) => (
                <Field key={k} label={k}>
                  {typeof fields[k] === "object"
                    ? <code>{JSON.stringify(fields[k])}</code>
                    : <MarkdownInline>{String(fields[k])}</MarkdownInline>}
                </Field>
              ))
            ) : (
              <div className="tc-text">{t("common.noChanges")}</div>
            )}
            {nonEmpty(args.reason) && <Field {...fieldProps(t, "reason")}><TextBlock>{args.reason}</TextBlock></Field>}
          </>
        ),
      };
    }

    case "advance_time": {
      return {
        icon: <Icon name="clock" size={14} />,
        accent: ACCENT.advance_time,
        title: t("tools.advanceTime.title"),
        body: (
          <>
            <div className="tc-chips">
              <Badge tone="warn" tip={t("tools.advanceTime.minutesHelp")}>{t("time.plusMinutes", { count: args.minutes ?? 0 })}</Badge>
            </div>
            {nonEmpty(args.reason) && <Field {...fieldProps(t, "reason")}><TextBlock>{args.reason}</TextBlock></Field>}
          </>
        ),
      };
    }

    case "get_npc_profile": {
      const fields = Array.isArray(args.fields) ? args.fields : [];
      return {
        icon: <Icon name="user" size={14} />,
        accent: ACCENT.get_npc_profile,
        title: (
          <>
            {t("tools.getNpcProfile.title")}<NpcRef id={args.npc_id} />
          </>
        ),
        body: (
          <div className="tc-chips">
            <Badge tone="muted" tip={t("tools.getNpcProfile.presetHelp")}>{t(`profilePresets.${args.preset || "visible"}`, { defaultValue: args.preset })}</Badge>
            {fields.map((f) => <Badge key={f}>{f}</Badge>)}
          </div>
        ),
      };
    }

    case "tool_search": {
      return {
        icon: <Icon name="sliders" size={14} />,
        accent: ACCENT.tool_search,
        title: t("tools.toolSearch.title"),
        body: (
          <Tooltip className="tc-query" tipClassName="tool-tip" content={t("tools.toolSearch.queryHelp")}>
            <MarkdownInline>{args.query || "—"}</MarkdownInline>
          </Tooltip>
        ),
      };
    }

    case "draft_world_bible": {
      const lore = args.world_lore && typeof args.world_lore === "object" ? args.world_lore : {};
      const sections = BIBLE_SECTIONS
        .map((field) => [
          field,
          Array.isArray(lore[field])
            ? lore[field].filter((item) => typeof item === "string" && item.trim())
            : [],
        ])
        .filter(([, items]) => items.length > 0);
      return {
        icon: <Icon name="scroll" size={14} />,
        accent: ACCENT.draft_world_bible,
        title: t("tools.draftBible.title"),
        body: (
          <>
            {nonEmpty(args.title) && (
              <Tooltip className="tc-scene-title" tipClassName="tool-tip" content={t("tools.draftBible.titleHelp")}>
                {args.title}
              </Tooltip>
            )}
            <div className="tc-chips">
              {nonEmpty(args.genre) && <Badge tone="muted">{args.genre}</Badge>}
              {nonEmpty(args.tone) && <Badge tone="muted">{args.tone}</Badge>}
            </div>
            {nonEmpty(args.world_size) && <Field label={t("bible.set.world_size")}><TextBlock>{args.world_size}</TextBlock></Field>}
            {nonEmpty(args.population) && <Field label={t("bible.set.population")}><TextBlock>{args.population}</TextBlock></Field>}
            {nonEmpty(args.public_premise) && (
              <Field label={t("bible.set.public_premise")}><TextBlock>{args.public_premise}</TextBlock></Field>
            )}
            {nonEmpty(lore.hidden_premise) && (
              <Field label={t("bible.set.hidden_premise")}><TextBlock tone="redo">{lore.hidden_premise}</TextBlock></Field>
            )}
            {BIBLE_VISUAL_PROMPTS.map((field) =>
              nonEmpty(lore[field]) ? (
                <Field key={field} label={t(`bible.set.${field}`)}>
                  <TextBlock>{lore[field]}</TextBlock>
                </Field>
              ) : null
            )}
            {sections.length > 0 && (
              <Field label={t("bible.sectionsLabel")}>
                <div className="tc-chips">
                  {sections.map(([field, items]) => (
                    <Badge key={field} tip={bibleTip(t(`bible.sections.${field}`), items)}>
                      {t(`bible.sections.${field}`)}: {items.length}
                    </Badge>
                  ))}
                </div>
              </Field>
            )}
          </>
        ),
      };
    }

    case "edit_world_bible": {
      const set = args.set && typeof args.set === "object" ? args.set : {};
      const setKeys = Object.keys(set).filter((k) => nonEmpty(set[k]));
      const ops = [
        ["add", args.add],
        ["remove", args.remove],
        ["replace", args.replace],
      ].map(([op, obj]) => [
        op,
        obj && typeof obj === "object"
          ? Object.entries(obj).filter(([, v]) => Array.isArray(v) && v.length)
          : [],
      ]);
      const empty = setKeys.length === 0 && ops.every(([, entries]) => entries.length === 0);
      return {
        icon: <Icon name="pen" size={14} />,
        accent: ACCENT.edit_world_bible,
        title: t("tools.editBible.title"),
        body: (
          <>
            {setKeys.map((k) => (
              <Field key={`set-${k}`} label={t(`bible.set.${k}`, { defaultValue: k })}>
                <TextBlock>{String(set[k])}</TextBlock>
              </Field>
            ))}
            {ops.map(([op, entries]) =>
              entries.length === 0 ? null : (
                <Field key={op} label={t(`bible.operations.${op}`)}>
                  <div className="tc-chips">
                    {entries.map(([section, items]) => (
                      <Badge key={section} tip={bibleTip(t(`bible.sections.${section}`, { defaultValue: section }), items)}>
                        {t(`bible.sections.${section}`, { defaultValue: section })}: {items.length}
                      </Badge>
                    ))}
                  </div>
                </Field>
              )
            )}
            {empty && <div className="tc-text">{t("common.noChanges")}</div>}
          </>
        ),
      };
    }

    default: {
      const entries = Object.entries(args || {});
      return {
        icon: <Icon name="sliders" size={14} />,
        accent: ACCENT._,
        title: <>{t("tools.fallback.title")} <code>{name}</code></>,
        body: entries.length ? (
          entries.map(([k, v]) => (
            <Field key={k} label={k}>
              {typeof v === "object" ? <code>{JSON.stringify(v)}</code> : <MarkdownInline>{String(v)}</MarkdownInline>}
            </Field>
          ))
        ) : (
          <div className="tc-text">{t("tools.fallback.noArguments")}</div>
        ),
      };
    }
  }
}

// Player-friendly minutes → "1 дн 2 ч 30 мин" (compact, no zero parts).
function prettyElapsed(minutes, t) {
  const m = Math.max(0, Math.round(Number(minutes) || 0));
  if (m === 0) return t("time.lessThanMinute");
  const days = Math.floor(m / 1440);
  const hours = Math.floor((m % 1440) / 60);
  const mins = m % 60;
  const parts = [];
  if (days) parts.push(t("time.daysShort", { count: days }));
  if (hours) parts.push(t("time.hoursShort", { count: hours }));
  if (mins) parts.push(t("time.minutesShort", { count: mins }));
  return parts.join(" ");
}

// Compact, player-facing time advance (used when tool internals are hidden).
function PlayerTimeCard({ payload }) {
  const { t } = useTranslation("developer");
  const p = payload || {};
  const current = p.current && typeof p.current === "object" ? p.current : {};
  const now = [current.current_date_label, current.time_of_day].filter(nonEmpty).join(" · ");
  return (
    <div className="play-card time" style={{ "--tc": "var(--md-em)" }}>
      <span className="play-ico" aria-hidden="true"><Icon name="clock" size={16} /></span>
      <span className="play-main">
        <b>{t("playerCards.timeElapsed", { value: prettyElapsed(p.elapsed_minutes, t) })}</b>
        {nonEmpty(now) && <span className="play-sub">{now}</span>}
      </span>
    </div>
  );
}

function PlayerChangeList({ payload, t, allowUpdatedFallback = true }) {
  const p = payload || {};
  const changes = normalizeCharacterChanges(p);
  const updated = Array.isArray(p.updated) ? p.updated : [];
  if (!changes.length && (!allowUpdatedFallback || !updated.length)) return null;
  return changes.length > 0 ? (
    <span className="play-change-list">
      {changes.map((change, changeIndex) => {
        const added = change.added.map((value) => formatCharacterChangeValue(value, t));
        const removed = change.removed.map((value) => formatCharacterChangeValue(value, t));
        const hasListDelta = added.length > 0 || removed.length > 0;
        return (
          <span className="play-change" key={`${change.field}-${changeIndex}`}>
            <span className="play-change-field">
              {characterChangeFieldLabel(change.field, t)}
            </span>
            {added.map((value, index) => (
              <span className="play-change-value is-added" key={`add-${index}-${value}`}>
                <span aria-hidden="true">+</span> {value}
              </span>
            ))}
            {removed.map((value, index) => (
              <span className="play-change-value is-removed" key={`remove-${index}-${value}`}>
                <span aria-hidden="true">−</span> {value}
              </span>
            ))}
            {!hasListDelta && (
              <span className="play-change-value is-changed">
                <span>{formatCharacterChangeValue(change.before, t)}</span>
                <span className="play-change-arrow" aria-hidden="true">→</span>
                <span>{formatCharacterChangeValue(change.after, t)}</span>
              </span>
            )}
          </span>
        );
      })}
    </span>
  ) : (
    <span className="play-sub">
      {updated.map((field) => characterChangeFieldLabel(field, t)).join(", ")}
    </span>
  );
}

// Compact, player-facing character-sheet update.
function PlayerSheetCard({ payload }) {
  const { t } = useTranslation("developer");
  return (
    <div className="play-card sheet" style={{ "--tc": "var(--player)" }}>
      <span className="play-ico" aria-hidden="true"><Icon name="shield" size={16} /></span>
      <span className="play-main">
        <b>{t("playerCards.sheetUpdated")}</b>
        <PlayerChangeList payload={payload} t={t} />
      </span>
    </div>
  );
}

const PLAYER_ACTION_ICONS = {
  roll_dice: "d20",
  advance_time: "clock",
  long_rest: "clock",
  update_character: "shield",
  update_player_character: "shield",
  take_item: "plus",
  drop_item: "minus",
  cast_spell: "sparkles",
  set_scene: "pin",
  move_player: "walk",
  travel_to: "map",
  relocate_player: "map",
  create_passage: "branch",
  set_passage_state: "branch",
};

function playerActionDetail(name, args, result) {
  if (name === "take_item" || name === "drop_item") return result?.name || args?.name || "";
  if (name === "cast_spell") return result?.spell || result?.name || args?.name || "";
  if (name === "travel_to" || name === "move_player" || name === "relocate_player") {
    return result?.title || result?.destination_name || "";
  }
  if (name === "create_passage") return result?.label || "";
  return "";
}

function PlayerActionCard({ name, args, result, hasResult }) {
  const { t } = useTranslation("developer");
  const failed = hasResult && (result?.ok === false || !!result?.error);
  const phase = !hasResult ? "pending" : failed ? "failed" : "completed";
  const detail = playerActionDetail(name, args, result);
  const defaultTitle = t(`playerCards.actionState.${phase}`);
  const title = t(`playerCards.actions.${name}.${phase}`, { defaultValue: defaultTitle });
  return (
    <div
      className={`play-card action is-${phase}`}
      style={{ "--tc": failed ? "var(--md-del)" : "var(--player)" }}
      aria-live="polite"
    >
      <span className="play-ico" aria-hidden="true">
        <Icon
          name={!hasResult ? (PLAYER_ACTION_ICONS[name] || "dots") : failed ? "x" : "check"}
          size={16}
        />
      </span>
      <span className="play-main">
        <b>{title}</b>
        {detail && <span className="play-sub">{detail}</span>}
        {hasResult && <PlayerChangeList payload={result} t={t} allowUpdatedFallback={false} />}
      </span>
    </div>
  );
}

// `result` is the tool's outcome payload (attached by the timeline once it arrives),
// rendered under the request inside the SAME card so call+result read as one unit.
// `mode` controls how much is shown:
//   'full'   — request + raw JSON + result (developer view)
//   'result' — header + result only (no request, no raw call JSON)
//   'player' — compact, player-facing result (dice / time / sheet)
export default function ToolCard({ name, args = {}, result, resultLive, rollId, mode = "full" }) {
  const { t } = useTranslation("developer");
  const statusLabels = useContext(StatusLabelsContext);
  const view = toolView(name, args || {}, statusLabels, t);
  const hasResult = result != null;
  const isDice = name === "roll_dice" && hasResult;
  const accent = isDice ? gradeAccent(result.grade) : view.accent;

  if (mode === "player") {
    if (name === "roll_dice" && hasResult) {
      return (
        <div className="tool-card play-dice" style={{ "--tc": gradeAccent(result.grade) }}>
          <DiceBody roll={result} animate={resultLive} rollId={rollId} />
        </div>
      );
    }
    if (name === "advance_time" && hasResult) return <PlayerTimeCard payload={result} />;
    if (name === "update_player_character" && hasResult) return <PlayerSheetCard payload={result} />;
    if (
      name === "update_character" &&
      hasResult &&
      (result?.target || args?.target || "player") === "player"
    ) {
      return <PlayerSheetCard payload={result} />;
    }
    return <PlayerActionCard name={name} args={args} result={result} hasResult={hasResult} />;
  }

  // 'full'   — developer view: rich body + raw JSON spoilers + raw tool name.
  // 'detail' — player view: the same rich body, no dev-only JSON/raw-name noise.
  const showBody = mode === "full" || mode === "detail";
  const showRaw = mode === "full";
  return (
    <div className={"tool-card" + (hasResult ? " has-result" : "")} style={{ "--tc": accent }}>
      <div className="tc-hd">
        <Tooltip className="tc-ico" tipClassName="tool-tip" content={toolHelp(t, name)}>
          {view.icon}
        </Tooltip>
        <span className="tc-title">{view.title}</span>
        {showRaw && (
          <Tooltip className="tc-name" tipClassName="tool-tip" content={`${t("raw.toolName", { name })}\n${toolHelp(t, name)}`}>
            {name}
          </Tooltip>
        )}
      </div>
      {showBody && <div className="tc-body">{view.body}</div>}
      {showRaw && (
        <Spoiler label={t("raw.callJson")}>
          <MarkdownText>{"```json\n" + JSON.stringify(args, null, 2) + "\n```"}</MarkdownText>
        </Spoiler>
      )}
      {hasResult && (
        <div className="tc-result-sec">
          <div className="tc-result-divider">{t("common.result")}</div>
          {isDice ? (
            <DiceBody roll={result} animate={resultLive} rollId={rollId} />
          ) : (
            <>
              <div className="tc-body"><ToolResultBody name={name} payload={result} /></div>
              {showRaw && (
                <Spoiler label={t("raw.resultJson")}>
                  <MarkdownText>{"```json\n" + JSON.stringify(result, null, 2) + "\n```"}</MarkdownText>
                </Spoiler>
              )}
            </>
          )}
        </div>
      )}
    </div>
  );
}
