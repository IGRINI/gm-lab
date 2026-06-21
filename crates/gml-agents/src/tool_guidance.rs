//! Shared model-facing tool capability guidance.
//!
//! Faithful port of `gm-lab/tool_guidance.py`. Only the constants that
//! `agents.py` splices into the STATIC GM tool descriptions are needed here:
//! the `WORLD_STATE_*_GUIDE` fragments spliced into `update_world_state`, and
//! [`TOOL_SEARCH_DESCRIPTION`] used by the `tool_search` tool. The remaining
//! `tool_guidance` text already lives baked into the captured `GM_SYSTEM`
//! prompt (see `gml-prompts`), so it is not re-derived here.
//!
//! These are `pub(crate)` module constants — byte-for-byte copies of the
//! Python module-level strings, so the assembled tool JSON matches the golden
//! `gm_tools.json` fixture exactly.

pub(crate) const WORLD_STATE_TYPE_GUIDE: &str = "Choose world-state type by meaning: fact = objective established truth or visible \
stable state; rumor = unverified claim, testimony, suspicion, accusation, or lead; \
npc_memory = a specific event or knowledge one NPC remembers, saw, was told, \
promised, hid, learned, or should later act on; relationship = ongoing attitude, \
trust, debt, leverage, fear, loyalty, hatred, love, suspicion, or obligation toward \
a target; goal = current want, plan, intent, agenda, or task.";

pub(crate) const WORLD_STATE_SCOPE_GUIDE: &str = "Choose scope by who may know it: public = anyone can know; shared = only npc_id \
plus target and/or participants know; npc = only npc_id knows/thinks/remembers; \
gm = hidden author truth not known by characters unless discovered.";

pub(crate) const WORLD_STATE_SPLIT_GUIDE: &str = "Do not store testimony as fact just because someone said it. If an NPC privately \
claims something to the player, store the claim as rumor with scope=shared and store \
that the NPC told/asked/was threatened as npc_memory with scope=shared or npc. If \
trust, debt, threat, leverage, affection, hatred, loyalty, fear, suspicion, a bargain, \
or blackmail changes, write or update relationship; if a plan or intent changes, \
write or update goal.";

pub(crate) const WORLD_STATE_CONSOLIDATION_GUIDE: &str = "Consolidate memory by durable meaning: one event, clue cluster, testimony block, \
or orientation for the same audience should usually be one record, not several \
near-duplicates. Put names, descriptions, and search words in text/aliases; use \
participants for extra actors who know the same record. Split only when truth status, \
access scope, owner, relationship target, or future update lifecycle is genuinely \
different.";

pub(crate) const WORLD_STATE_EXAMPLE_GUIDE: &str = "Examples: private witness claim -> rumor shared npc_id=<speaker> target=player; \
the witness remembers telling the player -> npc_memory shared npc_id=<speaker> \
target=player; private resentment after a threat -> relationship npc npc_id=<npc> \
target=player; secret plan to mislead the player -> goal npc npc_id=<npc>; \
identity revealed -> fact with entity_id=<npc> known_name=<player-facing name>.";

pub(crate) const WORLD_STATE_SEARCH_ANCHOR_GUIDE: &str = "Search anchors: when a note is tied to a place, use location_id/location_name for \
the concrete site, region_id/region_name for the broader town/area, scene_id only \
when the exact scene matters, and aliases for Russian names, case forms, \
transliterations, old names, nicknames, or spelling variants. Machine ids help exact lookup, but names \
and aliases help Russian semantic/keyword search.";

pub(crate) const TOOL_SEARCH_DESCRIPTION: &str = "Search and load hidden GM tools named in the system tool capability map. Use \
this when a needed hidden scene, NPC movement, NPC profile, or whereabouts capability is \
not visible. Query with keywords or exact selection such as \
select:tool_name or select:tool_a,tool_b; matching tools become available on \
the next GM step. Result is compact structured text: loaded and missing tools.";
