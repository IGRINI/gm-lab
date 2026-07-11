//! `Session::to_payload` / `Session::from_payload` — the on-disk snapshot seam
//! consumed by `gml-persistence`.
//!
//! Faithful port of `dialog_store._session_to_payload` / `_session_from_payload`
//! and `_world_to_payload` / `_world_from_payload` (gm-lab/dialog_store.py). The
//! emitted JSON object preserves Python's key insertion order and value shapes
//! **byte-for-byte** so a round-trip through `serde_json` (with `preserve_order`)
//! reproduces `DialogStore.save`'s compact payload exactly.
//!
//! Session is NOT serde-derived (it holds an `Arc<dyn Backend>`, a
//! `ClientFactory`, and transient fields), so these methods are the canonical
//! (de)serialization home — they live in this crate because `Session` and
//! `World` do.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde_json::{json, Map, Value};

use gml_llm::Backend;
use gml_world::{
    FactRecord, Npc, NpcWhereabouts, PlayerCharacter, Presence, Rumor, SceneExit, SceneItem, SpellEntry,
    SceneState, StateRecord, World, WorldEvent, WorldTime,
};
use gml_world::{MersenneTwister, RngState};

use crate::compact::usage_from_payload;
use crate::session::{ClientFactory, NpcClientState, PendingDraft, Session};

// =========================================================================
// loose JSON coercion helpers (mirror dialog_store._json_* / _int_or_none)
// =========================================================================

fn json_list(v: &Value) -> Vec<Value> {
    match v {
        Value::Array(a) => a.clone(),
        _ => Vec::new(),
    }
}

fn json_dict(v: Option<&Value>) -> Map<String, Value> {
    match v {
        Some(Value::Object(m)) => m.clone(),
        _ => Map::new(),
    }
}

/// Python `str(data.get(key) or "")` for a string-ish field.
fn s(m: &Map<String, Value>, key: &str) -> String {
    match m.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        // `str(value or "")`: falsy numbers/bools become "" (0/false), else str.
        Some(Value::Bool(false)) => String::new(),
        Some(Value::Number(n)) if n.as_f64() == Some(0.0) => String::new(),
        Some(other) => py_str(other),
    }
}

