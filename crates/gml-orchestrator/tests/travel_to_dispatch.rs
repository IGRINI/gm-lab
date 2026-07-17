//! Dispatch-level integration tests for canonical long-distance travel.
//!
//! Local [`Transition`] edges describe only immediate scene exits. These tests
//! prove that `travel_to` uses the separate authored travel network, respects
//! its explicit default policy, advances the shared clock by the selected
//! route's exact duration, and does not persist a synthetic local edge.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use serde_json::{json, Map, Value};

use gml_llm::{
    Backend, BackendError, ChatOutput, ChatStreamOutput, DeltaSink, JsonStreamOutput,
    SessionIdentity,
};
use gml_mock::MockClient;
use gml_orchestrator::{run_tool_collect, ClientFactory, Session};
use gml_stories::StoryStore;
use gml_world::{
    District, Place, Provenance, Region, Settlement, TravelAccess, TravelAnchor, TravelLink,
    TravelNetwork, World,
};

const DESTINATION_ID: &str = "visited_far_shop";
const SURFACE_NETWORK_ID: &str = "surface_routes";
const SEWER_NETWORK_ID: &str = "sewer_routes";
const SURFACE_MINUTES: i64 = 37;
const SEWER_MINUTES: i64 = 7;

struct ScriptedLocationBackend {
    inner: MockClient,
    identity: SessionIdentity,
    responses: Mutex<VecDeque<Map<String, Value>>>,
    observed_messages: Arc<Mutex<Vec<Value>>>,
}

impl ScriptedLocationBackend {
    fn new(responses: Vec<Map<String, Value>>, observed_messages: Arc<Mutex<Vec<Value>>>) -> Self {
        Self {
            inner: MockClient::new(),
            identity: SessionIdentity::new(),
            responses: Mutex::new(responses.into()),
            observed_messages,
        }
    }
}

#[async_trait]
impl Backend for ScriptedLocationBackend {
    fn model(&self) -> String {
        self.inner.model()
    }

    fn set_model(&self, model: &str) {
        self.inner.set_model(model);
    }

    fn set_session_identity(&self, session_id: Option<&str>, thread_id: Option<&str>) {
        self.identity.set(session_id, thread_id);
    }

    fn session_id(&self) -> String {
        self.identity.session_id()
    }

    fn thread_id(&self) -> String {
        self.identity.thread_id()
    }

    async fn list_models(&self) -> Vec<Value> {
        self.inner.list_models().await
    }

    async fn chat(
        &self,
        messages: &Value,
        tools: Option<&Value>,
        think: Option<bool>,
        reasoning_role: &str,
    ) -> Result<ChatOutput, BackendError> {
        self.inner
            .chat(messages, tools, think, reasoning_role)
            .await
    }

    async fn chat_json(
        &self,
        messages: &Value,
        _think: Option<bool>,
        _reasoning_role: &str,
    ) -> Result<Map<String, Value>, BackendError> {
        self.observed_messages
            .lock()
            .expect("observed message lock")
            .push(messages.clone());
        Ok(self
            .responses
            .lock()
            .expect("scripted response lock")
            .pop_front()
            .unwrap_or_default())
    }

    async fn summarize(&self, text: &str, proper_nouns: &[String]) -> Result<String, BackendError> {
        self.inner.summarize(text, proper_nouns).await
    }

    async fn chat_stream(
        &self,
        messages: &Value,
        tools: Option<&Value>,
        think: Option<bool>,
        reasoning_role: &str,
        sink: &mut (dyn DeltaSink + Send),
    ) -> Result<ChatStreamOutput, BackendError> {
        self.inner
            .chat_stream(messages, tools, think, reasoning_role, sink)
            .await
    }

    async fn chat_json_stream(
        &self,
        messages: &Value,
        think: Option<bool>,
        reasoning_role: &str,
        sink: &mut (dyn DeltaSink + Send),
    ) -> Result<JsonStreamOutput, BackendError> {
        self.inner
            .chat_json_stream(messages, think, reasoning_role, sink)
            .await
    }
}

