//! Scoped, hierarchical living-world memory.
//!
//! Memory is not objective truth by itself. It is an addressable account,
//! recollection, rumour, clue, or derived "crystal" tied to actors, places,
//! factions and canon events. Access is filtered before ranking so an NPC never
//! retrieves another actor's private memory just because the text matches.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::helpers::{actor_key, match_words};
use crate::state_record::RagDocument;

/// Memory granularity. Higher tiers are LLM/GM summaries derived from lower
/// tiers; lower tiers are kept for audit/debug even after consolidation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MemoryTier {
    #[default]
    Raw,
    Episode,
    Arc,
    Durable,
}

impl MemoryTier {
    pub fn as_str(&self) -> &'static str {
        match self {
            MemoryTier::Raw => "raw",
            MemoryTier::Episode => "episode",
            MemoryTier::Arc => "arc",
            MemoryTier::Durable => "durable",
        }
    }

    pub fn next_tier(&self) -> Option<MemoryTier> {
        match self {
            MemoryTier::Raw => Some(MemoryTier::Episode),
            MemoryTier::Episode => Some(MemoryTier::Arc),
            MemoryTier::Arc => Some(MemoryTier::Durable),
            MemoryTier::Durable => None,
        }
    }
}

/// Whether a memory is current prompt material or retained only for explicit
/// drill-down. Consolidation sets sources to `Cold` instead of deleting them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MemoryInjectionState {
    #[default]
    Hot,
    Warm,
    Cold,
    Archived,
}

impl MemoryInjectionState {
    pub fn as_str(&self) -> &'static str {
        match self {
            MemoryInjectionState::Hot => "hot",
            MemoryInjectionState::Warm => "warm",
            MemoryInjectionState::Cold => "cold",
            MemoryInjectionState::Archived => "archived",
        }
    }

    pub fn is_default_visible(&self) -> bool {
        matches!(self, MemoryInjectionState::Hot | MemoryInjectionState::Warm)
    }
}

/// Relation between the memory text and objective canon.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MemoryTruthStatus {
    Actual,
    Claim,
    Rumor,
    Belief,
    Lie,
    #[default]
    Unknown,
}

impl MemoryTruthStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            MemoryTruthStatus::Actual => "actual",
            MemoryTruthStatus::Claim => "claim",
            MemoryTruthStatus::Rumor => "rumor",
            MemoryTruthStatus::Belief => "belief",
            MemoryTruthStatus::Lie => "lie",
            MemoryTruthStatus::Unknown => "unknown",
        }
    }
}

/// Actor/place/faction/player/public/gm scope encoded as a stable string.
///
/// Examples: `actor:borin`, `place:tavern_main`, `faction:city_guard`,
/// `player`, `public`, `gm_private`, `true_canon`.
pub fn canonical_scope(raw: &str, fallback: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return fallback.to_string();
    }
    let lowered = trimmed.to_lowercase();
    let lowered = lowered.as_str();
    match lowered {
        "gm" | "gm_private" | "private_gm" => "gm_private".to_string(),
        "true" | "truth" | "true_canon" | "canon" => "true_canon".to_string(),
        "player" | "pc" | "игрок" => "player".to_string(),
        "public" | "common" => "public".to_string(),
        "legacy_public" => "legacy_public".to_string(),
        _ => {
            if let Some((kind, id)) = lowered.split_once(':') {
                let kind = match kind {
                    "npc" => "actor",
                    "location" => "place",
                    "settle" | "town" | "village" => "settlement",
                    other => other,
                };
                let id = actor_key(id);
                if id.is_empty() {
                    fallback.to_string()
                } else {
                    format!("{kind}:{id}")
                }
            } else {
                actor_key(lowered)
            }
        }
    }
}

fn normalize_scopes(scopes: &[String]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for scope in scopes {
        let scope = canonical_scope(scope, "");
        if !scope.is_empty() && seen.insert(scope.clone()) {
            out.push(scope);
        }
    }
    out
}

fn normalize_ids(ids: &[String]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for id in ids {
        let id = actor_key(id);
        if !id.is_empty() && seen.insert(id.clone()) {
            out.push(id);
        }
    }
    out
}

