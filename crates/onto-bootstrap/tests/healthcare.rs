//! Named Zhang 2026 instructional healthcare teaching cases.
//!
//! Each test fails if the mechanism is deleted or stubbed (missing evidence
//! Always-Allow, contraindication Always-Allow, confirm without `DecisionRecord`).

use onto::{dispatch, Actor, AgentTier, AsOf, Engine, KeyKind, Query, Session, Verdict};
use onto_bootstrap::{define_healthcare, install, install_healthcare};
use serde_json::json;

fn modeller() -> Session {
    Session::new(Actor::builder("human.modeler", &["modeler"]), "test")
}
fn operator() -> Session {
    Session::new(
        Actor::consumer("ops.maya", &["operator"], AgentTier::T2),
        "test",
    )
}
fn supervisor() -> Session {
    Session::new(
        Actor::consumer("ops.chen", &["supervisor", "operator"], AgentTier::T3),
        "test",
    )
}

fn order_params(patient: &str, observation: &str, order: &str) -> serde_json::Value {
    json!({
        "patient": patient,
        "observation": observation,
        "order": order,
        "need_clearance": 1,
        "rationale": "pedido instrucional"
    })
}

fn search_type(engine: &Engine, type_name: &str) -> Vec<onto::ObjectView> {
    engine
        .search_objects(
            &operator(),
            Query {
                type_name: Some(type_name.into()),
                ..Query::default()
            },
        )
        .unwrap()
}

fn forbidden_needles() -> [String; 3] {
    [
        ["fn", "dose"].join(" "),
        ["mg", "kg"].join("/"),
        ["ti", "trate"].concat(),
    ]
}

#[test]
fn does_omit_prescribed_quantity_calculator() {
    let root = env!("CARGO_MANIFEST_DIR");
    let src = std::fs::read_to_string(format!("{root}/src/healthcare.rs")).unwrap();
    let tests = std::fs::read_to_string(format!("{root}/tests/healthcare.rs")).unwrap();
    for (label, hay) in [("src", src.as_str()), ("tests", tests.as_str())] {
        for needle in forbidden_needles() {
            assert!(
                !hay.contains(&needle),
                "{label} must not contain {needle:?}"
            );
        }
    }
}

#[test]
fn does_review_and_not_allow_if_observation_is_missing() {
    let engine = Engine::memory().unwrap();
    let ids = install_healthcare(&engine).unwrap();
    let view = engine
        .get_object(&operator(), &ids.observation_missing, AsOf::Current)
        .unwrap();
    assert!(
        !view.properties.contains_key("last_reading_at"),
        "seed must omit last_reading_at"
    );
    assert!(
        view.missing.iter().any(|n| n == "last_reading_at"),
        "Complete fail: last_reading_at listed as missing, got {:?}",
        view.missing
    );
    let evidence = engine
        .list_missing_evidence(&operator(), &ids.observation_missing)
        .unwrap();
    assert!(
        evidence["missing"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n == "last_reading_at"),
        "missing evidence must surface last_reading_at, got {evidence}"
    );

    let out = engine
        .submit_action(
            &operator(),
            "propose_order",
            order_params(&ids.patient, &ids.observation_missing, &ids.order),
        )
        .unwrap();
    assert_eq!(
        out.verdict,
        Verdict::Review,
        "missing observation must Review, not Allow"
    );
    assert_ne!(out.verdict, Verdict::Allow);
    assert!(
        out.inbox_id.is_none(),
        "Review discards the stage; inbox is the complete-evidence Propose path"
    );
    assert_eq!(
        out.alternative.as_deref(),
        Some("request_observation"),
        "Review must name the replacement proposal"
    );
    assert!(
        out.guard_results.iter().any(|g| {
            g.verdict == Verdict::Review && (g.name == "complete" || g.name == "freshness")
        }),
        "missing reading must Review on complete/freshness, got {:?}",
        out.guard_results
    );
    assert!(
        search_type(&engine, "DecisionRecord").is_empty(),
        "Review must not write a DecisionRecord"
    );

    let alt = engine
        .submit_action(
            &operator(),
            "request_observation",
            json!({ "observation": ids.observation_missing }),
        )
        .unwrap();
    assert_eq!(alt.verdict, Verdict::Allow);
    assert!(
        !search_type(&engine, "ObservationRequest").is_empty(),
        "replacement proposal must create an ObservationRequest, not a stub Allow"
    );
}

