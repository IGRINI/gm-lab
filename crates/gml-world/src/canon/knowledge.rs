//! Knowledge & visibility scopes (TZ §11).
//!
//! The canon holds the full truth, but the player must only ever see what they
//! have actually witnessed/learned. A hidden cause of an event must NEVER leak
//! into a player-facing projection just because the engine can see the whole
//! canon. Every fact/event/account carries a [`Scope`]; the projection layer
//! gates on [`Scope::visible_to_player`].

use serde::{Deserialize, Serialize};

/// Who may know a piece of information.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Objective truth — the canon's private ground truth (never shown raw).
    TrueCanon,
    /// Knowledge the GM may use to run the game, but is not public.
    #[default]
    GmPrivate,
    /// Known to a specific actor (npc/actor id).
    Actor(String),
    /// The player has witnessed or learnt this.
    Player,
    /// Common knowledge in the region.
    Public,
    /// An unverified rumour / account.
    Rumor,
}

impl Scope {
    /// Whether this scope is allowed into the player-facing view. Only `Player`,
    /// `Public` and `Rumor` are; `TrueCanon`, `GmPrivate` and other actors'
    /// private knowledge are withheld.
    pub fn visible_to_player(&self) -> bool {
        matches!(self, Scope::Player | Scope::Public | Scope::Rumor)
    }

    /// Whether a given actor may know this.
    pub fn visible_to_actor(&self, actor_id: &str) -> bool {
        match self {
            Scope::Public | Scope::Rumor | Scope::Player => true,
            Scope::Actor(id) => id == actor_id,
            Scope::TrueCanon | Scope::GmPrivate => false,
        }
    }
}

/// How truthful an account/rumour is relative to the actual canon (TZ §7.5:
/// `actual history` vs `accounts`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Truthfulness {
    /// Matches what actually happened.
    Actual,
    /// Partially true / distorted.
    #[default]
    Partial,
    /// A false version (a lie, a cover story).
    False,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_player_public_rumor_reach_the_player() {
        assert!(Scope::Player.visible_to_player());
        assert!(Scope::Public.visible_to_player());
        assert!(Scope::Rumor.visible_to_player());
        assert!(!Scope::TrueCanon.visible_to_player());
        assert!(!Scope::GmPrivate.visible_to_player());
        assert!(!Scope::Actor("borin".into()).visible_to_player());
    }

    #[test]
    fn actor_scope_is_private_to_that_actor() {
        let s = Scope::Actor("borin".into());
        assert!(s.visible_to_actor("borin"));
        assert!(!s.visible_to_actor("lysa"));
    }
}