fn factory() -> ClientFactory {
    Arc::new(|| Arc::new(MockClient::new()) as Arc<dyn Backend>)
}

fn client() -> Arc<dyn Backend> {
    Arc::new(MockClient::new())
}

fn default_story_seed() -> Value {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = StoryStore::new(dir.path()).expect("open story store");
    store.default_seed()
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
        .block_on(future)
}

fn travel_session(destination_visited: bool) -> Session {
    let world = World::from_seed_with_dice_seed(&default_story_seed(), 20260717);
    let mut session = Session::with_world(client(), world, factory());
    let origin_id = session.world.world_canon.player_place_id.clone();
    let provenance = Provenance::by("test", "explicit travel fixture", 0);
    add_destination(&mut session, destination_visited, &provenance);

    add_direct_network(
        &mut session,
        &origin_id,
        SURFACE_NETWORK_ID,
        true,
        SURFACE_MINUTES,
        "surface",
        &provenance,
    );
    add_direct_network(
        &mut session,
        &origin_id,
        SEWER_NETWORK_ID,
        false,
        SEWER_MINUTES,
        "sewer",
        &provenance,
    );

    session
}

fn scripted_travel_session(
    response_for_origin: impl FnOnce(&str) -> Map<String, Value>,
) -> Session {
    scripted_travel_session_with_responses(|origin_id| {
        let response = response_for_origin(origin_id);
        vec![response.clone(), response]
    })
}

fn scripted_travel_session_with_responses(
    responses_for_origin: impl FnOnce(&str) -> Vec<Map<String, Value>>,
) -> Session {
    scripted_travel_session_with_responses_and_observer(responses_for_origin).0
}

fn scripted_travel_session_with_responses_and_observer(
    responses_for_origin: impl FnOnce(&str) -> Vec<Map<String, Value>>,
) -> (Session, Arc<Mutex<Vec<Value>>>) {
    let world = World::from_seed_with_dice_seed(&default_story_seed(), 20260717);
    let scripted_responses = Arc::new(responses_for_origin(&world.world_canon.player_place_id));
    let observed_messages = Arc::new(Mutex::new(Vec::new()));
    let factory_observer = Arc::clone(&observed_messages);
    let scripted_factory: ClientFactory = Arc::new(move || {
        Arc::new(ScriptedLocationBackend::new(
            scripted_responses.as_ref().clone(),
            Arc::clone(&factory_observer),
        )) as Arc<dyn Backend>
    });
    let mut session = Session::with_world(client(), world, scripted_factory);
    add_destination(
        &mut session,
        true,
        &Provenance::by("test", "generator travel fixture", 0),
    );
    (session, observed_messages)
}

fn add_destination(session: &mut Session, visited: bool, provenance: &Provenance) {
    let mut destination = Place {
        place_id: DESTINATION_ID.to_string(),
        name: "Лавка на дальней окраине".to_string(),
        kind: "shop".to_string(),
        default_description: "Знакомая лавка на дальней окраине города.".to_string(),
        provenance: provenance.clone(),
        ..Default::default()
    };
    if visited {
        destination.mark_visited();
    }
    session.world.world_canon.insert_place(destination);
}

