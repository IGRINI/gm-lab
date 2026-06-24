//! gml-types — shared cross-crate value types for GM-Lab.
//!
//! This is the single home for value types passed across module boundaries, so
//! there are no circular dependencies (see PORT_PLAN.md §1.2). Every type here is
//! a faithful port of a Python shape; the Python origin is cited per item.
//!
//! Dependency-light by design: only `serde`, `serde_json`, `thiserror`.

pub mod error;
pub mod event;
pub mod npc;
pub mod role;
pub mod tool;

pub use error::{ParseRoleError, TypesError};
pub use event::{event_kind, Event};
pub use npc::{NpcBeat, NpcResponse};
pub use role::{Role, REASONING_ROLES};
pub use tool::{ParsedCall, ToolExecutionResult};

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Map, Value};

    // --- Role string mapping ------------------------------------------------

    #[test]
    fn role_as_str_matches_python_config() {
        assert_eq!(Role::Gm.as_str(), "gm");
        assert_eq!(Role::Npc.as_str(), "npc");
        assert_eq!(Role::Compact.as_str(), "compact");
        assert_eq!(Role::Location.as_str(), "location");
    }

    #[test]
    fn role_parse_roundtrip() {
        for r in REASONING_ROLES {
            assert_eq!(Role::parse(r.as_str()).unwrap(), r);
            assert_eq!(r.to_string(), r.as_str());
        }
        assert!(Role::parse("GM").is_err());
        assert!(Role::parse("").is_err());
        assert!(Role::parse("dm").is_err());
    }

    #[test]
    fn role_reasoning_roles_order() {
        assert_eq!(
            REASONING_ROLES,
            [Role::Gm, Role::Npc, Role::Compact, Role::Location]
        );
    }

    #[test]
    fn role_serde_is_bare_string() {
        assert_eq!(serde_json::to_string(&Role::Gm).unwrap(), "\"gm\"");
        assert_eq!(serde_json::to_string(&Role::Npc).unwrap(), "\"npc\"");
        assert_eq!(
            serde_json::to_string(&Role::Compact).unwrap(),
            "\"compact\""
        );
        assert_eq!(
            serde_json::to_string(&Role::Location).unwrap(),
            "\"location\""
        );
        let r: Role = serde_json::from_str("\"location\"").unwrap();
        assert_eq!(r, Role::Location);
        assert!(serde_json::from_str::<Role>("\"bogus\"").is_err());
    }

    // --- Event serialization shape -----------------------------------------

    #[test]
    fn event_key_order_kind_agent_data_sid() {
        // Python ev() inserts keys in order: kind, agent, data, sid. With
        // serde_json preserve_order, struct field order is preserved on output.
        let e = Event::new(
            event_kind::PLAYER,
            Some("ГМ".to_string()),
            json!({"text": "привет"}),
            Some("s1".to_string()),
        );
        let s = serde_json::to_string(&e).unwrap();
        assert_eq!(
            s,
            r#"{"kind":"player","agent":"ГМ","data":{"text":"привет"},"sid":"s1"}"#
        );
    }

    #[test]
    fn event_all_four_keys_present_with_nulls() {
        // ev("done", None) -> {"kind":"done","agent":null,"data":null,"sid":null}
        let e = Event::bare(event_kind::DONE, None);
        let s = serde_json::to_string(&e).unwrap();
        assert_eq!(s, r#"{"kind":"done","agent":null,"data":null,"sid":null}"#);
    }

    #[test]
    fn event_non_ascii_is_raw() {
        // json.dumps(..., ensure_ascii=False): Cyrillic stays raw, not \uXXXX.
        let e = Event::bare("npc_speech", Some("Ива".to_string()));
        let s = serde_json::to_string(&e).unwrap();
        assert!(s.contains("Ива"));
        assert!(!s.contains("\\u"));
    }

    #[test]
    fn event_roundtrip() {
        let e = Event::new("dice", None, json!({"total": 17}), None);
        let s = serde_json::to_string(&e).unwrap();
        let back: Event = serde_json::from_str(&s).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn event_kind_all_contains_done_and_emitted() {
        // 28 distinct ev() kinds (incl. living-world `world_debug` and the
        // NPC-only tool call/result events) + server-pushed terminal `done`.
        assert_eq!(event_kind::ALL.len(), 29);
        assert!(event_kind::ALL.contains(&event_kind::DONE));
        assert!(event_kind::ALL.contains(&event_kind::NPC_SPEECH));
        assert!(event_kind::ALL.contains(&event_kind::NPC_TOOL_CALL));
        assert!(event_kind::ALL.contains(&event_kind::NPC_TOOL_RESULT));
        assert!(event_kind::ALL.contains(&event_kind::WORLD_DEBUG));
        assert_eq!(event_kind::DELTA_CHANNELS.len(), 3);
    }

    // --- NpcResponse contract ----------------------------------------------

    #[test]
    fn npc_response_field_order_and_defaults() {
        let r = NpcResponse {
            reasoning: "думаю".to_string(),
            response: "Борин хмурится: «Сейчас». Он отступает к двери.".to_string(),
            beats: vec![
                NpcBeat {
                    kind: "action".to_string(),
                    text: "Борин хмурится".to_string(),
                },
                NpcBeat {
                    kind: "speech".to_string(),
                    text: "Сейчас".to_string(),
                },
            ],
            speech: "сейчас".to_string(),
            action: "кивает".to_string(),
            claims: vec!["a".to_string()],
        };
        let s = serde_json::to_string(&r).unwrap();
        assert_eq!(
            s,
            r#"{"reasoning":"думаю","response":"Борин хмурится: «Сейчас». Он отступает к двери.","beats":[{"kind":"action","text":"Борин хмурится"},{"kind":"speech","text":"Сейчас"}],"speech":"сейчас","action":"кивает","claims":["a"]}"#
        );
        // Missing fields coerce to empty (mirrors _norm_npc on a partial dict).
        let partial: NpcResponse = serde_json::from_str("{}").unwrap();
        assert_eq!(partial, NpcResponse::default());
    }

    // --- ToolExecutionResult ------------------------------------------------

    #[test]
    fn tool_execution_result_terminal_defaults_false() {
        let t = ToolExecutionResult::new("full", "model");
        assert!(!t.terminal);
        let back: ToolExecutionResult =
            serde_json::from_str(r#"{"full":"f","model":"m"}"#).unwrap();
        assert!(!back.terminal);
        assert_eq!(back.full, "f");
        assert_eq!(back.model, "m");
    }

    // --- ParsedCall ---------------------------------------------------------

    #[test]
    fn parsed_call_shape() {
        let mut args = Map::new();
        args.insert("npc_id".to_string(), Value::String("iva".to_string()));
        let c = ParsedCall::new("ask_npc", args, "mock0");
        let s = serde_json::to_string(&c).unwrap();
        assert_eq!(
            s,
            r#"{"name":"ask_npc","arguments":{"npc_id":"iva"},"id":"mock0"}"#
        );
        // arguments defaults to {} (Python `args or {}`), id defaults to "".
        let back: ParsedCall = serde_json::from_str(r#"{"name":"roll_dice"}"#).unwrap();
        assert_eq!(back.name, "roll_dice");
        assert!(back.arguments.is_empty());
        assert_eq!(back.id, "");
    }
}
