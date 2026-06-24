//! gml-world — authoritative, code-side model of game-world truth for GM-Lab.
//!
//! Faithful port of `gm-lab/world.py` (PORT_PLAN.md §4.4–§4.6, subsystem map
//! "World state model"). Holds the deterministic-seeded dice RNG, the NPC
//! roster, the current scene, public/hidden facts, scoped state-records,
//! capped rumors, and per-NPC whereabouts; exposes projection methods consumed
//! by the orchestrator/agents/server/rag and the GM-tool mutators.
//!
//! Highest-fidelity concerns:
//! - [`rng`] — CPython-compatible MT19937 (getstate/setstate, randint).
//! - [`dice`] — deterministic dice + grading, forced-die overrides.
//! - [`state_record::state_record_hash`] — canonical-JSON sha256.
//! - Secret / hidden-canon isolation in [`World::retrieval_documents`].

pub mod canon;
pub mod dice;
pub mod helpers;
pub mod model;
pub mod rng;
pub mod seed;
pub mod state_record;
mod world;

pub use canon::{
    Account, Action, Actor, CanonEvent, Containment, Faction, MemoryAccess, MemoryInjectionState,
    MemoryStore, MemoryTier, MemoryTruthStatus, MemoryUnit, Place, PlayerView, ProposedAction,
    Provenance, Region, Rejection, Scope, Settlement, Transition, Truthfulness, Validator,
    WorldCanon, WorldLore, WorldSpec, GENERATOR_VERSION,
};
pub use model::{
    FactRecord, Npc, NpcWhereabouts, PlayerCharacter, Presence, Rumor, SceneExit, SceneItem,
    SceneState, StateRecord, WorldEvent, WorldFact, WorldTime,
};
pub use rng::{MersenneTwister, RngState};
pub use state_record::{state_record_hash, RagDocument};
pub use world::{
    public_gender, public_role, RagRetriever, RetrievedFact, StateRecordQuery, World,
    SOURCE_CURRENT_SCENE, SOURCE_DEFAULT_LORE, SOURCE_GM, SOURCE_MOVE_NPC, SOURCE_NPC_ROSTER,
    SOURCE_PREVIOUS_SCENE, SOURCE_SEED, WHEREABOUTS_STATUS_LABELS,
};

/// `StateRecordQuery` lives in the `world` module; this alias mirrors the
/// `world.py` call-site path used by downstream crates and tests.
pub mod world_query {
    pub use crate::world::StateRecordQuery;
}
