//! `view` — project the current player-facing [`SceneState`] out of the canon.
//!
//! TZ §6.10 / §8.1: the rule is `Place + Entities + Visibility + Time +
//! ActiveEvents -> CurrentView`. The world is mutated on the canon; the view is
//! *rebuilt* from it. Phase 1 implements the structural half of that rule and
//! proves it is lossless without yet taking over the live path.
//!
//! [`build_current_view`] takes the *structural* fields (location, name,
//! description, present occupants, exits) from the canonical [`Place`] and its
//! [`Transition`]s, and carries the still-ephemeral view fields (scene id,
//! presence detail, item bodies, constraints, tension, what the player has
//! seen) from the live scene. When the canon was derived from that same scene
//! (the Phase-1 case), the projection reproduces the scene exactly — the
//! regression guard asserted in `tests/canon.rs`.

use crate::model::{SceneExit, SceneState};
use crate::World;

/// Rebuild the current [`SceneState`] for the player's current place from the
/// canon. Structural fields come from the canonical [`super::Place`]; ephemeral
/// view state is carried over from `world.scene`.
///
/// The view is anchored on `canon.player_place_id` — the single source of truth
/// for where the player is — NOT on the (derived) `scene.location_id`. This is
/// what makes a `move_player` / `set_scene` through the canon flow straight into
/// the live scene, GM context and UI.
///
/// If the player's place has no canonical entry yet (e.g. an old save loaded
/// without canon), the live scene is returned unchanged so behaviour is never
/// worse than today.
pub fn build_current_view(world: &World) -> SceneState {
    let scene = &world.scene;
    // Anchor on the canonical player place. Fall back to the legacy scene
    // location only when the canon has no player place (pre-canon saves).
    let anchor_id = if world.world_canon.player_place_id.is_empty() {
        scene.location_id.clone()
    } else {
        world.world_canon.player_place_id.clone()
    };
    let place = match world.world_canon.place(&anchor_id) {
        Some(p) => p,
        None => return scene.clone(),
    };

    // Structural fields, rebuilt from canon.
    let location_id = place.place_id.clone();
    let title = place.name.clone();
    let description = place.default_description.clone();
    // Present NPCs are the LIVING actors physically at this place (the canonical
    // source for `present_npcs`, TZ §6.7) — not the place's static occupant set,
    // which can lag actor moves.
    let present_npcs: std::collections::BTreeSet<String> = world
        .world_canon
        .actors_at(&anchor_id)
        .into_iter()
        .map(|a| a.actor_id.clone())
        .collect();

    // Exits rebuilt from the place's ordered transition list, faithfully
    // reproducing each legacy `SceneExit`.
    let mut exits: Vec<SceneExit> = Vec::with_capacity(place.transition_ids.len());
    for tid in &place.transition_ids {
        if let Some(t) = world.world_canon.transition(tid) {
            exits.push(SceneExit {
                // Restore the original exit id (not the possibly-suffixed
                // transition id), so a duplicate-id seed round-trips exactly.
                exit_id: t.source_exit_id.clone(),
                name: t.label.clone(),
                destination: t.destination_hint.clone(),
                visible: t.visible,
                blocked_by: t.blocked_by.clone(),
            });
        }
    }

    // Presence is DERIVED from the canon actors at this place — one entry per
    // present npc — so `present_npcs` and `presence` agree and `ask_npc` (which
    // needs both) works for canon/procedural NPCs. Prior per-npc detail
    // (activity/attitude/location) is preserved when the actor was already in
    // the scene; otherwise it is synthesised from the actor's agenda/role.
    let mut presence: std::collections::BTreeMap<String, crate::model::Presence> =
        std::collections::BTreeMap::new();
    for actor_id in &present_npcs {
        let prior = scene.presence.get(actor_id);
        let actor = world.world_canon.actor(actor_id);
        let activity = prior
            .map(|p| p.activity.clone())
            .filter(|s| !s.is_empty())
            .or_else(|| actor.map(|a| a.agenda.clone()).filter(|s| !s.is_empty()))
            .or_else(|| actor.map(|a| format!("present as {}", a.role)))
            .unwrap_or_default();
        presence.insert(
            actor_id.clone(),
            crate::model::Presence {
                npc_id: actor_id.clone(),
                location: prior
                    .map(|p| p.location.clone())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "в сцене".to_string()),
                visible: true,
                can_hear: true,
                activity,
                attitude: prior.map(|p| p.attitude.clone()).unwrap_or_default(),
            },
        );
    }

    SceneState {
        // Ephemeral / not-yet-canonical fields are carried over from the live
        // scene; structural fields + presence are canon-derived.
        scene_id: scene.scene_id.clone(),
        location_id,
        title,
        description,
        present_npcs,
        presence,
        items: scene.items.clone(),
        exits,
        constraints: scene.constraints.clone(),
        tension: scene.tension.clone(),
        player_seen: scene.player_seen.clone(),
    }
}