fn add_direct_network(
    session: &mut Session,
    origin_id: &str,
    network_id: &str,
    default_for_normal_travel: bool,
    minutes: i64,
    id_prefix: &str,
    provenance: &Provenance,
) {
    let origin_anchor_id = format!("{id_prefix}_origin_anchor");
    let destination_anchor_id = format!("{id_prefix}_destination_anchor");

    let canon = &mut session.world.world_canon;
    canon.insert_travel_network(TravelNetwork {
        network_id: network_id.to_string(),
        scope_id: origin_id.to_string(),
        default_for_normal_travel,
        passable: true,
        provenance: provenance.clone(),
        ..Default::default()
    });
    canon.insert_travel_anchor(TravelAnchor {
        anchor_id: origin_anchor_id.clone(),
        network_id: network_id.to_string(),
        passable: true,
        provenance: provenance.clone(),
        ..Default::default()
    });
    canon.insert_travel_anchor(TravelAnchor {
        anchor_id: destination_anchor_id.clone(),
        network_id: network_id.to_string(),
        passable: true,
        provenance: provenance.clone(),
        ..Default::default()
    });
    canon.insert_travel_access(TravelAccess {
        access_id: format!("{id_prefix}_origin_access"),
        place_id: origin_id.to_string(),
        anchor_id: origin_anchor_id.clone(),
        passable: true,
        provenance: provenance.clone(),
        ..Default::default()
    });
    canon.insert_travel_access(TravelAccess {
        access_id: format!("{id_prefix}_destination_access"),
        place_id: DESTINATION_ID.to_string(),
        anchor_id: destination_anchor_id.clone(),
        passable: true,
        provenance: provenance.clone(),
        ..Default::default()
    });
    canon.insert_travel_link(TravelLink {
        link_id: format!("{id_prefix}_direct_link"),
        anchor_a: origin_anchor_id,
        anchor_b: destination_anchor_id,
        time_cost_minutes: minutes,
        risk: "none".to_string(),
        passable: true,
        provenance: provenance.clone(),
        ..Default::default()
    });
}

fn run_travel(session: &mut Session, network_id: Option<&str>) -> (Vec<gml_types::Event>, Value) {
    let mut args = json!({"destination_place_id": DESTINATION_ID});
    if let Some(network_id) = network_id {
        args["network_id"] = json!(network_id);
    }
    let (events, result) = block_on(run_tool_collect(session, "travel_to", &args));
    let payload = serde_json::from_str(&result.full).unwrap_or_else(|error| {
        panic!(
            "travel_to returned non-JSON payload ({error}): {}",
            result.full
        )
    });
    (events, payload)
}

fn direct_geography_response(
    origin_id: &str,
    scope_id: &str,
    network_id: &str,
    id_prefix: &str,
    minutes: i64,
) -> Map<String, Value> {
    let origin_anchor = format!("{id_prefix}_origin_anchor");
    let destination_anchor = format!("{id_prefix}_destination_anchor");
    serde_json::from_value(json!({
        "travel_geography": {
            "networks": [{
                "network_id": network_id,
                "scope_id": scope_id,
                "default_for_normal_travel": true,
                "passable": true,
                "blocked_by": ""
            }],
            "anchors": [
                {
                    "anchor_id": origin_anchor,
                    "network_id": network_id,
                    "passable": true,
                    "blocked_by": ""
                },
                {
                    "anchor_id": destination_anchor,
                    "network_id": network_id,
                    "passable": true,
                    "blocked_by": ""
                }
            ],
            "accesses": [
                {
                    "access_id": format!("{id_prefix}_origin_access"),
                    "place_id": origin_id,
                    "anchor_id": origin_anchor,
                    "passable": true,
                    "blocked_by": "",
                    "required_fact_ids": []
                },
                {
                    "access_id": format!("{id_prefix}_destination_access"),
                    "place_id": DESTINATION_ID,
                    "anchor_id": destination_anchor,
                    "passable": true,
                    "blocked_by": "",
                    "required_fact_ids": []
                }
            ],
            "links": [{
                "link_id": format!("{id_prefix}_direct_link"),
                "anchor_a": origin_anchor,
                "anchor_b": destination_anchor,
                "time_cost_minutes": minutes,
                "risk": "none",
                "passable": true,
                "blocked_by": "",
                "required_fact_ids": []
            }]
        }
    }))
    .expect("scripted direct travel geography is an object")
}

