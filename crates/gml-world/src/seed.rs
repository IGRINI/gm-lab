//! `_normalize_seed` — faithful port of the large, lenient seed normalizer in
//! world.py. Accepts the strict seed shape and the looser shapes local models
//! produce (npcs as list OR dict, items under visible_objects/objects/items,
//! exits under visible_exits/exits, title under several keys).

use serde_json::{json, Map, Value};

use crate::helpers::{as_list, as_str, get_str, safe_id};

/// Returns the seed unchanged when it already matches the strict shape; else
/// rebuilds it from the looser fields. Always returns a JSON object map.
pub fn normalize_seed(seed: &Value) -> Map<String, Value> {
    let seed_map = match seed {
        Value::Object(m) => m.clone(),
        _ => return Map::new(),
    };

    let raw_scene = match seed_map.get("scene") {
        Some(Value::Object(m)) => m.clone(),
        _ => Map::new(),
    };

    // Strict-shape short-circuit.
    let scene_is_obj = matches!(seed_map.get("scene"), Some(Value::Object(_)));
    let npcs_is_list = matches!(seed_map.get("npcs"), Some(Value::Array(_)));
    if scene_is_obj
        && npcs_is_list
        && raw_scene.contains_key("items")
        && raw_scene.contains_key("exits")
        && raw_scene.contains_key("title")
    {
        return seed_map;
    }

    // src = {**seed, **raw_scene}
    let mut src = seed_map.clone();
    for (k, v) in &raw_scene {
        src.insert(k.clone(), v.clone());
    }

    let public_facts: Vec<String> = as_list(src.get("public_facts").unwrap_or(&Value::Null))
        .iter()
        .map(as_str)
        .filter(|s| !s.is_empty())
        .collect();

    // npc_details: dict, or from seed.npcs (dict or list).
    let mut npc_details: Map<String, Value> = match src.get("npc_details") {
        Some(Value::Object(m)) => m.clone(),
        _ => Map::new(),
    };
    if npc_details.is_empty() {
        if let Some(Value::Object(m)) = seed_map.get("npcs") {
            npc_details = m.clone();
        }
    }
    if npc_details.is_empty() {
        if let Some(Value::Array(arr)) = seed_map.get("npcs") {
            for raw in arr {
                if let Value::Object(m) = raw {
                    let id = get_str(m, "id");
                    if !id.is_empty() {
                        npc_details.insert(id, raw.clone());
                    }
                }
            }
        }
    }

    let mut present: Vec<String> = as_list(src.get("present_npcs").unwrap_or(&Value::Null))
        .iter()
        .map(as_str)
        .filter(|s| !s.is_empty())
        .collect();
    if present.is_empty() && !npc_details.is_empty() {
        present = npc_details.keys().cloned().collect();
    }

    let mut npcs: Vec<Value> = Vec::new();
    let mut npc_presence: Map<String, Value> = Map::new();
    for (idx0, npc_id) in present.iter().enumerate() {
        let idx = idx0 + 1;
        let raw = match npc_details.get(npc_id) {
            Some(Value::Object(m)) => m.clone(),
            _ => Map::new(),
        };
        let name = {
            let n = get_str(&raw, "name");
            if n.is_empty() {
                npc_id.clone()
            } else {
                n
            }
        };
        let safe_npc_id = safe_id(npc_id, &format!("npc_{idx}"));
        let pronouns = {
            let p = get_str(&raw, "pronouns");
            if p.is_empty() {
                get_str(&raw, "gender")
            } else {
                p
            }
        };
        let persona = {
            let p = get_str(&raw, "persona");
            if !p.is_empty() {
                p
            } else {
                let d = get_str(&raw, "description");
                if !d.is_empty() {
                    d
                } else {
                    format!("{name} присутствует в стартовой сцене.")
                }
            }
        };
        let knowledge = {
            let k = get_str(&raw, "knowledge");
            if !k.is_empty() {
                k
            } else if !public_facts.is_empty() {
                format!("Публичные факты сцены: {}", public_facts.join("; "))
            } else {
                "Только то, что очевидно в текущей сцене.".to_string()
            }
        };
        npcs.push(json!({
            "id": safe_npc_id,
            "name": name,
            "role": nonempty(get_str(&raw, "role"), "персонаж сцены"),
            "pronouns": pronouns,
            "persona": persona,
            "voice": nonempty(get_str(&raw, "voice"), "Естественно, кратко, в образе."),
            "goals": nonempty(get_str(&raw, "goals"), "Реагировать правдоподобно и защищать свои интересы."),
            "knowledge": knowledge,
            "secret": nonempty(get_str(&raw, "secret"), "Личная тайна не задана."),
        }));
        let presence_location = {
            let l = get_str(&raw, "location");
            if !l.is_empty() {
                l
            } else {
                let p = get_str(&raw, "position");
                if !p.is_empty() {
                    p
                } else {
                    "в сцене".to_string()
                }
            }
        };
        let presence_activity = {
            let s = get_str(&raw, "state");
            if !s.is_empty() {
                s
            } else {
                get_str(&raw, "activity")
            }
        };
        let presence_attitude = {
            let mo = get_str(&raw, "mood");
            if !mo.is_empty() {
                mo
            } else {
                get_str(&raw, "attitude")
            }
        };
        npc_presence.insert(
            safe_npc_id.clone(),
            json!({
                "location": presence_location,
                "activity": presence_activity,
                "attitude": presence_attitude,
            }),
        );
    }

    let location = match src.get("location") {
        Some(Value::Object(m)) => m.clone(),
        _ => Map::new(),
    };

    // items: visible_objects / objects / items
    let items_src = first_present_list(&src, &["visible_objects", "objects", "items"]);
    let mut items: Vec<Value> = Vec::new();
    for (idx0, raw) in items_src.iter().enumerate() {
        let idx = idx0 + 1;
        match raw {
            Value::Object(m) => {
                let name = first_nonempty_str(
                    m,
                    &["name", "display_name", "description", "id"],
                    &format!("предмет {idx}"),
                );
                let details = {
                    let d = get_str(m, "details");
                    if !d.is_empty() {
                        d
                    } else {
                        get_str(m, "description")
                    }
                };
                items.push(json!({
                    "id": safe_id(&get_str(m, "id"), &format!("item_{idx}")),
                    "name": name,
                    "location": nonempty(get_str(m, "location"), "в сцене"),
                    "visible": m.get("visible").map(crate::seed::as_bool).unwrap_or(true),
                    "portable": m.get("portable").map(crate::seed::as_bool).unwrap_or(false),
                    "details": details,
                }));
            }
            _ => {
                let name = as_str(raw);
                if !name.is_empty() {
                    items.push(json!({
                        "id": safe_id(&name, &format!("item_{idx}")),
                        "name": name,
                        "location": "в сцене",
                        "visible": true,
                        "portable": false,
                    }));
                }
            }
        }
    }

    // exits: visible_exits / exits
    let exits_src = first_present_list(&src, &["visible_exits", "exits"]);
    let mut exits: Vec<Value> = Vec::new();
    for (idx0, raw) in exits_src.iter().enumerate() {
        let idx = idx0 + 1;
        match raw {
            Value::Object(m) => {
                let name = first_nonempty_str(
                    m,
                    &["name", "display_name", "description", "id"],
                    &format!("выход {idx}"),
                );
                let destination = first_nonempty_str(
                    m,
                    &["destination", "destination_scene_id", "direction"],
                    &name,
                );
                exits.push(json!({
                    "id": safe_id(&get_str(m, "id"), &format!("exit_{idx}")),
                    "name": name,
                    "destination": destination,
                    "visible": m.get("visible").map(crate::seed::as_bool).unwrap_or(true),
                    "blocked_by": get_str(m, "blocked_by"),
                }));
            }
            _ => {
                let name = as_str(raw);
                if !name.is_empty() {
                    exits.push(json!({
                        "id": safe_id(&name, &format!("exit_{idx}")),
                        "name": name.clone(),
                        "destination": name,
                        "visible": true,
                    }));
                }
            }
        }
    }

    let title = first_nonempty_str(
        &src,
        &["location_name", "scene_title", "title", "name"],
        "Стартовая сцена",
    );
    let description = {
        let candidates = [
            get_str(&src, "scene_description"),
            get_str(&src, "description"),
            get_str(&location, "description"),
            get_str(&seed_map, "public_intro"),
        ];
        let chosen = candidates.into_iter().find(|s| !s.is_empty());
        chosen.unwrap_or_else(|| {
            "Новая сцена готова. Игрок видит место, людей рядом и ближайший источник конфликта."
                .to_string()
        })
    };

    let mut proper_nouns: Vec<String> =
        as_list(seed_map.get("proper_nouns").unwrap_or(&Value::Null))
            .iter()
            .map(as_str)
            .filter(|s| !s.is_empty())
            .collect();
    for raw in &npcs {
        let n = get_str(raw.as_object().unwrap(), "name");
        if !n.is_empty() && !proper_nouns.contains(&n) {
            proper_nouns.push(n);
        }
    }
    if !title.is_empty() && !proper_nouns.contains(&title) {
        proper_nouns.push(title.clone());
    }

    let present_ids: Vec<Value> = npcs
        .iter()
        .map(|n| json!(get_str(n.as_object().unwrap(), "id")))
        .collect();

    let constraints = {
        let c = as_list(seed_map.get("constraints").unwrap_or(&Value::Null));
        if !c.is_empty() {
            c
        } else {
            vec![
                json!("Здесь существуют только перечисленные видимые предметы, видимые выходы и присутствующие именованные персонажи."),
                json!("Игрок может спрашивать о чём угодно, но неописанные факты остаются неизвестными, пока не будут установлены."),
            ]
        }
    };

    let public_intro = {
        let pi = get_str(&seed_map, "public_intro");
        if !pi.is_empty() {
            pi
        } else {
            let pi2 = get_str(&src, "public_intro");
            if !pi2.is_empty() {
                pi2
            } else {
                description.clone()
            }
        }
    };
    let story_brief = {
        let brief = get_str(&seed_map, "story_brief");
        if !brief.is_empty() {
            brief
        } else {
            let brief = get_str(&seed_map, "player_brief");
            if !brief.is_empty() {
                brief
            } else {
                let brief = get_str(&seed_map, "brief");
                if !brief.is_empty() {
                    brief
                } else {
                    public_intro.clone()
                }
            }
        }
    };
    let hidden_truth = {
        let ht = get_str(&seed_map, "hidden_truth");
        if !ht.is_empty() {
            ht
        } else {
            get_str(&seed_map, "canon")
        }
    };
    let state_records = first_present_list(&seed_map_or_src(&seed_map, &src), &["state_records"]);
    let scene_id = {
        let sid = get_str(&seed_map, "scene_id");
        if !sid.is_empty() {
            sid
        } else {
            let id = get_str(&seed_map, "id");
            if !id.is_empty() {
                id
            } else {
                "start_scene".to_string()
            }
        }
    };
    let location_id = safe_id(&title, "start_location");
    let tension = get_str(&seed_map, "tension");

    let mut out = Map::new();
    out.insert("public_intro".to_string(), json!(public_intro));
    out.insert("story_brief".to_string(), json!(story_brief));
    out.insert("hidden_truth".to_string(), json!(hidden_truth));
    out.insert("proper_nouns".to_string(), json!(proper_nouns));
    out.insert("public_facts".to_string(), json!(public_facts));
    out.insert("state_records".to_string(), json!(state_records));
    out.insert("npcs".to_string(), Value::Array(npcs));
    out.insert(
        "scene".to_string(),
        json!({
            "id": scene_id,
            "location_id": location_id,
            "title": title,
            "description": description,
            "present_npcs": present_ids,
            "items": items,
            "exits": exits,
            "constraints": constraints,
            "tension": tension,
            "npc_presence": npc_presence,
        }),
    );
    // Carry the launch player character through the rebuild path. The rebuild
    // otherwise emits a fixed key set (dropping `player_character`), so a
    // non-strict-shape seed would silently lose its PC and launch the default
    // hero. Preserve the canonical `player_character` key, falling back to the
    // `player` alias, mirroring how `load_seed` reads it back.
    if let Some(pc) = seed_map
        .get("player_character")
        .or_else(|| seed_map.get("player"))
    {
        out.insert("player_character".to_string(), pc.clone());
    }
    out
}

