//! Structured world history (TZ §6.9, §7.5, §12).
//!
//! History is an APPEND-ONLY log of structured events, not prose. Narrative
//! chronicles, rumours and NPC stories are *accounts* layered over the events
//! (TZ §7.5 `actual history` vs `accounts`), so investigations, false versions
//! and culturally-coloured legends are possible without corrupting the canon.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::knowledge::{Scope, Truthfulness};
use super::Provenance;

/// One structured change to the world. Answers TZ §6.9's questions: what / when
/// / where / who / why / what changed / who saw / who knows / traces / future.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CanonEvent {
    pub event_id: String,
    /// Monotonic sequence (append order) — the replay backbone.
    pub seq: i64,
    /// Event category, e.g. `move_player`, `create_place`, `caravan_missing`.
    pub kind: String,
    /// Absolute game time in minutes when it happened.
    #[serde(default)]
    pub time_minutes: i64,
    /// Human-readable time label (e.g. `day_2_night`).
    #[serde(default)]
    pub time_label: String,
    /// Where it happened (place id), if localised.
    #[serde(default)]
    pub place_id: String,
    /// Participants (actor/faction ids).
    #[serde(default)]
    pub actors: Vec<String>,
    /// Why it happened (cause ids / short reasons).
    #[serde(default)]
    pub causes: Vec<String>,
    /// What facts changed (short structured effect descriptors).
    #[serde(default)]
    pub effects: Vec<String>,
    /// Whether the player directly observed it.
    #[serde(default)]
    pub visible_to_player: bool,
    /// Visibility scope for projection gating.
    #[serde(default)]
    pub scope: Scope,
    /// Traces it may have left (a broken wheel, blood on snow, a tavern rumour).
    #[serde(default)]
    pub possible_traces: Vec<String>,
    /// Whether this is a scheduled (future) event not yet resolved.
    #[serde(default)]
    pub scheduled: bool,
    /// When a scheduled event is due (absolute minutes).
    #[serde(default)]
    pub due_minutes: i64,
    /// Short reason / provenance note.
    #[serde(default)]
    pub provenance: Provenance,
}

/// A told version of an event — what people, books, rumours or factions SAY,
/// which may diverge from the actual event (TZ §7.5).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Account {
    pub account_id: String,
    /// The event this account is about, if any.
    #[serde(default)]
    pub event_id: String,
    /// Who tells it (npc/faction id, "book", "rumor", ...).
    #[serde(default)]
    pub source: String,
    /// The account text.
    #[serde(default)]
    pub text: String,
    /// How truthful relative to the actual event.
    #[serde(default)]
    pub truth: Truthfulness,
    /// Who may hear this account.
    #[serde(default)]
    pub scope: Scope,
}

/// Append-only event log + the accounts layered over it.
///
/// The `events` vector is strictly append-only: a recorded [`CanonEvent`] is
/// never mutated in place. Resolution of a scheduled event is tracked as a
/// *projection* — the event id is added to `resolved`, and "pending" is computed
/// as `scheduled && !resolved` — so the historical record stays immutable and
/// replay-stable (the engine still appends a `resolved_*` marker event for the
/// causal trail).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct EventLog {
    #[serde(default)]
    pub events: Vec<CanonEvent>,
    #[serde(default)]
    pub accounts: Vec<Account>,
    /// Ids of scheduled events that have been resolved. A projection over the
    /// append-only log, never a mutation of the original event.
    #[serde(default)]
    pub resolved: BTreeSet<String>,
    #[serde(default)]
    seq: i64,
}

impl EventLog {
    pub fn is_empty(&self) -> bool {
        self.events.is_empty() && self.accounts.is_empty()
    }

    /// Append an event, stamping a non-zero sequence number. Returns the seq
    /// used. Every committed event therefore carries a unique, monotonic
    /// `seq >= 1` (the replay backbone).
    pub fn append(&mut self, mut event: CanonEvent) -> i64 {
        self.seq += 1;
        event.seq = self.seq;
        self.events.push(event);
        self.seq
    }

    /// Add an account/rumour layered over the log.
    pub fn add_account(&mut self, account: Account) {
        self.accounts.push(account);
    }

    /// All events whose scope is visible to the player (TZ §11 gate).
    ///
    /// Visibility is gated strictly on the [`Scope`]: an event reaches the
    /// player ONLY if `e.scope.visible_to_player()` (the old
    /// `|| e.visible_to_player` bypass leaked hidden-scoped events whose
    /// `visible_to_player` flag was set and is gone). The engine/validator
    /// guarantee `visible_to_player` is never true under a hidden scope.
    pub fn player_visible(&self) -> Vec<&CanonEvent> {
        self.events
            .iter()
            .filter(|e| e.scope.visible_to_player())
            .collect()
    }

    /// Whether a scheduled event id has been resolved.
    pub fn is_resolved(&self, event_id: &str) -> bool {
        self.resolved.contains(event_id)
    }

    /// Scheduled, still-pending events due at or before `now_minutes`, in seq
    /// order. Resolved ones are excluded (pending == scheduled && !resolved).
    pub fn due_scheduled(&self, now_minutes: i64) -> Vec<String> {
        self.events
            .iter()
            .filter(|e| {
                e.scheduled && !self.resolved.contains(&e.event_id) && e.due_minutes <= now_minutes
            })
            .map(|e| e.event_id.clone())
            .collect()
    }

    /// Mark a scheduled event resolved (no longer pending). Records the
    /// resolution as a projection — does NOT mutate the recorded event.
    pub fn resolve(&mut self, event_id: &str) {
        self.resolved.insert(event_id.to_string());
    }

    /// Whether a scheduled event is still pending (scheduled and not resolved).
    pub fn is_pending(&self, event_id: &str) -> bool {
        self.events
            .iter()
            .any(|e| e.event_id == event_id && e.scheduled)
            && !self.resolved.contains(event_id)
    }

    /// Events that touched a given place (for "why is this place like this").
    pub fn for_place(&self, place_id: &str) -> Vec<&CanonEvent> {
        self.events
            .iter()
            .filter(|e| e.place_id == place_id)
            .collect()
    }
}