/// One durable memory card. `summary` is what ordinary retrieval returns;
/// `details` is only returned when an explicit tool asks for details/debug.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MemoryUnit {
    #[serde(default)]
    pub memory_id: String,
    #[serde(default)]
    pub tier: MemoryTier,
    #[serde(default)]
    pub owner_scope: String,
    #[serde(default)]
    pub visibility_scopes: Vec<String>,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub details: String,
    #[serde(default)]
    pub facts_claimed: Vec<String>,
    #[serde(default)]
    pub uncertainties: Vec<String>,
    #[serde(default)]
    pub source_event_ids: Vec<String>,
    #[serde(default)]
    pub source_account_ids: Vec<String>,
    #[serde(default)]
    pub source_state_record_ids: Vec<String>,
    #[serde(default)]
    pub source_memory_ids: Vec<String>,
    #[serde(default)]
    pub consumed_by: Vec<String>,
    #[serde(default)]
    pub injection_state: MemoryInjectionState,
    #[serde(default)]
    pub time_start: i64,
    #[serde(default)]
    pub time_end: i64,
    #[serde(default)]
    pub place_ids: Vec<String>,
    #[serde(default)]
    pub actor_ids: Vec<String>,
    #[serde(default)]
    pub faction_ids: Vec<String>,
    #[serde(default)]
    pub topic_tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub metadata: Map<String, Value>,
    #[serde(default)]
    pub confidence: Option<u8>,
    #[serde(default)]
    pub truth_status: MemoryTruthStatus,
    #[serde(default)]
    pub created_by: String,
}

impl MemoryUnit {
    pub fn normalize(&mut self) {
        self.memory_id = actor_key(&self.memory_id);
        self.owner_scope = canonical_scope(&self.owner_scope, "gm_private");
        self.visibility_scopes = normalize_scopes(&self.visibility_scopes);
        self.source_event_ids = normalize_ids(&self.source_event_ids);
        self.source_account_ids = normalize_ids(&self.source_account_ids);
        self.source_state_record_ids = normalize_ids(&self.source_state_record_ids);
        self.source_memory_ids = normalize_ids(&self.source_memory_ids);
        self.consumed_by = normalize_ids(&self.consumed_by);
        self.place_ids = normalize_ids(&self.place_ids);
        self.actor_ids = normalize_ids(&self.actor_ids);
        self.faction_ids = normalize_ids(&self.faction_ids);
        self.topic_tags = normalize_ids(&self.topic_tags);
        if let Some(confidence) = self.confidence {
            self.confidence = Some(confidence.min(100));
        }
        if self.created_by.trim().is_empty() {
            self.created_by = "gm_tool".to_string();
        }
    }

    pub fn is_visible_to(&self, access: &MemoryAccess) -> bool {
        if access.gm {
            return true;
        }
        if matches!(self.owner_scope.as_str(), "gm_private" | "true_canon") {
            return false;
        }
        if access.scopes.contains(&self.owner_scope) {
            return true;
        }
        self.visibility_scopes
            .iter()
            .any(|scope| access.scopes.contains(scope))
    }

    fn searchable_text(&self, include_details: bool) -> String {
        let mut parts = vec![
            self.memory_id.clone(),
            self.owner_scope.clone(),
            self.summary.clone(),
            self.tier.as_str().to_string(),
            self.truth_status.as_str().to_string(),
        ];
        if include_details {
            parts.push(self.details.clone());
        }
        parts.extend(self.visibility_scopes.iter().cloned());
        parts.extend(self.facts_claimed.iter().cloned());
        parts.extend(self.uncertainties.iter().cloned());
        parts.extend(self.place_ids.iter().cloned());
        parts.extend(self.actor_ids.iter().cloned());
        parts.extend(self.faction_ids.iter().cloned());
        parts.extend(self.topic_tags.iter().cloned());
        parts.extend(
            self.metadata
                .values()
                .filter_map(Value::as_str)
                .map(String::from),
        );
        parts.join(" ")
    }

