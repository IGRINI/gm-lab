**English** | [Русский](ru/WORLD_CREATION_TAB_TZ.md)

# Specification: World Creation Tab

## Status

- [x] Define the tab's product model.
- [x] Separate a world from its stories and opening scene.
- [x] Establish that old world-creation fields are removed rather than retained as
  legacy fields.
- [x] Rebuild the world-creation frontend around a standalone `World` entity.
- [x] Replace the world architect's backend contract with a pure world bible.
- [x] Persist a world draft immediately after the first message to the architect.
- [x] Store architect chat history, model history, and cache id inside `World`.
- [x] Add/update tests for the new contract.
- [x] Verify the build and primary regressions.

## Core idea

The world-creation tab does not create a story or a chat. It creates a reusable
world: a set of rules, lore, constraints, and hidden truths that the GM, location
generator, and a future story-creation step can build on.

A world must exist independently of game sessions. The same world can support
multiple stories:

```text
World: Threshold of the Second Sky
  -> Story A: village by the living road
  -> Story B: intrigue in the capital
  -> Story C: expedition to the temple of debts
```

Stories are currently independent. Events in one story do not change the base
world or affect other stories. A shared world timeline is a sensible future layer,
but it is not part of this task.

## Remove from the world-creation tab

Remove these concepts and controls from the tab:

- `scale` as "village / city / outpost / region";
- `storyBrief` as the player's opening situation;
- `publicIntro` as the hook for a specific story;
- the "create world and chat" action and meaning;
- presets that immediately define an opening scene or opening quest;
- any prompts that say where the player starts or what is expected of them.

Reason: these belong to story creation, not world creation. Keeping them in the
world would make the model confuse a reusable world bible with a specific campaign.

## Keep in the tab

The tab builds a world bible. Users can do this in two ways:

1. Fill in fields manually.
2. Talk to the world architect, which helps fill in or improve the fields.

The world architect is optional. Manual entry must be a complete path with no hidden
dependency on AI.

## Architect behavior

The world architect must be an intelligent editor, not a questionnaire.

It must:

- understand free-form user text;
- ask guiding questions only when genuinely important;
- avoid a long questionnaire when the user says "decide for me";
- make reasonable decisions independently when the user grants permission;
- propose a structured world bible;
- not write an opening scene, opening quest, player role, or "what is expected of
  the player";
- not mix hidden GM truths with the public world description;
- ask open questions directly in its message to the user instead of placing them in
  a separate world field.

Example of correct behavior:

User: "I want a dark isekai about oaths and debtor gods. Decide the details for me."

Architect: "I'll use a large continent, tens of millions of inhabitants, several
cultures, oath-based magic, gods as creditors of souls, living roads, and a ban on
free resurrection. I've assembled a draft; you can adjust it below."

Example of incorrect behavior:

Architect: "Where does the player wake up? Who gives them a quest? Which village
does the story begin in?"

Those questions belong to the next story-creation step, not to the world.

## Manual entry

Every world field must be manually editable.

Requirements:

- a user can create a world without the architect;
- the architect must not be the only source of valid JSON;
- the user can change any field after the architect responds;
- save must use the current field state, not the "model's latest response";
- architect updates are applied to the fields, but the user can edit them afterward.

This phase does not require a complex system that protects manual edits from model
overwrites. The UI must, however, make clear that fields are not merely a preview;
they are the actual world-editing form.

## World fields

Minimum fields required by the tab:

- `title`: world name.
- `genre`: genre or genre blend.
- `tone`: the world's overall tone.
- `world_size`: the setting's extent as a description, not a gameplay limit.
  Examples: one castle in a large magical world, a city-state, a continent, a
  planet, a galactic sector, or a galaxy.
- `population`: approximate population or order of magnitude.
- `peoples`: sentient races, peoples, species, and cultures.
- `geography`: large-scale geography, regions, countries, planets, and dangerous
  zones.
- `power_centers`: states, factions, authorities, orders, corporations, and houses.
- `reality_laws`: rules of reality covering magic, technology, gods, death, time,
  communication, travel, and constraints.
- `religions_ideologies`: religions, cults, ideologies, dogmas, and taboos.
- `history`: world history, including origins, major ruptures, and recent causes of
  the current state.
- `economy_resources`: economy, resources, scarcity, trade, transport, and money.
- `daily_life`: daily customs, fears, holidays, punishments, education, food, and
  traditions.