fn observed_generator_request(observed_messages: &Arc<Mutex<Vec<Value>>>, call: usize) -> Value {
    let observed = observed_messages.lock().expect("observed message lock");
    let content = observed[call]
        .as_array()
        .and_then(|messages| messages.last())
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .expect("current generator request content");
    let request_json = content
        .split_once("## Generation Request JSON\n")
        .map(|(_, tail)| tail)
        .and_then(|tail| tail.split_once("\n\nGenerate the structured location/situation now."))
        .map(|(request, _)| request)
        .expect("generation request JSON section");
    serde_json::from_str(request_json).expect("current generator request must be JSON")
}

#[test]
fn default_surface_route_moves_player_and_spends_its_exact_time() {
    let mut session = travel_session(true);
    let before_minutes = session.world.time.absolute_minutes;

    let (events, payload) = run_travel(&mut session, None);

    assert_eq!(payload["ok"], json!(true), "travel must succeed: {payload}");
    assert_eq!(payload["status"], json!("travelled"));
    assert_eq!(payload["destination_place_id"], json!(DESTINATION_ID));
    assert_eq!(payload["place_id"], json!(DESTINATION_ID));
    assert_eq!(payload["network_id"], json!(SURFACE_NETWORK_ID));
    assert_eq!(payload["total_time_minutes"], json!(SURFACE_MINUTES));
    assert_eq!(payload["elapsed_minutes"], json!(SURFACE_MINUTES));
    assert_eq!(session.world.world_canon.player_place_id, DESTINATION_ID);
    assert_eq!(
        session.world.time.absolute_minutes,
        before_minutes + SURFACE_MINUTES
    );
    assert_eq!(
        session.world.world_canon.clock_minutes, session.world.time.absolute_minutes,
        "visible and canonical clocks must remain aligned"
    );
    assert!(events.iter().any(|event| {
        event.kind == "time" && event.data["elapsed_minutes"] == json!(SURFACE_MINUTES)
    }));
}

#[test]
fn normal_travel_never_silently_chooses_the_shorter_non_default_sewer() {
    let mut session = travel_session(true);

    let (_events, payload) = run_travel(&mut session, None);

    assert_eq!(payload["ok"], json!(true), "travel must succeed: {payload}");
    assert_eq!(payload["network_id"], json!(SURFACE_NETWORK_ID));
    assert_eq!(payload["link_ids"], json!(["surface_direct_link"]));
    assert_eq!(payload["total_time_minutes"], json!(SURFACE_MINUTES));
    assert_ne!(payload["network_id"], json!(SEWER_NETWORK_ID));
    assert_ne!(payload["total_time_minutes"], json!(SEWER_MINUTES));
}

#[test]
fn explicitly_requested_sewer_route_is_selected() {
    let mut session = travel_session(true);
    let before_minutes = session.world.time.absolute_minutes;

    let (_events, payload) = run_travel(&mut session, Some(SEWER_NETWORK_ID));

    assert_eq!(payload["ok"], json!(true), "travel must succeed: {payload}");
    assert_eq!(payload["network_id"], json!(SEWER_NETWORK_ID));
    assert_eq!(payload["link_ids"], json!(["sewer_direct_link"]));
    assert_eq!(payload["total_time_minutes"], json!(SEWER_MINUTES));
    assert_eq!(payload["elapsed_minutes"], json!(SEWER_MINUTES));
    assert_eq!(
        session.world.time.absolute_minutes,
        before_minutes + SEWER_MINUTES
    );
}

#[test]
fn unvisited_destination_is_rejected_without_mutating_world_or_time() {
    let mut session = travel_session(false);
    let before_canon = session.world.world_canon.clone();
    let before_scene = session.world.scene.clone();
    let before_minutes = session.world.time.absolute_minutes;

    let (events, result) = block_on(run_tool_collect(
        &mut session,
        "travel_to",
        &json!({"destination_place_id": DESTINATION_ID}),
    ));

    assert!(
        result.model.contains("code: invalid_travel_destination"),
        "unvisited destination must be a clean tool error: {}",
        result.model
    );
    assert!(events.iter().any(|event| event.kind == "error"));
    assert!(!events.iter().any(|event| event.kind == "scene_update"));
    assert_eq!(session.world.world_canon, before_canon);
    assert_eq!(session.world.scene, before_scene);
    assert_eq!(session.world.time.absolute_minutes, before_minutes);
}

