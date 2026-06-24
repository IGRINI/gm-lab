//! World-state mutation (`update_world_state`) and query (`query_world_state`)
//! ports from `orchestrator.py`, plus the world-query de-dup / pagination cache
//! and the player/gm/npc query-row assembly.

use serde_json::{json, Map, Value};
use std::collections::BTreeSet;

use gml_world::{
    state_record_hash, MemoryInjectionState, MemoryTier, MemoryTruthStatus, MemoryUnit,
    StateRecord, StateRecordQuery, World,
};

use crate::helpers::{
    clean_list, clean_text, clip_text, compact_sources, drop_empty, short_hash, visibility,
};
use crate::session::Session;

// =========================================================================
// scope/kind mapping helpers
// =========================================================================

/// `_state_record_kind(item_type)`.
fn state_record_kind_local(item_type: &str) -> String {
    if item_type == "goals" {
        "goal".to_string()
    } else {
        item_type.to_string()
    }
}

/// `_state_record_scope(scope)`.
fn state_record_scope_local(scope: &str) -> String {
    match scope {
        "player" | "public" => "public",
        "gm" => "gm",
        "npc" | "owner" => "owner",
        "subject" => "subject",
        "shared" | "participants" => "participants",
        _ => "public",
    }
    .to_string()
}

/// `_state_visibility_from_scope(scope)`.
fn state_visibility_from_scope(scope: &str) -> String {
    match clean_text(&Value::String(scope.to_string())).as_str() {
        "public" => "player",
        "gm" => "gm",
        "participants" => "shared",
        _ => "npc",
    }
    .to_string()
}

fn record_text_key(text: &str) -> String {
    crate::helpers::collapse_ws(&clean_text(&Value::String(text.to_string())).to_lowercase())
}

/// `_record_participant_ids(record)` — lowercased {owner, subject} + participants,
/// with empties discarded.
fn record_participant_ids(record: &StateRecord) -> BTreeSet<String> {
    let mut ids: BTreeSet<String> = BTreeSet::new();
    ids.insert(clean_text(&Value::String(record.owner.clone())).to_lowercase());
    ids.insert(clean_text(&Value::String(record.subject.clone())).to_lowercase());
    for item in &record.participants {
        ids.insert(clean_text(&Value::String(item.clone())).to_lowercase());
    }
    ids.remove("");
    ids
}

fn is_world_state_memory(unit: &MemoryUnit) -> bool {
    !unit.source_state_record_ids.is_empty()
        && matches!(
            unit.created_by.as_str(),
            "legacy_state_record_migration" | "world_state_memory"
        )
}