    pub fn score(&self, query: &str, include_details: bool) -> i64 {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return 1;
        }
        let hay = self.searchable_text(include_details).to_lowercase();
        let mut score = 0;
        if hay.contains(&q) {
            score += 100;
        }
        let q_words = match_words(&q);
        let h_words = match_words(&hay);
        for word in q_words {
            if h_words.contains(&word) {
                score += 10;
            }
        }
        score
    }

    pub fn to_row(&self, include_details: bool) -> Value {
        let mut row = Map::new();
        row.insert("memory_id".to_string(), json!(self.memory_id));
        row.insert("id".to_string(), json!(self.memory_id));
        row.insert("kind".to_string(), json!("memory"));
        row.insert("tier".to_string(), json!(self.tier.as_str()));
        row.insert("owner_scope".to_string(), json!(self.owner_scope));
        row.insert(
            "visibility_scopes".to_string(),
            json!(self.visibility_scopes),
        );
        row.insert(
            "truth_status".to_string(),
            json!(self.truth_status.as_str()),
        );
        row.insert(
            "injection_state".to_string(),
            json!(self.injection_state.as_str()),
        );
        row.insert("text".to_string(), json!(self.summary));
        if include_details && !self.details.trim().is_empty() {
            row.insert("details".to_string(), json!(self.details));
        }
        row.insert("source_event_ids".to_string(), json!(self.source_event_ids));
        row.insert(
            "source_account_ids".to_string(),
            json!(self.source_account_ids),
        );
        row.insert(
            "source_state_record_ids".to_string(),
            json!(self.source_state_record_ids),
        );
        row.insert(
            "source_memory_ids".to_string(),
            json!(self.source_memory_ids),
        );
        row.insert("consumed_by".to_string(), json!(self.consumed_by));
        row.insert("place_ids".to_string(), json!(self.place_ids));
        row.insert("actor_ids".to_string(), json!(self.actor_ids));
        row.insert("faction_ids".to_string(), json!(self.faction_ids));
        row.insert("topic_tags".to_string(), json!(self.topic_tags));
        if !self.metadata.is_empty() {
            row.insert("metadata".to_string(), Value::Object(self.metadata.clone()));
        }
        row.insert("created_by".to_string(), json!(self.created_by));
        if let Some(confidence) = self.confidence {
            row.insert("confidence".to_string(), json!(confidence));
        }
        Value::Object(row)
    }

    pub fn to_rag_document(&self) -> RagDocument {
        self.to_rag_document_with_details(true)
    }

    pub fn to_rag_document_with_details(&self, include_details: bool) -> RagDocument {
        let mut metadata = Map::new();
        metadata.insert("memory_id".to_string(), json!(self.memory_id));
        metadata.insert("tier".to_string(), json!(self.tier.as_str()));
        metadata.insert("owner_scope".to_string(), json!(self.owner_scope));
        metadata.insert(
            "truth_status".to_string(),
            json!(self.truth_status.as_str()),
        );
        metadata.insert(
            "injection_state".to_string(),
            json!(self.injection_state.as_str()),
        );
        metadata.insert("source_event_ids".to_string(), json!(self.source_event_ids));
        metadata.insert(
            "source_memory_ids".to_string(),
            json!(self.source_memory_ids),
        );
        if !self.metadata.is_empty() {
            metadata.insert("metadata".to_string(), Value::Object(self.metadata.clone()));
        }
        let mut tags = vec![
            self.tier.as_str().to_string(),
            self.owner_scope.clone(),
            self.truth_status.as_str().to_string(),
        ];
        tags.extend(self.visibility_scopes.iter().cloned());
        tags.extend(self.place_ids.iter().map(|id| format!("place:{id}")));
        tags.extend(self.actor_ids.iter().map(|id| format!("actor:{id}")));
        tags.extend(self.faction_ids.iter().map(|id| format!("faction:{id}")));
        tags.extend(self.topic_tags.iter().cloned());
        let text = if !include_details || self.details.trim().is_empty() {
            self.summary.clone()
        } else {
            format!("{}\nDetails: {}", self.summary, self.details)
        };
        RagDocument::new(
            format!("memory:{}", self.memory_id),
            format!("memory_{}", self.tier.as_str()),
            text,
            self.truth_status.as_str().to_string(),
            if self.source_event_ids.is_empty() {
                self.memory_id.clone()
            } else {
                self.source_event_ids.join(",")
            },
            self.owner_scope.clone(),
            tags,
            metadata,
        )
    }
}

#[derive(Clone, Debug, Default)]
pub struct MemoryAccess {
    pub gm: bool,
    pub scopes: BTreeSet<String>,
}

impl MemoryAccess {
    pub fn gm() -> Self {
        MemoryAccess {
            gm: true,
            scopes: BTreeSet::new(),
        }
    }

