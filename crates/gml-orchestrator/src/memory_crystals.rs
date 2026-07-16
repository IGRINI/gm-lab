//! Semantic automatic consolidation for living-world memory crystals.
//!
//! `gml-world` owns deterministic storage and access rules. This module owns
//! the model-boundary step that turns a batch of already-scoped raw memories into
//! a concise semantic summary while preserving scope/provenance in code.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::json;

use gml_llm::Backend;
use gml_world::{MemoryInjectionState, MemoryTier, MemoryTruthStatus, MemoryUnit};

use crate::session::Session;

const SOURCE_COUNT: usize = 4;
const MAX_BATCHES_PER_TURN: usize = 8;
const SUMMARY_MAX_CHARS: usize = 420;
const DETAILS_MAX_CHARS: usize = 1800;
const COMPACT_INPUT_MAX_CHARS: usize = 12_000;

pub async fn maybe_consolidate_memory_semantic(
    session: &mut Session,
    client: &dyn Backend,
) -> Vec<String> {
    let mut created = Vec::new();
    for _ in 0..MAX_BATCHES_PER_TURN {
        let Some((next_tier, source_ids)) = next_semantic_consolidation_batch(session) else {
            break;
        };
        let sources = source_ids
            .iter()
            .filter_map(|id| session.world.world_canon.memory.get(id).cloned())
            .collect::<Vec<_>>();
        if sources.len() < SOURCE_COUNT {
            break;
        }

        let prompt = render_memory_crystal_prompt(&next_tier, &sources);
        let semantic_summary = match client
            .summarize(
                &clip_chars(&prompt, COMPACT_INPUT_MAX_CHARS),
                &session.world.proper_nouns(),
            )
            .await
        {
            Ok(summary) => sanitize_summary(&summary),
            Err(_) => String::new(),
        };
        let summary = if semantic_summary.is_empty() {
            fallback_summary(&next_tier, &sources)
        } else {
            clip_chars(&semantic_summary, SUMMARY_MAX_CHARS)
        };
        let details = clip_chars(&semantic_details(&summary, &sources), DETAILS_MAX_CHARS);
        let unit = semantic_crystal_unit(next_tier, &sources, summary, details);
        let (id, _) = session.world.consolidate_memory_unit(unit);
        created.push(id);
    }
    if !created.is_empty() {
        session.reset_world_query_cache();
    }
    created
}

fn next_semantic_consolidation_batch(session: &Session) -> Option<(MemoryTier, Vec<String>)> {
    for tier in [MemoryTier::Raw, MemoryTier::Episode, MemoryTier::Arc] {
        let next_tier = tier.next_tier()?;
        let mut groups: BTreeMap<String, Vec<&MemoryUnit>> = BTreeMap::new();
        for unit in session.world.world_canon.memory.units.values() {
            if unit.tier != tier
                || !unit.injection_state.is_default_visible()
                || !unit.consumed_by.is_empty()
            {
                continue;
            }
            groups
                .entry(semantic_batch_key(unit))
                .or_default()
                .push(unit);
        }
        for (_, mut units) in groups {
            if units.len() < SOURCE_COUNT {
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
                    .take(SOURCE_COUNT)
                    .map(|unit| unit.memory_id.clone())
                    .collect(),
            ));
        }
    }
    None
}

fn semantic_batch_key(unit: &MemoryUnit) -> String {
    let mut visibility = unit.visibility_scopes.clone();
    visibility.sort();
    visibility.dedup();
    format!(
        "{}|{}|{}|{}",
        unit.tier.as_str(),
        unit.owner_scope,
        unit.truth_status.as_str(),
        visibility.join(",")
    )
}

