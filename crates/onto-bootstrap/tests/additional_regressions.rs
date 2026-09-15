//! Proposed native regression tests for d4ee79e9. NOT COMPILED OR RUN in this review.
//!
//! Copy to crates/onto-bootstrap/tests/additional_regressions.rs and run:
//! cargo test --locked -p onto-bootstrap --test `additional_regressions`
//!
//! Assertions express the desired safety contract. Several are expected to fail
//! against the reviewed snapshot. This file is not evidence of native execution.

use onto::{
    ActionOutcome, ActionTypeSpec, Actor, AgentTier, AsOf, AuthzDecision, AuthzLevel, AuthzOp,
    Engine, ExecutionMode, KeyKind, ParamSpec, PolicySpec, Session, Verdict,
};
use serde_json::{json, Value};

fn builder() -> Session {
    Session::new(Actor::builder("followup.builder", &["modeler"]), "followup")
}
fn reviewer() -> Session {
    Session::new(
        Actor::builder("followup.reviewer", &["reviewer"]),
        "followup",
    )
}
fn ops(id: &str) -> Session {
    Session::new(
        Actor::consumer(id, &["operator"], AgentTier::T2),
        "followup",
    )
}
fn boss() -> Session {
    Session::new(
        Actor::consumer("followup.boss", &["supervisor", "operator"], AgentTier::T3),
        "followup",
    )
}
fn is_blocked(result: onto::Result<ActionOutcome>) -> bool {
    match result {
        Err(_) => true,
        Ok(outcome) => outcome.verdict != Verdict::Allow,
    }
}
fn merge(e: &Engine, branch: &str) -> onto::Result<()> {
    let p = e.submit_proposal(&builder(), branch)?;
    e.review_proposal(&reviewer(), &p, true)?;
    e.merge_to_main(&reviewer(), &p)
}
fn setpoint(ids: &onto_bootstrap::WastewaterIds, target: f64) -> Value {
    json!({
        "tank": ids.tank1, "sensor": ids.sensor1, "permit": ids.permit,
        "target_do": target, "rationale": "followup regression"
    })
}

#[test]
fn property_write_deny_must_block_an_otherwise_valid_confirmation() {
    let e = Engine::memory().unwrap();
    let ids = onto_bootstrap::install(&e).unwrap();
    let b = e.open_branch(&builder(), "deny-target-property").unwrap();
    e.create_policy(
        &builder(),
        &b,
        PolicySpec {
            name: "deny_target_do_write".into(),
            level: AuthzLevel::Property,
            op: AuthzOp::Write,
            key: Some(KeyKind::Consumer),
            type_name: Some("AerationTank".into()),
            instance_id: Some(ids.tank1.clone()),
            property: Some("target_do".into()),
            roles: vec!["supervisor".into()],
            min_tier: 0,
            decision: AuthzDecision::Deny,
        },
    )
    .unwrap();
    merge(&e, &b).unwrap();
    let p = e
        .submit_action(
            &ops("proposer"),
            "propose_setpoint_change",
            setpoint(&ids, 2.0),
        )
        .unwrap();
    assert_eq!(
        p.verdict,
        Verdict::Allow,
        "the test must reach valid proposal creation"
    );
    assert!(
        is_blocked(e.confirm_action(&boss(), p.inbox_id.as_deref().unwrap())),
        "A pending proposal must not bypass the denied target property"
    );
}

#[test]
fn two_proposers_reusing_a_local_key_need_distinct_confirmations() {
    let e = Engine::memory().unwrap();
    let ids = onto_bootstrap::install(&e).unwrap();
    let mut a = setpoint(&ids, 2.0);
    let mut b = setpoint(&ids, 2.5);
    a["idempotency_key"] = json!("local-request-1");
    b["idempotency_key"] = json!("local-request-1");
    let pa = e
        .submit_action(&ops("alice"), "propose_setpoint_change", a)
        .unwrap();
    let pb = e
        .submit_action(&ops("bob"), "propose_setpoint_change", b)
        .unwrap();
    let ia = pa.inbox_id.unwrap();
    let ib = pb.inbox_id.unwrap();
    assert_ne!(ia, ib, "the two proposals must be independently created");
    assert_eq!(
        e.confirm_action(&boss(), &ia).unwrap().verdict,
        Verdict::Allow
    );
    let second = e.confirm_action(&boss(), &ib).unwrap();
    assert_eq!(second.verdict, Verdict::Allow);
    assert_eq!(
        second.inbox_id.as_deref(),
        Some(ib.as_str()),
        "response belongs to the wrong inbox"
    );
    let rows = e.list_inbox(&boss()).unwrap();
    assert_eq!(
        rows.iter().find(|r| r.id == ib).unwrap().status,
        "confirmed"
    );
    assert_eq!(
        e.get_object(&boss(), &ids.tank1, AsOf::Current)
            .unwrap()
            .properties["target_do"]
            .value,
        json!(2.5)
    );
}

