//! Remaining GPT kernel findings. Names fail if the hole returns.

use onto::{
    dispatch, drain_declared, Actor, AgentTier, AsOf, EffectStatus, Engine, ExecutionMode,
    IngestRecord, ObjectTypeSpec, PropertySource, PropertySpec, Session, Typology, Verdict,
};
use onto_bootstrap::{install, install_healthcare, install_highered};
use serde_json::json;

fn modeller() -> Session {
    Session::new(Actor::builder("human.modeler", &["modeler"]), "gpt")
}
fn reviewer() -> Session {
    Session::new(Actor::builder("human.reviewer", &["reviewer"]), "gpt")
}
fn operator() -> Session {
    Session::new(
        Actor::consumer("ops.maya", &["operator"], AgentTier::T2),
        "gpt",
    )
}
fn supervisor() -> Session {
    Session::new(
        Actor::consumer("ops.chen", &["supervisor", "operator"], AgentTier::T3),
        "gpt",
    )
}
fn automator() -> Session {
    Session::new(
        Actor::consumer("ops.auto", &["operator", "supervisor"], AgentTier::T4),
        "gpt",
    )
}

fn setpoint(ids: &onto_bootstrap::WastewaterIds, target: f64) -> serde_json::Value {
    json!({
        "tank": ids.tank1,
        "sensor": ids.sensor1,
        "permit": ids.permit,
        "target_do": target,
        "rationale": "gpt-close"
    })
}

#[test]
fn does_reject_funnel_if_field_is_undeclared() {
    let engine = Engine::memory().unwrap();
    install(&engine).unwrap();
    let err = engine
        .funnel_ingest(
            &operator(),
            vec![IngestRecord {
                type_name: "LabMeasurement".into(),
                id: Some("lab-undeclared".into()),
                properties: [
                    ("name".into(), json!("lab")),
                    ("value".into(), json!(1.0)),
                    ("secret".into(), json!("hole")),
                ]
                .into_iter()
                .collect(),
                as_of: Some(engine.now().to_string()),
                provenance: Some("test".into()),
            }],
        )
        .unwrap_err();
    assert!(
        matches!(err, onto::OntoError::Invalid(msg) if msg.contains("undeclared")),
        "{err}"
    );
}

#[test]
fn does_reject_funnel_if_value_type_is_wrong() {
    let engine = Engine::memory().unwrap();
    install(&engine).unwrap();
    let err = engine
        .funnel_ingest(
            &operator(),
            vec![IngestRecord {
                type_name: "LabMeasurement".into(),
                id: Some("lab-type".into()),
                properties: [
                    ("name".into(), json!("lab")),
                    ("value".into(), json!("not-a-number")),
                    ("rationale".into(), json!("typed")),
                ]
                .into_iter()
                .collect(),
                as_of: Some(engine.now().to_string()),
                provenance: Some("test".into()),
            }],
        )
        .unwrap_err();
    assert!(
        matches!(err, onto::OntoError::Invalid(msg) if msg.contains("number")),
        "{err}"
    );
}

#[test]
fn does_read_object_if_as_of_is_recorded() {
    let engine = Engine::memory().unwrap();
    let ids = install(&engine).unwrap();
    let t = engine.now();
    engine.set_clock(t + 50);
    engine
        .funnel_ingest(
            &operator(),
            vec![IngestRecord {
                type_name: "DO_Sensor".into(),
                id: Some(ids.sensor1.clone()),
                properties: [("last_reading_at".into(), json!(42))]
                    .into_iter()
                    .collect(),
                as_of: Some((t + 50).to_string()),
                provenance: Some("later".into()),
            }],
        )
        .unwrap();
    let known = engine
        .get_object(&operator(), &ids.sensor1, AsOf::Recorded(t))
        .unwrap();
    assert_ne!(
        known.properties["last_reading_at"].value,
        json!(42),
        "recorded-time query must not silently return the later write"
    );
    let via_dispatch = dispatch(
        &engine,
        &operator(),
        "get_object",
        json!({ "id": ids.sensor1, "recorded": t }),
    )
    .unwrap();
    assert_eq!(
        via_dispatch["properties"]["last_reading_at"]["value"],
        known.properties["last_reading_at"].value
    );
}

#[test]
fn does_correct_interval_if_older_effective_arrives_at_150_after_200() {
    let engine = Engine::memory().unwrap();
    let ids = install(&engine).unwrap();
    let t = engine.now();
    engine
        .funnel_ingest(
            &operator(),
            vec![IngestRecord {
                type_name: "DO_Sensor".into(),
                id: Some(ids.sensor1.clone()),
                properties: [("last_reading_at".into(), json!(200))]
                    .into_iter()
                    .collect(),
                as_of: Some((t + 200).to_string()),
                provenance: Some("current-200".into()),
            }],
        )
        .unwrap();
    engine.set_clock(t + 300);
    engine
        .funnel_ingest(
            &operator(),
            vec![IngestRecord {
                type_name: "DO_Sensor".into(),
                id: Some(ids.sensor1.clone()),
                properties: [("last_reading_at".into(), json!(150))]
                    .into_iter()
                    .collect(),
                as_of: Some((t + 150).to_string()),
                provenance: Some("late-150".into()),
            }],
        )
        .unwrap();
    let current = engine
        .get_object(&operator(), &ids.sensor1, AsOf::Current)
        .unwrap();
    assert_eq!(
        current.properties["last_reading_at"].value,
        json!(200),
        "older event at 150 must not become current after 200"
    );
    let mid = engine
        .get_object(&operator(), &ids.sensor1, AsOf::Valid(t + 175))
        .unwrap();
    assert_eq!(
        mid.properties["last_reading_at"].value,
        json!(150),
        "valid-time must reconstruct the late interval"
    );
}