/// `bool(raw.get(key, default))` for visible/portable in seed normalization.
pub fn as_bool(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Null => false,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

fn nonempty(value: String, fallback: &str) -> String {
    if value.is_empty() {
        fallback.to_string()
    } else {
        value
    }
}

fn first_nonempty_str(m: &Map<String, Value>, keys: &[&str], fallback: &str) -> String {
    for k in keys {
        let s = get_str(m, k);
        if !s.is_empty() {
            return s;
        }
    }
    fallback.to_string()
}

/// Python `_as_list(src.get("a") or src.get("b") or ...)` — first key whose
/// value is truthy (non-empty), passed through `_as_list`. world.py uses
/// `or`-chaining which stops at the first truthy value.
fn first_present_list(m: &Map<String, Value>, keys: &[&str]) -> Vec<Value> {
    for k in keys {
        if let Some(v) = m.get(*k) {
            if as_bool(v) {
                return as_list(v);
            }
        }
    }
    Vec::new()
}

/// state_records comes from `seed.get("state_records") or src.get("state_records")`.
fn seed_map_or_src(seed_map: &Map<String, Value>, src: &Map<String, Value>) -> Map<String, Value> {
    let mut combined = Map::new();
    // Prefer seed_map value when truthy; else src.
    let sr_seed = seed_map.get("state_records");
    let sr_src = src.get("state_records");
    if let Some(v) = sr_seed {
        if as_bool(v) {
            combined.insert("state_records".to_string(), v.clone());
            return combined;
        }
    }
    if let Some(v) = sr_src {
        combined.insert("state_records".to_string(), v.clone());
    }
    combined
}
