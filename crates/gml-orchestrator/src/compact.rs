//! Token estimation, `_meta` / `_meta_total`, run-usage accounting, GM/NPC
//! compaction, and `context_usage` — ports from `orchestrator.py`.

use std::sync::atomic::{AtomicI64, Ordering};

use serde_json::{json, Map, Value};

use gml_llm::Backend;
use gml_world::World;

use crate::session::Session;

/// `config.CHARS_PER_TOKEN` default = 3. The orchestrator uses the configured
/// value; the golden fixtures use the default (3).
pub const CHARS_PER_TOKEN: i64 = 3;

/// Accurate system-prompt token count, set by the server when an OpenAI dev key
/// is configured (real `/v1/responses/input_tokens`, disk-cached by SHA of the
/// system prompt). `-1` = unset → fall back to the chars/CHARS_PER_TOKEN
/// estimate. DISPLAY ONLY: `sys_est()` feeds `context_usage` / `meta_total`,
/// never compaction decisions, so overriding it cannot change game behavior.
static SYS_TOKENS_OVERRIDE: AtomicI64 = AtomicI64::new(-1);

/// Set the accurate system-prompt token count used as the starting context
/// baseline. Clamped to `>= 0`.
pub fn set_sys_tokens_override(tokens: i64) {
    SYS_TOKENS_OVERRIDE.store(tokens.max(0), Ordering::Relaxed);
}

/// Clear the override — `sys_est()` returns to the chars/token estimate.
pub fn clear_sys_tokens_override() {
    SYS_TOKENS_OVERRIDE.store(-1, Ordering::Relaxed);
}

/// `_estimate_tokens(text)` = `max(0, len(text) // CHARS_PER_TOKEN)` using
/// `.chars().count()` (NOT bytes — critical for Cyrillic).
pub fn estimate_tokens(text: &str) -> i64 {
    let chars = text.chars().count() as i64;
    (chars / CHARS_PER_TOKEN).max(0)
}

/// `_SYS_EST = len(prompts.GM_SYSTEM) // CHARS_PER_TOKEN`, unless an accurate
/// real-token count has been set via [`set_sys_tokens_override`] (OpenAI dev
/// key path) — then the real count is used as the starting baseline.
pub fn sys_est() -> i64 {
    let override_tokens = SYS_TOKENS_OVERRIDE.load(Ordering::Relaxed);
    if override_tokens >= 0 {
        return override_tokens;
    }
    estimate_tokens(gml_prompts::GM_SYSTEM)
}

/// `_msg_text(m)` — `"{role}: {content}".strip()`.
pub fn msg_text(m: &Value) -> String {
    let role = m.get("role").map(py_str).unwrap_or_default();
    let content = m.get("content").map(py_str).unwrap_or_default();
    format!("{role}: {content}").trim().to_string()
}

/// `_msg_text_for_summary(m)` — strips the PLAYER ACTION prefix for user msgs
/// and drops transient state messages (WORLD SNAPSHOT, player-options toggle
/// notices) entirely so they never bloat the compaction summary base.
pub fn msg_text_for_summary(m: &Value) -> String {
    let role = m.get("role").map(py_str).unwrap_or_default();
    let mut content = m.get("content").map(py_str).unwrap_or_default();
    if role == "user" {
        if content.starts_with(gml_agents::SNAPSHOT_PREFIX)
            || content.starts_with(gml_agents::OPTIONS_NOTICE_PREFIX)
        {
            return String::new();
        }
        let marker = gml_agents::PLAYER_ACTION_HEADER;
        if let Some(idx) = content.find(marker) {
            content = content[idx + marker.len()..].trim().to_string();
        }
    }
    format!("{role}: {content}").trim().to_string()
}

/// Python `str(value)` of a JSON content value (default "").
fn py_str(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// `json.dumps(data, ensure_ascii=False)` with Python DEFAULT separators
/// (`", "` and `": "`, spaces after delimiters) without touching string bodies.
pub fn py_json_dumps_default(v: &Value) -> String {
    let compact = serde_json::to_string(v).unwrap_or_default();
    let mut out = String::with_capacity(compact.len() + compact.len() / 8);
    let mut in_string = false;
    let mut escaped = false;
    for c in compact.chars() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            ',' => {
                out.push(',');
                out.push(' ');
            }
            ':' => {
                out.push(':');
                out.push(' ');
            }
            _ => out.push(c),
        }
    }
    out
}