/// Python `str(value)` (used where the value is already known non-empty).
fn py_str(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// `int(data.get(key) or 0)`.
fn i(m: &Map<String, Value>, key: &str) -> i64 {
    match m.get(key) {
        Some(v) => v.as_i64().unwrap_or(0),
        None => 0,
    }
}

/// `_int_or_none(value)` — None for None/bool/non-numeric; else int.
fn int_or_none(v: Option<&Value>) -> Option<i64> {
    match v {
        None | Some(Value::Null) | Some(Value::Bool(_)) => None,
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Some(Value::String(s)) => s.trim().parse::<i64>().ok(),
        _ => None,
    }
}

/// Python `bool(data.get(key, default))` where default may be true.
fn b(m: &Map<String, Value>, key: &str, default: bool) -> bool {
    match m.get(key) {
        Some(v) => crate::truthy(v),
        None => default,
    }
}

/// `[str(item) for item in _json_list(v)]`.
fn str_list(v: Option<&Value>) -> Vec<String> {
    v.map(json_list)
        .unwrap_or_default()
        .iter()
        .map(py_str)
        .collect()
}

// =========================================================================
// Session::to_payload / from_payload
// =========================================================================

impl Session {
    /// `_session_to_payload(session)` — the `"session"` object of the snapshot.
    pub fn to_payload(&self) -> Value {
        let mut m = Map::new();
        // client_model: getattr(session, "client_model") or client.model or ""
        let client_model = if !self.client_model.is_empty() {
            self.client_model.clone()
        } else {
            self.client.model()
        };
        m.insert("client_model".into(), json!(client_model));
        m.insert("client_backend".into(), json!(self.client_backend));
        let session_id = if !self.client_session_id.is_empty() {
            self.client_session_id.clone()
        } else {
            self.client.session_id()
        };
        m.insert("client_session_id".into(), json!(session_id));
        let thread_id = if !self.client_thread_id.is_empty() {
            self.client_thread_id.clone()
        } else {
            self.client.thread_id()
        };
        m.insert("client_thread_id".into(), json!(thread_id));

        m.insert("world".into(), world_to_payload(&self.world));
        m.insert("gm_messages".into(), Value::Array(self.gm_messages.clone()));
        m.insert("gm_summary".into(), json!(self.gm_summary));

        let mut npc_messages = Map::new();
        for (npc_id, msgs) in &self.npc_messages {
            npc_messages.insert(npc_id.clone(), Value::Array(msgs.clone()));
        }
        m.insert("npc_messages".into(), Value::Object(npc_messages));

        let mut npc_summaries = Map::new();
        for (npc_id, summary) in &self.npc_summaries {
            npc_summaries.insert(npc_id.clone(), json!(summary));
        }
        m.insert("npc_summaries".into(), Value::Object(npc_summaries));

        m.insert("run_usage".into(), Value::Object(self.run_usage.clone()));

        let mut npc_client_state = Map::new();
        for (npc_id, state) in &self.npc_client_state {
            npc_client_state.insert(
                npc_id.clone(),
                json!({
                    "model": state.model,
                    "session_id": state.session_id,
                    "thread_id": state.thread_id,
                }),
            );
        }
        m.insert("npc_client_state".into(), Value::Object(npc_client_state));

        if !self.location_generator_client_state.model.is_empty()
            || !self.location_generator_client_state.session_id.is_empty()
            || !self.location_generator_client_state.thread_id.is_empty()
        {
            m.insert(
                "location_generator_client_state".into(),
                json!({
                    "model": self.location_generator_client_state.model.clone(),
                    "session_id": self.location_generator_client_state.session_id.clone(),
                    "thread_id": self.location_generator_client_state.thread_id.clone(),
                }),
            );
        }
        if !self.location_generator_anti_repeat.is_empty() {
            m.insert(
                "location_generator_anti_repeat".into(),
                json!(self.location_generator_anti_repeat),
            );
        }
        if !self.location_generator_messages.is_empty() {
            m.insert(
                "location_generator_messages".into(),
                Value::Array(self.location_generator_messages.clone()),
            );
        }

        if !self.character_generator_client_state.model.is_empty()
            || !self.character_generator_client_state.session_id.is_empty()
            || !self.character_generator_client_state.thread_id.is_empty()
        {
            m.insert(
                "character_generator_client_state".into(),
                json!({
                    "model": self.character_generator_client_state.model.clone(),
                    "session_id": self.character_generator_client_state.session_id.clone(),
                    "thread_id": self.character_generator_client_state.thread_id.clone(),
                }),
            );
        }
        if !self.character_generator_anti_repeat.is_empty() {
            m.insert(
                "character_generator_anti_repeat".into(),
                json!(self.character_generator_anti_repeat),
            );
        }
        if !self.character_generator_messages.is_empty() {
            m.insert(
                "character_generator_messages".into(),
                Value::Array(self.character_generator_messages.clone()),
            );
        }

        let mut world_query_seen = Map::new();
        for (scope, keys) in &self.world_query_seen {
            // sorted(str(item) for item in keys if str(item)) — BTreeSet already
            // yields sorted unique strings; drop empties.
            let sorted: Vec<String> = keys.iter().filter(|k| !k.is_empty()).cloned().collect();
            world_query_seen.insert(scope.clone(), json!(sorted));
        }
        m.insert("world_query_seen".into(), Value::Object(world_query_seen));

        m.insert("last_player_action".into(), json!(self.last_player_action));
        m.insert("sid".into(), json!(self.sid_counter));

        let events: Vec<Value> = self.events.iter().map(event_to_payload).collect();
        m.insert("events".into(), Value::Array(events));

        m.insert("seq".into(), json!(self.seq));
        m.insert("turn".into(), json!(self.turn));

        let mut delivered = Map::new();
        for (k, v) in &self.delivered {
            delivered.insert(k.clone(), json!(*v));
        }
        m.insert("delivered".into(), Value::Object(delivered));

        let mut shown = Map::new();
        for (k, v) in &self.shown {
            shown.insert(k.clone(), json!(*v));
        }
        m.insert("shown".into(), Value::Object(shown));

        m.insert("pending".into(), pending_to_payload(&self.pending));

        let mut commitments = Map::new();
        for (npc_id, blocks) in &self.commitments {
            commitments.insert(npc_id.clone(), json!(blocks));
        }
        m.insert("commitments".into(), Value::Object(commitments));

        if !self.npc_last_contact_minutes.is_empty() {
            let mut last_contact = Map::new();
            for (npc_id, minutes) in &self.npc_last_contact_minutes {
                last_contact.insert(npc_id.clone(), json!(*minutes));
            }
            m.insert(
                "npc_last_contact_minutes".into(),
                Value::Object(last_contact),
            );
        }

        if let Some(state) = self.snapshot_options_state {
            m.insert("snapshot_options_state".into(), json!(state));
        }

        if !self.npc_injected_card_revision.is_empty() {
            let mut injected = Map::new();
            for (npc_id, rev) in &self.npc_injected_card_revision {
                injected.insert(npc_id.clone(), json!(*rev));
            }
            m.insert(
                "npc_injected_card_revision".into(),
                Value::Object(injected),
            );
        }

        // Compaction-prune staleness signals for SEARCHED/loaded tools. Trailing +
        // emitted only when non-empty, so pre-prune saves stay byte-identical (a
        // fresh session never populates them until a searched tool runs / loads).
        if !self.tool_last_used.is_empty() {
            let mut used = Map::new();
            for (name, turn) in &self.tool_last_used {
                used.insert(name.clone(), json!(*turn));
            }
            m.insert("tool_last_used".into(), Value::Object(used));
        }
        if !self.tool_loaded_turn.is_empty() {
            let mut loaded = Map::new();
            for (name, turn) in &self.tool_loaded_turn {
                loaded.insert(name.clone(), json!(*turn));
            }
            m.insert("tool_loaded_turn".into(), Value::Object(loaded));
        }

        Value::Object(m)
    }

    /// `_session_from_payload(data, client_factory)` — rebuild a `Session`.
    ///
    /// `client` is the live GM backend (rebuilt by the caller via the
    /// `make_client` factory — mirrors Python passing a fresh client). The
    /// per-NPC clients are NOT recreated here (only their ids), exactly like
    /// Python: they are rebuilt lazily by `ensure_npc_client`.
    ///
    /// Returns `Err` when the world payload is missing required fields
    /// (`dice_seed` / `rng_state` / `npcs` / `scene` / `fact_records` / ...).
    pub fn from_payload(
        data: &Value,
        client: Arc<dyn Backend>,
        npc_client_factory: ClientFactory,
    ) -> Result<Session, String> {
        let data = match data {
            Value::Object(m) => m.clone(),
            _ => Map::new(),
        };
        let world = world_from_payload(data.get("world"))?;
        let mut session = Session::with_world(client, world, npc_client_factory);

        session.client_model = s(&data, "client_model");
        session.client_backend = s(&data, "client_backend");
        session.client_session_id = s(&data, "client_session_id");
        session.client_thread_id = s(&data, "client_thread_id");

        session.gm_messages = json_list(data.get("gm_messages").unwrap_or(&Value::Null));
        session.gm_summary = s(&data, "gm_summary");

        let mut npc_messages: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        for (k, v) in json_dict(data.get("npc_messages")) {
            npc_messages.insert(k, json_list(&v));
        }
        session.npc_messages = npc_messages;

        let mut npc_summaries: BTreeMap<String, String> = BTreeMap::new();
        for (k, v) in json_dict(data.get("npc_summaries")) {
            npc_summaries.insert(k, py_str(&v));
        }
        session.npc_summaries = npc_summaries;

        session.set_run_usage(&Value::Object(usage_from_payload(
            data.get("run_usage").unwrap_or(&Value::Null),
        )));

        let mut npc_client_state: BTreeMap<String, NpcClientState> = BTreeMap::new();
        for (k, v) in json_dict(data.get("npc_client_state")) {
            let row = json_dict(Some(&v));
            npc_client_state.insert(
                k,
                NpcClientState {
                    model: s(&row, "model"),
                    session_id: s(&row, "session_id"),
                    thread_id: s(&row, "thread_id"),
                },
            );
        }
        session.npc_client_state = npc_client_state;

        let generator_state = json_dict(data.get("location_generator_client_state"));
        if !generator_state.is_empty() {
            session.location_generator_client_state = NpcClientState {
                model: s(&generator_state, "model"),
                session_id: s(&generator_state, "session_id"),
                thread_id: s(&generator_state, "thread_id"),
            };
        }
        session.location_generator_anti_repeat =
            str_list(data.get("location_generator_anti_repeat"));
        session.location_generator_messages = json_list(
            data.get("location_generator_messages")
                .unwrap_or(&Value::Null),
        );

        let character_generator_state = json_dict(data.get("character_generator_client_state"));
        if !character_generator_state.is_empty() {
            session.character_generator_client_state = NpcClientState {
                model: s(&character_generator_state, "model"),
                session_id: s(&character_generator_state, "session_id"),
                thread_id: s(&character_generator_state, "thread_id"),
            };
        }
        session.character_generator_anti_repeat =
            str_list(data.get("character_generator_anti_repeat"));
        session.character_generator_messages = json_list(
            data.get("character_generator_messages")
                .unwrap_or(&Value::Null),
        );

        let mut world_query_seen: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (scope, keys) in json_dict(data.get("world_query_seen")) {
            let set: BTreeSet<String> = json_list(&keys)
                .iter()
                .map(py_str)
                .filter(|s| !s.is_empty())
                .collect();
            world_query_seen.insert(scope, set);
        }
        session.world_query_seen = world_query_seen;

        session.last_player_action = s(&data, "last_player_action");
        session.sid_counter = i(&data, "sid");
        session.events = json_list(data.get("events").unwrap_or(&Value::Null))
            .iter()
            .map(event_from_payload)
            .collect();
        session.seq = i(&data, "seq");
        session.turn = i(&data, "turn");

        let mut delivered: BTreeMap<String, i64> = BTreeMap::new();
        for (k, v) in json_dict(data.get("delivered")) {
            delivered.insert(k, v.as_i64().unwrap_or(0));
        }
        session.delivered = delivered;

        let mut shown: BTreeMap<String, i64> = BTreeMap::new();
        for (k, v) in json_dict(data.get("shown")) {
            shown.insert(k, v.as_i64().unwrap_or(0));
        }
        session.shown = shown;

        session.pending = pending_from_payload(data.get("pending"));

        let mut commitments: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (k, v) in json_dict(data.get("commitments")) {
            commitments.insert(k, str_list(Some(&v)));
        }
        session.commitments = commitments;

        let mut last_contact: BTreeMap<String, i64> = BTreeMap::new();
        for (k, v) in json_dict(data.get("npc_last_contact_minutes")) {
            last_contact.insert(k, v.as_i64().unwrap_or(0));
        }
        session.npc_last_contact_minutes = last_contact;

        session.snapshot_options_state =
            data.get("snapshot_options_state").and_then(Value::as_bool);

        let mut injected_card_revision: BTreeMap<String, i64> = BTreeMap::new();
        for (k, v) in json_dict(data.get("npc_injected_card_revision")) {
            injected_card_revision.insert(k, v.as_i64().unwrap_or(0));
        }
        session.npc_injected_card_revision = injected_card_revision;

        // Compaction-prune staleness signals (legacy payloads => empty maps).
        let mut tool_last_used: BTreeMap<String, i64> = BTreeMap::new();
        for (k, v) in json_dict(data.get("tool_last_used")) {
            tool_last_used.insert(k, v.as_i64().unwrap_or(0));
        }
        session.tool_last_used = tool_last_used;

        let mut tool_loaded_turn: BTreeMap<String, i64> = BTreeMap::new();
        for (k, v) in json_dict(data.get("tool_loaded_turn")) {
            tool_loaded_turn.insert(k, v.as_i64().unwrap_or(0));
        }
        session.tool_loaded_turn = tool_loaded_turn;

        Ok(session)
    }
}

// =========================================================================
// events / pending / rng
// =========================================================================

fn event_to_payload(e: &WorldEvent) -> Value {
    let witnesses: Vec<String> = e.witnesses.iter().cloned().collect();
    let mut out = Map::new();
    out.insert("seq".to_string(), json!(e.seq));
    out.insert("turn".to_string(), json!(e.turn));
    if e.time_minutes != 0 {
        out.insert("time_minutes".to_string(), json!(e.time_minutes));
    }
    out.insert("actor".to_string(), json!(e.actor));
    out.insert("kind".to_string(), json!(e.kind));
    if !e.response.is_empty() {
        out.insert("response".to_string(), json!(e.response));
    }
    if !e.beats.is_empty() {
        out.insert("beats".to_string(), json!(e.beats));
    }
    out.insert("speech".to_string(), json!(e.speech));
    out.insert("action".to_string(), json!(e.action));
    out.insert("witnesses".to_string(), json!(witnesses));
    Value::Object(out)
}

fn event_from_payload(v: &Value) -> WorldEvent {
    let m = json_dict(Some(v));
    WorldEvent {
        seq: i(&m, "seq"),
        turn: i(&m, "turn"),
        time_minutes: i(&m, "time_minutes"),
        actor: s(&m, "actor"),
        kind: s(&m, "kind"),
        response: s(&m, "response"),
        beats: m
            .get("beats")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default(),
        speech: s(&m, "speech"),
        action: s(&m, "action"),
        witnesses: str_list(m.get("witnesses")).into_iter().collect(),
    }
}

fn pending_to_payload(pending: &BTreeMap<String, PendingDraft>) -> Value {
    let mut out = Map::new();
    for (npc_id, row) in pending {
        let witnesses: Vec<String> = row.witnesses.iter().cloned().collect();
        let mut pending_row = Map::new();
        pending_row.insert("seq".to_string(), json!(row.seq));
        if row.time_minutes != 0 {
            pending_row.insert("time_minutes".to_string(), json!(row.time_minutes));
        }
        if !row.response.is_empty() {
            pending_row.insert("response".to_string(), json!(row.response));
        }
        if !row.beats.is_empty() {
            pending_row.insert("beats".to_string(), json!(row.beats));
        }
        pending_row.insert("speech".to_string(), json!(row.speech));
        pending_row.insert("action".to_string(), json!(row.action));
        pending_row.insert("claims".to_string(), json!(row.claims));
        pending_row.insert("witnesses".to_string(), json!(witnesses));
        pending_row.insert(
            "user_message".to_string(),
            row.user_message.clone().unwrap_or(Value::Null),
        );
        pending_row.insert(
            "assistant_message".to_string(),
            row.assistant_message.clone().unwrap_or(Value::Null),
        );
        out.insert(npc_id.clone(), Value::Object(pending_row));
    }
    Value::Object(out)
}

fn pending_from_payload(v: Option<&Value>) -> BTreeMap<String, PendingDraft> {
    let mut out = BTreeMap::new();
    for (npc_id, row) in json_dict(v) {
        let m = match row {
            Value::Object(ref m) => m.clone(),
            _ => continue,
        };
        let user_message = match m.get("user_message") {
            Some(Value::Object(_)) => m.get("user_message").cloned(),
            _ => None,
        };
        let assistant_message = match m.get("assistant_message") {
            Some(Value::Object(_)) => m.get("assistant_message").cloned(),
            _ => None,
        };
        out.insert(
            npc_id,
            PendingDraft {
                seq: i(&m, "seq"),
                time_minutes: i(&m, "time_minutes"),
                response: s(&m, "response"),
                beats: m
                    .get("beats")
                    .cloned()
                    .and_then(|value| serde_json::from_value(value).ok())
                    .unwrap_or_default(),
                speech: s(&m, "speech"),
                action: s(&m, "action"),
                claims: str_list(m.get("claims")),
                witnesses: str_list(m.get("witnesses")).into_iter().collect(),
                user_message,
                assistant_message,
            },
        );
    }
    out
}

fn rng_state_to_payload(state: &RngState) -> Value {
    json!({
        "version": state.version,
        "internal": state.internal,
        "gauss": state.gauss,
    })
}

/// `_rng_state_from_payload` — None on malformed input.
fn rng_state_from_payload(v: Option<&Value>) -> Option<RngState> {
    let m = match v {
        Some(Value::Object(m)) => m,
        _ => return None,
    };
    let internal = match m.get("internal") {
        Some(Value::Array(a)) => a,
        _ => return None,
    };
    let internal_u64: Vec<u64> = internal
        .iter()
        .map(|x| {
            x.as_u64()
                .or_else(|| x.as_i64().map(|n| n as u64))
                .unwrap_or(0)
        })
        .collect();
    let version = m
        .get("version")
        .and_then(|x| x.as_u64())
        .map(|n| n as u32)
        .unwrap_or(3);
    let gauss = m.get("gauss").and_then(|x| x.as_f64());
    Some(RngState {
        version,
        internal: internal_u64,
        gauss,
    })
}

// =========================================================================
// world payload
// =========================================================================

fn world_to_payload(world: &World) -> Value {
    let mut m = Map::new();
    m.insert("story_id".into(), json!(world.story_id));
    m.insert("story_title".into(), json!(world.story_title));
    m.insert("story_brief".into(), json!(world.story_brief));
    m.insert("dice_seed".into(), json!(world.dice_seed as u64));
    m.insert(
        "forced_die_next".into(),
        world
            .forced_die_next
            .map(Value::from)
            .unwrap_or(Value::Null),
    );
    m.insert(
        "forced_die_all".into(),
        world.forced_die_all.map(Value::from).unwrap_or(Value::Null),
    );
    m.insert("rng_state".into(), rng_state_to_payload(&world.rng_state()));
    m.insert("hidden_events".into(), json!(world.hidden_events));

    let rumors: Vec<Value> = world.rumors.iter().map(rumor_to_payload).collect();
    m.insert("rumors".into(), Value::Array(rumors));
    m.insert("rumor_seq".into(), json!(world.rumor_seq));

    let mut npcs = Map::new();
    for (npc_id, npc) in &world.npcs {
        npcs.insert(npc_id.clone(), npc_to_payload(npc));
    }
    m.insert("npcs".into(), Value::Object(npcs));

    m.insert("public".into(), json!(world.public));
    m.insert("canon".into(), json!(world.canon));
    m.insert("time".into(), time_to_payload(&world.time));
    m.insert(
        "player_character".into(),
        player_character_to_payload(&world.player_character),
    );
    m.insert("extra_proper_nouns".into(), json!(world.extra_proper_nouns));
    m.insert("scene".into(), scene_to_payload(&world.scene));

    let mut whereabouts = Map::new();
    for (npc_id, w) in &world.npc_whereabouts {
        whereabouts.insert(npc_id.clone(), whereabouts_to_payload(w));
    }
    m.insert("npc_whereabouts".into(), Value::Object(whereabouts));

    let fact_records: Vec<Value> = world.fact_records.iter().map(fact_to_payload).collect();
    m.insert("fact_records".into(), Value::Array(fact_records));

    let state_records: Vec<Value> = world
        .state_records
        .iter()
        .map(state_record_to_payload)
        .collect();
    m.insert("state_records".into(), Value::Array(state_records));

    // Living-world canon (Place/Transition graph). Emitted as a trailing
    // `world_canon` key ONLY when non-empty so pre-canon saves stay
    // byte-identical (the golden_payload_roundtrip gate). The canon types are
    // serde-derived with BTreeMap ordering, so the subtree is deterministic and
    // round-trip-stable. (Key is `world_canon`, distinct from the legacy
    // `canon` hidden-truth string above.)
    if !world.world_canon.is_empty() {
        if let Ok(canon) = serde_json::to_value(&world.world_canon) {
            m.insert("world_canon".into(), canon);
        }
    }

    // Phase-4 package provenance (`docs/MODS_PACKAGES_TZ.md`): emitted as
    // trailing keys ONLY when set, so pre-Phase-4 saves (and every world not
    // launched from a package) stay byte-identical to before.
    if let Some(world_ref) = &world.world_ref {
        m.insert("world_ref".into(), package_ref_to_payload(world_ref));
    }
    if let Some(story_ref) = &world.story_ref {
        m.insert("story_ref".into(), package_ref_to_payload(story_ref));
    }
    // The authored world-version pin recorded at launch (see
    // `World::world_ref_authored_version`). Trailing + emitted only when `Some`,
    // so worlds launched without a pinned story ref stay byte-identical.
    if let Some(authored) = world.world_ref_authored_version {
        m.insert(
            "world_ref_authored_version".into(),
            Value::Number(authored.into()),
        );
    }
    // K1 CHARACTER package provenance (`docs/CHARACTERS_AND_STORY_TZ.md` §К1.3):
    // which character package's hero was overlaid at launch. Trailing + emitted
    // only when `Some` (right after `world_ref_authored_version`), so pre-K1 and
    // no-character launches stay byte-identical. Provenance only — the PC
    // snapshot itself lives in `player_character` above.
    if let Some(char_ref) = &world.char_ref {
        m.insert("char_ref".into(), package_ref_to_payload(char_ref));
    }

    // Phase-И per-place scene-item store (`docs/ITEMS_AND_SPELLS_TZ.md` §И2):
    // emitted as a trailing `place_items` key ONLY when non-empty, so pre-Phase-И
    // saves stay byte-identical (the whole-payload roundtrip golden). The map is
    // a `BTreeMap`, so the emitted object key order is deterministic; each value
    // is a list of `SceneItem` payloads shaped exactly like `scene.items`.
    if !world.place_items.is_empty() {
        let mut store = Map::new();
        for (place_id, items) in &world.place_items {
            let items: Vec<Value> = items.iter().map(item_to_payload).collect();
            store.insert(place_id.clone(), Value::Array(items));
        }
        m.insert("place_items".into(), Value::Object(store));
    }

    Value::Object(m)
}

fn package_ref_to_payload(r: &gml_world::PackageRef) -> Value {
    json!({ "id": r.id, "version": r.version })
}

fn package_ref_from_payload(v: Option<&Value>) -> Option<gml_world::PackageRef> {
    let obj = v?.as_object()?;
    let id = obj.get("id").and_then(Value::as_str).unwrap_or_default();
    if id.is_empty() {
        return None;
    }
    let version = obj.get("version").and_then(Value::as_u64).unwrap_or(0);
    Some(gml_world::PackageRef {
        id: id.to_string(),
        version,
    })
}

fn world_from_payload(v: Option<&Value>) -> Result<World, String> {
    let data = match v {
        Some(Value::Object(m)) => m.clone(),
        _ => return Err("invalid world payload".to_string()),
    };
    let required = [
        "story_id",
        "story_title",
        "npcs",
        "public",
        "canon",
        "scene",
        "fact_records",
    ];
    let missing: Vec<&str> = required
        .iter()
        .copied()
        .filter(|k| !data.contains_key(*k))
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "unsupported world payload: missing {}",
            missing.join(", ")
        ));
    }

    // dice_seed / rng_state are required (mirrors world_from_payload).
    if data.get("dice_seed").map(|v| v.is_null()).unwrap_or(true) {
        return Err("unsupported world payload: dice_seed is required".to_string());
    }
    let rng_state = rng_state_from_payload(data.get("rng_state"))
        .ok_or_else(|| "unsupported world payload: rng_state is required".to_string())?;

    // Build the MT from the restored state (never reseed), mirroring Python's
    // `random.Random(); setstate(rng_state)`.
    let mut mt = MersenneTwister::from_u128_seed(0);
    mt.setstate(&rng_state)
        .map_err(|e| format!("unsupported world payload: rng_state invalid: {e}"))?;
    let mut world = World::empty_with_rng(mt);

    world.story_id = s(&data, "story_id");
    world.story_title = s(&data, "story_title");
    world.story_brief = s(&data, "story_brief");
    world.hidden_events = str_list(data.get("hidden_events"));
    world.rumors = json_list(data.get("rumors").unwrap_or(&Value::Null))
        .iter()
        .map(rumor_from_payload)
        .collect();
    world.rumor_seq = i(&data, "rumor_seq");

    // `npcs` must be PRESENT (enforced by the `required` list above) and be an
    // OBJECT, but an EMPTY roster is now valid: procedural worlds start with
    // zero actors (NPCs are generated lazily at play time), so `{}` is accepted.
    if !matches!(data.get("npcs"), Some(Value::Object(_))) {
        return Err("unsupported world payload: npcs must be an object".to_string());
    }
    let npcs_raw = json_dict(data.get("npcs"));
    let mut npcs: BTreeMap<String, Npc> = BTreeMap::new();
    for (npc_id, npc_v) in &npcs_raw {
        if let Value::Object(_) = npc_v {
            npcs.insert(npc_id.clone(), npc_from_payload(npc_id, npc_v));
        }
    }
    world.npcs = npcs;
    world.public = s(&data, "public");
    if world.story_brief.is_empty() {
        world.story_brief = world.public.clone();
    }
    world.canon = s(&data, "canon");
    world.time = time_from_payload(data.get("time"));
    world.player_character = player_character_from_payload(data.get("player_character"));
    world.extra_proper_nouns = str_list(data.get("extra_proper_nouns"));

    if !matches!(data.get("scene"), Some(Value::Object(_))) {
        return Err("unsupported world payload: scene is required".to_string());
    }
    world.scene = scene_from_payload(data.get("scene").unwrap());
    world.constraints = world.scene.constraints.clone();

    let whereabouts_raw = json_dict(data.get("npc_whereabouts"));
    if !whereabouts_raw.is_empty() {
        let mut wb: BTreeMap<String, NpcWhereabouts> = BTreeMap::new();
        for (npc_id, row) in &whereabouts_raw {
            if let Value::Object(_) = row {
                wb.insert(npc_id.clone(), whereabouts_from_payload(npc_id, row));
            }
        }
        world.npc_whereabouts = wb;
    }

    // `fact_records` must be PRESENT (enforced in `required` above) but may be
    // EMPTY: a canon-authoritative world (e.g. a procedural campaign derived
    // from worldgen) legitimately carries no legacy public facts — those live in
    // the canon now. The old "non-empty" Python-byte-compat invariant is dropped
    // (locked decision #7: canon is core).
    let facts = json_list(data.get("fact_records").unwrap_or(&Value::Null));
    world.fact_records = facts.iter().map(fact_from_payload).collect();

    world.state_records = json_list(data.get("state_records").unwrap_or(&Value::Null))
        .iter()
        .filter(|r| matches!(r, Value::Object(_)))
        .map(state_record_from_payload)
        .collect();

    world.ensure_npc_whereabouts();

    // Living-world canon: the canon is now CORE, so it is part of every save.
    // Locked decision #5: only DEFAULT when the `world_canon` key is ABSENT (a
    // pre-canon save). When the key is PRESENT but fails to deserialize, RETURN
    // AN ERROR rather than silently defaulting — a malformed canon means data
    // loss, and the canon is the source of truth. (No lazy rebuild on load: that
    // would change the re-serialized bytes; canon is derived at seed/worldgen
    // time for new campaigns instead.)
    world.world_canon = match data.get("world_canon") {
        None => Default::default(),
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| {
            format!("unsupported world payload: world_canon present but malformed: {e}")
        })?,
    };

    world.dice_seed = data
        .get("dice_seed")
        .and_then(|v| v.as_u64())
        .map(|n| n as u128)
        .unwrap_or(0);
    world.forced_die_next = int_or_none(data.get("forced_die_next"));
    world.forced_die_all = int_or_none(data.get("forced_die_all"));
    world.world_ref = package_ref_from_payload(data.get("world_ref"));
    world.story_ref = package_ref_from_payload(data.get("story_ref"));
    world.world_ref_authored_version = data
        .get("world_ref_authored_version")
        .and_then(Value::as_u64);
    world.char_ref = package_ref_from_payload(data.get("char_ref"));
    // Phase-И per-place scene-item store (§И2): parsed with a DEFAULT when the
    // `place_items` key is absent (pre-Phase-И save) and BEFORE the refresh below
    // so a restored player standing at a stored place gets their items back.
    if let Some(Value::Object(store)) = data.get("place_items") {
        let mut place_items: BTreeMap<String, Vec<SceneItem>> = BTreeMap::new();
        for (place_id, items) in store {
            let items: Vec<SceneItem> = json_list(items).iter().map(item_from_payload).collect();
            place_items.insert(place_id.clone(), items);
        }
        world.place_items = place_items;
    }
    if !world.world_canon.is_empty() {
        world.migrate_legacy_state_records_to_memory();
    }

    // Canon is the source of truth: after restore, rebuild the live scene FROM
    // the canon so /state, the GM context and the UI reflect the canonical
    // player place — not the stale persisted legacy scene. No-op for pre-canon
    // saves (empty canon), where the loaded legacy scene is kept as-is.
    world.refresh_scene_from_canon();

    Ok(world)
}

