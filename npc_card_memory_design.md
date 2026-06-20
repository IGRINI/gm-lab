# NPC Card, Memory, And Time Design

This document captures the intended design before implementation. It is a planning contract for the next NPC-card and memory work, not a description of the current finished code.

## Goals

- Make NPCs behave like stable characters, not just speech generators.
- Keep permanent character traits separate from dynamic memories, relationships, goals, and player-known facts.
- Avoid leaking secrets to player-facing narration or to the GM model when a narrower lookup is enough.
- Keep tool calls token-efficient through presets and field selection.
- Prepare the same model for a future player-character card with abilities, stats, appearance, and identity.

## Core Split

Use three layers:

1. Static NPC card: stable author data about who the NPC is.
2. Dynamic state records: what changed during play, indexed and scoped.
3. Visibility/query tools: what the GM, NPC, player, or debug UI may retrieve right now.

The important mental model: relationships and NPC memories can live in a shared indexed store, but they are not "world truth" by default. They are records owned by an NPC or shared between participants.

## NPC Card Fields

These are the fields we want on the NPC card.

| Field | Meaning | Storage | Default Exposure |
| --- | --- | --- | --- |
| `id` | Stable internal npc_id. Lowercase ASCII snake_case. | Static NPC card. | Internal/tool only. |
| `name` | True/canonical name known to the GM. | Static NPC card. | Only player-visible if identity is known. |
| `public_label` | What the player can call this NPC before knowing the name, e.g. "трактирщик", "женщина у двери". | Static NPC card or derived from role/appearance. | Visible. |
| `known_name` | Optional player-known display name once introduced. Usually equals `name` after contact. | Dynamic player-known state, not necessarily card. | Visible after established. |
| `role` | Social/world role: трактирщик, капитан стражи, служанка, вор, жрец. | Static NPC card. | Usually visible if obvious; otherwise via `public_label`. |
| `gender` | Russian grammatical gender/pronoun marker: M, F, N, PL, OTHER/custom. | Static NPC card. | Visible only as grammar, not a secret fact. |
| `age` | Free text containing actual and apparent age if relevant, e.g. "Фактически 1000 лет; выглядит примерно на 25." | Static NPC card. | Full value is GM/NPC-private unless established. Player sees only apparent age if visible. |
| `physical_type` | Combined species/type/size/body impression, e.g. "огромный огр", "маленький дракончик", "худой пожилой человек". | Static NPC card. | Visible if observable. |
| `distinctive_features` | Recognizable marks/cues: scars, smell, gait, jewelry, accent, unusual clothes, visible symbols. | Static NPC card. | Visible if observable; can be used for clues. |
| `life_status` | Coarse state: alive, dead, missing, unconscious, dying, incapacitated. | Static/current NPC card. | Only visible if known/obvious. |
| `life_status_note` | Free text details, including factual death date/time and what is known. Example: "Фактически умер 17 день Туманных жатв, 03:20; игрок пока знает только, что он пропал." | Static/current NPC card plus state records for discovered parts. | GM/private by default. |
| `condition` | Current physical/mental condition: "ранен в плечо", "отравлен", "истощен", "паникует", "здоров". | Current NPC card for current baseline; state records for discovered/changed condition. | Visible only if observable or known. |
| `personality` | Character decision style: cautious, proud, impulsive, kind but secretive. | Static NPC card. | GM/NPC; used by NPC model. |
| `values` | What the NPC protects: family, reputation, money, law, safety, secret, power. | Static NPC card. | GM/NPC; may become known through play. |
| `habits` | Behavioral habits/tells: counts coins, rubs ring, avoids eye contact, fixes apron. | Static NPC card. | Visible when used in scene. |
| `pressure_response` | How the NPC reacts under pressure: bargains, lies, gets angry, calls guards, gives partial truth. | Static NPC card. | GM/NPC. |
| `boundaries` | What the NPC almost will not do without strong cause: betray daughter, allow cellar access, attack first. | Static NPC card. | GM/NPC. |
| `voice` | Speech manner: short, rude, ornate, whispering, evasive, uses sayings. | Static NPC card. | Used by NPC model; visible through speech. |
| `goals` | Current baseline goals or active agenda. | Static/current card for baseline; dynamic `goal` records for changes. | NPC/GM; player only if learned. |
| `knowledge` | Starting knowledge the NPC may rely on. | Static NPC card. | NPC/GM, not automatically player-visible. |
| `secret` | Private secret. | Static NPC card. | NPC model/debug only; not returned by normal GM profile lookups. |

## D&D Mechanics Fields

