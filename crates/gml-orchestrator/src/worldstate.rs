//! World-state mutation (`update_world_state`) and query (`query_world_state`)
//! ports from `orchestrator.py`, plus the world-query de-dup / pagination cache
//! and the player/gm/npc query-row assembly.

use serde_json::{json, Map, Value};
use std::collections::BTreeSet;

use gml_world::{state_record_hash, StateRecord, StateRecordQuery, World};

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
    for record in &world.state_records {
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
            let (id, error) =
                resolve_npc_id(world, &Value::String(item.clone()));
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

fn supports_state_records(_world: &World) -> bool {
    // gml-world always implements add_state_records (callable in Python check).
    true
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
        if supports_state_records(&session.world) {
            return apply_state_record_item(session, index, &op, &item_type, &text, "player", &source, item);
        }
        return (
            None,
            Some(err_row(&[
                ("index", json!(index)),
                ("op", json!(op)),
                ("type", json!(item_type)),
                ("error", json!("delete requires state-record support")),
            ])),
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
    apply_state_record_item(session, index, &op, &item_type, &text, &scope, &source, item)
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
                    Some(hash_conflict_error(&session.world, index, op, item_type, &record_id, &expected)),
                );
            }
        }
        let deleted = session
            .world
            .delete_state_records(&json!([record_id.clone()]), false);
        if deleted == 0 {
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
        return apply_state_record_update(session, index, op, item_type, text, scope, source, item, &record_id);
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

fn record_by_id<'a>(world: &'a World, record_id: &str) -> Option<&'a StateRecord> {
    let wanted = record_id.trim();
    if wanted.is_empty() {
        return None;
    }
    world.state_records.iter().find(|r| r.record_id == wanted)
}