// --- per-dataclass payload helpers (exact Python key order) ----------------

fn rumor_to_payload(r: &Rumor) -> Value {
    let witnesses: Vec<String> = r.witnesses.iter().cloned().collect();
    let known_in: Vec<String> = r.known_in.iter().cloned().collect();
    let carriers: Vec<String> = r.carriers.iter().cloned().collect();
    json!({
        "rumor_id": r.rumor_id,
        "seq": r.seq,
        "turn": r.turn,
        "speaker": r.speaker,
        "text": r.text,
        "witnesses": witnesses,
        "origin_scope": r.origin_scope,
        "known_in": known_in,
        "carriers": carriers,
        "strength": r.strength,
        "distortion": r.distortion,
        "created_minutes": r.created_minutes,
        "last_spread_minutes": r.last_spread_minutes,
        "confirmed": r.confirmed,
    })
}

fn rumor_from_payload(v: &Value) -> Rumor {
    let m = json_dict(Some(v));
    Rumor {
        rumor_id: s(&m, "rumor_id"),
        seq: i(&m, "seq"),
        turn: i(&m, "turn"),
        speaker: s(&m, "speaker"),
        text: s(&m, "text"),
        witnesses: str_list(m.get("witnesses")).into_iter().collect(),
        origin_scope: s(&m, "origin_scope"),
        known_in: str_list(m.get("known_in")).into_iter().collect(),
        carriers: str_list(m.get("carriers")).into_iter().collect(),
        strength: i(&m, "strength"),
        distortion: i(&m, "distortion"),
        created_minutes: i(&m, "created_minutes"),
        last_spread_minutes: i(&m, "last_spread_minutes"),
        confirmed: b(&m, "confirmed", false),
    }
}