fn string_array_meta(unit: &MemoryUnit, key: &str) -> Vec<String> {
    unit.metadata
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn state_record_from_memory(unit: &MemoryUnit) -> Option<StateRecord> {
    if !is_world_state_memory(unit) {
        return None;
    }
    let mut record_id = memory_meta_string(unit, "record_id");
    if record_id.is_empty() {
        record_id = unit.source_state_record_ids.first()?.clone();
    }
    let known_name = memory_meta_string(unit, "known_name");
    let mut metadata = Map::new();
    if !known_name.is_empty() {
        metadata.insert("known_name".to_string(), json!(known_name));
    }
    Some(StateRecord {
        record_id,
        kind: {
            let kind = memory_meta_string(unit, "legacy_kind");
            if kind.is_empty() {
                "fact".to_string()
            } else {
                kind
            }
        },
        text: unit.summary.clone(),
        scope: {
            let scope = memory_meta_string(unit, "legacy_scope");
            if scope.is_empty() {
                "public".to_string()
            } else {
                scope
            }
        },
        active: unit.injection_state != MemoryInjectionState::Archived,
        owner: memory_meta_string(unit, "owner"),
        subject: memory_meta_string(unit, "subject"),
        source: memory_meta_string(unit, "source"),
        status: {
            let status = memory_meta_string(unit, "status");
            if status.is_empty() {
                "known".to_string()
            } else {
                status
            }
        },
        tags: string_array_meta(unit, "tags"),
        entity_id: memory_meta_string(unit, "entity_id"),
        source_npc: memory_meta_string(unit, "source_npc"),
        participants: string_array_meta(unit, "participants"),
        location_id: memory_meta_string(unit, "location_id"),
        location_name: memory_meta_string(unit, "location_name"),
        region_id: memory_meta_string(unit, "region_id"),
        region_name: memory_meta_string(unit, "region_name"),
        scene_id: memory_meta_string(unit, "scene_id"),
        importance: memory_meta_string(unit, "importance"),
        aliases: string_array_meta(unit, "aliases"),
        metadata,
    })
}

fn state_record_hash_for_id(world: &World, record_id: &str) -> Option<String> {
    let wanted = record_id.trim();
    if wanted.is_empty() {
        return None;
    }
    for unit in world.world_canon.memory.units.values() {
        if !is_world_state_memory(unit) {
            continue;
        }
        let id = memory_meta_string(unit, "record_id");
        if id == wanted || unit.source_state_record_ids.iter().any(|src| src == wanted) {
            let hash = memory_meta_string(unit, "hash");
            return Some(if hash.is_empty() {
                state_record_from_memory(unit)
                    .map(|record| state_record_hash(&record))
                    .unwrap_or_default()
            } else {
                hash
            });
        }
    }
    None
}

fn world_state_records(world: &World) -> Vec<StateRecord> {
    world
        .world_canon
        .memory
        .units
        .values()
        .filter_map(state_record_from_memory)
        .collect()
}

fn record_matches_query(record: &StateRecord, query: &StateRecordQuery) -> bool {
    if let Some(active) = query.active {
        if record.active != active {
            return false;
        }
    }
    if let Some(kinds) = query.kinds.as_ref() {
        let allowed: BTreeSet<String> = kinds
            .iter()
            .map(|kind| gml_world::state_record::state_record_kind(kind))
            .collect();
        if !allowed.contains(&gml_world::state_record::state_record_kind(&record.kind)) {
            return false;
        }
    }
    if let Some(scopes) = query.scopes.as_ref() {
        let allowed: BTreeSet<String> = scopes
            .iter()
            .map(|scope| gml_world::state_record::state_record_scope(scope))
            .collect();
        if !allowed.contains(&gml_world::state_record::state_record_scope(&record.scope)) {
            return false;
        }
    }
    let actor_key = |raw: &str| gml_world::helpers::actor_key(raw);
    if !query.owner.is_empty() && actor_key(&record.owner) != actor_key(query.owner) {
        return false;
    }
    if !query.subject.is_empty() && actor_key(&record.subject) != actor_key(query.subject) {
        return false;
    }
    if !query.entity_id.is_empty() && actor_key(&record.entity_id) != actor_key(query.entity_id) {
        return false;
    }
    if !query.source_npc.is_empty() && actor_key(&record.source_npc) != actor_key(query.source_npc)
    {
        return false;
    }
    if !query.location_id.is_empty()
        && actor_key(&record.location_id) != actor_key(query.location_id)
    {
        return false;
    }
    if !query.region_id.is_empty() && actor_key(&record.region_id) != actor_key(query.region_id) {
        return false;
    }
    if !query.scene_id.is_empty() && actor_key(&record.scene_id) != actor_key(query.scene_id) {
        return false;
    }
    gml_world::state_record::state_record_visible_to(record, query.actor_id)
}

fn world_state_records_for(world: &World, query: &StateRecordQuery) -> Vec<StateRecord> {
    world_state_records(world)
        .into_iter()
        .filter(|record| record_matches_query(record, query))
        .collect()
}

fn apply_update_value(record: &mut StateRecord, key: &str, value: &Value) {
    match key {
        "kind" => record.kind = clean_text(value),
        "text" => record.text = clean_text(value),
        "scope" => record.scope = clean_text(value),
        "owner" => record.owner = clean_text(value),
        "subject" => record.subject = clean_text(value),
        "source" => record.source = clean_text(value),
        "status" => record.status = clean_text(value),
        "tags" => record.tags = clean_list(value),
        "entity_id" => record.entity_id = clean_text(value),
        "source_npc" => record.source_npc = clean_text(value),
        "participants" => record.participants = clean_list(value),
        "location_id" => record.location_id = clean_text(value),
        "location_name" => record.location_name = clean_text(value),
        "region_id" => record.region_id = clean_text(value),
        "region_name" => record.region_name = clean_text(value),
        "scene_id" => record.scene_id = clean_text(value),
        "importance" => record.importance = clean_text(value),
        "aliases" => record.aliases = clean_list(value),
        "metadata" => {
            if let Some(map) = value.as_object() {
                record.metadata = map.clone();
            }
        }
        "active" => {
            record.active = value
                .as_bool()
                .unwrap_or_else(|| clean_text(value).to_lowercase() != "false");
        }
        _ => {}
    }
}

/// `_find_mergeable_state_record(world, kind, text, scope, owner, entity_id,
/// source_npc, location_id, region_id, scene_id)` — returns the `record_id` of an
/// active, identical-text, same-anchors record of the same kind/scope/owner.
/// `relationship`/`goal` never merge.
#[allow(clippy::too_many_arguments)]
fn find_mergeable_state_record(
    world: &World,
    kind: &str,
    text: &str,
    scope: &str,
    owner: &str,
    entity_id: &str,
    source_npc: &str,
    location_id: &str,
    region_id: &str,
    scene_id: &str,
) -> Option<String> {
    if matches!(kind, "relationship" | "goal") {
        return None;
    }
    let text_key = record_text_key(text);
    if text_key.is_empty() {
        return None;
    }
    let wanted_scope = gml_world::state_record::state_record_scope(scope);
    let want_kind = gml_world::state_record::state_record_kind(kind);
    let lc = |s: &str| clean_text(&Value::String(s.to_string())).to_lowercase();
    for record in world_state_records(world) {
        if !record.active {
            continue;
        }
        if want_kind != gml_world::state_record::state_record_kind(&record.kind) {
            continue;
        }
        if wanted_scope != gml_world::state_record::state_record_scope(&record.scope) {
            continue;
        }
        if lc(&record.owner) != lc(owner) {
            continue;
        }
        if lc(&record.entity_id) != lc(entity_id) {
            continue;
        }
        if lc(&record.source_npc) != lc(source_npc) {
            continue;
        }
        if lc(&record.location_id) != lc(location_id) {
            continue;
        }
        if lc(&record.region_id) != lc(region_id) {
            continue;
        }
        if lc(&record.scene_id) != lc(scene_id) {
            continue;
        }
        if record_text_key(&record.text) == text_key {
            return Some(record.record_id.clone());
        }
    }
    None
}

// =========================================================================
// resolve helpers
// =========================================================================

/// `_resolve_npc_id(world, raw)` -> (id, error).
fn resolve_npc_id(world: &World, raw: &Value) -> (String, String) {
    let npc_ref = clean_text(raw);
    if npc_ref.is_empty() {
        return (String::new(), "npc_id is required".to_string());
    }
    match world.resolve(&npc_ref) {
        Ok(id) => (id, String::new()),
        Err(e) => (String::new(), e),
    }
}

/// `_resolve_participants(world, raw)` -> (ids, error).
fn resolve_participants(world: &World, raw: &Value) -> (Vec<String>, String) {
    let mut participants: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for item in clean_list(raw) {
        let key = item.to_lowercase();
        let actor_id = if key == "player" || key == "игрок" {
            "player".to_string()
        } else {
            let (id, error) = resolve_npc_id(world, &Value::String(item.clone()));
            if !error.is_empty() {
                return (
                    Vec::new(),
                    format!("participants contains unknown actor: {item}"),
                );
            }
            id
        };
        if !seen.contains(&actor_id) {
            seen.insert(actor_id.clone());
            participants.push(actor_id);
        }
    }
    (participants, String::new())
}

/// `_resolve_actor_target(world, raw)`.
fn resolve_actor_target(world: &World, raw: &str) -> String {
    let target = raw.trim();
    if target.is_empty() {
        return String::new();
    }
    let low = target.to_lowercase();
    if low == "player" || low == "игрок" {
        return "player".to_string();
    }
    world.resolve(target).unwrap_or_default()
}

// =========================================================================
// update_world_state batch
// =========================================================================

/// `_apply_world_state_batch(session, args)`.
pub fn apply_world_state_batch(session: &mut Session, args: &Value) -> Value {
    let items = match args.get("items") {
        Some(Value::Array(a)) => a.clone(),
        _ => {
            return json!({
                "ok": false,
                "applied": [],
                "errors": [{"index": 0, "error": "items[] is required"}],
            });
        }
    };
    let mut applied = Vec::new();
    let mut errors = Vec::new();
    for (idx, raw_item) in items.iter().enumerate() {
        let index = (idx + 1) as i64;
        if !raw_item.is_object() {
            errors.push(json!({"index": index, "error": "item must be an object"}));
            continue;
        }
        let (row, error) = apply_world_state_item(session, index, raw_item);
        if let Some(r) = row {
            applied.push(r);
        }
        if let Some(e) = error {
            errors.push(e);
        }
    }
    drop_empty(&json!({
        "ok": errors.is_empty(),
        "applied": applied,
        "errors": errors,
    }))
}

fn err_row(fields: &[(&str, Value)]) -> Value {
    let mut m = Map::new();
    for (k, v) in fields {
        m.insert((*k).to_string(), v.clone());
    }
    Value::Object(m)
}

/// `_apply_world_state_item(session, index, item)` -> (row, error).
fn apply_world_state_item(
    session: &mut Session,
    index: i64,
    item: &Value,
) -> (Option<Value>, Option<Value>) {
    let mut op = clean_text(item.get("op").unwrap_or(&Value::Null)).to_lowercase();
    if op.is_empty() {
        op = "add".to_string();
    }
    if !matches!(op.as_str(), "add" | "update" | "delete") {
        return (
            None,
            Some(err_row(&[
                ("index", json!(index)),
                ("op", json!(op)),
                ("error", json!("unsupported op")),
            ])),
        );
    }
    let mut item_type = clean_text(item.get("type").unwrap_or(&Value::Null)).to_lowercase();
    let text = clean_text(item.get("text").unwrap_or(&Value::Null));
    let source = clean_text(item.get("source").unwrap_or(&Value::Null));
    if item_type == "goals" {
        item_type = "goal".to_string();
    }
    if op == "delete" {
        return apply_state_record_item(
            session, index, &op, &item_type, &text, "player", &source, item,
        );
    }
    if !item_type.is_empty()
        && !matches!(
            item_type.as_str(),
            "fact" | "rumor" | "npc_memory" | "relationship" | "goal"
        )
    {
        return (
            None,
            Some(err_row(&[
                ("index", json!(index)),
                ("type", json!(item_type)),
                ("error", json!("unsupported item type")),
            ])),
        );
    }
    if op == "add" && item_type.is_empty() {
        return (
            None,
            Some(err_row(&[
                ("index", json!(index)),
                ("op", json!(op)),
                ("error", json!("type is required for add")),
            ])),
        );
    }
    if op == "add" && text.is_empty() {
        return (
            None,
            Some(err_row(&[
                ("index", json!(index)),
                ("op", json!(op)),
                ("type", json!(item_type)),
                ("error", json!("text is required")),
            ])),
        );
    }

    let default_scope = if matches!(item_type.as_str(), "fact" | "rumor") {
        "player"
    } else {
        "npc"
    };
    let scope_value = match item.get("scope") {
        Some(v) => v.clone(),
        None => item.get("visibility").cloned().unwrap_or(Value::Null),
    };
    let scope = visibility(&scope_value, default_scope);
    apply_state_record_item(
        session, index, &op, &item_type, &text, &scope, &source, item,
    )
}

/// `_apply_state_record_item(...)` — faithful port (add/update/delete with merge,
/// hash-conflict, scope/known_name validation). Returns (row, error).
#[allow(clippy::too_many_arguments)]
fn apply_state_record_item(
    session: &mut Session,
    index: i64,
    op: &str,
    item_type: &str,
    text: &str,
    scope: &str,
    source: &str,
    item: &Value,
) -> (Option<Value>, Option<Value>) {
    let record_id = {
        let r = clean_text(item.get("id").unwrap_or(&Value::Null));
        if r.is_empty() {
            clean_text(item.get("record_id").unwrap_or(&Value::Null))
        } else {
            r
        }
    };

    if op == "delete" {
        if record_id.is_empty() {
            return (
                None,
                Some(err_row(&[
                    ("index", json!(index)),
                    ("op", json!(op)),
                    ("type", json!(item_type)),
                    ("error", json!("id is required for delete")),
                ])),
            );
        }
        let existing_hash = record_hash_by_id(&session.world, &record_id);
        let expected = expected_hash(item);
        if let Some(h) = &existing_hash {
            if !expected.is_empty() && expected.to_lowercase() != h.to_lowercase() {
                return (
                    None,
                    Some(hash_conflict_error(
                        &session.world,
                        index,
                        op,
                        item_type,
                        &record_id,
                        &expected,
                    )),
                );
            }
        }
        if !session.world.archive_state_record_memory(&record_id) {
            return (
                None,
                Some(err_row(&[
                    ("index", json!(index)),
                    ("op", json!(op)),
                    ("type", json!(item_type)),
                    ("id", json!(record_id)),
                    ("error", json!("record id not found or already inactive")),
                ])),
            );
        }
        return (
            Some(json!({
                "index": index,
                "op": op,
                "type": if item_type.is_empty() { "state" } else { item_type },
                "id": record_id,
                "hash": existing_hash.unwrap_or_default(),
                "status": "deleted",
            })),
            None,
        );
    }

    // For add/update we delegate to a faithful but condensed implementation.
    // The golden turns do not exercise update/add of state records; the full
    // validation suite for these paths lives in dedicated contract tests
    // (followups). We implement the common add path and a direct update path.
    if op == "update" {
        return apply_state_record_update(
            session, index, op, item_type, text, scope, source, item, &record_id,
        );
    }

    apply_state_record_add(session, index, op, item_type, text, scope, source, item)
}

fn expected_hash(item: &Value) -> String {
    for key in ["expected_hash", "expectedHash", "record_hash", "hash"] {
        let v = clean_text(item.get(key).unwrap_or(&Value::Null));
        if !v.is_empty() {
            return v;
        }
    }
    String::new()
}

fn record_by_id(world: &World, record_id: &str) -> Option<StateRecord> {
    let wanted = record_id.trim();
    if wanted.is_empty() {
        return None;
    }
    world_state_records(world)
        .into_iter()
        .find(|record| record.record_id == wanted)
}

fn record_hash_by_id(world: &World, record_id: &str) -> Option<String> {
    state_record_hash_for_id(world, record_id)
        .or_else(|| record_by_id(world, record_id).map(|record| state_record_hash(&record)))
}

fn world_state_record_ids(world: &World) -> BTreeSet<String> {
    world_state_records(world)
        .into_iter()
        .map(|record| record.record_id)
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn unique_world_state_record_id(
    world: &World,
    preferred_id: &str,
    kind: &str,
    text: &str,
    scope: &str,
    owner: &str,
    target: &str,
    entity_id: &str,
) -> String {
    let existing = world_state_record_ids(world);
    let base = {
        let preferred = clean_text(&Value::String(preferred_id.to_string()));
        if preferred.is_empty() {
            format!(
                "state_{}_{}",
                gml_world::state_record::state_record_kind(kind),
                short_hash(&format!(
                    "{kind}|{scope}|{owner}|{target}|{entity_id}|{text}"
                ))
            )
        } else {
            preferred
        }
    };
    if !existing.contains(&base) {
        return base;
    }
    let mut suffix = 2usize;
    loop {
        let candidate = format!("{base}_{suffix}");
        if !existing.contains(&candidate) {
            return candidate;
        }
        suffix += 1;
    }
}

fn hash_conflict_error(
    world: &World,
    index: i64,
    op: &str,
    item_type: &str,
    record_id: &str,
    expected: &str,
) -> Value {
    let record = record_by_id(world, record_id);
    let actual_hash = record_hash_by_id(world, record_id).unwrap_or_default();
    let (kind, owner, subject, location_id, region_id, scene_id, scope) = match record {
        Some(r) => (
            r.kind.clone(),
            r.owner.clone(),
            r.subject.clone(),
            r.location_id.clone(),
            r.region_id.clone(),
            r.scene_id.clone(),
            state_visibility_from_scope(&r.scope),
        ),
        None => (
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            state_visibility_from_scope(""),
        ),
    };
    json!({
        "index": index,
        "op": op,
        "type": if item_type.is_empty() { kind } else { item_type.to_string() },
        "id": record_id,
        "npc_id": owner,
        "target": subject,
        "location_id": location_id,
        "region_id": region_id,
        "scene_id": scene_id,
        "scope": scope,
        "expected_hash": expected,
        "actual_hash": actual_hash,
        "status": "conflict",
        "error": "record changed; not applied. Re-query world state and retry with the current hash.",
    })
}

/// Condensed faithful port of the add branch of `_apply_state_record_item`.
#[allow(clippy::too_many_arguments)]
fn apply_state_record_add(
    session: &mut Session,
    index: i64,
    op: &str,
    item_type: &str,
    text: &str,
    scope: &str,
    source: &str,
    item: &Value,
) -> (Option<Value>, Option<Value>) {
    let world = &mut session.world;
    let mut owner = String::new();
    let mut target = clean_text(item.get("target").unwrap_or(&Value::Null));
    let entity_id = first_present(item, &["entity_id", "entity", "about"]);
    let mut source_npc = first_present(item, &["source_npc", "source_npc_id"]);
    let known_name = clean_text(item.get("known_name").unwrap_or(&Value::Null));
    let location_id = clean_text(item.get("location_id").unwrap_or(&Value::Null));
    let location_name = clean_text(item.get("location_name").unwrap_or(&Value::Null));
    let region_id = clean_text(item.get("region_id").unwrap_or(&Value::Null));
    let region_name = clean_text(item.get("region_name").unwrap_or(&Value::Null));
    let scene_id = clean_text(item.get("scene_id").unwrap_or(&Value::Null));
    let importance = clean_text(item.get("importance").unwrap_or(&Value::Null));
    let aliases = clean_list(item.get("aliases").unwrap_or(&Value::Null));
    let (participants, error) =
        resolve_participants(world, item.get("participants").unwrap_or(&Value::Null));
    if !error.is_empty() {
        return (
            None,
            Some(err_row(&[
                ("index", json!(index)),
                ("op", json!(op)),
                ("type", json!(item_type)),
                ("error", json!(error)),
            ])),
        );
    }
    if !source_npc.is_empty() {
        let (id, e) = resolve_npc_id(world, &Value::String(source_npc.clone()));
        if !e.is_empty() {
            return (
                None,
                Some(err_row(&[
                    ("index", json!(index)),
                    ("op", json!(op)),
                    ("type", json!(item_type)),
                    ("error", json!(e)),
                ])),
            );
        }
        source_npc = id;
    }
    let mut entity_id = entity_id;
    if !known_name.is_empty() {
        if entity_id.is_empty() {
            return (
                None,
                Some(err_row(&[
                    ("index", json!(index)),
                    ("op", json!(op)),
                    ("type", json!(item_type)),
                    (
                        "error",
                        json!("entity_id is required when setting known_name"),
                    ),
                ])),
            );
        }
        let (id, e) = resolve_npc_id(world, &Value::String(entity_id.clone()));
        if !e.is_empty() {
            return (
                None,
                Some(err_row(&[
                    ("index", json!(index)),
                    ("op", json!(op)),
                    ("type", json!(item_type)),
                    ("entity_id", json!(entity_id)),
                    (
                        "error",
                        json!("known_name requires entity_id to be an NPC id"),
                    ),
                ])),
            );
        }
        entity_id = id;
    }
    let needs_npc = matches!(item_type, "npc_memory" | "relationship" | "goal" | "goals")
        || matches!(scope, "npc" | "shared");
    if needs_npc {
        let (id, e) = resolve_npc_id(world, item.get("npc_id").unwrap_or(&Value::Null));
        if !e.is_empty() {
            return (
                None,
                Some(err_row(&[
                    ("index", json!(index)),
                    ("op", json!(op)),
                    ("type", json!(item_type)),
                    ("error", json!(e)),
                ])),
            );
        }
        owner = id;
    }
    if item_type == "relationship" && target.is_empty() {
        return (
            None,
            Some(err_row(&[
                ("index", json!(index)),
                ("op", json!(op)),
                ("type", json!(item_type)),
                ("npc_id", json!(owner)),
                ("error", json!("target is required for relationship")),
            ])),
        );
    }
    if !target.is_empty() && scope == "shared" {
        let actor = resolve_actor_target(world, &target);
        if actor.is_empty() {
            return (None, Some(err_row(&[("index", json!(index)), ("op", json!(op)), ("type", json!(item_type)), ("target", json!(target)), ("error", json!("target for shared scope must be player or a known npc_id; use participants for multiple actors"))])));
        }
        target = actor;
    }
    if scope == "shared" && target.is_empty() && participants.is_empty() {
        return (
            None,
            Some(err_row(&[
                ("index", json!(index)),
                ("op", json!(op)),
                ("type", json!(item_type)),
                (
                    "error",
                    json!("target or participants is required for shared scope"),
                ),
            ])),
        );
    }

    // `_find_mergeable_state_record` — merge participants of an identical active
    // memory, or reject as `not_added`.
    let mergeable_id = find_mergeable_state_record(
        world,
        item_type,
        text,
        scope,
        &owner,
        &entity_id,
        &source_npc,
        &location_id,
        &region_id,
        &scene_id,
    );
    // `record_by_id` is guaranteed to resolve `mergeable_id` (it came from the
    // same memory-backed state projection); the `if let` keeps the borrow
    // checker happy.
    if let Some(existing) = mergeable_id.and_then(|id| record_by_id(world, &id)) {
        let existing_participants = record_participant_ids(&existing);
        let mut wanted_participants: BTreeSet<String> =
            participants.iter().map(|p| p.to_lowercase()).collect();
        let target_actor = resolve_actor_target(world, &target);
        if !target_actor.is_empty() {
            wanted_participants.insert(target_actor);
        }
        // sorted((existing | wanted) - {owner.lower(), subject.lower()})
        let drop: BTreeSet<String> = [
            clean_text(&Value::String(existing.owner.clone())).to_lowercase(),
            clean_text(&Value::String(existing.subject.clone())).to_lowercase(),
        ]
        .into_iter()
        .collect();
        let merged_participants: Vec<String> = existing_participants
            .union(&wanted_participants)
            .filter(|p| !drop.contains(*p))
            .cloned()
            .collect(); // BTreeSet union iterates sorted -> already sorted, deduped

        let current_participants: BTreeSet<String> =
            existing.participants.iter().cloned().collect();
        let merged_set: BTreeSet<String> = merged_participants.iter().cloned().collect();
        if current_participants != merged_set {
            let mut rec = existing.clone();
            rec.participants = merged_participants;
            let _ = world.upsert_state_record_memory(&rec, "world_state_memory");
            let row = json!({
                "index": index,
                "op": "update",
                "type": rec.kind,
                "id": rec.record_id,
                "npc_id": rec.owner,
                "target": rec.subject,
                "participants": rec.participants,
                "scope": state_visibility_from_scope(&rec.scope),
                "hash": state_record_hash(&rec),
                "status": "merged",
            });
            return (Some(row), None);
        }
        // Identical active memory already exists with no participant change.
        let err = json!({
            "index": index,
            "op": op,
            "type": item_type,
            "npc_id": owner,
            "target": target,
            "participants": participants,
            "scope": scope,
            "existing_id": existing.record_id,
            "existing_hash": state_record_hash(&existing),
            "status": "not_added",
            "error": "not added: identical active memory already exists; update the existing record instead",
        });
        return (None, Some(err));
    }

    // Relationship-already-exists branch (via memory-backed projection).
    if item_type == "relationship" {
        let kinds = vec!["relationship".to_string()];
        let scopes = vec![gml_world::state_record::state_record_scope(scope)];
        let mut query = StateRecordQuery::new("debug");
        query.kinds = Some(&kinds);
        let owner_owned = owner.clone();
        let subject_owned = target.clone();
        query.owner = &owner_owned;
        query.subject = &subject_owned;
        query.scopes = Some(&scopes);
        let existing: Option<StateRecord> =
            world_state_records_for(world, &query).into_iter().next();
        if let Some(rec) = existing {
            let err = json!({
                "index": index,
                "op": op,
                "type": item_type,
                "npc_id": owner,
                "target": target,
                "entity_id": entity_id,
                "source_npc": source_npc,
                "location_id": location_id,
                "location_name": location_name,
                "region_id": region_id,
                "region_name": region_name,
                "scene_id": scene_id,
                "importance": importance,
                "aliases": aliases,
                "scope": scope,
                "existing_id": rec.record_id,
                "existing_hash": state_record_hash(&rec),
                "status": "not_added",
                "error": "not added: active relationship already exists; use op=update with existing_id and existing_hash",
            });
            return (None, Some(err));
        }
    }

    let mode = clean_text(item.get("mode").unwrap_or(&Value::Null)).to_lowercase();
    if matches!(item_type, "goal" | "goals") && mode == "replace" {
        let mut query = StateRecordQuery::new("debug");
        let kinds = vec!["goal".to_string()];
        query.kinds = Some(&kinds);
        let owner_owned = owner.clone();
        query.owner = &owner_owned;
        let existing_ids: Vec<String> = world_state_records_for(world, &query)
            .into_iter()
            .map(|r| r.record_id)
            .collect();
        for id in existing_ids {
            let _ = world.archive_state_record_memory(&id);
        }
    }

    let status = if item_type == "rumor" {
        "unconfirmed"
    } else {
        "known"
    };
    let mut metadata = Map::new();
    if !known_name.is_empty() {
        metadata.insert("known_name".to_string(), json!(known_name));
    }
    let record = StateRecord {
        record_id: unique_world_state_record_id(
            world, "", item_type, text, scope, &owner, &target, &entity_id,
        ),
        kind: state_record_kind_local(item_type),
        text: text.to_string(),
        scope: state_record_scope_local(scope),
        active: true,
        owner: owner.clone(),
        subject: target.clone(),
        source: if source.is_empty() {
            "gm_tool".to_string()
        } else {
            source.to_string()
        },
        status: status.to_string(),
        tags: Vec::new(),
        entity_id,
        source_npc,
        participants,
        location_id,
        location_name,
        region_id,
        region_name,
        scene_id,
        importance,
        aliases,
        metadata,
    };
    let _ = world.upsert_state_record_memory(&record, "world_state_memory");
    let row = json!({
        "index": index,
        "op": op,
        "type": item_type,
        "id": record.record_id,
        "npc_id": owner,
        "target": target,
        "entity_id": record.entity_id,
        "source_npc": record.source_npc,
        "participants": record.participants,
        "known_name": record.metadata.get("known_name").and_then(Value::as_str).unwrap_or(""),
        "location_id": record.location_id,
        "location_name": record.location_name,
        "region_id": record.region_id,
        "region_name": record.region_name,
        "scene_id": record.scene_id,
        "importance": record.importance,
        "aliases": record.aliases,
        "scope": scope,
        "mode": if matches!(item_type, "goal" | "goals") && mode == "replace" { mode } else { String::new() },
        "hash": state_record_hash(&record),
        "status": "stored",
        "text": record.text,
    });
    (Some(row), None)
}

/// Full update path (`op == "update"`) — faithful port of the Python `op ==
/// "update"` branch (scope-change validation, owner/target/entity_id/source_npc/
/// known_name resolution, anchor passthrough, full return row).
#[allow(clippy::too_many_arguments)]
fn apply_state_record_update(
    session: &mut Session,
    index: i64,
    op: &str,
    item_type: &str,
    text: &str,
    scope: &str,
    source: &str,
    item: &Value,
    record_id: &str,
) -> (Option<Value>, Option<Value>) {
    // Helper for update-branch error rows (Python always includes "id").
    let uerr = |fields: &[(&str, Value)]| -> Value {
        let mut m = Map::new();
        m.insert("index".to_string(), json!(index));
        m.insert("op".to_string(), json!(op));
        m.insert("type".to_string(), json!(item_type));
        m.insert("id".to_string(), json!(record_id));
        for (k, v) in fields {
            m.insert((*k).to_string(), v.clone());
        }
        Value::Object(m)
    };

    if record_id.is_empty() {
        return (
            None,
            Some(err_row(&[
                ("index", json!(index)),
                ("op", json!(op)),
                ("type", json!(item_type)),
                ("error", json!("id is required for update")),
            ])),
        );
    }
    let world = &mut session.world;
    let existing = match record_by_id(world, record_id) {
        Some(r) => r.clone(),
        None => {
            return (None, Some(uerr(&[("error", json!("record id not found"))])));
        }
    };
    let expected = expected_hash(item);
    if !expected.is_empty()
        && expected.to_lowercase() != state_record_hash(&existing).to_lowercase()
    {
        return (
            None,
            Some(hash_conflict_error(
                world, index, op, item_type, record_id, &expected,
            )),
        );
    }

    let mut update_payload = Map::new();
    update_payload.insert("id".to_string(), json!(record_id));
    if !item_type.is_empty() {
        update_payload.insert(
            "kind".to_string(),
            json!(state_record_kind_local(item_type)),
        );
    }
    if !text.is_empty() {
        update_payload.insert("text".to_string(), json!(text));
    }

    // owner from npc_id (only when npc_id present).
    let mut owner = String::new();
    if let Some(npc_id_val) = item.get("npc_id") {
        if !clean_text(npc_id_val).is_empty() {
            let (id, error) = resolve_npc_id(world, npc_id_val);
            if !error.is_empty() {
                return (None, Some(uerr(&[("error", json!(error))])));
            }
            owner = id;
        }
    }

    let mut target = clean_text(item.get("target").unwrap_or(&Value::Null));
    let mut entity_id = first_present(item, &["entity_id", "entity", "about"]);
    let mut source_npc = first_present(item, &["source_npc", "source_npc_id"]);
    let known_name = clean_text(item.get("known_name").unwrap_or(&Value::Null));
    let location_id = clean_text(item.get("location_id").unwrap_or(&Value::Null));
    let location_name = clean_text(item.get("location_name").unwrap_or(&Value::Null));
    let region_id = clean_text(item.get("region_id").unwrap_or(&Value::Null));
    let region_name = clean_text(item.get("region_name").unwrap_or(&Value::Null));
    let scene_id = clean_text(item.get("scene_id").unwrap_or(&Value::Null));
    let importance = clean_text(item.get("importance").unwrap_or(&Value::Null));
    let aliases = clean_list(item.get("aliases").unwrap_or(&Value::Null));
    let (participants, error) =
        resolve_participants(world, item.get("participants").unwrap_or(&Value::Null));
    if !error.is_empty() {
        return (None, Some(uerr(&[("error", json!(error))])));
    }

    let scope_changing = item.get("scope").is_some() || item.get("visibility").is_some();
    if scope_changing {
        update_payload.insert("scope".to_string(), json!(state_record_scope_local(scope)));
        let existing_owner = clean_text(&Value::String(existing.owner.clone()));
        if scope == "npc" && owner.is_empty() && existing_owner.is_empty() {
            return (
                None,
                Some(uerr(&[(
                    "error",
                    json!("npc_id is required when changing scope to npc"),
                )])),
            );
        }
        if scope == "shared" {
            let has_owner = !owner.is_empty() || !existing_owner.is_empty();
            let existing_subject = clean_text(&Value::String(existing.subject.clone()));
            let has_target_or_participants = !target.is_empty()
                || !participants.is_empty()
                || !existing_subject.is_empty()
                || !existing.participants.is_empty();
            if !has_owner || !has_target_or_participants {
                return (
                    None,
                    Some(uerr(&[(
                        "error",
                        json!("npc_id and target or participants are required when changing scope to shared"),
                    )])),
                );
            }
        }
    }

    if !source_npc.is_empty() {
        let (id, error) = resolve_npc_id(world, &Value::String(source_npc.clone()));
        if !error.is_empty() {
            return (None, Some(uerr(&[("error", json!(error))])));
        }
        source_npc = id;
    }

    let effective_scope = if scope_changing {
        scope.to_string()
    } else {
        state_visibility_from_scope(&existing.scope)
    };
    if !target.is_empty() && effective_scope == "shared" {
        let target_actor = resolve_actor_target(world, &target);
        if target_actor.is_empty() {
            return (
                None,
                Some(uerr(&[
                    ("target", json!(target)),
                    ("error", json!("target for shared scope must be player or a known npc_id; use participants for multiple actors")),
                ])),
            );
        }
        target = target_actor;
    }

    if !known_name.is_empty() {
        let known_entity_id = if entity_id.is_empty() {
            existing.entity_id.clone()
        } else {
            entity_id.clone()
        };
        if known_entity_id.is_empty() {
            return (
                None,
                Some(uerr(&[(
                    "error",
                    json!("entity_id is required when setting known_name"),
                )])),
            );
        }
        // `_resolve_npc_id` returns ("", err) on failure — the Python error row
        // carries that (empty) resolved id, which `_drop_empty` then prunes.
        let (id, error) = resolve_npc_id(world, &Value::String(known_entity_id));
        if !error.is_empty() {
            return (
                None,
                Some(uerr(&[
                    ("entity_id", json!(id)),
                    (
                        "error",
                        json!("known_name requires entity_id to be an NPC id"),
                    ),
                ])),
            );
        }
        entity_id = id;
    }

    if !owner.is_empty() {
        update_payload.insert("owner".to_string(), json!(owner));
    }
    if !target.is_empty() {
        update_payload.insert("subject".to_string(), json!(target));
    }
    if !entity_id.is_empty() {
        update_payload.insert("entity_id".to_string(), json!(entity_id));
    }
    if !source_npc.is_empty() {
        update_payload.insert("source_npc".to_string(), json!(source_npc));
    }
    if item.get("participants").is_some() {
        update_payload.insert("participants".to_string(), json!(participants));
    }
    if item.get("location_id").is_some() {
        update_payload.insert("location_id".to_string(), json!(location_id));
    }
    if item.get("location_name").is_some() {
        update_payload.insert("location_name".to_string(), json!(location_name));
    }
    if item.get("region_id").is_some() {
        update_payload.insert("region_id".to_string(), json!(region_id));
    }
    if item.get("region_name").is_some() {
        update_payload.insert("region_name".to_string(), json!(region_name));
    }
    if item.get("scene_id").is_some() {
        update_payload.insert("scene_id".to_string(), json!(scene_id));
    }
    if item.get("importance").is_some() {
        update_payload.insert("importance".to_string(), json!(importance));
    }
    if item.get("aliases").is_some() {
        update_payload.insert("aliases".to_string(), json!(aliases));
    }
    if !source.is_empty() {
        update_payload.insert("source".to_string(), json!(source));
    }
    if !known_name.is_empty() {
        let mut metadata = existing.metadata.clone();
        metadata.insert("known_name".to_string(), json!(known_name));
        update_payload.insert("metadata".to_string(), Value::Object(metadata));
    }
    if let Some(active) = item.get("active") {
        update_payload.insert("active".to_string(), active.clone());
    }

    let mut record = existing.clone();
    for (key, value) in update_payload {
        apply_update_value(&mut record, &key, &value);
    }
    let _ = world.upsert_state_record_memory(&record, "world_state_memory");
    let row = json!({
        "index": index,
        "op": op,
        "type": record.kind,
        "id": record.record_id,
        "npc_id": record.owner,
        "target": record.subject,
        "entity_id": record.entity_id,
        "source_npc": record.source_npc,
        "participants": record.participants,
        "known_name": record.metadata.get("known_name").and_then(Value::as_str).unwrap_or(""),
        "location_id": record.location_id,
        "location_name": record.location_name,
        "region_id": record.region_id,
        "region_name": record.region_name,
        "scene_id": record.scene_id,
        "importance": record.importance,
        "aliases": record.aliases,
        "scope": state_visibility_from_scope(&record.scope),
        "hash": state_record_hash(&record),
        "status": "updated",
    });
    (Some(row), None)
}

fn first_present(item: &Value, keys: &[&str]) -> String {
    for key in keys {
        if let Some(v) = item.get(*key) {
            let s = clean_text(v);
            if !s.is_empty() {
                return s;
            }
        }
    }
    String::new()
}

// =========================================================================
// world-query (query_world_state) + de-dup cache
// =========================================================================

/// `_query_scope_key(scope, npc_id)`.
pub fn query_scope_key(scope: &str, npc_id: &str) -> String {
    let scope = if scope.trim().is_empty() {
        "player".to_string()
    } else {
        scope.trim().to_lowercase()
    };
    let npc_id = npc_id.trim().to_lowercase();
    if scope == "npc" && !npc_id.is_empty() {
        format!("npc:{npc_id}")
    } else {
        scope
    }
}

/// `_query_row_key(row)`.
fn query_row_key(row: &Value) -> String {
    let kind = clean_text(row.get("kind").unwrap_or(&Value::Null)).to_lowercase();
    let row_id = clean_text(row.get("id").unwrap_or(&Value::Null)).to_lowercase();
    let hash = clean_text(row.get("hash").unwrap_or(&Value::Null));
    let stable = if hash.is_empty() {
        clean_text(row.get("text").unwrap_or(&Value::Null))
    } else {
        hash
    };
    let digest = short_hash(&stable);
    if !row_id.is_empty() {
        format!("row:{kind}:{row_id}:{digest}")
    } else {
        format!("row:{kind}:text:{digest}")
    }
}

/// `_select_new_query_rows(session, scope_key, rows, limit)` -> (fresh, skipped).
fn select_new_query_rows(
    session: &mut Session,
    scope_key: &str,
    rows: &[Value],
    limit: usize,
) -> (Vec<Value>, i64) {
    let seen = session.query_seen_set(scope_key);
    let mut selected_keys: BTreeSet<String> = BTreeSet::new();
    let mut fresh: Vec<Value> = Vec::new();
    let mut skipped = 0;
    for row in rows {
        let key = query_row_key(row);
        if !key.is_empty() && (seen.contains(&key) || selected_keys.contains(&key)) {
            skipped += 1;
            continue;
        }
        if fresh.len() >= limit {
            continue;
        }
        fresh.push(row.clone());
        if !key.is_empty() {
            selected_keys.insert(key);
        }
    }
    let seen_mut = session.query_seen_set(scope_key);
    for k in &selected_keys {
        seen_mut.insert(k.clone());
    }
    (fresh, skipped)
}

/// `_query_terms(query)`.
fn query_terms(query: &str) -> Vec<String> {
    let q = clean_text(&Value::String(query.to_string())).to_lowercase();
    let re = regex::Regex::new(r"[\w\u{0400}-\u{04FF}\u{0401}\u{0451}-]+").unwrap();
    re.find_iter(&q)
        .map(|m| m.as_str().to_string())
        .filter(|t| t.chars().count() > 1)
        .collect()
}

/// `_score_query_text(query, terms, text)`.
fn score_query_text(query: &str, terms: &[String], text: &str) -> i64 {
    let haystack = clean_text(&Value::String(text.to_string())).to_lowercase();
    if haystack.is_empty() {
        return 0;
    }
    let mut score = 0;
    let clean_query = clean_text(&Value::String(query.to_string())).to_lowercase();
    if !clean_query.is_empty() && haystack.contains(&clean_query) {
        score += 100;
    }
    for term in terms {
        if haystack.contains(term) {
            score += 10;
        }
    }
    score
}

/// `_query_row(kind, text, **extra)`.
fn query_row(kind: &str, text: &str, extra: &[(&str, Value)]) -> Value {
    let mut row = Map::new();
    row.insert("kind".to_string(), json!(kind));
    row.insert(
        "text".to_string(),
        json!(clip_text(&Value::String(text.to_string()), 700)),
    );
    for (k, v) in extra {
        row.insert((*k).to_string(), v.clone());
    }
    drop_empty(&Value::Object(row))
}

/// `_scored_rows(query, rows, limit)`.
fn scored_rows(query: &str, rows: &[Value], limit: usize) -> Vec<Value> {
    let terms = query_terms(query);
    let mut scored: Vec<(i64, Value)> = Vec::new();
    for row in rows {
        let search_text = [
            "kind",
            "id",
            "npc_id",
            "target",
            "entity_id",
            "source_npc",
            "participants",
            "known_name",
            "location_id",
            "location_name",
            "region_id",
            "region_name",
            "scene_id",
            "importance",
            "aliases",
            "scope",
            "visibility",
            "status",
            "text",
        ]
        .iter()
        .filter_map(|key| {
            let t = clean_text(row.get(*key).unwrap_or(&Value::Null));
            if t.is_empty() {
                None
            } else {
                Some(t)
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
        let score = score_query_text(query, &terms, &search_text);
        if score > 0 {
            scored.push((score, row.clone()));
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    scored.into_iter().take(limit).map(|(_, r)| r).collect()
}

fn memory_meta_string(unit: &MemoryUnit, key: &str) -> String {
    unit.metadata
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string()
}

fn memory_meta_array(unit: &MemoryUnit, key: &str) -> Value {
    unit.metadata
        .get(key)
        .and_then(Value::as_array)
        .cloned()
        .map(Value::Array)
        .unwrap_or_else(|| Value::Array(Vec::new()))
}

fn memory_meta_joined_array(unit: &MemoryUnit, key: &str) -> String {
    unit.metadata
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

fn state_record_rows(world: &World, actor_id: &str) -> Vec<Value> {
    let access = world.memory_access_for_actor(actor_id);
    let mut rows = Vec::new();
    for unit in world.world_canon.memory.units.values() {
        if !unit.injection_state.is_default_visible() || !unit.is_visible_to(&access) {
            continue;
        }
        if !is_world_state_memory(unit) {
            continue;
        }
        let record_id = memory_meta_string(unit, "record_id");
        let mut legacy_kind = memory_meta_string(unit, "legacy_kind");
        if legacy_kind.is_empty() {
            legacy_kind = "memory".to_string();
        }
        let mut legacy_scope = memory_meta_string(unit, "legacy_scope");
        if legacy_scope.is_empty() {
            legacy_scope = "public".to_string();
        }
        let hash = {
            let metadata_hash = memory_meta_string(unit, "hash");
            if metadata_hash.is_empty() {
                short_hash(&format!("{}:{}", unit.memory_id, unit.summary))
            } else {
                metadata_hash
            }
        };
        rows.push(query_row(
            &format!("state_{legacy_kind}"),
            &unit.summary,
            &[
                ("id", json!(record_id)),
                ("memory_id", json!(unit.memory_id)),
                ("npc_id", json!(memory_meta_string(unit, "owner"))),
                ("target", json!(memory_meta_string(unit, "subject"))),
                ("entity_id", json!(memory_meta_string(unit, "entity_id"))),
                ("source_npc", json!(memory_meta_string(unit, "source_npc"))),
                ("participants", memory_meta_array(unit, "participants")),
                ("known_name", json!(memory_meta_string(unit, "known_name"))),
                (
                    "location_id",
                    json!(memory_meta_string(unit, "location_id")),
                ),
                (
                    "location_name",
                    json!(memory_meta_string(unit, "location_name")),
                ),
                ("region_id", json!(memory_meta_string(unit, "region_id"))),
                (
                    "region_name",
                    json!(memory_meta_string(unit, "region_name")),
                ),
                ("scene_id", json!(memory_meta_string(unit, "scene_id"))),
                ("importance", json!(memory_meta_string(unit, "importance"))),
                ("aliases", json!(memory_meta_joined_array(unit, "aliases"))),
                (
                    "visibility",
                    json!(state_visibility_from_scope(&legacy_scope)),
                ),
                ("status", json!(memory_meta_string(unit, "status"))),
                ("hash", json!(hash)),
            ],
        ));
    }
    rows
}

/// `_already_delivered_text(scope)`.
fn already_delivered_text(scope: &str) -> String {
    let label = match scope {
        "npc" => "NPC",
        "gm" => "GM",
        _ => "player",
    };
    format!(
        "No new matching world-state rows in {label} scope. \
Previous matches were already delivered in the active conversation context; \
after GM history compaction this delivery cache resets."
    )
}

fn query_payload_with_delivery_status(payload: Value, delivered: i64, scope: &str) -> Value {
    if delivered <= 0 {
        return payload;
    }
    let mut out = match payload {
        Value::Object(m) => m,
        _ => return Value::Object(Map::new()),
    };
    out.insert("already_delivered".to_string(), json!(delivered));
    let results_empty = match out.get("results") {
        Some(Value::Array(a)) => a.is_empty(),
        _ => true,
    };
    let text_empty = clean_text(out.get("text").unwrap_or(&Value::Null)).is_empty();
    if results_empty && text_empty {
        out.insert("status".to_string(), json!("already_delivered"));
        out.insert("text".to_string(), json!(already_delivered_text(scope)));
    }
    Value::Object(out)
}

/// `_player_query_payload(session, query, limit)`.
fn player_query_payload(session: &mut Session, query: &str, limit: usize) -> Value {
    let scope_key = query_scope_key("player", "");
    let fact = session.world.fact(query, "player", None);
    let payload = fact.as_tool_payload();
    let (payload, fact_delivered) =
        crate::query_dedup::filter_new_fact_payload(session, &scope_key, payload, query);
    let candidate_rows = state_record_rows(&session.world, "player");
    let rows = scored_rows(query, &candidate_rows, limit.max(candidate_rows.len()));
    let (rows, row_delivered) = select_new_query_rows(session, &scope_key, &rows, limit);
    let mut status = clean_text(payload.get("status").unwrap_or(&Value::Null));
    if status.is_empty() {
        status = "unknown".to_string();
    }
    if !rows.is_empty() && status == "unknown" {
        status = "known".to_string();
    }
    let out = json!({
        "scope": "player",
        "status": status,
        "text": payload.get("text").cloned().unwrap_or(json!("")),
        "results": rows,
        "sources": compact_sources(payload.get("sources").unwrap_or(&Value::Null), 3),
    });
    drop_empty(&query_payload_with_delivery_status(
        out,
        fact_delivered + row_delivered,
        "player",
    ))
}

fn npc_query_rows(session: &Session, npc_id: &str) -> Vec<Value> {
    let npc = match session.world.npcs.get(npc_id) {
        Some(n) => n.clone(),
        None => return Vec::new(),
    };
    let mut rows = state_record_rows(&session.world, npc_id);
    rows.push(query_row(
        "npc_goals",
        &npc.goals,
        &[("npc_id", json!(npc_id)), ("visibility", json!("npc"))],
    ));
    rows.push(query_row(
        "npc_knowledge",
        &npc.knowledge,
        &[("npc_id", json!(npc_id)), ("visibility", json!("npc"))],
    ));
    rows.push(query_row(
        "npc_secret",
        &npc.secret,
        &[("npc_id", json!(npc_id)), ("visibility", json!("npc"))],
    ));
    if let Some(summary) = session.npc_summaries.get(npc_id) {
        if !summary.is_empty() {
            rows.push(query_row(
                "npc_summary",
                summary,
                &[("npc_id", json!(npc_id)), ("visibility", json!("npc"))],
            ));
        }
    }
    if let Some(blocks) = session.commitments.get(npc_id) {
        let tail = blocks
            .iter()
            .rev()
            .take(crate::session::COMMIT_BLOCKS)
            .collect::<Vec<_>>();
        for (i, block) in tail.into_iter().rev().enumerate() {
            rows.push(query_row(
                "npc_memory",
                block,
                &[
                    ("id", json!(format!("{npc_id}:memory:{}", i + 1))),
                    ("npc_id", json!(npc_id)),
                    ("visibility", json!("npc")),
                ],
            ));
        }
    }
    rows
}

fn gm_query_rows(session: &Session) -> Vec<Value> {
    let world = &session.world;
    let mut rows = vec![
        query_row(
            "public_intro",
            &world.public,
            &[("visibility", json!("player"))],
        ),
        query_row("gm_canon", &world.canon, &[("visibility", json!("gm"))]),
    ];
    rows.extend(state_record_rows(world, "debug"));
    for (i, event_text) in world.hidden_events.iter().enumerate() {
        rows.push(query_row(
            "hidden_event",
            event_text,
            &[
                ("id", json!(format!("hidden:{}", i + 1))),
                ("visibility", json!("gm")),
            ],
        ));
    }
    for record in &world.fact_records {
        if record.kind == "truth" && record.fact_id == "hidden_truth" {
            continue;
        }
        let visibility = if record.kind == "truth" {
            "gm"
        } else {
            "player"
        };
        rows.push(query_row(
            &format!("{}_fact", record.kind),
            &record.text,
            &[
                ("id", json!(record.fact_id)),
                ("visibility", json!(visibility)),
                (
                    "status",
                    json!(if record.confirmed {
                        "known"
                    } else {
                        "unconfirmed"
                    }),
                ),
            ],
        ));
    }
    for (i, rumor) in world.rumors.iter().enumerate() {
        rows.push(query_row(
            "rumor",
            &rumor.text,
            &[
                ("id", json!(format!("rumor:{}", i + 1))),
                ("npc_id", json!(rumor.speaker)),
                ("visibility", json!("player")),
                ("status", json!("unconfirmed")),
            ],
        ));
    }
    for (npc_id, npc) in &world.npcs {
        rows.push(query_row(
            "npc_role",
            &format!("{}: {}", npc.name, npc.role),
            &[("npc_id", json!(npc_id)), ("visibility", json!("player"))],
        ));
        rows.push(query_row(
            "npc_goals",
            &npc.goals,
            &[("npc_id", json!(npc_id)), ("visibility", json!("npc"))],
        ));
        rows.push(query_row(
            "npc_knowledge",
            &npc.knowledge,
            &[("npc_id", json!(npc_id)), ("visibility", json!("npc"))],
        ));
        rows.push(query_row(
            "npc_secret",
            &npc.secret,
            &[("npc_id", json!(npc_id)), ("visibility", json!("npc"))],
        ));
        if let Some(summary) = session.npc_summaries.get(npc_id) {
            if !summary.is_empty() {
                rows.push(query_row(
                    "npc_summary",
                    summary,
                    &[("npc_id", json!(npc_id)), ("visibility", json!("npc"))],
                ));
            }
        }
        if let Some(blocks) = session.commitments.get(npc_id) {
            let tail = blocks
                .iter()
                .rev()
                .take(crate::session::COMMIT_BLOCKS)
                .collect::<Vec<_>>();
            for (i, block) in tail.into_iter().rev().enumerate() {
                rows.push(query_row(
                    "npc_memory",
                    block,
                    &[
                        ("id", json!(format!("{npc_id}:memory:{}", i + 1))),
                        ("npc_id", json!(npc_id)),
                        ("visibility", json!("npc")),
                    ],
                ));
            }
        }
    }
    if !session.gm_summary.trim().is_empty() {
        rows.push(query_row(
            "gm_summary",
            &session.gm_summary,
            &[("visibility", json!("gm"))],
        ));
    }
    rows
}

/// `_query_world_state(session, args)`.
pub fn query_world_state(session: &mut Session, args: &Value) -> Value {
    let scope = visibility(args.get("scope").unwrap_or(&Value::Null), "player");
    let query = clean_text(args.get("query").unwrap_or(&Value::Null));
    if query.is_empty() {
        return json!({"scope": scope, "status": "error", "error": "query is required"});
    }
    let limit = {
        let raw = args
            .get("max_results")
            .and_then(|v| v.as_i64())
            .unwrap_or(5);
        raw.clamp(1, 12) as usize
    };

    if scope == "player" {
        return player_query_payload(session, &query, limit);
    }

    if scope == "npc" {
        let (npc_id, error) =
            resolve_npc_id(&session.world, args.get("npc_id").unwrap_or(&Value::Null));
        if !error.is_empty() {
            return json!({"scope": scope, "status": "error", "error": error});
        }
        let scope_key = query_scope_key("npc", &npc_id);
        let candidate_rows = npc_query_rows(session, &npc_id);
        let mut rows = scored_rows(&query, &candidate_rows, limit.max(candidate_rows.len()));
        let public_payload = session.world.fact(&query, "public", None).as_tool_payload();
        let public_text = clean_text(public_payload.get("text").unwrap_or(&Value::Null));
        let public_status = clean_text(public_payload.get("status").unwrap_or(&Value::Null));
        if public_status != "unknown" && !public_text.is_empty() {
            rows.insert(
                0,
                query_row(
                    "public_lookup",
                    &public_text,
                    &[
                        ("visibility", json!("player")),
                        ("status", json!(public_status)),
                    ],
                ),
            );
        }
        let (rows, delivered) = select_new_query_rows(session, &scope_key, &rows, limit);
        let status = if !rows.is_empty() {
            "known"
        } else if delivered > 0 {
            "already_delivered"
        } else {
            "unknown"
        };
        let text = if !rows.is_empty() {
            String::new()
        } else if delivered > 0 {
            already_delivered_text(&scope)
        } else {
            "Nothing in this NPC scope matched the query.".to_string()
        };
        return drop_empty(&json!({
            "scope": scope,
            "status": status,
            "results": rows,
            "text": text,
            "already_delivered": delivered,
        }));
    }

    let scope_key = query_scope_key("gm", "");
    let candidate_rows = gm_query_rows(session);
    let rows = scored_rows(&query, &candidate_rows, limit.max(candidate_rows.len()));
    let (rows, delivered) = select_new_query_rows(session, &scope_key, &rows, limit);
    let status = if !rows.is_empty() {
        "known"
    } else if delivered > 0 {
        "already_delivered"
    } else {
        "unknown"
    };
    let text = if !rows.is_empty() {
        String::new()
    } else if delivered > 0 {
        already_delivered_text(&scope)
    } else {
        "Nothing in GM scope matched the query.".to_string()
    };
    drop_empty(&json!({
        "scope": "gm",
        "status": status,
        "results": rows,
        "text": text,
        "already_delivered": delivered,
    }))
}

fn memory_tier(raw: &Value, default: MemoryTier) -> MemoryTier {
    match clean_text(raw).to_lowercase().as_str() {
        "episode" => MemoryTier::Episode,
        "arc" => MemoryTier::Arc,
        "durable" | "long" | "long_term" => MemoryTier::Durable,
        "raw" | "" => default,
        _ => default,
    }
}

fn memory_truth(raw: &Value) -> MemoryTruthStatus {
    match clean_text(raw).to_lowercase().as_str() {
        "actual" | "true" | "truth" => MemoryTruthStatus::Actual,
        "claim" => MemoryTruthStatus::Claim,
        "rumor" | "rumour" => MemoryTruthStatus::Rumor,
        "belief" => MemoryTruthStatus::Belief,
        "lie" | "false" => MemoryTruthStatus::Lie,
        _ => MemoryTruthStatus::Unknown,
    }
}

fn memory_limit(args: &Value, default: i64, max: i64) -> usize {
    args.get("max_results")
        .and_then(Value::as_i64)
        .unwrap_or(default)
        .clamp(1, max) as usize
}

fn bool_arg(args: &Value, key: &str) -> bool {
    args.get(key).map(crate::truthy).unwrap_or(false)
}

fn memory_access_payload(
    session: &Session,
    args: &Value,
) -> Result<(String, gml_world::MemoryAccess), String> {
    let mut scope = clean_text(args.get("scope").unwrap_or(&Value::Null)).to_lowercase();
    let npc_id = clean_text(args.get("npc_id").unwrap_or(&Value::Null));
    if scope.is_empty() {
        scope = if npc_id.is_empty() {
            "player".to_string()
        } else {
            "actor".to_string()
        };
    }
    match scope.as_str() {
        "gm" | "debug" | "gm_private" | "true_canon" => {
            Ok(("gm".to_string(), gml_world::MemoryAccess::gm()))
        }
        "npc" | "actor" => {
            let (resolved, error) =
                resolve_npc_id(&session.world, args.get("npc_id").unwrap_or(&Value::Null));
            if !error.is_empty() {
                return Err(error);
            }
            Ok((
                format!("actor:{resolved}"),
                session.world.memory_access_for_actor(&resolved),
            ))
        }
        "player" => Ok((
            "player".to_string(),
            session.world.memory_access_for_player(),
        )),
        "public" => Ok((
            "public".to_string(),
            session.world.memory_access_for_public(),
        )),
        other => {
            let scope_id = clean_text(args.get("scope_id").unwrap_or(&Value::Null));
            let access = session.world.memory_access_for_scope(other, &scope_id)?;
            let label = if scope_id.is_empty() {
                other.to_string()
            } else {
                format!("{other}:{scope_id}")
            };
            Ok((label, access))
        }
    }
}

/// `get_memory(query, scope, ...)` — scoped memory lookup with access gate
/// before ranking.
pub fn get_memory(session: &mut Session, args: &Value) -> Value {
    let query = clean_text(args.get("query").unwrap_or(&Value::Null));
    if query.is_empty() {
        return json!({"scope": "memory", "status": "error", "error": "query is required"});
    }
    let (scope_label, access) = match memory_access_payload(session, args) {
        Ok(v) => v,
        Err(error) => return json!({"scope": "memory", "status": "error", "error": error}),
    };
    let limit = memory_limit(args, 5, 12);
    let include_cold = bool_arg(args, "include_cold");
    let include_details = bool_arg(args, "include_details");
    let retrieval_report = crate::rag::retrieve_memory_rows_report(
        &session.world,
        &access,
        &query,
        limit,
        include_cold,
        include_details,
    );
    let retrieval_status = retrieval_report.status;
    let rows = retrieval_report.rows.unwrap_or_else(|| {
        session
            .world
            .memory_rows_for_access(&access, &query, limit, include_cold, include_details)
    });
    let status = if rows.is_empty() { "unknown" } else { "known" };
    drop_empty(&json!({
        "scope": scope_label,
        "status": status,
        "query": query,
        "include_cold": include_cold,
        "include_details": include_details,
        "retrieval": retrieval_status,
        "results": rows,
        "text": if status == "unknown" { "No scoped memory matched the query." } else { "" },
    }))
}

/// Actor-bound memory recall for the NPC runtime tool `remember`.
///
/// The public tool schema never accepts `npc_id`; `turn.rs` injects the current
/// actor before calling this helper.
pub fn npc_memory_recall(session: &mut Session, args: &Value) -> Value {
    let query = clean_text(args.get("query").unwrap_or(&Value::Null));
    if query.is_empty() {
        return json!({"scope": "npc", "status": "error", "error": "query is required"});
    }
    let (npc_id, error) =
        resolve_npc_id(&session.world, args.get("npc_id").unwrap_or(&Value::Null));
    if !error.is_empty() {
        return json!({"scope": "npc", "status": "error", "error": error});
    }
    let access = session.world.memory_access_for_actor(&npc_id);
    let limit = memory_limit(args, 5, 8);
    let include_cold = bool_arg(args, "include_cold");
    let retrieval_report = crate::rag::retrieve_memory_rows_report(
        &session.world,
        &access,
        &query,
        limit,
        include_cold,
        false,
    );
    let retrieval_status = retrieval_report.status;
    let rows = retrieval_report.rows.unwrap_or_else(|| {
        session
            .world
            .memory_rows_for_access(&access, &query, limit, include_cold, false)
    });
    let status = if rows.is_empty() { "unknown" } else { "known" };
    drop_empty(&json!({
        "scope": "npc",
        "npc_id": npc_id,
        "status": status,
        "query": query,
        "retrieval": retrieval_status,
        "results": rows,
        "text": if status == "unknown" { "This NPC has no accessible matching memory." } else { "" },
    }))
}

/// `npc_note_memory(...)` — actor-bound write for the NPC runtime tool.
///
/// The tool schema does not expose `npc_id`; `turn.rs` injects the active actor.
/// The note stays actor-private even if the NPC asks for shared/public privacy:
/// public rumour spread is a separate GM/world-simulation action.
pub fn npc_note_memory(session: &mut Session, args: &Value) -> Value {
    let text = clip_text(args.get("text").unwrap_or(&Value::Null), 900);
    if text.is_empty() {
        return json!({"ok": false, "scope": "npc", "status": "error", "error": "text is required"});
    }
    let (npc_id, error) =
        resolve_npc_id(&session.world, args.get("npc_id").unwrap_or(&Value::Null));
    if !error.is_empty() {
        return json!({"ok": false, "scope": "npc", "status": "error", "error": error});
    }

    let kind = {
        let raw = clean_text(args.get("kind").unwrap_or(&Value::Null));
        if raw.is_empty() {
            "interaction".to_string()
        } else {
            raw
        }
    };
    let about = clean_text(args.get("about").unwrap_or(&Value::Null));
    let requested_privacy = {
        let raw = clean_text(args.get("privacy").unwrap_or(&Value::Null));
        if raw.is_empty() {
            "private".to_string()
        } else {
            raw
        }
    };
    let anchors = clean_list(args.get("anchors").unwrap_or(&Value::Null));
    let now = session
        .world
        .world_canon
        .clock_minutes
        .max(session.world.time.absolute_minutes);
    let current_place = session
        .world
        .world_canon
        .actor(&npc_id)
        .and_then(|actor| actor.location.place().map(ToString::to_string))
        .unwrap_or_else(|| session.world.scene.location_id.clone());

    let mut place_ids = BTreeSet::new();
    if !current_place.is_empty() {
        place_ids.insert(current_place);
    }
    let mut actor_ids = BTreeSet::new();
    actor_ids.insert(npc_id.clone());
    let mut faction_ids = BTreeSet::new();
    let mut topic_tags = BTreeSet::new();
    topic_tags.insert(kind.clone());
    if !about.is_empty() {
        topic_tags.insert(about.clone());
    }
    for anchor in &anchors {
        let trimmed = anchor.trim();
        if let Some(rest) = trimmed.strip_prefix("place:") {
            if !rest.trim().is_empty() {
                place_ids.insert(rest.trim().to_string());
            }
        } else if let Some(rest) = trimmed.strip_prefix("actor:") {
            if !rest.trim().is_empty() {
                actor_ids.insert(rest.trim().to_string());
            }
        } else if let Some(rest) = trimmed.strip_prefix("faction:") {
            if !rest.trim().is_empty() {
                faction_ids.insert(rest.trim().to_string());
            }
        } else if !trimmed.is_empty() {
            topic_tags.insert(trimmed.to_string());
        }
    }
    let mut metadata = Map::new();
    metadata.insert("kind".to_string(), json!(kind));
    metadata.insert("requested_privacy".to_string(), json!(requested_privacy));
    if !about.is_empty() {
        metadata.insert("about".to_string(), json!(about));
    }
    if !anchors.is_empty() {
        metadata.insert("anchors".to_string(), json!(anchors));
    }

    let id = session.world.add_memory_unit(MemoryUnit {
        tier: MemoryTier::Raw,
        owner_scope: format!("actor:{npc_id}"),
        visibility_scopes: vec![format!("actor:{npc_id}")],
        summary: text,
        time_start: now,
        time_end: now,
        place_ids: place_ids.into_iter().collect(),
        actor_ids: actor_ids.into_iter().collect(),
        faction_ids: faction_ids.into_iter().collect(),
        topic_tags: topic_tags.into_iter().collect(),
        metadata,
        truth_status: MemoryTruthStatus::Belief,
        injection_state: MemoryInjectionState::Hot,
        created_by: format!("npc_tool:{npc_id}"),
        ..Default::default()
    });
    let row = session
        .world
        .world_canon
        .memory
        .get(&id)
        .map(|unit| unit.to_row(false))
        .unwrap_or_else(|| json!({}));
    drop_empty(&json!({
        "ok": true,
        "scope": "npc",
        "npc_id": npc_id,
        "status": "stored",
        "memory_id": id,
        "result": row,
        "privacy_note": "Stored as this NPC's private memory. Public rumour spread is handled separately.",
    }))
}

/// `npc_recall_relationship(target)` — actor-bound relationship recall.
pub fn npc_recall_relationship(session: &mut Session, args: &Value) -> Value {
    let (npc_id, error) =
        resolve_npc_id(&session.world, args.get("npc_id").unwrap_or(&Value::Null));
    if !error.is_empty() {
        return json!({"scope": "npc", "status": "error", "error": error});
    }
    let raw_target = clean_text(args.get("target").unwrap_or(&Value::Null));
    if raw_target.is_empty() {
        return json!({"scope": "npc", "status": "error", "error": "target is required"});
    }
    let target = if raw_target.eq_ignore_ascii_case("player")
        || raw_target.eq_ignore_ascii_case("игрок")
    {
        "player".to_string()
    } else {
        let (resolved, resolve_error) =
            resolve_npc_id(&session.world, &Value::String(raw_target.clone()));
        if resolve_error.is_empty() {
            resolved
        } else {
            raw_target
        }
    };
    let limit = memory_limit(args, 5, 8);
    let access = session.world.memory_access_for_actor(&npc_id);
    let query = format!("relationship {npc_id} {target} отношения {target}");
    let retrieval_report = crate::rag::retrieve_memory_rows_report(
        &session.world,
        &access,
        &query,
        limit,
        false,
        false,
    );
    let retrieval_status = retrieval_report.status;
    let rows = retrieval_report.rows.unwrap_or_else(|| {
        session
            .world
            .memory_rows_for_access(&access, &query, limit, false, false)
    });
    let canon_attitude = session
        .world
        .world_canon
        .actor(&npc_id)
        .map(|actor| {
            if target == "player" {
                actor.attitude_to_player
            } else {
                actor.relations.get(&target).copied().unwrap_or(0)
            }
        })
        .unwrap_or(0);
    let scene_attitude = session
        .world
        .scene
        .presence
        .get(&npc_id)
        .map(|presence| presence.attitude.clone())
        .unwrap_or_default();
    let status = if rows.is_empty() && canon_attitude == 0 && scene_attitude.trim().is_empty() {
        "unknown"
    } else {
        "known"
    };
    drop_empty(&json!({
        "scope": "npc",
        "npc_id": npc_id,
        "target": target,
        "status": status,
        "canon_attitude": canon_attitude,
        "scene_attitude": scene_attitude,
        "retrieval": retrieval_status,
        "results": rows,
        "text": if status == "unknown" {
            "This NPC has no specific accessible relationship memory for the target."
        } else {
            ""
        },
    }))
}

fn build_memory_unit_from_args(
    args: &Value,
    default_tier: MemoryTier,
) -> Result<MemoryUnit, String> {
    let summary = clip_text(args.get("summary").unwrap_or(&Value::Null), 900);
    if summary.is_empty() {
        return Err("summary is required".to_string());
    }
    let owner_scope = clean_text(args.get("owner_scope").unwrap_or(&Value::Null));
    if owner_scope.is_empty() {
        return Err("owner_scope is required".to_string());
    }
    let confidence = args
        .get("confidence")
        .and_then(Value::as_i64)
        .map(|n| n.clamp(0, 100) as u8);
    let mut metadata = Map::new();
    for key in ["entity_id", "known_name"] {
        let value = clean_text(args.get(key).unwrap_or(&Value::Null));
        if !value.is_empty() {
            metadata.insert(key.to_string(), json!(value));
        }
    }
    Ok(MemoryUnit {
        tier: memory_tier(args.get("tier").unwrap_or(&Value::Null), default_tier),
        owner_scope,
        visibility_scopes: clean_list(args.get("visibility_scopes").unwrap_or(&Value::Null)),
        summary,
        details: clip_text(args.get("details").unwrap_or(&Value::Null), 4000),
        facts_claimed: clean_list(args.get("facts_claimed").unwrap_or(&Value::Null)),
        uncertainties: clean_list(args.get("uncertainties").unwrap_or(&Value::Null)),
        source_event_ids: clean_list(args.get("source_event_ids").unwrap_or(&Value::Null)),
        source_account_ids: clean_list(args.get("source_account_ids").unwrap_or(&Value::Null)),
        source_state_record_ids: clean_list(
            args.get("source_state_record_ids").unwrap_or(&Value::Null),
        ),
        source_memory_ids: clean_list(args.get("source_memory_ids").unwrap_or(&Value::Null)),
        time_start: 0,
        time_end: 0,
        place_ids: clean_list(args.get("place_ids").unwrap_or(&Value::Null)),
        actor_ids: clean_list(args.get("actor_ids").unwrap_or(&Value::Null)),
        faction_ids: clean_list(args.get("faction_ids").unwrap_or(&Value::Null)),
        topic_tags: clean_list(args.get("topic_tags").unwrap_or(&Value::Null)),
        metadata,
        confidence,
        truth_status: memory_truth(args.get("truth_status").unwrap_or(&Value::Null)),
        created_by: "gm_tool".to_string(),
        ..Default::default()
    })
}

fn validate_memory_owner(session: &Session, unit: &MemoryUnit) -> Result<(), String> {
    let owner = gml_world::canon::canonical_scope(&unit.owner_scope, "");
    if let Some(actor_id) = owner.strip_prefix("actor:") {
        if !session.world.npcs.contains_key(actor_id)
            && !session.world.world_canon.actors.contains_key(actor_id)
        {
            return Err(format!("unknown actor owner_scope: {actor_id}"));
        }
    }
    if let Some(place_id) = owner.strip_prefix("place:") {
        if !session.world.world_canon.places.contains_key(place_id) {
            return Err(format!("unknown place owner_scope: {place_id}"));
        }
    }
    if let Some(faction_id) = owner.strip_prefix("faction:") {
        if !session.world.world_canon.factions.contains_key(faction_id) {
            return Err(format!("unknown faction owner_scope: {faction_id}"));
        }
    }
    Ok(())
}

fn validate_memory_identity(session: &mut Session, unit: &mut MemoryUnit) -> Result<(), String> {
    let known_name = unit
        .metadata
        .get("known_name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if known_name.is_empty() {
        return Ok(());
    }
    let raw_entity = unit
        .metadata
        .get("entity_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if raw_entity.is_empty() {
        return Err("entity_id is required when setting known_name".to_string());
    }
    let (entity_id, error) = resolve_npc_id(&session.world, &Value::String(raw_entity));
    if !error.is_empty() {
        return Err("known_name requires entity_id to be an NPC id".to_string());
    }
    unit.metadata
        .insert("entity_id".to_string(), json!(entity_id.clone()));
    unit.metadata
        .insert("known_name".to_string(), json!(known_name));
    if !unit.actor_ids.contains(&entity_id) {
        unit.actor_ids.push(entity_id);
    }
    if !unit.topic_tags.iter().any(|tag| tag == "known_name") {
        unit.topic_tags.push("known_name".to_string());
    }
    Ok(())
}

fn validate_consolidation_sources(
    session: &Session,
    unit: &mut MemoryUnit,
    source_ids: &[String],
    args: &Value,
) -> Result<(), String> {
    let mut sources = Vec::with_capacity(source_ids.len());
    for id in source_ids {
        let Some(source) = session.world.world_canon.memory.get(id) else {
            return Err("unknown source_memory_ids".to_string());
        };
        sources.push(source);
    }
    let Some(first) = sources.first() else {
        return Err("source_memory_ids is required".to_string());
    };

    let owner = gml_world::canon::canonical_scope(&unit.owner_scope, "");
    if owner != first.owner_scope {
        return Err("source memories must match the crystal owner_scope".to_string());
    }
    unit.owner_scope = owner;

    for source in &sources {
        if source.owner_scope != first.owner_scope {
            return Err("source memories must share one owner_scope".to_string());
        }
        if source.truth_status != first.truth_status {
            return Err("source memories must share one truth_status".to_string());
        }
        if source.visibility_scopes != first.visibility_scopes {
            return Err("source memories must share one visibility scope set".to_string());
        }
    }

    let truth_supplied = !clean_text(args.get("truth_status").unwrap_or(&Value::Null)).is_empty();
    if truth_supplied && unit.truth_status != first.truth_status {
        return Err("crystal truth_status must match its source memories".to_string());
    }
    unit.truth_status = first.truth_status.clone();

    let visibility_supplied = matches!(args.get("visibility_scopes"), Some(Value::Array(_)));
    if visibility_supplied {
        unit.visibility_scopes = unit
            .visibility_scopes
            .iter()
            .map(|scope| gml_world::canon::canonical_scope(scope, ""))
            .filter(|scope| !scope.is_empty())
            .collect();
        if unit.visibility_scopes != first.visibility_scopes {
            return Err("crystal visibility_scopes must match its source memories".to_string());
        }
    }
    unit.visibility_scopes = first.visibility_scopes.clone();
    Ok(())
}

/// `note_memory(...)` — write one scoped memory card.
pub fn note_memory(session: &mut Session, args: &Value) -> Value {
    let mut unit = match build_memory_unit_from_args(args, MemoryTier::Raw) {
        Ok(unit) => unit,
        Err(error) => {
            return json!({"ok": false, "status": "error", "error": error, "errors": [{"type": "memory", "status": "error", "error": error}]});
        }
    };
    if let Err(error) = validate_memory_owner(session, &unit) {
        return json!({"ok": false, "status": "error", "error": error, "errors": [{"type": "memory", "status": "error", "error": error}]});
    }
    if let Err(error) = validate_memory_identity(session, &mut unit) {
        return json!({"ok": false, "status": "error", "error": error, "errors": [{"type": "memory", "status": "error", "error": error}]});
    }
    let now = session
        .world
        .world_canon
        .clock_minutes
        .max(session.world.time.absolute_minutes);
    unit.time_start = now;
    unit.time_end = now;
    let id = session.world.add_memory_unit(unit);
    let row = session
        .world
        .world_canon
        .memory
        .get(&id)
        .map(|unit| unit.to_row(false))
        .unwrap_or_else(|| json!({}));
    drop_empty(&json!({
        "ok": true,
        "status": "stored",
        "memory_id": id,
        "applied": [{
            "op": "add",
            "type": "memory",
            "id": id,
            "scope": row.get("owner_scope").cloned().unwrap_or(Value::Null),
            "status": "stored",
            "tier": row.get("tier").cloned().unwrap_or(Value::Null),
        }],
        "result": row,
    }))
}

/// `consolidate_memory(...)` — derive a higher-tier memory and keep sources.
pub fn consolidate_memory(session: &mut Session, args: &Value) -> Value {
    let source_ids = clean_list(args.get("source_memory_ids").unwrap_or(&Value::Null));
    if source_ids.is_empty() {
        return json!({"ok": false, "status": "error", "error": "source_memory_ids is required", "errors": [{"type": "memory", "status": "error", "error": "source_memory_ids is required"}]});
    }
    let mut missing = Vec::new();
    for id in &source_ids {
        if session.world.world_canon.memory.get(id).is_none() {
            missing.push(id.clone());
        }
    }
    if !missing.is_empty() {
        return json!({"ok": false, "status": "error", "error": "unknown source_memory_ids", "missing": missing, "errors": [{"type": "memory", "status": "error", "error": "unknown source_memory_ids"}]});
    }
    let mut unit = match build_memory_unit_from_args(args, MemoryTier::Episode) {
        Ok(unit) => unit,
        Err(error) => {
            return json!({"ok": false, "status": "error", "error": error, "errors": [{"type": "memory", "status": "error", "error": error}]});
        }
    };
    if let Err(error) = validate_memory_owner(session, &unit) {
        return json!({"ok": false, "status": "error", "error": error, "errors": [{"type": "memory", "status": "error", "error": error}]});
    }
    if let Err(error) = validate_consolidation_sources(session, &mut unit, &source_ids, args) {
        return json!({"ok": false, "status": "error", "error": error, "errors": [{"type": "memory", "status": "error", "error": error}]});
    }
    unit.source_memory_ids = source_ids.clone();
    let now = session
        .world
        .world_canon
        .clock_minutes
        .max(session.world.time.absolute_minutes);
    unit.time_start = now;
    unit.time_end = now;
    let (crystal_id, consumed_source_ids) = session.world.consolidate_memory_unit(unit);
    let row = session
        .world
        .world_canon
        .memory
        .get(&crystal_id)
        .map(|unit| unit.to_row(false))
        .unwrap_or_else(|| json!({}));
    drop_empty(&json!({
        "ok": true,
        "status": "stored",
        "crystal_id": crystal_id,
        "memory_id": crystal_id,
        "source_memory_ids": source_ids,
        "consumed_source_ids": consumed_source_ids,
        "not_deleted": true,
        "applied": [{
            "op": "consolidate",
            "type": "memory",
            "id": crystal_id,
            "scope": row.get("owner_scope").cloned().unwrap_or(Value::Null),
            "status": "stored",
            "tier": row.get("tier").cloned().unwrap_or(Value::Null),
        }],
        "result": row,
    }))
}