#[test]
fn completed_long_distance_travel_leaves_no_fake_local_transition() {
    let mut session = travel_session(true);
    let origin_id = session.world.world_canon.player_place_id.clone();
    let transitions_before = session.world.world_canon.transitions.clone();
    let origin_exits_before = session
        .world
        .world_canon
        .place(&origin_id)
        .expect("origin place")
        .transition_ids
        .clone();

    let (_events, payload) = run_travel(&mut session, None);

    assert_eq!(payload["ok"], json!(true), "travel must succeed: {payload}");
    assert_eq!(
        session.world.world_canon.transitions, transitions_before,
        "the temporary movement edge must not become local geography"
    );
    assert_eq!(
        session
            .world
            .world_canon
            .place(&origin_id)
            .expect("origin remains canonical")
            .transition_ids,
        origin_exits_before,
        "the origin must not retain the temporary edge id"
    );
    assert!(
        session
            .world
            .world_canon
            .transitions
            .values()
            .all(|transition| {
                !(transition.from_place == origin_id
                    && transition.to_place == DESTINATION_ID
                    && transition.kind == "travel_network")
            }),
        "a travel-network route is not a persistent local Transition"
    );
}

#[test]
fn missing_travel_graph_is_authored_committed_and_recorded_before_travel() {
    const AUTHORED_NETWORK_ID: &str = "authored_surface_routes";
    const AUTHORED_MINUTES: i64 = 24;
    let mut session = scripted_travel_session(|origin_id| {
        direct_geography_response(
            origin_id,
            origin_id,
            AUTHORED_NETWORK_ID,
            "authored",
            AUTHORED_MINUTES,
        )
    });
    assert!(session.world.world_canon.travel_networks.is_empty());
    let origin_id = session.world.world_canon.player_place_id.clone();
    let before_minutes = session.world.time.absolute_minutes;

    let (_events, payload) = run_travel(&mut session, None);

    assert_eq!(payload["ok"], json!(true), "travel must succeed: {payload}");
    assert_eq!(payload["status"], json!("travelled"));
    assert_eq!(payload["network_id"], json!(AUTHORED_NETWORK_ID));
    assert_eq!(payload["link_ids"], json!(["authored_direct_link"]));
    assert_eq!(payload["total_time_minutes"], json!(AUTHORED_MINUTES));
    assert_eq!(payload["elapsed_minutes"], json!(AUTHORED_MINUTES));
    assert_eq!(
        payload["generated_geography"]["networks"]["added"],
        json!(1)
    );
    assert_eq!(payload["generated_geography"]["anchors"]["added"], json!(2));
    assert_eq!(
        payload["generated_geography"]["accesses"]["added"],
        json!(2)
    );
    assert_eq!(payload["generated_geography"]["links"]["added"], json!(1));
    assert!(session
        .world
        .world_canon
        .travel_networks
        .contains_key(AUTHORED_NETWORK_ID));
    assert!(session
        .world
        .world_canon
        .travel_links
        .contains_key("authored_direct_link"));
    assert_eq!(session.world.world_canon.player_place_id, DESTINATION_ID);
    assert_eq!(
        session.world.time.absolute_minutes,
        before_minutes + AUTHORED_MINUTES
    );

    assert_eq!(session.location_generator_messages.len(), 2);
    let request_history = session.location_generator_messages[0]["content"]
        .as_str()
        .expect("request history content");
    assert!(request_history.contains("\"purpose\":\"travel_route\""));
    assert!(request_history.contains(&origin_id));
    assert!(request_history.contains(DESTINATION_ID));
}