fn time_to_payload(t: &WorldTime) -> Value {
    json!({
        "calendar_name": t.calendar_name,
        "absolute_minutes": t.absolute_minutes,
        "current_date_label": t.current_date_label,
        "minutes_per_hour": t.minutes_per_hour,
        "hours_per_day": t.hours_per_day,
        "day_names": t.day_names,
        "month_names": t.month_names,
        "last_advance_minutes": t.last_advance_minutes,
        "last_advance_reason": t.last_advance_reason,
    })
}

fn time_from_payload(v: Option<&Value>) -> WorldTime {
    let m = json_dict(v);
    WorldTime {
        calendar_name: s(&m, "calendar_name"),
        absolute_minutes: int_or_none(m.get("absolute_minutes")).unwrap_or(0).max(0),
        current_date_label: {
            let c = s(&m, "current_date_label");
            if c.is_empty() {
                "День 1".to_string()
            } else {
                c
            }
        },
        minutes_per_hour: int_or_none(m.get("minutes_per_hour")).unwrap_or(60).max(1),
        hours_per_day: int_or_none(m.get("hours_per_day")).unwrap_or(24).max(1),
        day_names: str_list(m.get("day_names")),
        month_names: str_list(m.get("month_names")),
        last_advance_minutes: int_or_none(m.get("last_advance_minutes"))
            .unwrap_or(0)
            .max(0),
        last_advance_reason: s(&m, "last_advance_reason"),
    }
}

