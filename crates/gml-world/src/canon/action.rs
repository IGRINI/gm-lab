//! Structured actions (TZ §8.2): the typed change-proposals the GM/LLM emits
//! instead of free prose. Each is validated (TZ §8.3) before it can mutate the
//! canon, so the LLM proposes but never owns the truth (TZ §5).

use serde::{Deserialize, Serialize};

use super::knowledge::Scope;
use super::PassageDirectionality;

/// The MVP action set from TZ §8.2. Tagged by `op` for stable serialization.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Action {
    /// Move the player along a known transition from their current place.
    MovePlayer { transition_id: String },
    /// Move the player between visited places over an already-planned canonical
    /// travel network. This is deliberately separate from immediate exits.
    TravelPlayer {
        destination_place_id: String,
        #[serde(default)]
        network_id: Option<String>,
    },
    /// One-off story-authoritative movement between two existing places.
    /// Unlike a transition or travel network, this never creates reusable
    /// geography.
    RelocatePlayer {
        destination_place_id: String,
        #[serde(default)]
        elapsed_minutes: i64,
    },
    /// Create a new place (optionally as a shell).
    CreatePlace {
        place_id: String,
        name: String,
        #[serde(default)]
        kind: String,
        #[serde(default)]
        parent: String,
        #[serde(default)]
        region_id: String,
        #[serde(default)]
        district_id: String,
        #[serde(default)]
        description: String,
        #[serde(default)]
        features: Vec<String>,
        #[serde(default)]
        visited: bool,
        #[serde(default)]
        shell: bool,
    },
    /// Update an existing place's player-facing structural fields.
    UpdatePlace {
        place_id: String,
        #[serde(default)]
        name: String,
        #[serde(default)]
        kind: String,
        #[serde(default)]
        description: String,
        #[serde(default)]
        features: Vec<String>,
        #[serde(default)]
        visited: bool,
    },
    /// Create a directed transition between two places.
    CreateTransition {
        transition_id: String,
        passage_id: String,
        directionality: PassageDirectionality,
        from_place: String,
        to_place: String,
        #[serde(default)]
        destination_hint: String,
        #[serde(default)]
        label: String,
        #[serde(default)]
        kind: String,
        #[serde(default)]
        visible: Option<bool>,
        #[serde(default)]
        passable: Option<bool>,
        #[serde(default)]
        blocked_by: String,
        #[serde(default)]
        time_cost: i64,
        #[serde(default)]
        risk: String,
    },
    /// Atomically create one complete physical passage between existing
    /// places. A bidirectional passage owns two directed transitions sharing
    /// one `passage_id`; a one-way passage owns exactly one.
    CreatePassage {
        passage_id: String,
        directionality: PassageDirectionality,
        forward_transition_id: String,
        #[serde(default)]
        reverse_transition_id: String,
        from_place: String,
        to_place: String,
        forward_label: String,
        #[serde(default)]
        reverse_label: String,
        kind: String,
        time_cost: i64,
        risk: String,
    },
    /// Open or close an existing physical passage selected by one exact
    /// transition id. Bidirectional sides are changed together by the engine
    /// through their shared `passage_id`.
    SetPassageState {
        transition_id: String,
        open: bool,
        state_reason: String,
    },
    /// Resolve or replace the target and complete travel profile of an existing
    /// transition. This is the only action that may turn a legacy shell edge
    /// into a traversable edge; every profile field is required and validated.
    ConfigureTransition {
        transition_id: String,
        passage_id: String,
        directionality: PassageDirectionality,
        to_place: String,
        label: String,
        kind: String,
        time_cost: i64,
        risk: String,
    },
    /// Create a world-side actor for an NPC.
    CreateActor {
        actor_id: String,
        #[serde(default)]
        public_label: String,
        #[serde(default)]
        place_id: String,
        #[serde(default)]
        role: String,
        #[serde(default)]
        faction_id: String,
    },
    /// Move an actor to a place.
    MoveActor { actor_id: String, to_place: String },
    /// Update a relation value between two actors.
    UpdateRelation {
        actor_id: String,
        other_id: String,
        value: i32,
    },
    /// Append a structured event to the log.
    CreateEvent {
        kind: String,
        #[serde(default)]
        place_id: String,
        #[serde(default)]
        actors: Vec<String>,
        #[serde(default)]
        causes: Vec<String>,
        #[serde(default)]
        effects: Vec<String>,
        #[serde(default)]
        visible_to_player: bool,
        #[serde(default)]
        scope: Scope,
        #[serde(default)]
        traces: Vec<String>,
    },
    /// Schedule a future event due at an absolute time.
    ScheduleEvent {
        kind: String,
        due_minutes: i64,
        #[serde(default)]
        place_id: String,
        #[serde(default)]
        actors: Vec<String>,
        #[serde(default)]
        causes: Vec<String>,
    },
    /// Resolve a scheduled event now.
    ResolveEvent { event_id: String },
    /// Change a numeric resource on an actor or faction.
    ChangeResource {
        target_id: String,
        resource: String,
        delta: i32,
    },
    /// Reveal information to the player or an actor (promotes its scope).
    RevealInformation { fact_id: String, to: Scope },
    /// Create/update a fact/state record (delegated to the existing
    /// `World.state_records` layer by the engine).
    CreateOrUpdateFact {
        fact_id: String,
        text: String,
        #[serde(default)]
        scope: Scope,
    },
    /// Advance the game clock by a number of minutes.
    AdvanceClock { minutes: i64 },
}

/// A proposal wrapping an [`Action`] with the TZ §8.2 metadata: source, reason,
/// visibility and confidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposedAction {
    pub action: Action,
    /// Who/what proposed it (actor id, "gm", "worldgen", "offscreen").
    #[serde(default)]
    pub source: String,
    /// Short reason.
    #[serde(default)]
    pub reason: String,
    /// Visibility of the resulting change.
    #[serde(default)]
    pub scope: Scope,
    /// Time delta in minutes this action implies (0 if none).
    #[serde(default)]
    pub time_delta: i64,
    /// Optional confidence/uncertainty 0..=100 (100 = certain).
    #[serde(default)]
    pub confidence: Option<u8>,
}

impl ProposedAction {
    /// Convenience constructor for an engine/worldgen-sourced action.
    pub fn new(action: Action, source: &str, reason: &str) -> Self {
        ProposedAction {
            action,
            source: source.to_string(),
            reason: reason.to_string(),
            scope: Scope::GmPrivate,
            time_delta: 0,
            confidence: None,
        }
    }
}