fn record_hash_by_id(world: &World, record_id: &str) -> Option<String> {
    record_by_id(world, record_id).map(state_record_hash)
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
    let actual_hash = record.map(state_record_hash).unwrap_or_default();
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
    let (participants, error) = resolve_participants(world, item.get("participants").unwrap_or(&Value::Null));
    if !error.is_empty() {
        return (None, Some(err_row(&[("index", json!(index)), ("op", json!(op)), ("type", json!(item_type)), ("error", json!(error))])));
    }
    if !source_npc.is_empty() {
        let (id, e) = resolve_npc_id(world, &Value::String(source_npc.clone()));
        if !e.is_empty() {
            return (None, Some(err_row(&[("index", json!(index)), ("op", json!(op)), ("type", json!(item_type)), ("error", json!(e))])));
        }
        source_npc = id;
    }
    let mut entity_id = entity_id;
    if !known_name.is_empty() {
        if entity_id.is_empty() {
            return (None, Some(err_row(&[("index", json!(index)), ("op", json!(op)), ("type", json!(item_type)), ("error", json!("entity_id is required when setting known_name"))])));
        }
        let (id, e) = resolve_npc_id(world, &Value::String(entity_id.clone()));
        if !e.is_empty() {
            return (None, Some(err_row(&[("index", json!(index)), ("op", json!(op)), ("type", json!(item_type)), ("entity_id", json!(entity_id)), ("error", json!("known_name requires entity_id to be an NPC id"))])));
        }
        entity_id = id;
    }
    let needs_npc = matches!(item_type, "npc_memory" | "relationship" | "goal" | "goals")
        || matches!(scope, "npc" | "shared");
    if needs_npc {
        let (id, e) = resolve_npc_id(world, item.get("npc_id").unwrap_or(&Value::Null));
        if !e.is_empty() {
            return (None, Some(err_row(&[("index", json!(index)), ("op", json!(op)), ("type", json!(item_type)), ("error", json!(e))])));
        }
        owner = id;
    }
    if item_type == "relationship" && target.is_empty() {
        return (None, Some(err_row(&[("index", json!(index)), ("op", json!(op)), ("type", json!(item_type)), ("npc_id", json!(owner)), ("error", json!("target is required for relationship"))])));
    }
    if !target.is_empty() && scope == "shared" {
        let actor = resolve_actor_target(world, &target);
        if actor.is_empty() {
            return (None, Some(err_row(&[("index", json!(index)), ("op", json!(op)), ("type", json!(item_type)), ("target", json!(target)), ("error", json!("target for shared scope must be player or a known npc_id; use participants for multiple actors"))])));
        }
        target = actor;
    }
    if scope == "shared" && target.is_empty() && participants.is_empty() {
        return (None, Some(err_row(&[("index", json!(index)), ("op", json!(op)), ("type", json!(item_type)), ("error", json!("target or participants is required for shared scope"))])));
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
    // same `world.state_records`); the `if let` keeps the borrow checker happy.
    if let Some(existing) = mergeable_id.and_then(|id| record_by_id(world, &id).cloned()) {
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
            let update_payload = json!({
                "id": existing.record_id,
                "participants": merged_participants,
            });
            let updated = world.update_state_records(&Value::Array(vec![update_payload]));
            let rec = updated.into_iter().next().unwrap_or(existing);
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

    // Relationship-already-exists branch (via state_records_for).
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
            world.state_records_for(&query).first().map(|r| (*r).clone());
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
        let existing_ids: Vec<String> = world
            .state_records_for(&query)
            .iter()
            .map(|r| r.record_id.clone())
            .collect();
        if !existing_ids.is_empty() {
            let ids: Vec<Value> = existing_ids.into_iter().map(Value::String).collect();
            world.delete_state_records(&Value::Array(ids), false);
        }
    }

    let status = if item_type == "rumor" { "unconfirmed" } else { "known" };
    let mut record_payload = Map::new();
    record_payload.insert("kind".to_string(), json!(state_record_kind_local(item_type)));
    record_payload.insert("text".to_string(), json!(text));
    record_payload.insert("scope".to_string(), json!(state_record_scope_local(scope)));
    record_payload.insert("owner".to_string(), json!(owner));
    record_payload.insert("subject".to_string(), json!(target));
    record_payload.insert(
        "source".to_string(),
        json!(if source.is_empty() { "gm_tool" } else { source }),
    );
    record_payload.insert("status".to_string(), json!(status));
    record_payload.insert("entity_id".to_string(), json!(entity_id));
    record_payload.insert("source_npc".to_string(), json!(source_npc));
    if !participants.is_empty() {
        record_payload.insert("participants".to_string(), json!(participants));
    }
    for (key, value) in [
        ("location_id", &location_id),
        ("location_name", &location_name),
        ("region_id", &region_id),
        ("region_name", &region_name),
        ("scene_id", &scene_id),
        ("importance", &importance),
    ] {
        if !value.is_empty() {
            record_payload.insert(key.to_string(), json!(value));
        }
    }
    if !aliases.is_empty() {
        record_payload.insert("aliases".to_string(), json!(aliases));
    }
    if !known_name.is_empty() {
        record_payload.insert("metadata".to_string(), json!({"known_name": known_name}));
    }

    let added = world.add_state_records(&Value::Array(vec![Value::Object(record_payload)]));
    let record = match added.into_iter().next() {
        Some(r) => r,
        None => {
            return (None, Some(err_row(&[("index", json!(index)), ("op", json!(op)), ("type", json!(item_type)), ("error", json!("state record was not stored"))])));
        }
    };
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
    if !expected.is_empty() && expected.to_lowercase() != state_record_hash(&existing).to_lowercase() {
        return (None, Some(hash_conflict_error(world, index, op, item_type, record_id, &expected)));
    }

    let mut update_payload = Map::new();
    update_payload.insert("id".to_string(), json!(record_id));
    if !item_type.is_empty() {
        update_payload.insert("kind".to_string(), json!(state_record_kind_local(item_type)));
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
                Some(uerr(&[("error", json!("npc_id is required when changing scope to npc"))])),
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
                Some(uerr(&[("error", json!("entity_id is required when setting known_name"))])),
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
                    ("error", json!("known_name requires entity_id to be an NPC id")),
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

    let updated = world.update_state_records(&Value::Array(vec![Value::Object(update_payload)]));
    let record = match updated.into_iter().next() {
        Some(r) => r,
        None => {
            return (None, Some(uerr(&[("error", json!("record id not found"))])));
        }
    };
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
    row.insert("text".to_string(), json!(clip_text(&Value::String(text.to_string()), 700)));
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
            "kind", "id", "npc_id", "target", "entity_id", "source_npc", "participants",
            "known_name", "location_id", "location_name", "region_id", "region_name",
            "scene_id", "importance", "aliases", "scope", "visibility", "status", "text",
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

fn state_record_rows(world: &World, actor_id: &str) -> Vec<Value> {
    let query = StateRecordQuery::new(actor_id);
    let mut rows = Vec::new();
    for record in world.state_records_for(&query) {
        let known_name = record
            .metadata
            .get("known_name")
            .and_then(Value::as_str)
            .unwrap_or("");
        rows.push(query_row(
            &format!("state_{}", record.kind),
            &record.text,
            &[
                ("id", json!(record.record_id)),
                ("npc_id", json!(record.owner)),
                ("target", json!(record.subject)),
                ("entity_id", json!(record.entity_id)),
                ("source_npc", json!(record.source_npc)),
                ("participants", json!(record.participants)),
                ("known_name", json!(known_name)),
                ("location_id", json!(record.location_id)),
                ("location_name", json!(record.location_name)),
                ("region_id", json!(record.region_id)),
                ("region_name", json!(record.region_name)),
                ("scene_id", json!(record.scene_id)),
                ("importance", json!(record.importance)),
                ("aliases", json!(record.aliases.join(", "))),
                ("visibility", json!(state_visibility_from_scope(&record.scope))),
                ("status", json!(record.status)),
                ("hash", json!(state_record_hash(record))),
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
    rows.push(query_row("npc_goals", &npc.goals, &[("npc_id", json!(npc_id)), ("visibility", json!("npc"))]));
    rows.push(query_row("npc_knowledge", &npc.knowledge, &[("npc_id", json!(npc_id)), ("visibility", json!("npc"))]));
    rows.push(query_row("npc_secret", &npc.secret, &[("npc_id", json!(npc_id)), ("visibility", json!("npc"))]));
    if let Some(summary) = session.npc_summaries.get(npc_id) {
        if !summary.is_empty() {
            rows.push(query_row("npc_summary", summary, &[("npc_id", json!(npc_id)), ("visibility", json!("npc"))]));
        }
    }
    if let Some(blocks) = session.commitments.get(npc_id) {
        let tail = blocks.iter().rev().take(crate::session::COMMIT_BLOCKS).collect::<Vec<_>>();
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
        query_row("public_intro", &world.public, &[("visibility", json!("player"))]),
        query_row("gm_canon", &world.canon, &[("visibility", json!("gm"))]),
    ];
    rows.extend(state_record_rows(world, "debug"));
    for (i, event_text) in world.hidden_events.iter().enumerate() {
        rows.push(query_row(
            "hidden_event",
            event_text,
            &[("id", json!(format!("hidden:{}", i + 1))), ("visibility", json!("gm"))],
        ));
    }
    for record in &world.fact_records {
        if record.kind == "truth" && record.fact_id == "hidden_truth" {
            continue;
        }
        let visibility = if record.kind == "truth" { "gm" } else { "player" };
        rows.push(query_row(
            &format!("{}_fact", record.kind),
            &record.text,
            &[
                ("id", json!(record.fact_id)),
                ("visibility", json!(visibility)),
                ("status", json!(if record.confirmed { "known" } else { "unconfirmed" })),
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
        rows.push(query_row("npc_role", &format!("{}: {}", npc.name, npc.role), &[("npc_id", json!(npc_id)), ("visibility", json!("player"))]));
        rows.push(query_row("npc_goals", &npc.goals, &[("npc_id", json!(npc_id)), ("visibility", json!("npc"))]));
        rows.push(query_row("npc_knowledge", &npc.knowledge, &[("npc_id", json!(npc_id)), ("visibility", json!("npc"))]));
        rows.push(query_row("npc_secret", &npc.secret, &[("npc_id", json!(npc_id)), ("visibility", json!("npc"))]));
        if let Some(summary) = session.npc_summaries.get(npc_id) {
            if !summary.is_empty() {
                rows.push(query_row("npc_summary", summary, &[("npc_id", json!(npc_id)), ("visibility", json!("npc"))]));
            }
        }
        if let Some(blocks) = session.commitments.get(npc_id) {
            let tail = blocks.iter().rev().take(crate::session::COMMIT_BLOCKS).collect::<Vec<_>>();
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
        rows.push(query_row("gm_summary", &session.gm_summary, &[("visibility", json!("gm"))]));
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
        let raw = args.get("max_results").and_then(|v| v.as_i64()).unwrap_or(5);
        raw.clamp(1, 12) as usize
    };

    if scope == "player" {
        return player_query_payload(session, &query, limit);
    }

    if scope == "npc" {
        let (npc_id, error) = resolve_npc_id(&session.world, args.get("npc_id").unwrap_or(&Value::Null));
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
                    &[("visibility", json!("player")), ("status", json!(public_status))],
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