fn npc_to_payload(npc: &Npc) -> Value {
    json!({
        "npc_id": npc.npc_id,
        "name": npc.name,
        "persona": npc.persona,
        "voice": npc.voice,
        "goals": npc.goals,
        "knowledge": npc.knowledge,
        "secret": npc.secret,
        "role": npc.role,
        "pronouns": npc.pronouns,
        "color": npc.color,
        "public_label": npc.public_label,
        "age": npc.age,
        "physical_type": npc.physical_type,
        "distinctive_features": npc.distinctive_features,
        "life_status": npc.life_status,
        "life_status_note": npc.life_status_note,
        "condition": npc.condition,
        "personality": npc.personality,
        "values": npc.values,
        "habits": npc.habits,
        "pressure_response": npc.pressure_response,
        "boundaries": npc.boundaries,
        "abilities": npc.abilities,
        "skills": npc.skills,
        "saving_throws": npc.saving_throws,
        "passive_perception": npc.passive_perception,
        "ac": npc.ac,
        "hp": npc.hp,
        "speed": npc.speed,
        "senses": npc.senses,
        "languages": npc.languages,
        "default_whereabouts": npc.default_whereabouts.clone().map(Value::Object).unwrap_or(Value::Null),
        "card_revision": npc.card_revision,
    })
}

fn npc_from_payload(npc_id: &str, v: &Value) -> Npc {
    let m = json_dict(Some(v));
    let dw = match m.get("default_whereabouts") {
        Some(Value::Object(o)) => Some(o.clone()),
        _ => None,
    };
    Npc {
        npc_id: {
            let id = s(&m, "npc_id");
            if id.is_empty() {
                npc_id.to_string()
            } else {
                id
            }
        },
        name: s(&m, "name"),
        persona: s(&m, "persona"),
        voice: s(&m, "voice"),
        goals: s(&m, "goals"),
        knowledge: s(&m, "knowledge"),
        secret: s(&m, "secret"),
        role: s(&m, "role"),
        pronouns: s(&m, "pronouns"),
        color: s(&m, "color"),
        public_label: s(&m, "public_label"),
        age: s(&m, "age"),
        physical_type: s(&m, "physical_type"),
        distinctive_features: s(&m, "distinctive_features"),
        life_status: {
            let ls = s(&m, "life_status");
            if ls.is_empty() {
                "alive".to_string()
            } else {
                ls
            }
        },
        life_status_note: s(&m, "life_status_note"),
        condition: s(&m, "condition"),
        personality: s(&m, "personality"),
        values: s(&m, "values"),
        habits: s(&m, "habits"),
        pressure_response: s(&m, "pressure_response"),
        boundaries: s(&m, "boundaries"),
        abilities: json_dict(m.get("abilities")),
        skills: json_dict(m.get("skills")),
        saving_throws: json_dict(m.get("saving_throws")),
        passive_perception: int_or_none(m.get("passive_perception")),
        ac: json_value_or_null(m.get("ac")),
        hp: json_dict(m.get("hp")),
        speed: s(&m, "speed"),
        senses: s(&m, "senses"),
        languages: s(&m, "languages"),
        default_whereabouts: dw,
        card_revision: card_revision(&m),
    }
}