    pub fn scoped(scopes: BTreeSet<String>) -> Self {
        MemoryAccess { gm: false, scopes }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MemoryStore {
    #[serde(default)]
    pub units: BTreeMap<String, MemoryUnit>,
}

impl MemoryStore {
    pub fn is_empty(&self) -> bool {
        self.units.is_empty()
    }

    pub fn insert(&mut self, mut unit: MemoryUnit, world_seed: &str) -> String {
        unit.normalize();
        if unit.memory_id.is_empty() {
            unit.memory_id = super::ids::stable_id(
                world_seed,
                &unit.owner_scope,
                "memory",
                &format!(
                    "{}|{}|{}|{}",
                    unit.tier.as_str(),
                    unit.truth_status.as_str(),
                    unit.summary,
                    unit.source_memory_ids.join(",")
                ),
            );
        }
        let base = unit.memory_id.clone();
        let mut id = base.clone();
        let mut suffix = 2usize;
        while self.units.contains_key(&id) {
            id = format!("{base}_{suffix}");
            suffix += 1;
        }
        unit.memory_id = id.clone();
        self.units.insert(id.clone(), unit);
        id
    }

    pub fn get(&self, memory_id: &str) -> Option<&MemoryUnit> {
        self.units.get(&actor_key(memory_id))
    }

    pub fn query(
        &self,
        access: &MemoryAccess,
        query: &str,
        limit: usize,
        include_cold: bool,
    ) -> Vec<&MemoryUnit> {
        self.query_with_details(access, query, limit, include_cold, false)
    }

    pub fn query_with_details(
        &self,
        access: &MemoryAccess,
        query: &str,
        limit: usize,
        include_cold: bool,
        include_details: bool,
    ) -> Vec<&MemoryUnit> {
        let mut scored = Vec::new();
        for unit in self.units.values() {
            if !include_cold && !unit.injection_state.is_default_visible() {
                continue;
            }
            if !unit.is_visible_to(access) {
                continue;
            }
            let score = unit.score(query, include_details);
            if score > 0 {
                scored.push((score, unit));
            }
        }
        scored.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| a.1.memory_id.cmp(&b.1.memory_id))
        });
        scored
            .into_iter()
            .take(limit)
            .map(|(_, unit)| unit)
            .collect()
    }

    pub fn mark_consumed(&mut self, source_ids: &[String], crystal_id: &str) -> Vec<String> {
        let crystal_id = actor_key(crystal_id);
        let mut consumed = Vec::new();
        for raw_id in source_ids {
            let id = actor_key(raw_id);
            if let Some(unit) = self.units.get_mut(&id) {
                if !unit.consumed_by.contains(&crystal_id) {
                    unit.consumed_by.push(crystal_id.clone());
                }
                unit.injection_state = MemoryInjectionState::Cold;
                consumed.push(id);
            }
        }
        consumed
    }

    pub fn auto_consolidate(&mut self, world_seed: &str) -> Vec<String> {
        const SOURCE_COUNT: usize = 4;
        let mut created = Vec::new();
        let mut guard = 0usize;
        while guard < 64 {
            guard += 1;
            let Some((next_tier, source_ids)) = self.next_consolidation_batch(SOURCE_COUNT) else {
                break;
            };
            let sources: Vec<MemoryUnit> = source_ids
                .iter()
                .filter_map(|id| self.units.get(id).cloned())
                .collect();
            if sources.len() < SOURCE_COUNT {
                break;
            }
            let owner_scope = sources[0].owner_scope.clone();
            let truth_status = sources[0].truth_status.clone();
            let source_summaries = sources
                .iter()
                .map(|unit| unit.summary.trim())
                .filter(|summary| !summary.is_empty())
                .collect::<Vec<_>>();
            let summary = if source_summaries.is_empty() {
                "Memory crystal: consolidated empty observations.".to_string()
            } else {
                format!(
                    "Memory crystal: {}",
                    source_summaries
                        .iter()
                        .take(SOURCE_COUNT)
                        .copied()
                        .collect::<Vec<_>>()
                        .join(" / ")
                )
            };
            let mut unit = MemoryUnit {
                tier: next_tier,
                owner_scope,
                summary,
                details: sources
                    .iter()
                    .map(|unit| format!("- [{}] {}", unit.memory_id, unit.summary))
                    .collect::<Vec<_>>()
                    .join("\n"),
                source_memory_ids: source_ids.clone(),
                truth_status,
                injection_state: MemoryInjectionState::Hot,
                created_by: "memory_crystal_auto".to_string(),
                ..Default::default()
            };
            unit.visibility_scopes =
                union_strings(sources.iter().flat_map(|u| u.visibility_scopes.clone()));
            unit.source_event_ids =
                union_strings(sources.iter().flat_map(|u| u.source_event_ids.clone()));
            unit.source_account_ids =
                union_strings(sources.iter().flat_map(|u| u.source_account_ids.clone()));
            unit.source_state_record_ids = union_strings(
                sources
                    .iter()
                    .flat_map(|u| u.source_state_record_ids.clone()),
            );
            unit.place_ids = union_strings(sources.iter().flat_map(|u| u.place_ids.clone()));
            unit.actor_ids = union_strings(sources.iter().flat_map(|u| u.actor_ids.clone()));
            unit.faction_ids = union_strings(sources.iter().flat_map(|u| u.faction_ids.clone()));
            unit.topic_tags = union_strings(sources.iter().flat_map(|u| u.topic_tags.clone()));
            unit.time_start = sources.iter().map(|u| u.time_start).min().unwrap_or(0);
            unit.time_end = sources.iter().map(|u| u.time_end).max().unwrap_or(0);
            let id = self.insert(unit, world_seed);
            self.mark_consumed(&source_ids, &id);
            created.push(id);
        }
        created
    }

    pub fn next_consolidation_batch(&self, count: usize) -> Option<(MemoryTier, Vec<String>)> {
        for tier in [MemoryTier::Raw, MemoryTier::Episode, MemoryTier::Arc] {
            let next_tier = tier.next_tier()?;
            let mut groups: BTreeMap<String, Vec<&MemoryUnit>> = BTreeMap::new();
            for unit in self.units.values() {
                if unit.tier != tier
                    || !unit.injection_state.is_default_visible()
                    || !unit.consumed_by.is_empty()
                {
                    continue;
                }
                let key = format!(
                    "{}|{}|{}",
                    unit.owner_scope,
                    unit.truth_status.as_str(),
                    unit.visibility_scopes.join("\u{1f}")
                );
                groups.entry(key).or_default().push(unit);
            }
            for (_, mut units) in groups {
                if units.len() < count {
                    continue;
                }
                units.sort_by(|a, b| {
                    a.time_start
                        .cmp(&b.time_start)
                        .then_with(|| a.memory_id.cmp(&b.memory_id))
                });
                return Some((
                    next_tier,
                    units
                        .into_iter()
                        .take(count)
                        .map(|unit| unit.memory_id.clone())
                        .collect(),
                ));
            }
        }
        None
    }
}