#[test]
fn does_keep_closed_link_if_occupies_is_released() {
    let engine = Engine::memory().unwrap();
    let ids = install_highered(&engine).unwrap();
    let proposed = engine
        .submit_action(
            &operator(),
            "propose_enroll",
            json!({
                "student": ids.ana,
                "section": ids.section,
                "seat": ids.seat,
                "claim": 1,
                "enrolled": 0
            }),
        )
        .unwrap();
    let confirmed = engine
        .confirm_action(&supervisor(), proposed.inbox_id.as_deref().unwrap())
        .unwrap();
    assert_eq!(confirmed.verdict, Verdict::Allow);
    engine
        .compensate_action(
            &supervisor(),
            confirmed.decision_record_id.as_deref().unwrap(),
            json!({}),
        )
        .unwrap();
    let live = engine
        .traverse_links(&operator(), &ids.ana, "occupies")
        .unwrap();
    assert!(live.is_empty(), "live occupies must be empty");
    let closed = engine.list_closed_links(&operator()).unwrap();
    assert!(
        closed
            .iter()
            .any(|c| c.type_name == "occupies" && c.from_id == ids.ana && c.to_id == ids.seat),
        "{closed:?}"
    );
}

#[test]
fn does_deny_release_if_occupant_is_not_the_student() {
    let engine = Engine::memory().unwrap();
    let ids = install_highered(&engine).unwrap();
    let proposed = engine
        .submit_action(
            &operator(),
            "propose_enroll",
            json!({
                "student": ids.ana,
                "section": ids.section,
                "seat": ids.seat,
                "claim": 1,
                "enrolled": 0
            }),
        )
        .unwrap();
    engine
        .confirm_action(&supervisor(), proposed.inbox_id.as_deref().unwrap())
        .unwrap();
    let out = engine
        .submit_action(
            &supervisor(),
            "release_seat",
            json!({
                "student": ids.bruno,
                "section": ids.section,
                "seat": ids.seat,
                "claim": 1,
                "enrolled": 0
            }),
        )
        .unwrap();
    assert_eq!(out.verdict, Verdict::Deny, "{}", out.reason);
    let live = engine
        .traverse_links(&operator(), &ids.ana, "occupies")
        .unwrap();
    assert_eq!(
        live.len(),
        1,
        "wrong-student release must not close occupies"
    );
}

#[test]
fn does_deny_override_if_source_action_has_no_override() {
    let engine = Engine::memory().unwrap();
    install(&engine).unwrap();
    let branch = engine.open_branch(&modeller(), "no-override").unwrap();
    engine
        .create_action_type(
            &modeller(),
            &branch,
            onto::ActionTypeSpec {
                name: "propose_bare".into(),
                mode: ExecutionMode::Propose,
                parameters: vec![],
                guards: json!([]),
                required_roles: vec!["operator".into()],
                required_tier: AgentTier::T2,
                effects: json!([]),
                compensation: None,
                side_effects: json!({}),
                on_review: None,
                interfaces: vec!["Reviewable".into()],
            },
        )
        .unwrap();
    let proposal = engine.submit_proposal(&modeller(), &branch).unwrap();
    engine
        .review_proposal(&reviewer(), &proposal, true)
        .unwrap();
    engine.merge_to_main(&reviewer(), &proposal).unwrap();
    let out = engine
        .submit_action(&operator(), "propose_bare", json!({}))
        .unwrap();
    let err = engine
        .override_action(
            &supervisor(),
            out.inbox_id.as_deref().unwrap(),
            "ops",
            "no card",
        )
        .unwrap_err();
    assert!(
        matches!(err, onto::OntoError::Invalid(msg) if msg.contains("override")),
        "{err}"
    );
}

#[test]
fn does_deny_t4_submit_if_bound_is_empty() {
    let engine = Engine::memory().unwrap();
    let ids = install(&engine).unwrap();
    let err = engine
        .submit_action(
            &automator(),
            "request_sensor_calibration",
            json!({ "sensor": ids.sensor1 }),
        )
        .unwrap_err();
    assert!(
        matches!(err, onto::OntoError::Denied(msg) if msg.contains("auto_action")),
        "{err}"
    );
}