fn player_character_to_payload(pc: &PlayerCharacter) -> Value {
    json!({
        "name": pc.name,
        "pronouns": pc.pronouns,
        "class_role": pc.class_role,
        "level": pc.level,
        "background": pc.background,
        "age": pc.age,
        "physical_type": pc.physical_type,
        "distinctive_features": pc.distinctive_features,
        "life_status": pc.life_status,
        "life_status_note": pc.life_status_note,
        "condition": pc.condition,
        "personality": pc.personality,
        "values": pc.values,
        "gm_notes": pc.gm_notes,
        "abilities": pc.abilities,
        "skills": pc.skills,
        "saving_throws": pc.saving_throws,
        "passive_perception": pc.passive_perception,
        "ac": pc.ac,
        "hp": pc.hp,
        "speed": pc.speed,
        "senses": pc.senses,
        "languages": pc.languages,
        "inventory": pc.inventory,
        "equipment": pc.equipment,
        "features": pc.features,
        // Фаза С §С1: unconditional emit per the 26→30-field discipline (old
        // real saves gain empty keys on next save-back — this is the established
        // additive re-save behaviour, gated by the re-blessed roundtrip golden).
        "spells": pc.spells,
        "spell_slots": pc.spell_slots,
        "spell_slots_max": pc.spell_slots_max,
        "concentration": pc.concentration,
        "card_revision": pc.card_revision,
    })
}

fn player_character_from_payload(v: Option<&Value>) -> PlayerCharacter {
    let m = match v {
        Some(Value::Object(m)) => m.clone(),
        _ => return PlayerCharacter::default(),
    };
    PlayerCharacter {
        name: nonempty(s(&m, "name"), "Искатель"),
        pronouns: nonempty(s(&m, "pronouns"), "OTHER"),
        class_role: s(&m, "class_role"),
        level: int_or_none(m.get("level")),
        background: s(&m, "background"),
        age: s(&m, "age"),
        physical_type: s(&m, "physical_type"),
        distinctive_features: s(&m, "distinctive_features"),
        life_status: nonempty(s(&m, "life_status"), "alive"),
        life_status_note: s(&m, "life_status_note"),
        condition: s(&m, "condition"),
        personality: s(&m, "personality"),
        values: s(&m, "values"),
        gm_notes: s(&m, "gm_notes"),
        abilities: json_dict(m.get("abilities")),
        skills: json_dict(m.get("skills")),
        saving_throws: json_dict(m.get("saving_throws")),
        passive_perception: int_or_none(m.get("passive_perception")),
        ac: json_value_or_null(m.get("ac")),
        hp: json_dict(m.get("hp")),
        speed: s(&m, "speed"),
        senses: s(&m, "senses"),
        languages: s(&m, "languages"),
        inventory: str_list(m.get("inventory")),
        equipment: str_list(m.get("equipment")),
        features: str_list(m.get("features")),
        // Фаза С §С1: spells is an OBJECT array — parse via serde (NOT str_list),
        // keeping only object entries and dropping any that fail to deserialize so
        // old saves / malformed payloads load with a clean (possibly empty) list.
        // Slots are flat dicts (json_dict, like hp); concentration is a string.
        spells: spell_list(m.get("spells")),
        spell_slots: json_dict(m.get("spell_slots")),
        spell_slots_max: json_dict(m.get("spell_slots_max")),
        concentration: s(&m, "concentration"),
        card_revision: card_revision(&m),
    }
}

/// Фаза С §С1: coerce a payload `spells` value into `Vec<SpellEntry>`. Non-array
/// input yields an empty list; within an array only OBJECT entries are kept and
/// each is deserialized with serde defaults (a record that fails to parse is
/// dropped, never poisoning the rest). This mirrors the bespoke apply-path
/// coercion in `World::apply_player_character_fields`.
fn spell_list(v: Option<&Value>) -> Vec<SpellEntry> {
    v.map(json_list)
        .unwrap_or_default()
        .into_iter()
        .filter(|x| x.is_object())
        .filter_map(|x| serde_json::from_value::<SpellEntry>(x).ok())
        .collect()
}

/// THE canonical player-character serializer for the K1 character package
/// (`docs/CHARACTERS_AND_STORY_TZ.md` §К1.1): the character package's
/// `payload.player_character` uses the EXACT same shape as the save payload's
/// `player_character` (this is `player_character_to_payload`), NOT the
/// UI/tool-facing `World::player_character_export` projection. Exposed so the
/// SERVER (which has both `gml-persistence` and this crate as deps) can build
/// the package payload while the `CharacterStore` treats `player_character` as an
/// opaque object it never interprets (the clean dependency seam).
pub fn player_character_payload(pc: &PlayerCharacter) -> Value {
    player_character_to_payload(pc)
}

/// Inverse of [`player_character_payload`]: coerce a canonical
/// `player_character` payload object back into a [`PlayerCharacter`] (missing
/// fields fall back to the default hero). Public for the same seam.
pub fn player_character_from_value(v: Option<&Value>) -> PlayerCharacter {
    player_character_from_payload(v)
}

fn scene_to_payload(scene: &SceneState) -> Value {
    let present_npcs: Vec<String> = scene.present_npcs.iter().cloned().collect();
    let mut presence = Map::new();
    for (npc_id, p) in &scene.presence {
        presence.insert(npc_id.clone(), presence_to_payload(p));
    }
    let items: Vec<Value> = scene.items.iter().map(item_to_payload).collect();
    let exits: Vec<Value> = scene.exits.iter().map(exit_to_payload).collect();
    json!({
        "scene_id": scene.scene_id,
        "location_id": scene.location_id,
        "title": scene.title,
        "description": scene.description,
        "present_npcs": present_npcs,
        "presence": presence,
        "items": items,
        "exits": exits,
        "constraints": scene.constraints,
        "tension": scene.tension,
        "player_seen": scene.player_seen,
    })
}

fn scene_from_payload(v: &Value) -> SceneState {
    let m = json_dict(Some(v));
    let present_npcs: BTreeSet<String> = str_list(m.get("present_npcs")).into_iter().collect();
    let mut presence: BTreeMap<String, Presence> = BTreeMap::new();
    for (npc_id, p) in json_dict(m.get("presence")) {
        if let Value::Object(_) = p {
            presence.insert(npc_id.clone(), presence_from_payload(&npc_id, &p));
        }
    }
    SceneState {
        scene_id: s(&m, "scene_id"),
        location_id: s(&m, "location_id"),
        title: s(&m, "title"),
        description: s(&m, "description"),
        present_npcs,
        presence,
        items: json_list(m.get("items").unwrap_or(&Value::Null))
            .iter()
            .map(item_from_payload)
            .collect(),
        exits: json_list(m.get("exits").unwrap_or(&Value::Null))
            .iter()
            .map(exit_from_payload)
            .collect(),
        constraints: str_list(m.get("constraints")),
        tension: s(&m, "tension"),
        player_seen: str_list(m.get("player_seen")),
    }
}

fn presence_to_payload(p: &Presence) -> Value {
    json!({
        "npc_id": p.npc_id,
        "location": p.location,
        "visible": p.visible,
        "can_hear": p.can_hear,
        "activity": p.activity,
        "attitude": p.attitude,
    })
}

fn presence_from_payload(_npc_id: &str, v: &Value) -> Presence {
    let m = json_dict(Some(v));
    Presence {
        npc_id: s(&m, "npc_id"),
        location: s(&m, "location"),
        visible: b(&m, "visible", true),
        can_hear: b(&m, "can_hear", true),
        activity: s(&m, "activity"),
        attitude: s(&m, "attitude"),
    }
}

fn whereabouts_to_payload(w: &NpcWhereabouts) -> Value {
    json!({
        "npc_id": w.npc_id,
        "location_id": w.location_id,
        "location_name": w.location_name,
        "status": w.status,
        "details": w.details,
        "source": w.source,
    })
}

