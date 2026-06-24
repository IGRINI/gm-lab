//! High-level world premise and generation guardrails.
//!
//! `WorldLore` is the top layer of procedural canon: it tells the GM and
//! specialist generators what kind of world this campaign is, what can plausibly
//! exist there, and which hidden truths must remain GM-only until revealed.

use serde::{Deserialize, Serialize};

use super::{ids, Provenance};

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
    /// High-level creative references and anti-flattening style anchors.
    #[serde(default)]
    pub inspirations: Vec<String>,
    /// Named macro regions, kingdoms, planes, planets, districts, or frontiers.
    #[serde(default)]
    pub regions: Vec<String>,
    /// Governments, rulers, orders, corporations, councils, clans, or empires.
    #[serde(default)]
    pub power_centers: Vec<String>,
    /// Religions, cults, philosophies, official creeds, or secular doctrines.
    #[serde(default)]
    pub religions: Vec<String>,
    /// Gods, spirits, machine-minds, cosmic forces, saints, or false divinities.
    #[serde(default)]
    pub gods: Vec<String>,
    /// Cultures, peoples, classes, languages, customs, and social identities.
    #[serde(default)]
    pub cultures: Vec<String>,
    /// Timeline anchors: ancient origin, major breaks, recent causes.
    #[serde(default)]
    pub history: Vec<String>,
    /// Resources, money, trade, scarcity, production, and travel economy.
    #[serde(default)]
    pub economy: Vec<String>,
    /// What ordinary people eat, fear, celebrate, study, punish, and expect.
    #[serde(default)]
    pub daily_life: Vec<String>,
    /// Campaign-facing situation seeds that fit this world.
    #[serde(default)]
    pub story_hooks: Vec<String>,
    /// GM-only truths that explain the setting but must not leak to players.
    #[serde(default)]
    pub hidden_secrets: Vec<String>,
    /// Direct constraints for location/situation generation.
    #[serde(default)]
    pub location_rules: Vec<String>,
    /// Explicit anti-rules: things that should not appear without explanation.
    #[serde(default)]
    pub prohibited_elements: Vec<String>,
    /// Known gaps the architect intentionally left for future clarification.
    #[serde(default)]
    pub open_questions: Vec<String>,
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
            && self.inspirations.is_empty()
            && self.regions.is_empty()
            && self.power_centers.is_empty()
            && self.religions.is_empty()
            && self.gods.is_empty()
            && self.cultures.is_empty()
            && self.history.is_empty()
            && self.economy.is_empty()
            && self.daily_life.is_empty()
            && self.story_hooks.is_empty()
            && self.hidden_secrets.is_empty()
            && self.location_rules.is_empty()
            && self.prohibited_elements.is_empty()
            && self.open_questions.is_empty()
    }

    /// Normalize model-authored lore so it is safe to put into generated canon.
    /// Empty title/spec fields inherit the procedural request; ids and
    /// provenance are deterministic and do not consume campaign dice RNG.
    pub fn normalize_for_worldgen(&mut self, seed: &str, genre: &str, tone: &str, scale: &str) {
        trim_string(&mut self.lore_id);
        trim_string(&mut self.name);
        trim_string(&mut self.genre);
        trim_string(&mut self.tone);
        trim_string(&mut self.scale);
        trim_string(&mut self.public_premise);
        trim_string(&mut self.hidden_premise);
        trim_list(&mut self.dogmas);
        trim_list(&mut self.world_laws);
        trim_list(&mut self.inhabitants);
        trim_list(&mut self.creatures);
        trim_list(&mut self.power_sources);
        trim_list(&mut self.technologies);
        trim_list(&mut self.taboos);
        trim_list(&mut self.conflicts);
        trim_list(&mut self.inspirations);
        trim_list(&mut self.regions);
        trim_list(&mut self.power_centers);
        trim_list(&mut self.religions);
        trim_list(&mut self.gods);
        trim_list(&mut self.cultures);
        trim_list(&mut self.history);
        trim_list(&mut self.economy);
        trim_list(&mut self.daily_life);
        trim_list(&mut self.story_hooks);
        trim_list(&mut self.hidden_secrets);
        trim_list(&mut self.location_rules);
        trim_list(&mut self.prohibited_elements);
        trim_list(&mut self.open_questions);

        if self.lore_id.is_empty() {
            self.lore_id = ids::stable_id(seed, "world", "lore", "architect");
        }
        if self.genre.is_empty() {
            self.genre = genre.trim().to_string();
        }
        if self.tone.is_empty() {
            self.tone = tone.trim().to_string();
        }
        if self.scale.is_empty() {
            self.scale = scale.trim().to_string();
        }
        if self.provenance.origin.trim().is_empty() {
            self.provenance = Provenance::by("world_architect", "model-authored world bible", 0);
        }
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
        push_list(&mut lines, "Inspirations", &self.inspirations);
        push_list(&mut lines, "Macro regions", &self.regions);
        push_list(&mut lines, "Power centers", &self.power_centers);
        push_list(&mut lines, "Religions/creeds", &self.religions);
        push_list(&mut lines, "Gods/forces", &self.gods);
        push_list(&mut lines, "Cultures", &self.cultures);
        push_list(&mut lines, "History anchors", &self.history);
        push_list(&mut lines, "Economy/scarcity", &self.economy);
        push_list(&mut lines, "Daily life", &self.daily_life);
        push_list(&mut lines, "Story hooks", &self.story_hooks);
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
        push_list(
            &mut lines,
            "Open worldbuilding questions",
            &self.open_questions,
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

fn trim_string(value: &mut String) {
    let trimmed = value.trim();
    if trimmed.len() != value.len() {
        *value = trimmed.to_string();
    }
}

fn trim_list(values: &mut Vec<String>) {
    values.retain_mut(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return false;
        }
        if trimmed.len() != value.len() {
            *value = trimmed.to_string();
        }
        true
    });
}