fn render_memory_crystal_prompt(next_tier: &MemoryTier, sources: &[MemoryUnit]) -> String {
    let owner_scope = sources
        .first()
        .map(|unit| unit.owner_scope.as_str())
        .unwrap_or("");
    let source_block = sources
        .iter()
        .enumerate()
        .map(|(idx, unit)| {
            let details = unit.details.trim();
            if details.is_empty() {
                format!(
                    "{}. id={} tier={} truth={} text={}",
                    idx + 1,
                    unit.memory_id,
                    unit.tier.as_str(),
                    unit.truth_status.as_str(),
                    unit.summary.trim()
                )
            } else {
                format!(
                    "{}. id={} tier={} truth={} text={} details={}",
                    idx + 1,
                    unit.memory_id,
                    unit.tier.as_str(),
                    unit.truth_status.as_str(),
                    unit.summary.trim(),
                    details
                )
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    gml_prompts::render_prompt(
        gml_prompts::PromptId::MemoryCrystal,
        json!({
            "next_tier": next_tier.as_str(),
            "owner_scope": owner_scope,
            "source_block": source_block,
        }),
    )
    .unwrap_or_else(|error| panic!("failed to render memory-crystal prompt: {error:#}"))
}

fn semantic_crystal_unit(
    next_tier: MemoryTier,
    sources: &[MemoryUnit],
    summary: String,
    details: String,
) -> MemoryUnit {
    let owner_scope = sources
        .first()
        .map(|unit| unit.owner_scope.clone())
        .unwrap_or_default();
    let mut unit = MemoryUnit {
        tier: next_tier,
        owner_scope,
        summary,
        details,
        source_memory_ids: union_strings(sources.iter().map(|unit| unit.memory_id.clone())),
        truth_status: merged_truth_status(sources),
        injection_state: MemoryInjectionState::Hot,
        created_by: "memory_crystal_semantic_auto".to_string(),
        ..Default::default()
    };
    unit.visibility_scopes = union_strings(
        sources
            .iter()
            .flat_map(|unit| unit.visibility_scopes.clone()),
    );
    unit.source_event_ids = union_strings(
        sources
            .iter()
            .flat_map(|unit| unit.source_event_ids.clone()),
    );
    unit.source_account_ids = union_strings(
        sources
            .iter()
            .flat_map(|unit| unit.source_account_ids.clone()),
    );
    unit.source_state_record_ids = union_strings(
        sources
            .iter()
            .flat_map(|unit| unit.source_state_record_ids.clone()),
    );
    unit.place_ids = union_strings(sources.iter().flat_map(|unit| unit.place_ids.clone()));
    unit.actor_ids = union_strings(sources.iter().flat_map(|unit| unit.actor_ids.clone()));
    unit.faction_ids = union_strings(sources.iter().flat_map(|unit| unit.faction_ids.clone()));
    unit.topic_tags = union_strings(sources.iter().flat_map(|unit| unit.topic_tags.clone()));
    unit.facts_claimed = union_strings(sources.iter().flat_map(|unit| unit.facts_claimed.clone()));
    unit.uncertainties = union_strings(sources.iter().flat_map(|unit| {
        unit.uncertainties
            .clone()
            .into_iter()
            .chain(truth_uncertainty(unit))
    }));
    unit.time_start = sources
        .iter()
        .map(|unit| unit.time_start)
        .min()
        .unwrap_or(0);
    unit.time_end = sources.iter().map(|unit| unit.time_end).max().unwrap_or(0);
    unit.metadata.insert(
        "semantic_auto".to_string(),
        json!("compact_model_summary_scope_preserved"),
    );
    unit
}

fn semantic_details(summary: &str, sources: &[MemoryUnit]) -> String {
    let mut lines = vec![
        format!("Semantic memory crystal: {summary}"),
        "Sources:".to_string(),
    ];
    lines.extend(sources.iter().map(|unit| {
        format!(
            "- [{}] tier={} truth={} scope={} summary={}",
            unit.memory_id,
            unit.tier.as_str(),
            unit.truth_status.as_str(),
            unit.owner_scope,
            unit.summary.trim()
        )
    }));
    lines.join("\n")
}

fn fallback_summary(next_tier: &MemoryTier, sources: &[MemoryUnit]) -> String {
    let joined = sources
        .iter()
        .map(|unit| unit.summary.trim())
        .filter(|summary| !summary.is_empty())
        .take(SOURCE_COUNT)
        .collect::<Vec<_>>()
        .join(" / ");
    if joined.is_empty() {
        format!(
            "Memory crystal {}: consolidated observations.",
            next_tier.as_str()
        )
    } else {
        clip_chars(
            &format!("Memory crystal {}: {joined}", next_tier.as_str()),
            SUMMARY_MAX_CHARS,
        )
    }
}

fn sanitize_summary(raw: &str) -> String {
    raw.trim()
        .trim_matches('`')
        .trim_matches('"')
        .trim()
        .to_string()
}

fn merged_truth_status(sources: &[MemoryUnit]) -> MemoryTruthStatus {
    let Some(first) = sources.first().map(|unit| unit.truth_status.clone()) else {
        return MemoryTruthStatus::Unknown;
    };
    if sources.iter().all(|unit| unit.truth_status == first) {
        first
    } else {
        MemoryTruthStatus::Unknown
    }
}

fn truth_uncertainty(unit: &MemoryUnit) -> impl Iterator<Item = String> + '_ {
    let should_mark = matches!(
        unit.truth_status,
        MemoryTruthStatus::Claim
            | MemoryTruthStatus::Rumor
            | MemoryTruthStatus::Belief
            | MemoryTruthStatus::Lie
            | MemoryTruthStatus::Unknown
    );
    should_mark
        .then(|| format!("{}:{}", unit.memory_id, unit.truth_status.as_str()))
        .into_iter()
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

fn clip_chars(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}
