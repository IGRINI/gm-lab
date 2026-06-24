use std::sync::Arc;

use gml_llm::{Backend, MockClient};
use gml_orchestrator::memory_crystals::maybe_consolidate_memory_semantic;
use gml_orchestrator::Session;
use gml_world::{MemoryInjectionState, MemoryTier, MemoryTruthStatus, MemoryUnit};

fn session() -> Session {
    let client: Arc<dyn Backend> = Arc::new(MockClient::new());
    Session::new(client)
}

fn memory(id: &str, truth_status: MemoryTruthStatus, visibility: &[&str]) -> MemoryUnit {
    MemoryUnit {
        memory_id: id.to_string(),
        tier: MemoryTier::Raw,
        owner_scope: "actor:borin".to_string(),
        visibility_scopes: visibility.iter().map(|item| item.to_string()).collect(),
        summary: format!("source note {id}"),
        time_start: id
            .chars()
            .last()
            .and_then(|ch| ch.to_digit(10))
            .unwrap_or(0) as i64,
        time_end: id
            .chars()
            .last()
            .and_then(|ch| ch.to_digit(10))
            .unwrap_or(0) as i64,
        truth_status,
        topic_tags: vec!["semantic".to_string()],
        ..Default::default()
    }
}

#[tokio::test]
async fn semantic_auto_memory_crystal_uses_compact_summary_and_preserves_scope() {
    let mut s = session();
    for idx in 0..4 {
        s.world.add_memory_unit(memory(
            &format!("raw_{idx}"),
            MemoryTruthStatus::Rumor,
            &["place:tavern"],
        ));
    }
    s.world_query_seen
        .entry("actor:borin".to_string())
        .or_default()
        .insert("semantic".to_string());

    let client = s.client.clone();
    let created = maybe_consolidate_memory_semantic(&mut s, client.as_ref()).await;

    assert_eq!(created.len(), 1);
    assert!(s.world_query_seen.is_empty());
    let crystal = s
        .world
        .world_canon
        .memory
        .get(&created[0])
        .expect("semantic crystal");
    assert_eq!(crystal.tier, MemoryTier::Episode);
    assert_eq!(crystal.created_by, "memory_crystal_semantic_auto");
    assert_eq!(crystal.truth_status, MemoryTruthStatus::Rumor);
    assert_eq!(crystal.owner_scope, "actor:borin");
    assert_eq!(crystal.visibility_scopes, vec!["place:tavern".to_string()]);
    assert_eq!(crystal.source_memory_ids.len(), 4);
    assert_eq!(crystal.summary, "(compressed summary of previous turns)");
    assert!(crystal.details.contains("source note raw_0"));

    let raw = s.world.world_canon.memory.get("raw_0").unwrap();
    assert_eq!(raw.injection_state, MemoryInjectionState::Cold);
    assert_eq!(raw.consumed_by, vec![created[0].clone()]);
}

#[tokio::test]
async fn semantic_auto_memory_crystal_does_not_mix_visibility_or_truth_classes() {
    let mut s = session();
    for idx in 0..2 {
        s.world.add_memory_unit(memory(
            &format!("public_{idx}"),
            MemoryTruthStatus::Actual,
            &["public"],
        ));
        s.world.add_memory_unit(memory(
            &format!("place_{idx}"),
            MemoryTruthStatus::Actual,
            &["place:tavern"],
        ));
    }
    for idx in 0..3 {
        s.world.add_memory_unit(memory(
            &format!("claim_{idx}"),
            MemoryTruthStatus::Claim,
            &["place:tavern"],
        ));
    }
    s.world.add_memory_unit(memory(
        "rumor_0",
        MemoryTruthStatus::Rumor,
        &["place:tavern"],
    ));

    let client = s.client.clone();
    let created = maybe_consolidate_memory_semantic(&mut s, client.as_ref()).await;

    assert!(
        created.is_empty(),
        "semantic consolidation must not use unioned visibility/truth to force a mixed crystal"
    );
    assert!(s
        .world
        .world_canon
        .memory
        .units
        .values()
        .all(|unit| unit.created_by != "memory_crystal_semantic_auto"));
}