fn whereabouts_from_payload(npc_id: &str, v: &Value) -> NpcWhereabouts {
    let m = json_dict(Some(v));
    NpcWhereabouts {
        npc_id: nonempty(s(&m, "npc_id"), npc_id),
        location_id: s(&m, "location_id"),
        location_name: s(&m, "location_name"),
        status: nonempty(s(&m, "status"), "unknown"),
        details: s(&m, "details"),
        source: s(&m, "source"),
    }
}

fn item_to_payload(item: &SceneItem) -> Value {
    json!({
        "item_id": item.item_id,
        "name": item.name,
        "location": item.location,
        "visible": item.visible,
        "portable": item.portable,
        "owner": item.owner,
        "details": item.details,
    })
}

fn item_from_payload(v: &Value) -> SceneItem {
    let m = json_dict(Some(v));
    SceneItem {
        item_id: s(&m, "item_id"),
        name: s(&m, "name"),
        location: s(&m, "location"),
        visible: b(&m, "visible", true),
        portable: b(&m, "portable", false),
        owner: s(&m, "owner"),
        details: s(&m, "details"),
    }
}

fn exit_to_payload(e: &SceneExit) -> Value {
    json!({
        "exit_id": e.exit_id,
        "name": e.name,
        "destination": e.destination,
        "visible": e.visible,
        "blocked_by": e.blocked_by,
    })
}

fn exit_from_payload(v: &Value) -> SceneExit {
    let m = json_dict(Some(v));
    SceneExit {
        exit_id: s(&m, "exit_id"),
        name: s(&m, "name"),
        destination: s(&m, "destination"),
        visible: b(&m, "visible", true),
        blocked_by: s(&m, "blocked_by"),
    }
}

fn fact_to_payload(r: &FactRecord) -> Value {
    json!({
        "fact_id": r.fact_id,
        "kind": r.kind,
        "text": r.text,
        "keywords": r.keywords,
        "source": r.source,
        "confirmed": r.confirmed,
    })
}

fn fact_from_payload(v: &Value) -> FactRecord {
    let m = json_dict(Some(v));
    FactRecord {
        fact_id: s(&m, "fact_id"),
        kind: s(&m, "kind"),
        text: s(&m, "text"),
        keywords: str_list(m.get("keywords")),
        source: s(&m, "source"),
        confirmed: b(&m, "confirmed", true),
    }
}

fn state_record_to_payload(r: &StateRecord) -> Value {
    let metadata = if r.metadata.is_empty() {
        Map::new()
    } else {
        r.metadata.clone()
    };
    json!({
        "record_id": r.record_id,
        "kind": r.kind,
        "text": r.text,
        "scope": r.scope,
        "active": r.active,
        "owner": r.owner,
        "subject": r.subject,
        "source": r.source,
        "status": r.status,
        "tags": r.tags,
        "entity_id": r.entity_id,
        "source_npc": r.source_npc,
        "participants": r.participants,
        "location_id": r.location_id,
        "location_name": r.location_name,
        "region_id": r.region_id,
        "region_name": r.region_name,
        "scene_id": r.scene_id,
        "importance": r.importance,
        "aliases": r.aliases,
        "metadata": metadata,
    })
}

fn state_record_from_payload(v: &Value) -> StateRecord {
    let m = json_dict(Some(v));
    StateRecord {
        record_id: first_nonempty(&m, &["record_id", "id"]),
        kind: nonempty(s(&m, "kind"), "fact"),
        text: s(&m, "text"),
        scope: nonempty(s(&m, "scope"), "public"),
        active: b(&m, "active", true),
        owner: first_nonempty(&m, &["owner", "owner_id"]),
        subject: first_nonempty(&m, &["subject", "subject_id"]),
        source: s(&m, "source"),
        status: nonempty(s(&m, "status"), "known"),
        tags: str_list(m.get("tags")),
        entity_id: first_nonempty(&m, &["entity_id", "entity", "about"]),
        source_npc: first_nonempty(&m, &["source_npc", "source_npc_id"]),
        participants: str_list(m.get("participants"))
            .into_iter()
            .map(|x| x.trim().to_lowercase())
            .filter(|x| !x.is_empty())
            .collect(),
        location_id: s(&m, "location_id"),
        location_name: s(&m, "location_name"),
        region_id: s(&m, "region_id"),
        region_name: s(&m, "region_name"),
        scene_id: s(&m, "scene_id"),
        importance: s(&m, "importance"),
        aliases: str_list(m.get("aliases")),
        metadata: json_dict(m.get("metadata")),
    }
}

// --- small shared helpers --------------------------------------------------

/// `int(data.get("card_revision") or 0)` with the TypeError/ValueError guard.
fn card_revision(m: &Map<String, Value>) -> i64 {
    match m.get("card_revision") {
        Some(Value::Number(n)) => n
            .as_i64()
            .unwrap_or_else(|| n.as_f64().map(|f| f as i64).unwrap_or(0)),
        Some(Value::String(s)) => s.trim().parse::<i64>().unwrap_or(0),
        _ => 0,
    }
}

/// `_json_value(value)` for the loosely-typed `ac` field — keep as-is or null.
fn json_value_or_null(v: Option<&Value>) -> Value {
    v.cloned().unwrap_or(Value::Null)
}

fn nonempty(value: String, fallback: &str) -> String {
    if value.is_empty() {
        fallback.to_string()
    } else {
        value
    }
}

/// First non-empty string among the given keys (`str(data.get(k) or "")`).
fn first_nonempty(m: &Map<String, Value>, keys: &[&str]) -> String {
    for k in keys {
        let v = s(m, k);
        if !v.is_empty() {
            return v;
        }
    }
    String::new()
}

#[cfg(test)]
mod package_ref_tests {
    use super::*;
    use gml_world::{PackageRef, World, WorldSpec};

    /// A deterministic, fully-formed World (worldgen populates npcs/scene/
    /// fact_records so the payload round-trips through `world_from_payload`).
    fn worldgen_world() -> World {
        World::from_worldgen_with_dice_seed(&WorldSpec::from_seed("20260622"), 20260622)
    }

    /// `Some` world_ref / story_ref => the payload carries `{id, version}` and a
    /// round-trip restores an EQUAL `PackageRef`.
    #[test]
    fn package_refs_round_trip_when_set() {
        let mut world = worldgen_world();
        world.world_ref = Some(PackageRef {
            id: "world-abc".to_string(),
            version: 7,
        });
        world.story_ref = Some(PackageRef {
            id: "story-xyz".to_string(),
            version: 0,
        });
        // The story was authored against v3 of the world, which has since moved
        // to v7 (the `world_ref` above) — recorded drift.
        world.world_ref_authored_version = Some(3);

        let payload = world_to_payload(&world);
        let obj = payload.as_object().expect("payload object");

        // The payload carries both refs as `{id, version}` objects.
        assert_eq!(obj["world_ref"]["id"], json!("world-abc"));
        assert_eq!(obj["world_ref"]["version"], json!(7));
        assert_eq!(obj["story_ref"]["id"], json!("story-xyz"));
        assert_eq!(obj["story_ref"]["version"], json!(0));
        assert_eq!(obj["world_ref_authored_version"], json!(3));

        // Restore the whole world; the refs come back equal.
        let restored = world_from_payload(Some(&payload)).expect("restore world");
        assert_eq!(
            restored.world_ref,
            Some(PackageRef {
                id: "world-abc".to_string(),
                version: 7,
            })
        );
        assert_eq!(
            restored.story_ref,
            Some(PackageRef {
                id: "story-xyz".to_string(),
                version: 0,
            })
        );
        assert_eq!(restored.world_ref_authored_version, Some(3));
    }