- `creatures_threats`: creatures, monsters, anomalies, threats, what can exist, and
  why.
- `hidden_truths`: GM-only truths that must not leak directly to the player.
- `location_generation_rules`: rules for generating locations and scenes: what fits,
  what does not, recurring motifs, and how the world appears in rooms, cities, and
  roads.
- `prohibited_elements`: content that must not be added without a special reason.

Fields may be strings or lists in the UI, but their meaning must remain intact. Most
importantly, do not bring back `storyBrief`, an opening scene, or an "opening-area
scale" as part of the world.

## World size versus opening focus

The `world_size` and `population` fields describe the setting's scope, not a limit
on the player.

Examples:

- Hogwarts-like: the game may often take place at a school, but the world is not the
  school. Describe the wider magical society, rules of magic, institutions,
  prohibitions, population, and the school as an important place within the world.
- Game of Thrones-like: one large continent, houses, religions, armies, inheritance,
  cities, roads, and economy.
- Star Wars-like: many planets, species, factions, technologies, routes, and local
  cultures.

Do not store "gameplay focus: school" as world canon. If the player leaves the
school, the world must expand naturally. Store what exists in the world and how
broadly it is structured instead.

## What the location generator receives

The location generator receives the world bible as its consistency frame.

It must understand:

- which creatures and peoples are allowed;
- which technologies and forms of magic are allowed;
- which factions and religions may appear;
- which constraints must not be broken;
- which world motifs should appear in rooms, settlements, roads, and dungeons;
- which elements are prohibited without a special reason.

Example: if magic in the world works through oaths, the generator must not suddenly
create ordinary "mana from thin air" unless an exception explains it.

## What the GM receives

The GM receives the world bible as the source of world rules. It can later use those
rules to create a specific story.

The GM must not assume the world bible already contains an opening campaign. The
story is created separately in the next step.

## Tab backend contract

The new tab contract must be clean:

- architect input: user message, model history, current world draft, stable cache id;
- architect output: UI response, updated world draft, model-history messages for
  tail append, and the saved world id;
- world persistence: a separate "create/save world" action that does not launch a
  game chat;
- remove the old world-tab fields (`scale`, `storyBrief`, story-style `publicIntro`).

Implemented solution:

- the world list is read separately from the chat list;
- saving a world does not return `state`, `transcript`, or `chat`;
- the first architect request without `world_id` creates a `World` with `draft`
  status before invoking the model;
- if the model fails, the world draft remains in the list with the user's first
  message;
- subsequent architect requests include `world_id` and update the same world;
- `World` stores `architect_messages` for the UI,
  `architect_model_history` for tail append, and
  `architect_cache_session_id` / `architect_cache_thread_id` for stable cache
  identity;
- manual "Save world" updates the existing draft and sets `status: ready` instead
  of creating a duplicate;
- opening the world list does not create an active chat;
- deleting a world does not affect the current game session;
- `/worlds` rejects story fields: `scale`, `seed`, `story_id`, `story_brief`,
  `public_intro`, `activate`.

Do not change older gameplay paths elsewhere in the application as part of this task
unless they directly relate to the world-creation tab.

## UI contract

The tab must retain the current visual layout:

- architect chat on the left;
- editable world form on the right;
- bottom action: "Save world" or "Create world";
- the world list in shared left navigation remains a list of worlds, not chats;
- selecting a world in the list opens it in the studio and restores its fields,
  architect history, and cache id;
- "+ Create world" opens a blank new draft.

Do not break the updated layout: the main pane must remain a complete world-creation
studio, not a small block in the sidebar.

## Acceptance criteria

- The world-creation tab no longer contains `scale`, `storyBrief`, or an opening
  story hook as required world fields.
- Creating a world does not launch a game chat.
- A user can fill fields manually and save a world without the architect.
- The first architect message creates a persistent draft world.
- Architect chat history is restored when a world is selected from the list.
- Saving a ready world preserves architect history and cache identity.
- The architect discusses the world bible, not an opening scene.
- Architect requests remain cache-stable: stable head, stable cache id, append-only
  tail.
- The backend accepts and returns a clean world draft.
- Tests cover the absence of legacy fields from the architect contract.
- The frontend build passes.
- Rust tests/clippy pass for affected packages.

## Out of scope for now

- Creating a story from a world.
- Selecting a world when creating a story.
- A shared world timeline across stories.
- Migrating old procedural chats into worlds.
- A deep array/table editor for every field when a simple textarea already supports
  manual input.