#[test]
fn invalid_invented_scope_is_repaired_once_using_only_supplied_scope_ids() {
    const REPAIRED_NETWORK_ID: &str = "repaired_surface_routes";
    const REPAIRED_MINUTES: i64 = 29;
    let (mut session, observed_messages) =
        scripted_travel_session_with_responses_and_observer(|origin_id| {
            vec![
                direct_geography_response(
                    origin_id,
                    "town",
                    "invalid_scope_network",
                    "invalid_scope",
                    11,
                ),
                direct_geography_response(
                    origin_id,
                    origin_id,
                    REPAIRED_NETWORK_ID,
                    "repaired",
                    REPAIRED_MINUTES,
                ),
            ]
        });
    let origin_id = session.world.world_canon.player_place_id.clone();
    let before_minutes = session.world.time.absolute_minutes;

    let (_events, payload) = run_travel(&mut session, None);

    assert_eq!(payload["ok"], json!(true), "repair must succeed: {payload}");
    assert_eq!(payload["network_id"], json!(REPAIRED_NETWORK_ID));
    assert_eq!(payload["total_time_minutes"], json!(REPAIRED_MINUTES));
    assert_eq!(
        session.world.time.absolute_minutes,
        before_minutes + REPAIRED_MINUTES
    );
    assert!(!session
        .world
        .world_canon
        .travel_networks
        .contains_key("invalid_scope_network"));
    assert!(session
        .world
        .world_canon
        .travel_networks
        .contains_key(REPAIRED_NETWORK_ID));

    assert_eq!(session.location_generator_messages.len(), 4);
    let initial_request = observed_generator_request(&observed_messages, 0);
    let allowed_scope_ids = initial_request["allowed_scope_ids"]
        .as_array()
        .expect("allowed scope ids");
    assert!(allowed_scope_ids.contains(&json!(origin_id)));
    assert!(allowed_scope_ids.contains(&json!(DESTINATION_ID)));
    assert!(!allowed_scope_ids.contains(&json!("town")));

    let repair_request = observed_generator_request(&observed_messages, 1);
    assert_eq!(
        repair_request["allowed_scope_ids"],
        initial_request["allowed_scope_ids"]
    );
    assert_eq!(repair_request["repair"]["attempt"], json!(1));
    assert_eq!(
        repair_request["repair"]["validation_error"],
        json!("travel network 'invalid_scope_network' references unknown scope 'town'")
    );
}

#[test]
fn free_form_generator_refusal_is_not_canonical_and_preserves_state() {
    let (mut session, observed_messages) =
        scripted_travel_session_with_responses_and_observer(|_origin_id| {
            let response: Map<String, Value> = serde_json::from_value(json!({
                "travel_unavailable_reason": "No truthful route is established in canon"
            }))
            .expect("scripted unavailable result is an object");
            vec![response.clone(), response]
        });
    let before_canon = session.world.world_canon.clone();
    let before_scene = session.world.scene.clone();
    let before_minutes = session.world.time.absolute_minutes;

    let (events, result) = block_on(run_tool_collect(
        &mut session,
        "travel_to",
        &json!({"destination_place_id": DESTINATION_ID}),
    ));

    assert!(
        result.model.contains("code: travel_route_unavailable"),
        "a response without explicit geography must be a clean tool error: {}",
        result.model
    );
    assert!(result.model.contains("no travel_geography object"));
    assert!(
        result.model.contains("never substitute move_player"),
        "travel errors must remind the GM not to walk the local-exit chain: {}",
        result.model
    );
    assert!(events.iter().any(|event| event.kind == "error"));
    assert!(!events.iter().any(|event| event.kind == "scene_update"));
    assert_eq!(session.world.world_canon, before_canon);
    assert_eq!(session.world.scene, before_scene);
    assert_eq!(session.world.time.absolute_minutes, before_minutes);
    assert_eq!(
        session.location_generator_messages.len(),
        4,
        "one initial generation plus exactly one repair attempt"
    );
    let repair_request = observed_generator_request(&observed_messages, 1);
    assert!(repair_request["repair"]["validation_error"]
        .as_str()
        .is_some_and(|error| error.contains("no travel_geography object")));
}