#[test]
fn does_invalidate_pin_if_schema_merges_after_propose() {
    let engine = Engine::memory().unwrap();
    let ids = install(&engine).unwrap();
    let proposed = engine
        .submit_action(&operator(), "propose_setpoint_change", setpoint(&ids, 2.2))
        .unwrap();
    let branch = engine.open_branch(&modeller(), "pin-closure").unwrap();
    engine
        .create_object_type(
            &modeller(),
            &branch,
            ObjectTypeSpec {
                name: "PinProbe".into(),
                typology: Typology::Entity,
                title_prop: None,
                interfaces: vec![],
                freshness_budget_secs: None,
                properties: vec![PropertySpec {
                    name: "name".into(),
                    value_type: "Text".into(),
                    source: PropertySource::Mapped,
                    nullable: false,
                    function: None,
                }],
            },
        )
        .unwrap();
    let proposal = engine.submit_proposal(&modeller(), &branch).unwrap();
    engine
        .review_proposal(&reviewer(), &proposal, true)
        .unwrap();
    engine.merge_to_main(&reviewer(), &proposal).unwrap();
    let err = engine
        .confirm_action(&supervisor(), proposed.inbox_id.as_deref().unwrap())
        .unwrap_err();
    assert!(matches!(err, onto::OntoError::Conflict(_)), "{err}");
}

#[test]
fn does_ack_declared_if_host_drains() {
    let engine = Engine::memory().unwrap();
    let ids = install(&engine).unwrap();
    let proposed = engine
        .submit_action(&operator(), "propose_setpoint_change", setpoint(&ids, 2.4))
        .unwrap();
    let confirmed = engine
        .confirm_action(&supervisor(), proposed.inbox_id.as_deref().unwrap())
        .unwrap();
    let rec = confirmed.decision_record_id.expect("decision");
    let before = engine
        .list_effect_intentions(&supervisor(), Some(EffectStatus::Declared))
        .unwrap();
    assert!(before.iter().any(|e| e.decision_record_id == rec));
    let acked = drain_declared(&engine, &supervisor()).unwrap();
    assert!(acked.contains(&rec), "{acked:?}");
    let leftover = engine
        .list_effect_intentions(&supervisor(), Some(EffectStatus::Declared))
        .unwrap();
    assert!(!leftover.iter().any(|e| e.decision_record_id == rec));
}

#[test]
fn does_dispatch_each_listed_tool() {
    let engine = Engine::memory().unwrap();
    install(&engine).unwrap();
    for session in [operator(), modeller()] {
        for tool in engine.list_tools(&session).unwrap() {
            match dispatch(&engine, &session, &tool.name, json!({})) {
                Ok(_) => {}
                Err(onto::OntoError::NotFound(msg)) if msg.contains("unknown") => {
                    panic!("{} listed but unknown to dispatch: {msg}", tool.name);
                }
                Err(_) => {}
            }
        }
    }
}

#[test]
fn does_mark_guard_reads_as_delegated() {
    let engine = Engine::memory().unwrap();
    let ids = install(&engine).unwrap();
    let proposed = engine
        .submit_action(&operator(), "propose_setpoint_change", setpoint(&ids, 2.1))
        .unwrap();
    let rec = engine
        .get_decision_record(&operator(), proposed.decision_record_id.as_deref().unwrap())
        .unwrap();
    let objects = rec.data_snapshot["objects"].as_array().expect("objects");
    let sensor = objects
        .iter()
        .find(|o| o["id"] == ids.sensor1)
        .expect("sensor");
    assert_eq!(
        sensor["delegated"],
        json!(true),
        "guard load of the sensor must be delegated"
    );
}

#[test]
fn does_deny_order_if_observation_is_not_linked_to_patient() {
    let engine = Engine::memory().unwrap();
    let ids = install_healthcare(&engine).unwrap();
    engine
        .funnel_ingest(
            &operator(),
            vec![IngestRecord {
                type_name: "Patient".into(),
                id: Some("patient-unbound".into()),
                properties: [("name".into(), json!("Outro"))].into_iter().collect(),
                as_of: Some(engine.now().to_string()),
                provenance: Some("test".into()),
            }],
        )
        .unwrap();
    let out = engine
        .submit_action(
            &operator(),
            "propose_order",
            json!({
                "patient": "patient-unbound",
                "observation": ids.observation_complete,
                "order": ids.order,
                "need_clearance": 1,
                "rationale": "unbound"
            }),
        )
        .unwrap();
    assert_eq!(out.verdict, Verdict::Deny, "{}", out.reason);
    assert!(out.reason.contains("linked"), "{}", out.reason);
}

#[test]
fn does_keep_override_named_by_source_action() {
    let engine = Engine::memory().unwrap();
    let ids = install(&engine).unwrap();
    let proposed = engine
        .submit_action(&operator(), "propose_setpoint_change", setpoint(&ids, 2.0))
        .unwrap();
    let over = engine
        .override_action(
            &supervisor(),
            proposed.inbox_id.as_deref().unwrap(),
            "ops",
            "named",
        )
        .unwrap();
    assert_eq!(over.verdict, Verdict::Allow);
    let rec = engine
        .get_decision_record(&operator(), over.decision_record_id.as_deref().unwrap())
        .unwrap();
    assert_eq!(rec.action_name, "override_setpoint");
}