#[test]
fn confirmed_retry_must_recheck_revoked_role() {
    let e = Engine::memory().unwrap();
    let ids = onto_bootstrap::install(&e).unwrap();
    let p = e
        .submit_action(
            &ops("proposer"),
            "propose_setpoint_change",
            setpoint(&ids, 2.0),
        )
        .unwrap();
    let i = p.inbox_id.unwrap();
    assert_eq!(
        e.confirm_action(&boss(), &i).unwrap().verdict,
        Verdict::Allow
    );
    let revoked = Session::new(
        Actor::consumer("followup.boss", &[], AgentTier::T3),
        "followup",
    );
    assert!(
        is_blocked(e.confirm_action(&revoked, &i)),
        "Confirmed-outcome cache branch must not skip current action authority"
    );
}

#[test]
fn healthcare_direct_approval_requires_a_pending_proposal() {
    let e = Engine::memory().unwrap();
    let ids = onto_bootstrap::install_healthcare(&e).unwrap();
    let params = json!({
        "patient": ids.patient, "observation": ids.observation_complete,
        "order": ids.order, "need_clearance": 1, "rationale": "review"
    });
    assert!(
        is_blocked(e.submit_action(&boss(), "approve_order", params)),
        "Healthcare must use the same required-review contract as the other domains"
    );
}

#[test]
fn healthcare_zero_cannot_neutralize_flagged_evidence() {
    let e = Engine::memory().unwrap();
    let ids = onto_bootstrap::install_healthcare(&e).unwrap();
    let params = json!({
        "patient": ids.patient, "observation": ids.observation_flagged,
        "order": ids.order, "need_clearance": 0, "rationale": "review"
    });
    let proposal = e.submit_action(&ops("proposer"), "propose_order", params);
    match proposal {
        Err(_) => {}
        Ok(p) if p.verdict != Verdict::Allow => {}
        Ok(p) => assert!(
            is_blocked(e.confirm_action(&boss(), p.inbox_id.as_deref().unwrap())),
            "A caller-selected zero must not permit the flagged order"
        ),
    }
}

#[test]
fn published_clearance_schema_must_match_its_numeric_runtime_type() {
    let e = Engine::memory().unwrap();
    onto_bootstrap::install_healthcare(&e).unwrap();
    let tools = e.list_tools(&ops("observer")).unwrap();
    let tool = tools
        .iter()
        .find(|t| t.name == "action.propose_order")
        .unwrap();
    assert_eq!(
        tool.input_schema["properties"]["need_clearance"]["type"],
        json!("number")
    );
}

#[test]
fn redacted_properties_must_also_lose_their_snapshot_metadata() {
    let e = Engine::memory().unwrap();
    let ids = onto_bootstrap::install(&e).unwrap();
    let p = e
        .submit_action(
            &ops("proposer"),
            "propose_setpoint_change",
            setpoint(&ids, 2.0),
        )
        .unwrap();
    let restricted = Session::new(
        Actor::consumer("restricted", &["restricted"], AgentTier::T2),
        "followup",
    );
    assert!(!e
        .get_object(&restricted, &ids.permit, AsOf::Current)
        .unwrap()
        .properties
        .contains_key("do_max"));
    let rec = e
        .get_decision_record(&restricted, p.decision_record_id.as_deref().unwrap())
        .unwrap();
    let object = rec.data_snapshot["objects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["id"] == json!(ids.permit))
        .unwrap();
    assert!(object["properties"].get("do_max").is_none());
    assert!(
        object["as_of"].get("do_max").is_none(),
        "denied property's timestamp leaked"
    );
    assert!(
        object["provenance"].get("do_max").is_none(),
        "denied property's provenance leaked"
    );
}

#[test]
fn compensation_tool_should_be_available_to_the_sample_supervisor() {
    let e = Engine::memory().unwrap();
    onto_bootstrap::install_highered(&e).unwrap();
    assert!(
        e.list_tools(&boss())
            .unwrap()
            .iter()
            .any(|t| t.name == "compensate_action"),
        "Public tool tier and direct T3 inverse-action capability disagree"
    );
}

#[test]
fn newly_created_link_endpoints_still_require_the_declared_type() {
    let e = Engine::memory().unwrap();
    let ids = onto_bootstrap::install(&e).unwrap();
    let b = e.open_branch(&builder(), "created-endpoint-type").unwrap();
    e.create_action_type(
        &builder(),
        &b,
        ActionTypeSpec {
            name: "wrong_created_endpoint".into(),
            mode: ExecutionMode::Auto,
            parameters: vec![ParamSpec {
                name: "sensor".into(),
                value_type: "Text".into(),
                object_type: Some("DO_Sensor".into()),
                required: true,
            }],
            guards: json!([]),
            required_roles: vec!["supervisor".into()],
            required_tier: AgentTier::T3,
            effects: json!([
                {"create": "DO_Sensor", "properties": {"name": "wrong target type"}},
                {"link": "monitors", "from": "sensor"}
            ]),
            compensation: None,
            side_effects: json!({}),
            on_review: None,
            interfaces: vec![],
        },
    )
    .unwrap();
    merge(&e, &b).unwrap();
    assert!(
        is_blocked(e.submit_action(
            &boss(),
            "wrong_created_endpoint",
            json!({"sensor": ids.sensor1})
        )),
        "monitors requires AerationTank, not the newly created DO_Sensor"
    );
}