#[test]
fn does_review_and_request_observation_if_observation_is_stale() {
    let engine = Engine::memory().unwrap();
    let ids = install_healthcare(&engine).unwrap();
    engine.set_clock(engine.now() + 10_000);
    let evidence = engine
        .list_missing_evidence(&operator(), &ids.observation_complete)
        .unwrap();
    assert!(
        evidence["stale"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n == "last_reading_at"),
        "stale last_reading_at must surface as missing evidence, got {evidence}"
    );

    let out = engine
        .submit_action(
            &operator(),
            "propose_order",
            order_params(&ids.patient, &ids.observation_complete, &ids.order),
        )
        .unwrap();
    assert_eq!(
        out.verdict,
        Verdict::Review,
        "stale observation must Review, not Allow"
    );
    assert_ne!(out.verdict, Verdict::Allow);
    assert!(
        out.inbox_id.is_none(),
        "Review discards the stage; inbox is the complete-evidence Propose path"
    );
    assert_eq!(
        out.alternative.as_deref(),
        Some("request_observation"),
        "Review must name the replacement proposal"
    );
    assert!(
        out.guard_results
            .iter()
            .any(|g| g.name == "freshness" && g.verdict == Verdict::Review),
        "stale path must be the freshness guard, got {:?}",
        out.guard_results
    );
    assert!(
        search_type(&engine, "DecisionRecord").is_empty(),
        "Review must not write a DecisionRecord"
    );
}

#[test]
fn does_open_inbox_if_order_evidence_is_complete() {
    let engine = Engine::memory().unwrap();
    let ids = install_healthcare(&engine).unwrap();
    let proposed = engine
        .submit_action(
            &operator(),
            "propose_order",
            order_params(&ids.patient, &ids.observation_complete, &ids.order),
        )
        .unwrap();
    assert_eq!(proposed.verdict, Verdict::Allow);
    let inbox = proposed
        .inbox_id
        .expect("complete evidence propose_order goes to inbox");
    let items = engine.list_inbox(&supervisor()).unwrap();
    assert!(
        items
            .iter()
            .any(|i| i.id == inbox && i.action_name == "propose_order"),
        "inbox must hold propose_order"
    );
    let order = engine
        .get_object(&operator(), &ids.order, AsOf::Current)
        .unwrap();
    assert!(
        !order.properties.contains_key("status"),
        "Propose must not confirm the order"
    );
}

#[test]
fn does_record_decision_record_if_order_is_confirmed() {
    let engine = Engine::memory().unwrap();
    let ids = install_healthcare(&engine).unwrap();
    let proposed = engine
        .submit_action(
            &operator(),
            "propose_order",
            order_params(&ids.patient, &ids.observation_complete, &ids.order),
        )
        .unwrap();
    let confirmed = engine
        .confirm_action(&supervisor(), proposed.inbox_id.as_deref().unwrap())
        .unwrap();
    assert_eq!(confirmed.verdict, Verdict::Allow);
    let rec_id = confirmed
        .decision_record_id
        .clone()
        .expect("confirm seals a write-path DecisionRecord");
    let rec = engine.get_decision_record(&operator(), &rec_id).unwrap();
    assert_eq!(rec.verdict, Verdict::Allow);
    assert_eq!(rec.action_name, "approve_order");

    let order = engine
        .get_object(&operator(), &ids.order, AsOf::Current)
        .unwrap();
    assert_eq!(order.properties["status"].value, json!("confirmed"));
    assert_eq!(
        order.properties["status"].source,
        onto::PropertySource::ActionWritten
    );

    let recorded = search_type(&engine, "DecisionRecord");
    assert!(
        !recorded.is_empty(),
        "confirm must create a DecisionRecord object through the write path"
    );
    assert_eq!(recorded[0].properties["status"].value, json!("recorded"));
    assert_eq!(
        recorded[0].properties["rationale"].value,
        json!("pedido instrucional")
    );
}

