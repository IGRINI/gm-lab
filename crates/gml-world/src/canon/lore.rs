//! High-level world premise and generation guardrails.
//!
//! `WorldLore` is the top layer of procedural canon: it tells the GM and
//! specialist generators what kind of world this campaign is, what can plausibly
//! exist there, and which hidden truths must remain GM-only until revealed.

use serde::{Deserialize, Serialize};

use super::Provenance;

/// A compact "world passport" generated before regions, settlements, and
/// locations. It is deliberately broad: lower layers decide concrete places,
/// but they must stay inside these rules.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct WorldLore {
    #[serde(default)]
    pub lore_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub genre: String,
    #[serde(default)]
    pub tone: String,
    #[serde(default)]
    pub scale: String,
    /// Player-safe one-paragraph premise.
    #[serde(default)]
    pub public_premise: String,
    /// GM-only backstage premise.
    #[serde(default)]
    pub hidden_premise: String,
    /// Broad cultural/metaphysical assumptions people live by.
    #[serde(default)]
    pub dogmas: Vec<String>,
    /// Hard rules of reality: magic, technology, ecology, travel, survival.
    #[serde(default)]
    pub world_laws: Vec<String>,
    /// Common peoples, societies, or intelligent populations.
    #[serde(default)]
    pub inhabitants: Vec<String>,
    /// Non-human threats, monsters, machines, spirits, anomalies, etc.
    #[serde(default)]
    pub creatures: Vec<String>,
    /// Important powers: magic systems, old-world tech, divine forces, signals.
    #[serde(default)]
    pub power_sources: Vec<String>,
    /// Material culture and tools that can plausibly appear.
    #[serde(default)]
    pub technologies: Vec<String>,
    /// Social rules, prohibitions, laws, or taboos.
    #[serde(default)]
    pub taboos: Vec<String>,
    /// Recurring world pressures that should drive locations and situations.
    #[serde(default)]
    pub conflicts: Vec<String>,
    /// GM-only truths that explain the setting but must not leak to players.
    #[serde(default)]
    pub hidden_secrets: Vec<String>,
    /// Direct constraints for location/situation generation.
    #[serde(default)]
    pub location_rules: Vec<String>,
    /// Explicit anti-rules: things that should not appear without explanation.
    #[serde(default)]
    pub prohibited_elements: Vec<String>,
    #[serde(default)]
    pub provenance: Provenance,
}

impl WorldLore {
    pub fn is_empty(&self) -> bool {
        self.lore_id.is_empty()
            && self.name.is_empty()
            && self.genre.is_empty()
            && self.tone.is_empty()
            && self.scale.is_empty()
            && self.public_premise.is_empty()
            && self.hidden_premise.is_empty()
            && self.dogmas.is_empty()
            && self.world_laws.is_empty()
            && self.inhabitants.is_empty()
            && self.creatures.is_empty()
            && self.power_sources.is_empty()
            && self.technologies.is_empty()
            && self.taboos.is_empty()
            && self.conflicts.is_empty()
            && self.hidden_secrets.is_empty()
            && self.location_rules.is_empty()
            && self.prohibited_elements.is_empty()
    }

    /// Render a compact GM/generator context block. This can include GM-only
    /// fields because it is never player-facing; player-visible generation must
    /// still keep hidden fields out of visible prose.
    pub fn gm_context_lines(&self) -> Vec<String> {
        if self.is_empty() {
            return Vec::new();
        }
        let mut lines = Vec::new();
        let mut header = if self.name.is_empty() {
            "World".to_string()
        } else {
            format!("World: {}", self.name)
        };
        let mut tags = Vec::new();
        if !self.genre.is_empty() {
            tags.push(format!("genre {}", self.genre));
        }
        if !self.tone.is_empty() {
            tags.push(format!("tone {}", self.tone));
        }
        if !self.scale.is_empty() {
            tags.push(format!("scale {}", self.scale));
        }
        if !tags.is_empty() {
            header.push_str(&format!(" ({})", tags.join(", ")));
        }
        lines.push(header);
        push_text(&mut lines, "Public premise", &self.public_premise);
        push_list(&mut lines, "Dogmas", &self.dogmas);
        push_list(&mut lines, "World laws", &self.world_laws);
        push_list(&mut lines, "Inhabitants", &self.inhabitants);
        push_list(&mut lines, "Creatures/anomalies", &self.creatures);
        push_list(&mut lines, "Power sources", &self.power_sources);
        push_list(
            &mut lines,
            "Technology/material culture",
            &self.technologies,
        );
        push_list(&mut lines, "Taboos/laws", &self.taboos);
        push_list(&mut lines, "Recurring conflicts", &self.conflicts);
        push_list(
            &mut lines,
            "Location generation rules",
            &self.location_rules,
        );
        push_list(
            &mut lines,
            "Do not add without cause",
            &self.prohibited_elements,
        );
        push_text(
            &mut lines,
            "Hidden world premise (GM only)",
            &self.hidden_premise,
        );
        push_list(
            &mut lines,
            "Hidden world secrets (GM only)",
            &self.hidden_secrets,
        );
        lines
    }
}

fn push_text(lines: &mut Vec<String>, label: &str, value: &str) {
    if !value.trim().is_empty() {
        lines.push(format!("{label}: {}", value.trim()));
    }
}

fn push_list(lines: &mut Vec<String>, label: &str, values: &[String]) {
    if !values.is_empty() {
        lines.push(format!("{label}: {}", values.join("; ")));
    }
}