    /// `None` refs => the payload has NO `world_ref` / `story_ref` keys at all.
    /// This guards the byte-identical claim for pre-Phase-4 / unbound saves: an
    /// absent ref must never serialize a `null` (or any) key.
    #[test]
    fn absent_package_refs_emit_no_keys() {
        let mut world = worldgen_world();
        world.world_ref = None;
        world.story_ref = None;
        world.world_ref_authored_version = None;

        let payload = world_to_payload(&world);
        let obj = payload.as_object().expect("payload object");

        assert!(
            !obj.contains_key("world_ref"),
            "an unset world_ref must not appear in the payload"
        );
        assert!(
            !obj.contains_key("story_ref"),
            "an unset story_ref must not appear in the payload"
        );
        assert!(
            !obj.contains_key("world_ref_authored_version"),
            "an unset world_ref_authored_version must not appear in the payload"
        );

        // And a restore yields None (no fabricated ref).
        let restored = world_from_payload(Some(&payload)).expect("restore world");
        assert_eq!(restored.world_ref, None);
        assert_eq!(restored.story_ref, None);
        assert_eq!(restored.world_ref_authored_version, None);
    }

    /// K1: a `Some` char_ref serializes `{id, version}` (right after
    /// `world_ref_authored_version`) and round-trips to an EQUAL `PackageRef`.
    #[test]
    fn char_ref_round_trips_when_set() {
        let mut world = worldgen_world();
        world.char_ref = Some(PackageRef {
            id: "char-hero".to_string(),
            version: 4,
        });

        let payload = world_to_payload(&world);
        let obj = payload.as_object().expect("payload object");
        assert_eq!(obj["char_ref"]["id"], json!("char-hero"));
        assert_eq!(obj["char_ref"]["version"], json!(4));

        let restored = world_from_payload(Some(&payload)).expect("restore world");
        assert_eq!(
            restored.char_ref,
            Some(PackageRef {
                id: "char-hero".to_string(),
                version: 4,
            })
        );
    }

    /// K1: an unset char_ref must emit NO `char_ref` key (byte-identity for pre-K1
    /// and no-character launches), and a restore yields `None`.
    #[test]
    fn absent_char_ref_emits_no_key() {
        let mut world = worldgen_world();
        world.char_ref = None;

        let payload = world_to_payload(&world);
        let obj = payload.as_object().expect("payload object");
        assert!(
            !obj.contains_key("char_ref"),
            "an unset char_ref must not appear in the payload"
        );

        let restored = world_from_payload(Some(&payload)).expect("restore world");
        assert_eq!(restored.char_ref, None);
    }

    /// §И2: a non-empty `place_items` store serializes as a trailing object of
    /// `place_id -> [SceneItem]` and round-trips to an EQUAL map. Keyed on a
    /// place OTHER than the current one so the post-restore refresh (same-place)
    /// does not touch the store.
    #[test]
    fn place_items_round_trip_when_non_empty() {
        use gml_world::SceneItem;
        let mut world = worldgen_world();
        let parked = vec![
            SceneItem {
                item_id: "torch".to_string(),
                name: "Факел".to_string(),
                location: "у стены".to_string(),
                visible: true,
                portable: true,
                owner: String::new(),
                details: "горит 1 час".to_string(),
            },
            SceneItem {
                item_id: "anvil".to_string(),
                name: "Наковальня".to_string(),
                location: "в углу".to_string(),
                visible: true,
                portable: false,
                owner: String::new(),
                details: String::new(),
            },
        ];
        world
            .place_items
            .insert("some_other_place".to_string(), parked.clone());

        let payload = world_to_payload(&world);
        let obj = payload.as_object().expect("payload object");
        assert!(
            obj["place_items"]["some_other_place"].is_array(),
            "place_items is a place_id -> [item] object"
        );
        assert_eq!(
            obj["place_items"]["some_other_place"][0]["item_id"],
            json!("torch")
        );
        assert_eq!(
            obj["place_items"]["some_other_place"][1]["portable"],
            json!(false)
        );

        let restored = world_from_payload(Some(&payload)).expect("restore world");
        assert_eq!(
            restored.place_items.get("some_other_place"),
            Some(&parked),
            "the per-place store round-trips exactly"
        );
    }

    /// §И2: an empty `place_items` store must emit NO `place_items` key at all,
    /// so pre-Phase-И saves stay byte-identical; a restore yields an empty map.
    #[test]
    fn absent_place_items_emits_no_key() {
        let world = worldgen_world();
        assert!(world.place_items.is_empty(), "fresh world has no parked items");

        let payload = world_to_payload(&world);
        let obj = payload.as_object().expect("payload object");
        assert!(
            !obj.contains_key("place_items"),
            "an empty place_items store must not appear in the payload"
        );

        let restored = world_from_payload(Some(&payload)).expect("restore world");
        assert!(restored.place_items.is_empty());
    }

    // --- Фаза С §С1: player_character spell fields round-trip ----------------

    /// A caster PC's spells / flat slots / concentration round-trip through the
    /// canonical save payload exactly. Spells parse via serde (NOT str_list);
    /// slots are flat dicts (like hp); concentration is a string.
    #[test]
    fn player_character_spell_fields_round_trip() {
        use gml_world::SpellEntry;
        let mut spell_slots = Map::new();
        spell_slots.insert("1".to_string(), json!(3));
        let mut spell_slots_max = Map::new();
        spell_slots_max.insert("1".to_string(), json!(4));
        let pc = PlayerCharacter {
            spells: vec![
                SpellEntry {
                    name: "Луч холода".to_string(),
                    level: 0,
                    concentration: false,
                    ritual: false,
                    effect: "1d8 холодом".to_string(),
                },
                SpellEntry {
                    name: "Огненная хватка".to_string(),
                    level: 1,
                    concentration: true,
                    ritual: false,
                    effect: "конц.; 2d6 огнём".to_string(),
                },
            ],
            spell_slots,
            spell_slots_max,
            concentration: "Огненная хватка".to_string(),
            ..Default::default()
        };

        let payload = player_character_to_payload(&pc);
        // Emitted unconditionally (30-field discipline).
        assert_eq!(payload["spells"][1]["name"], json!("Огненная хватка"));
        assert_eq!(payload["spells"][1]["concentration"], json!(true));
        assert_eq!(payload["spell_slots"]["1"], json!(3));
        assert_eq!(payload["spell_slots_max"]["1"], json!(4));
        assert_eq!(payload["concentration"], json!("Огненная хватка"));

        let restored = player_character_from_payload(Some(&payload));
        assert_eq!(restored.spells, pc.spells);
        assert_eq!(restored.spell_slots, pc.spell_slots);
        assert_eq!(restored.spell_slots_max, pc.spell_slots_max);
        assert_eq!(restored.concentration, pc.concentration);
    }

    /// Old-save back-compat: a `player_character` payload WITHOUT the Фаза С keys
    /// (a pre-Phase-С save) loads with empty spells / slots and no concentration
    /// — the `serde(default)` + null-safe getters guarantee (§С1). Junk in the
    /// spells array is dropped, never fatal.
    #[test]
    fn player_character_absent_spell_keys_default_and_junk_is_dropped() {
        // A minimal legacy payload: the pre-Phase-С shape, no spell keys at all.
        let legacy = json!({
            "name": "Старый герой",
            "inventory": ["кинжал"],
        });
        let restored = player_character_from_payload(Some(&legacy));
        assert_eq!(restored.name, "Старый герой");
        assert!(restored.spells.is_empty(), "absent spells default to empty");
        assert!(restored.spell_slots.is_empty());
        assert!(restored.spell_slots_max.is_empty());
        assert!(restored.concentration.is_empty());

        // A payload whose spells array carries junk keeps only the object entries.
        let with_junk = json!({
            "spells": ["не спелл", 7, {"name": "Свет", "level": 0}],
        });
        let restored = player_character_from_payload(Some(&with_junk));
        assert_eq!(restored.spells.len(), 1, "only the object entry survives");
        assert_eq!(restored.spells[0].name, "Свет");
    }
}