#[test]
fn does_deny_if_contraindication_flag_is_set() {
    let engine = Engine::memory().unwrap();
    let ids = install_healthcare(&engine).unwrap();
    let order_before = engine
        .get_object(&operator(), &ids.order, AsOf::Current)
        .unwrap();
    let out = engine
        .submit_action(
            &operator(),
            "propose_order",
            order_params(&ids.patient, &ids.observation_flagged, &ids.order),
        )
        .unwrap();
    assert_eq!(out.verdict, Verdict::Deny);
    assert!(out.inbox_id.is_none());
    assert!(out.created_ids.is_empty());
    assert!(
        out.guard_results
            .iter()
            .any(|g| g.name == "permit_limit" && g.verdict == Verdict::Deny),
        "coded flag Deny must come from the permit_limit guard, got {:?}",
        out.guard_results
    );
    let rec = engine
        .get_decision_record(&operator(), out.decision_record_id.as_deref().unwrap())
        .unwrap();
    assert_eq!(rec.verdict, Verdict::Deny);
    assert_eq!(rec.action_name, "propose_order");
    let order_after = engine
        .get_object(&operator(), &ids.order, AsOf::Current)
        .unwrap();
    assert_eq!(
        serde_json::to_value(&order_before.properties).unwrap(),
        serde_json::to_value(&order_after.properties).unwrap(),
        "Deny must not write the order"
    );
    assert!(search_type(&engine, "DecisionRecord").is_empty());
}

#[test]
fn does_deny_consumer_create_object_type() {
    let engine = Engine::memory().unwrap();
    install_healthcare(&engine).unwrap();
    let denied = dispatch(
        &engine,
        &operator(),
        "create_object_type",
        json!({
            "branch": "x",
            "spec": {
                "name": "Clinic",
                "typology": "entity",
                "title_prop": "name",
                "interfaces": [],
                "properties": []
            }
        }),
    );
    assert!(denied.is_err(), "consumer cannot mutate schema");
    assert_ne!(KeyKind::Builder, KeyKind::Consumer);
}

#[test]
fn does_leave_wastewater_unchanged_if_healthcare_is_unmerged() {
    let engine = Engine::memory().unwrap();
    install(&engine).unwrap();
    let tanks_before = search_type(&engine, "AerationTank").len();
    assert!(tanks_before > 0);

    let b = engine.open_branch(&modeller(), "healthcare-v1").unwrap();
    define_healthcare(&engine, &modeller(), &b).unwrap();

    let schema = engine.get_schema(&operator(), None).unwrap();
    assert!(
        !schema.object_types.iter().any(|t| t.name == "Patient"),
        "unmerged healthcare must not appear on main"
    );
    assert!(schema.object_types.iter().any(|t| t.name == "AerationTank"));
    let tanks_after = search_type(&engine, "AerationTank").len();
    assert_eq!(tanks_before, tanks_after);
    assert!(search_type(&engine, "Patient").is_empty());
    let tools = engine.list_tools(&operator()).unwrap();
    assert!(
        !tools.iter().any(|t| t.name == "action.propose_order"),
        "unmerged propose_order must not project"
    );
    assert!(tools
        .iter()
        .any(|t| t.name == "action.propose_setpoint_change"));
}

#[test]
fn does_keep_both_if_healthcare_merges_after_wastewater() {
    let engine = Engine::memory().unwrap();
    install(&engine).unwrap();
    install_healthcare(&engine).unwrap();
    assert!(!search_type(&engine, "AerationTank").is_empty());
    assert!(!search_type(&engine, "Patient").is_empty());
    let schema = engine.get_schema(&operator(), None).unwrap();
    assert!(schema.object_types.iter().any(|t| t.name == "AerationTank"));
    assert!(schema.object_types.iter().any(|t| t.name == "Patient"));
    assert!(schema.object_types.iter().any(|t| t.name == "Order"));
    assert!(schema.object_types.iter().any(|t| t.name == "Observation"));
    assert!(schema
        .object_types
        .iter()
        .any(|t| t.name == "DecisionRecord"));
}