fn union_strings<I>(items: I) -> Vec<String>
where
    I: IntoIterator<Item = String>,
{
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for item in items {
        let item = item.trim().to_string();
        if !item.is_empty() && seen.insert(item.clone()) {
            out.push(item);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scopes(items: &[&str]) -> BTreeSet<String> {
        items
            .iter()
            .map(|s| canonical_scope(s, ""))
            .filter(|s| !s.is_empty())
            .collect()
    }

    fn unit(id: &str, owner_scope: &str, text: &str) -> MemoryUnit {
        MemoryUnit {
            memory_id: id.to_string(),
            owner_scope: owner_scope.to_string(),
            summary: text.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn access_is_filtered_before_ranking() {
        let mut store = MemoryStore::default();
        store.insert(unit("a", "actor:borin", "LYSA_ONLY_SENTINEL"), "seed");
        store.insert(unit("b", "actor:lysa", "LYSA_ONLY_SENTINEL"), "seed");
        let rows = store.query(
            &MemoryAccess::scoped(scopes(&["actor:borin", "public"])),
            "LYSA_ONLY_SENTINEL",
            10,
            false,
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].memory_id, "a");
    }

    #[test]
    fn consumed_sources_go_cold_but_remain_addressable() {
        let mut store = MemoryStore::default();
        store.insert(unit("raw_a", "actor:borin", "raw one"), "seed");
        let crystal_id = store.insert(
            MemoryUnit {
                memory_id: "crystal".to_string(),
                tier: MemoryTier::Episode,
                owner_scope: "actor:borin".to_string(),
                summary: "episode crystal".to_string(),
                source_memory_ids: vec!["raw_a".to_string()],
                ..Default::default()
            },
            "seed",
        );
        let consumed = store.mark_consumed(&["raw_a".to_string()], &crystal_id);
        assert_eq!(consumed, vec!["raw_a".to_string()]);
        let raw = store.get("raw_a").unwrap();
        assert_eq!(raw.injection_state, MemoryInjectionState::Cold);
        assert_eq!(raw.consumed_by, vec!["crystal".to_string()]);
        assert!(store.get("raw_a").is_some());
    }
}