Only include D&D fields that are directly useful for checks, saves, perception, or combat. Do not add separate `species`, `type`, `size`, or `alignment`; `physical_type` covers those better for RP.

| Field | Meaning | Storage | Notes |
| --- | --- | --- | --- |
| `abilities` | STR/DEX/CON/INT/WIS/CHA values or modifiers. | Static NPC mechanics card. | Useful for NPC checks and opposed rolls. |
| `skills` | Only notable skills/modifiers. | Static NPC mechanics card. | Omit empty/unimportant skills. |
| `saving_throws` | Only notable saves/modifiers. | Static NPC mechanics card. | Omit if no combat/mechanical need. |
| `passive_perception` | Passive Perception. | Static NPC mechanics card. | Useful for stealth/noticing. |
| `ac` | Armor Class. | Static NPC mechanics card. | Optional for non-combat NPCs. |
| `hp` | Hit points/current hit points if tracked. | Current NPC mechanics/status. | If damage matters, this becomes dynamic. |
| `speed` | Movement speed. | Static NPC mechanics card. | Optional. |
| `senses` | Darkvision, blindsight, tremorsense, special sight/hearing/smell. | Static NPC mechanics card. | Useful for scene logic. |
| `languages` | Known languages. | Static NPC mechanics card. | Useful for communication constraints. |

Mechanics should be retrievable separately. The GM should not need to fetch the entire NPC card just to decide a roll modifier.

Player-facing narration must not reveal raw NPC mechanics by default. In D&D-style play, NPC stat blocks are GM/internal data unless the GM explicitly grants a companion/stat block. The GM should use mechanics for resolution and describe only observable effects to the player: wounded, barely standing, armor deflects a hit, unusually quick, hard to read, and similar.

## Identity And Names

The GM must not automatically reveal canonical NPC names just because the internal card has them.

Rules:

- If the player has not met or identified an NPC, describe them using `public_label`, `role`, visible `physical_type`, or `distinctive_features`.
- If the NPC introduces themselves, another character names them, a document identifies them, or the player already knows them, then the player may see `name`/`known_name`.
- If the player asks "who is that?", use visible description first unless the identity is established.
- Internal IDs and canonical names can be used by tools, but player-facing text should use the player-known label.

Examples:

- Before contact: "у стойки стоит плотный трактирщик с медным кольцом", not "Борин".
- After introduction: "Борин".
- If another NPC says it: write a player-known state record such as "Игрок знает со слов Борина, что служанку зовут Лиза."

Needed concept:

`entity_knowledge` / entity-scoped state record: a durable note about what an actor knows about a specific entity.

Example:

```json
{
  "type": "fact",
  "scope": "shared",
  "npc_id": "borin",
  "target": "player",
  "entity_id": "lysa",
  "known_name": "Лиза",
  "text": "Игрок знает со слов Борина: служанку зовут Лиза, ей 32, хотя внешне она выглядит моложе."
}
```

This means Borin told the player something about Liza. Liza does not automatically know that Borin revealed it.

## Dynamic Memory Records

Dynamic records should remain indexed and scoped. They should not be baked into the static card unless they become a lasting card edit.

Record types:

- `fact`: established objective truth or visible stable state.
- `rumor`: unverified testimony, suspicion, accusation, lead, or claim.
- `npc_memory`: what one NPC remembers, saw, was told, promised, hid, learned, or should later act on.
- `relationship`: ongoing attitude, trust, debt, leverage, fear, loyalty, hatred, love, suspicion, or obligation toward a target.
- `goal`: current want, plan, intent, agenda, or task.

Scopes:

- `public`: anyone can know.
- `shared`: only `npc_id` and `target` know.
- `npc`: only `npc_id` knows/thinks/remembers.
- `gm`: hidden author truth not known by characters unless discovered.

Relationship ownership:

- A relationship is usually one active record per `owner/npc_id + target + scope`.
- Complex feelings belong in the single relationship text string, not in enum values.
- Update the existing relationship record when it changes; do not add duplicates.

Player-known facts about an NPC:

- Do not store these only in the NPC's own memory.
- A different NPC can reveal a fact about them.
- Store as player-visible/shared entity-scoped state with source information.

## Profile Retrieval Tool

Add a `get_npc_profile`-style tool for the GM model. It should not return the full private card by default.

Shape:

```json
{
  "npc_id": "npc_id",
  "preset": "visible",
  "fields": ["abilities", "passive_perception"]
}
```

`preset` and `fields` can be combined. If both are present, return the union, still filtered by visibility rules.

Presets:

| Preset | Returns | Must Not Return |
| --- | --- | --- |
| `visible` | Player-observable identity: player-known name/label, role if visible, apparent age, physical_type, distinctive_features, visible condition/life_status. | Secret, hidden age, hidden death facts, goals, private knowledge. |
| `social` | personality, values, habits, pressure_response, boundaries, voice. | Secret and hidden factual knowledge unless explicitly known. |
| `mechanics` | abilities, skills, saving_throws, passive_perception, ac, hp, speed, senses, languages. | Secret, goals, private knowledge. |
| `status` | life_status, life_status_note filtered by visibility, condition, hp/current injury if known or visible. | Hidden cause/time of death unless discovered. |
| `identity` | name only if known, public_label, role, age/player-known age, physical_type, distinctive_features. | Hidden true name/age if not established. |
| `private_npc` | Full NPC-private card slice needed to play the NPC. | Not exposed as a normal GM tool preset; restricted to NPC model/debug. |

Field selection:

- `fields=["abilities"]` should return only ability data.
- `fields=["passive_perception", "senses"]` should return only those mechanics.
- `fields=["age"]` is GM-internal lookup data. The GM must not expose hidden actual age to the player unless established in fiction.

Rationale:

- The GM often only needs one or two stats.
- Full card retrieval costs tokens and increases secret-leak risk.
- Presets cover common needs; `fields` handles precise mechanical lookups.
- Mechanics results are internal resolution data, not player-facing stat disclosure.

## Secret Handling

`ask_npc` may give the NPC model the full private card, including `secret`, because the NPC needs it to roleplay truthfully and protect secrets.

The GM model should normally see only:

- NPC speech/action returned by `ask_npc`.
- Visible/profile fields requested through safe presets.
- Scoped state records that are visible to the current actor.

The GM model should not receive `secret`, hidden actual age, hidden death time, hidden goals, or private motives unless a privileged/debug mode explicitly asks for them.

Debug UI may show private reasoning/claims, but that is not player-facing and must not be inserted into GM narration.

## World Time

Add world-level time state:

| Field | Meaning |
| --- | --- |
| `calendar_name` | Name of the calendar/era. |
| `absolute_minutes` | Canonical internal time counter. |
| `current_date_label` | Human-readable date label if needed. |
| `minutes_per_hour` | Usually 60, but world-configurable. |
| `hours_per_day` | Usually 24, but world-configurable. |
| `day_names` | Optional. |
| `month_names` | Optional. |

Do not parse elapsed time from final narration.

Preferred tool:

```json
{
  "minutes": 7,
  "reason": "Private questioning and tense bargaining in the tavern."
}
```

Tool name can be `advance_time`.

Rules:

- One time advance per player turn, usually after resolution tools and before final narration.
- Short reply: 1-2 minutes.
- Normal exchange: 3-10 minutes.
- Tense bargaining/interrogation: 5-15 minutes.
- Searching a room: 10-30 minutes.
- City travel: 15-60 minutes.
- Rest or downtime: hours/days.

Why tool instead of final JSON:

- Final narration may stream before and after tools.
- JSON in final text risks leaking/breaking.
- Backend needs reliable time advancement for time of day, evidence decay, travel, waiting, and consequences that already follow from the scene.

Do not add a separate scheduled-event queue by default. The GM should carry active
pressure as fiction/state memory, e.g. "the guards have been called and are approaching",
then use elapsed time to pay it off naturally when the player spends time. Each new GM turn
should receive a compact time block with current world time plus the previous turn's
elapsed minutes/reason, so this survives prompt compaction without turning the game into a
task scheduler.

## Future Player Character Card

After NPC-card enrichment, add a player-character card with parallel fields where useful:

- name/alias
- gender
- age
- physical_type
- distinctive_features
- personality/values if player wants them
- abilities
- skills
- saving_throws
- passive_perception
- ac
- hp
- speed
- senses
- languages
- inventory/equipment later if needed

Interaction with NPC/roll tools:

- Player abilities should be the default source for player-side roll modifiers.
- NPC abilities should be the source for NPC-side opposed checks, saves, perception, and combat.
- `roll_dice` should not invent unknown bonuses. It should use known player/NPC mechanics or roll plain dice.

## Implementation Order

1. Extend NPC dataclass/schema/persistence with the new card fields.
2. Update world seed generation to fill compact useful defaults.
3. Update NPC card prompt rendering.
4. Add safe `get_npc_profile` with presets and `fields`.
5. Add entity-scoped state support (`entity_id`, optional `source_npc`) for player-known facts about other NPCs.
6. Add time state and `advance_time`.
7. Add contract tests for visibility, secret filtering, field selection, persistence, and time advancement.
8. Future pass: add player-character card and connect roll modifiers to known abilities/skills.
9. Future pass: design deferred events/appointments/schedule processing.