/// `_messages_tokens(messages)`.
pub fn messages_tokens(messages: &[Value]) -> i64 {
    messages.iter().map(|m| estimate_tokens(&msg_text(m))).sum()
}

/// `_world_context_tokens(world)`.
pub fn world_context_tokens(world: &mut World) -> i64 {
    let scene_export = world.scene_export();
    let npc_lines = world
        .npcs
        .values()
        .map(|npc| {
            format!(
                "{}: {}; {}; {}; {}",
                npc.name, npc.role, npc.pronouns, npc.persona, npc.knowledge
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let parts = [
        world.public.clone(),
        world.canon.clone(),
        world.constraints.join("\n"),
        // json.dumps(scene_export, ensure_ascii=False, default=str) — DEFAULT
        // separators (", " / ": "), NOT compact. The spacing is load-bearing
        // for the token estimate (and thus context_usage parity).
        py_json_dumps_default(&scene_export),
        npc_lines,
    ];
    estimate_tokens(&parts.join("\n"))
}

// =========================================================================
// _meta / _meta_total
// =========================================================================

/// `_meta(label, stats, scope="npc")`.
pub fn meta(label: &str, stats: &Map<String, Value>, scope: &str) -> Value {
    let pin = stats
        .get("prompt_eval_count")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let pout = stats
        .get("eval_count")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let cached = stats
        .get("cached_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let ed = stats
        .get("eval_duration")
        .and_then(|v| v.as_i64())
        .unwrap_or(0) as f64
        / 1e9;
    let pd = stats
        .get("prompt_eval_duration")
        .and_then(|v| v.as_i64())
        .unwrap_or(0) as f64
        / 1e9;
    let td = stats
        .get("total_duration")
        .and_then(|v| v.as_i64())
        .unwrap_or(0) as f64
        / 1e9;
    let ld = stats
        .get("load_duration")
        .and_then(|v| v.as_i64())
        .unwrap_or(0) as f64
        / 1e9;
    let tps = if ed > 0.0 {
        (pout as f64 / ed).round() as i64
    } else {
        0
    };
    json!({
        "label": label,
        "scope": scope,
        "in": pin,
        "out": pout,
        "secs": round2(td),
        "cached": cached,
        "tps": tps,
        "prompt_secs": round2(pd),
        "eval_secs": round2(ed),
        "load_secs": round2(ld),
    })
}

/// `_meta_total(metas, total_secs)`.
pub fn meta_total(metas: &[Value], total_secs: f64) -> Value {
    let sum_i = |key: &str| {
        metas
            .iter()
            .map(|m| m.get(key).and_then(|v| v.as_i64()).unwrap_or(0))
            .sum::<i64>()
    };
    let in_total = sum_i("in");
    let out_total = sum_i("out");
    let cached_total = sum_i("cached");
    let peak = metas
        .iter()
        .map(|m| m.get("in").and_then(|v| v.as_i64()).unwrap_or(0))
        .max()
        .unwrap_or(0);
    json!({
        "calls": metas,
        "in": in_total,
        "out": out_total,
        "cached": cached_total,
        "tokens": in_total + out_total,
        "peak_context": peak,
        "secs": total_secs,
        "sys_estimate": sys_est(),
    })
}

/// Python `round(x, 2)`.
pub fn round2(x: f64) -> f64 {
    crate::round_half_even(x, 2)
}

/// `total["context"] = context_usage(session)` — insert the context block into
/// the meta_total payload (run is inserted by the caller after add_turn_usage).
pub fn add_total_context(total: &mut Value, context: Value) {
    if let Value::Object(m) = total {
        m.insert("context".to_string(), context);
    }
}

// =========================================================================
// usage
// =========================================================================

/// `_empty_usage()`.
pub fn empty_usage() -> Map<String, Value> {
    let mut m = Map::new();
    for key in ["turns", "calls", "in", "out", "cached", "tokens"] {
        m.insert(key.to_string(), json!(0));
    }
    m.insert("secs".to_string(), json!(0.0));
    for key in [
        "peak_context",
        "gm_calls",
        "gm_tokens",
        "npc_calls",
        "npc_tokens",
        "other_calls",
        "other_tokens",
    ] {
        m.insert(key.to_string(), json!(0));
    }
    m
}

/// `_usage_from_payload(value)`.
pub fn usage_from_payload(value: &Value) -> Map<String, Value> {
    let mut usage = empty_usage();
    if let Value::Object(v) = value {
        for key in usage.keys().cloned().collect::<Vec<_>>() {
            if let Some(val) = v.get(&key) {
                usage.insert(key, val.clone());
            }
        }
    }
    let int_keys = [
        "turns",
        "calls",
        "in",
        "out",
        "cached",
        "tokens",
        "peak_context",
        "gm_calls",
        "gm_tokens",
        "npc_calls",
        "npc_tokens",
        "other_calls",
        "other_tokens",
    ];
    for key in int_keys {
        let v = usage.get(key).and_then(|v| v.as_i64()).unwrap_or(0);
        usage.insert(key.to_string(), json!(v));
    }
    let secs = usage.get("secs").and_then(|v| v.as_f64()).unwrap_or(0.0);
    usage.insert("secs".to_string(), json!(round2(secs)));
    usage
}

// =========================================================================
// compaction
// =========================================================================

/// `_maybe_compact(session)` (async because of client.summarize).
///
/// Thresholds (`GM_HISTORY_TOKENS` / `GM_KEEP_TURNS` / `GM_COMPACT_INPUT_CHARS`)
/// are read from `session.compaction` — the Rust home for the `config.*` globals
/// Python reads at call time. Production defaults match `config`; tests lower
/// them on the session exactly like the Python tests monkeypatch `config.*`.
pub async fn maybe_compact(session: &mut Session, client: &dyn Backend) {
    let gm_history_tokens = session.compaction.gm_history_tokens;
    let gm_keep_turns = session.compaction.gm_keep_turns;
    if messages_tokens(&session.gm_messages) < gm_history_tokens {
        return;
    }
    let starts: Vec<usize> = session
        .gm_messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.get("role").and_then(Value::as_str) == Some("user"))
        .map(|(i, _)| i)
        .collect();
    if (starts.len() as i64) <= gm_keep_turns {
        return;
    }
    // Python: `cut = starts[-config.GM_KEEP_TURNS]`.
    let cut = starts[starts.len() - gm_keep_turns.max(0) as usize];
    let old: Vec<Value> = session.gm_messages[..cut].to_vec();
    let recent: Vec<Value> = session.gm_messages[cut..].to_vec();
    let old_text = old
        .iter()
        .map(msg_text_for_summary)
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let base = format!("{}\n{}", session.gm_summary, old_text)
        .trim()
        .to_string();
    let proper_nouns = session.world.proper_nouns();
    let clip = session.compaction.compact_input_chars.max(0) as usize;
    let clipped: String = base.chars().take(clip).collect();
    let summary = client
        .summarize(&clipped, &proper_nouns)
        .await
        .unwrap_or_default();
    session.gm_summary = summary;
    // Snapshot-once (GM_CONTEXT_TZ §2): the retained history must START with a
    // FRESH snapshot, replacing any prior snapshot that got compacted away.
    let recent_contact_ids = session.recent_contact_ids();
    let options_state = session.snapshot_options_state.unwrap_or(false);
    let snapshot =
        gml_agents::gm_world_snapshot(&mut session.world, &recent_contact_ids, options_state);
    let mut fresh = vec![gml_agents::gm_snapshot_message(&snapshot)];
    // A snapshot can sit INSIDE the retained tail (legacy saves lazily inject it
    // at the END of the loaded history, which the keep-window then straddles).
    // Keeping it would feed the GM a second, stale full-state block positioned
    // LATER than the fresh head — strip any tail snapshots before extending.
    fresh.extend(
        recent
            .into_iter()
            .filter(|m| !gml_agents::is_snapshot_message(m)),
    );
    session.gm_messages = fresh;
    session.reset_world_query_cache();
    // Drop SEARCHED/loaded tools that fell out of the retained window (the GM
    // prompt cache resets at compaction, so this is the cheap moment to shrink
    // the visible tool set back toward the initial defaults). The retained tail
    // keeps the last `gm_keep_turns` user boundaries, i.e. turns
    // [turn - gm_keep_turns + 1 .. turn]; anything last used/loaded before that
    // first retained turn is stale. The INITIAL set is untouched.
    let oldest_retained_turn = (session.turn - gm_keep_turns.max(0) + 1).max(0);
    session.prune_stale_loaded_tools(oldest_retained_turn);
}

/// `_maybe_compact_npc(session, npc, client)`.
pub async fn maybe_compact_npc(session: &mut Session, npc_id: &str, client: &dyn Backend) {
    let npc_history_tokens = session.compaction.npc_history_tokens;
    let npc_keep_exchanges = session.compaction.npc_keep_exchanges;
    let msgs = session
        .npc_messages
        .get(npc_id)
        .cloned()
        .unwrap_or_default();
    if messages_tokens(&msgs) < npc_history_tokens {
        return;
    }
    let starts: Vec<usize> = msgs
        .iter()
        .enumerate()
        .filter(|(_, m)| m.get("role").and_then(Value::as_str) == Some("user"))
        .map(|(i, _)| i)
        .collect();
    if (starts.len() as i64) <= npc_keep_exchanges {
        return;
    }
    let cut = starts[starts.len() - npc_keep_exchanges.max(0) as usize];
    let old: Vec<Value> = msgs[..cut].to_vec();
    let recent: Vec<Value> = msgs[cut..].to_vec();
    let old_text = old
        .iter()
        .map(msg_text)
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let prev_summary = session
        .npc_summaries
        .get(npc_id)
        .cloned()
        .unwrap_or_default();
    let base = format!("{prev_summary}\n{old_text}").trim().to_string();
    let clip = session.compaction.compact_input_chars.max(0) as usize;
    let summary = summarize_npc_history(client, &session.world, &base, clip).await;
    session.npc_summaries.insert(npc_id.to_string(), summary);
    session.npc_messages.insert(npc_id.to_string(), recent);
}

/// `_summarize_npc_history(client, npc, world, text)`.
async fn summarize_npc_history(
    client: &dyn Backend,
    world: &World,
    text: &str,
    clip_chars: usize,
) -> String {
    let proper_nouns = world.proper_nouns().join(", ");
    let system = gml_prompts::render_npc_compact_system(&proper_nouns);
    let clipped: String = text.chars().take(clip_chars).collect();
    let messages = json!([
        {"role": "system", "content": system},
        {"role": "user", "content": clipped},
    ]);
    match client
        .chat(
            &messages,
            None,
            Some(true),
            gml_types::Role::Compact.as_str(),
        )
        .await
    {
        Ok(out) => out.content.trim().to_string(),
        Err(_) => String::new(),
    }
}

// =========================================================================
// context_usage
// =========================================================================

/// `context_usage(session)`.
pub fn context_usage(session: &mut Session) -> Value {
    let world_tokens = world_context_tokens(&mut session.world);
    let gm_history = messages_tokens(&session.gm_messages);
    let gm_summary = estimate_tokens(&session.gm_summary);
    let gm_active = sys_est() + world_tokens + gm_summary + gm_history;
    let gm_limit = session.compaction.gm_history_tokens;
    let gm_remaining = (gm_limit - gm_history).max(0);

    let npc_history_tokens = session.compaction.npc_history_tokens;

    // Collect candidate npc ids.
    let mut npc_ids: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    npc_ids.extend(session.world.npcs.keys().cloned());
    npc_ids.extend(session.npc_messages.keys().cloned());
    npc_ids.extend(session.npc_summaries.keys().cloned());
    npc_ids.extend(session.npc_client_state.keys().cloned());

    let mut npc_entries: Vec<Value> = Vec::new();
    for npc_id in &npc_ids {
        let messages = session
            .npc_messages
            .get(npc_id)
            .cloned()
            .unwrap_or_default();
        let npc = session.world.npcs.get(npc_id);
        let name = npc
            .map(|n| n.name.clone())
            .unwrap_or_else(|| npc_id.clone());
        let history = messages_tokens(&messages);
        let summary = estimate_tokens(
            session
                .npc_summaries
                .get(npc_id)
                .map(|s| s.as_str())
                .unwrap_or(""),
        );
        let persona = match npc {
            None => 0,
            Some(n) => estimate_tokens(&format!(
                "{} {} {} {} {} {} {}",
                n.name, n.role, n.pronouns, n.persona, n.voice, n.goals, n.knowledge
            )),
        };
        let active = world_tokens + persona + summary + history;
        let has_session =
            !messages.is_empty() || summary > 0 || session.npc_client_state.contains_key(npc_id);
        npc_entries.push(json!({
            "id": npc_id,
            "name": name,
            "color": npc.map(|n| n.color.clone()).unwrap_or_default(),
            "has_session": has_session,
            "active": active,
            "history": history,
            "summary": summary,
            "limit": npc_history_tokens,
            "remaining": (npc_history_tokens - history).max(0),
        }));
    }
    // Sort: (not has_session, -history, name).
    npc_entries.sort_by(|a, b| {
        let ah = !a["has_session"].as_bool().unwrap_or(false);
        let bh = !b["has_session"].as_bool().unwrap_or(false);
        ah.cmp(&bh)
            .then_with(|| {
                b["history"]
                    .as_i64()
                    .unwrap_or(0)
                    .cmp(&a["history"].as_i64().unwrap_or(0))
            })
            .then_with(|| {
                a["name"]
                    .as_str()
                    .unwrap_or("")
                    .cmp(b["name"].as_str().unwrap_or(""))
            })
    });

    let npc_max = npc_entries
        .iter()
        .max_by_key(|e| e["active"].as_i64().unwrap_or(0))
        .cloned();

    let mut candidates: Vec<Value> = vec![json!({
        "scope": "gm",
        "label": "GM",
        "used": gm_history,
        "limit": gm_limit,
        "remaining": gm_remaining,
    })];
    for entry in &npc_entries {
        if !entry["has_session"].as_bool().unwrap_or(false) {
            continue;
        }
        candidates.push(json!({
            "scope": "npc",
            "label": entry["name"],
            "used": entry["history"],
            "limit": entry["limit"],
            "remaining": entry["remaining"],
        }));
    }
    let next_compact = candidates
        .iter()
        .min_by_key(|c| c["remaining"].as_i64().unwrap_or(i64::MAX))
        .cloned()
        .unwrap();

    let npc_active = npc_max
        .as_ref()
        .map(|n| n["active"].as_i64().unwrap_or(0))
        .unwrap_or(0);
    let current = gm_active.max(npc_active);

    let npc_obj = npc_max.unwrap_or_else(|| {
        json!({
            "id": "",
            "name": "",
            "active": 0,
            "history": 0,
            "summary": 0,
            "limit": npc_history_tokens,
            "remaining": npc_history_tokens,
        })
    });

    json!({
        "current": current,
        "world": world_tokens,
        "next_compact": next_compact,
        "gm": {
            "active": gm_active,
            "history": gm_history,
            "summary": gm_summary,
            "limit": gm_limit,
            "remaining": gm_remaining,
        },
        "npc": npc_obj,
        "npcs": npc_entries,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sys_est_uses_override_then_falls_back_to_estimate() {
        let estimate = estimate_tokens(gml_prompts::GM_SYSTEM);
        // Default: the chars/token estimate.
        clear_sys_tokens_override();
        assert_eq!(sys_est(), estimate);
        // With an override (dev-key real count): the override wins.
        set_sys_tokens_override(12345);
        assert_eq!(sys_est(), 12345);
        // Negative inputs clamp to 0.
        set_sys_tokens_override(-50);
        assert_eq!(sys_est(), 0);
        // Cleared: back to the estimate (don't leak global state to other tests).
        clear_sys_tokens_override();
        assert_eq!(sys_est(), estimate);
    }
}