#[test]
fn unknown_explicit_network_is_rejected_without_creator_authoring() {
    let mut session = scripted_travel_session(|origin_id| {
        serde_json::from_value(json!({
            "travel_geography": {
                "networks": [{
                    "network_id": "must_not_be_authored",
                    "scope_id": origin_id,
                    "default_for_normal_travel": false,
                    "passable": true
                }],
                "anchors": [],
                "accesses": [],
                "links": []
            }
        }))
        .expect("unused scripted response is an object")
    });
    let before_canon = session.world.world_canon.clone();
    let before_minutes = session.world.time.absolute_minutes;

    let (events, result) = block_on(run_tool_collect(
        &mut session,
        "travel_to",
        &json!({
            "destination_place_id": DESTINATION_ID,
            "network_id": "unknown_explicit_network"
        }),
    ));

    assert!(result.model.contains("code: invalid_travel_network"));
    assert!(events.iter().any(|event| event.kind == "error"));
    assert!(session.location_generator_messages.is_empty());
    assert_eq!(session.world.world_canon, before_canon);
    assert_eq!(session.world.time.absolute_minutes, before_minutes);
}

#[test]
fn travel_creator_receives_exact_district_context_for_both_endpoints() {
    let (mut session, observed_messages) =
        scripted_travel_session_with_responses_and_observer(|_origin_id| {
            vec![Map::new(), Map::new()]
        });
    let origin_id = session.world.world_canon.player_place_id.clone();
    let region_id = "district_test_region";
    let settlement_id = "district_test_city";
    let district_id = "district_test_market_ward";
    session.world.world_canon.regions.insert(
        region_id.to_string(),
        Region {
            region_id: region_id.to_string(),
            name: "Test Region".to_string(),
            ..Default::default()
        },
    );
    session.world.world_canon.settlements.insert(
        settlement_id.to_string(),
        Settlement {
            settlement_id: settlement_id.to_string(),
            name: "Test City".to_string(),
            region_id: region_id.to_string(),
            place_ids: vec![origin_id.clone(), DESTINATION_ID.to_string()],
            ..Default::default()
        },
    );

    let mut origin = session
        .world
        .world_canon
        .place(&origin_id)
        .expect("origin")
        .clone();
    origin.parent = district_id.to_string();
    origin.region_id = region_id.to_string();
    origin.district_id = district_id.to_string();
    session.world.world_canon.insert_place(origin.clone());

    let mut destination = session
        .world
        .world_canon
        .place(DESTINATION_ID)
        .expect("destination")
        .clone();
    destination.parent = district_id.to_string();
    destination.region_id = region_id.to_string();
    destination.district_id = district_id.to_string();
    session.world.world_canon.insert_place(destination);
    session
        .world
        .world_canon
        .insert_district(District {
            district_id: district_id.to_string(),
            name: "Market Ward".to_string(),
            settlement_id: settlement_id.to_string(),
            region_id: region_id.to_string(),
            place_ids: vec![origin_id.clone(), DESTINATION_ID.to_string()],
            ..Default::default()
        })
        .expect("explicit district fixture");

    let (_events, result) = block_on(run_tool_collect(
        &mut session,
        "travel_to",
        &json!({"destination_place_id": DESTINATION_ID}),
    ));
    assert!(result.model.contains("code: travel_route_unavailable"));

    let request = observed_generator_request(&observed_messages, 0);
    assert_eq!(request["origin"]["district_id"], json!(origin.district_id));
    assert_eq!(
        request["origin"]["district"]["district_id"],
        request["origin"]["district_id"]
    );
    assert_eq!(
        request["destination"]["district_id"],
        request["origin"]["district_id"]
    );
    assert_eq!(
        request["preferred_scope_id"],
        request["origin"]["district_id"]
    );
    assert!(request["allowed_scope_ids"]
        .as_array()
        .expect("allowed scopes")
        .contains(&request["origin"]["district_id"]));
}
