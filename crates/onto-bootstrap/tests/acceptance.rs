use onto::{
    dispatch, ActionOutcome, ActionTypeSpec, Actor, AgentTier, AsOf, Engine, ExecutionMode,
    IngestRecord, KeyKind, ObjectSet, ObjectSetFilter, ObjectSetSpec, ObjectTypeSpec, ParamSpec,
    PropertySource, PropertySpec, Query, Result, RiskBand, Session, Typology, Verdict,
    WritePathStep,
};
use onto_bootstrap::{install, WastewaterIds};
use serde_json::json;
use std::collections::BTreeMap;

fn modeller() -> Session {
    Session::new(Actor::builder("human.modeler", &["modeler"]), "test")
}
fn reviewer() -> Session {
    Session::new(Actor::builder("human.reviewer", &["reviewer"]), "test")
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
fn intern() -> Session {
    Session::new(Actor::consumer("ops.intern", &[], AgentTier::T1), "test")
}

fn action_is_blocked(result: &Result<ActionOutcome>) -> bool {
    match result {
        Err(_) => true,
        Ok(out) => match out.verdict {
            Verdict::Deny => true,
            Verdict::Allow | Verdict::Review => false,
        },
    }
}
fn restricted() -> Session {
    Session::new(
        Actor::consumer("ops.restricted", &["restricted"], AgentTier::T2),
        "test",
    )
}
fn automator() -> Session {
    Session::new(
        Actor::consumer("ops.auto", &["operator", "supervisor"], AgentTier::T4),
        "test",
    )
}

fn merge_object_set(engine: &Engine, branch: &str, spec: ObjectSetSpec) {
    let b = engine.open_branch(&modeller(), branch).unwrap();
    engine.create_object_set(&modeller(), &b, spec).unwrap();
    let proposal = engine.submit_proposal(&modeller(), &b).unwrap();
    engine
        .review_proposal(&reviewer(), &proposal, true)
        .unwrap();
    engine.merge_to_main(&reviewer(), &proposal).unwrap();
}

#[test]
fn does_expose_kernel_types_before_user_work() {
    assert!(Engine::kernel_types().contains(&"ObjectType"));
    assert!(Engine::kernel_types().contains(&"OntologyProposal"));
    assert!(Engine::kernel_types().contains(&"FunctionType"));
}

#[test]
fn does_project_consumer_tools_after_merge() {
    let engine = Engine::memory().unwrap();
    let before = engine.list_tools(&operator()).unwrap();
    assert!(
        !before
            .iter()
            .any(|t| t.name == "action.propose_setpoint_change"),
        "action must not exist before merge"
    );
    install(&engine).unwrap();
    let after = engine.list_tools(&operator()).unwrap();
    assert!(after
        .iter()
        .any(|t| t.name == "action.propose_setpoint_change"));
}

#[test]
fn does_hide_branch_alter_until_merge() {
    let engine = Engine::memory().unwrap();
    install(&engine).unwrap();
    let main_before = engine
        .get_schema(&operator(), None)
        .unwrap()
        .object_types
        .iter()
        .find(|t| t.name == "AerationTank")
        .unwrap()
        .properties
        .len();

    let b = engine.open_branch(&modeller(), "add-note").unwrap();
    engine
        .add_property(
            &modeller(),
            &b,
            "AerationTank",
            onto::PropertySpec {
                name: "operator_note".into(),
                value_type: "Text".into(),
                source: onto::PropertySource::Mapped,
                nullable: true,
                function: None,
            },
        )
        .unwrap();

    let main_mid = engine
        .get_schema(&operator(), None)
        .unwrap()
        .object_types
        .iter()
        .find(|t| t.name == "AerationTank")
        .unwrap()
        .properties
        .len();
    assert_eq!(
        main_before, main_mid,
        "unmerged branch must not affect main"
    );

    let proposal = engine.submit_proposal(&modeller(), &b).unwrap();
    engine
        .review_proposal(&reviewer(), &proposal, true)
        .unwrap();
    engine.merge_to_main(&reviewer(), &proposal).unwrap();

    let main_after = engine
        .get_schema(&operator(), None)
        .unwrap()
        .object_types
        .iter()
        .find(|t| t.name == "AerationTank")
        .unwrap()
        .properties
        .len();
    assert_eq!(main_after, main_before + 1);
}

#[test]
fn does_isolate_builder_and_consumer_keys() {
    let engine = Engine::memory().unwrap();
    install(&engine).unwrap();
    let builder = modeller();
    let consumer = operator();

    let denied = dispatch(
        &engine,
        &consumer,
        "create_object_type",
        json!({
            "branch": "x",
            "spec": {
                "name": "Nope",
                "typology": "entity",
                "title_prop": "name",
                "interfaces": [],
                "properties": []
            }
        }),
    );
    assert!(denied.is_err(), "consumer cannot mutate schema");

    let denied_read = dispatch(&engine, &builder, "get_object", json!({ "id": "tank-1" }));
    assert!(denied_read.is_err(), "builder cannot get_object");

    let denied_search = dispatch(&engine, &builder, "search_objects", json!({}));
    assert!(denied_search.is_err());
}

#[test]
fn does_confirm_setpoint_and_seal_decision_record() {
    let engine = Engine::memory().unwrap();
    let ids = install(&engine).unwrap();
    let proposed = engine
        .submit_action(
            &operator(),
            "propose_setpoint_change",
            json!({
                "tank": ids.tank1,
                "sensor": ids.sensor1,
                "permit": ids.permit,
                "target_do": 2.5,
                "rationale": "hold DO band"
            }),
        )
        .unwrap();
    assert_eq!(proposed.verdict, Verdict::Allow);
    let inbox = proposed.inbox_id.expect("inbox");
    let confirmed = engine.confirm_action(&supervisor(), &inbox).unwrap();
    assert_eq!(confirmed.verdict, Verdict::Allow);
    let tank = engine
        .get_object(&operator(), &ids.tank1, onto::AsOf::Current)
        .unwrap();
    assert_eq!(tank.properties["target_do"].value, json!(2.5));
    assert_eq!(
        tank.properties["target_do"].source,
        onto::PropertySource::ActionWritten
    );
    let rec = engine
        .get_decision_record(
            &operator(),
            confirmed.decision_record_id.as_deref().unwrap(),
        )
        .unwrap();
    assert_eq!(rec.verdict, Verdict::Allow);
    assert_eq!(rec.engine_version, onto::ENGINE_VERSION);
}

#[test]
fn does_review_and_name_calibration_if_sensor_is_stale() {
    let engine = Engine::memory().unwrap();
    let ids = install(&engine).unwrap();
    engine.set_clock(engine.now() + 10_000);
    let out = engine
        .submit_action(
            &operator(),
            "propose_setpoint_change",
            json!({
                "tank": ids.tank1,
                "sensor": ids.sensor1,
                "permit": ids.permit,
                "target_do": 2.2,
                "rationale": "stale path"
            }),
        )
        .unwrap();
    assert_eq!(out.verdict, Verdict::Review);
    assert_eq!(
        out.alternative.as_deref(),
        Some("request_sensor_calibration")
    );
    let alt = engine
        .submit_action(
            &operator(),
            "request_sensor_calibration",
            json!({ "sensor": ids.sensor1 }),
        )
        .unwrap();
    assert_eq!(alt.verdict, Verdict::Allow);
}

#[test]
fn does_deny_if_permit_exceeded_or_role_missing() {
    let engine = Engine::memory().unwrap();
    let ids = install(&engine).unwrap();
    let over = engine
        .submit_action(
            &operator(),
            "propose_setpoint_change",
            json!({
                "tank": ids.tank1,
                "sensor": ids.sensor1,
                "permit": ids.permit,
                "target_do": 9.0,
                "rationale": "too high"
            }),
        )
        .unwrap();
    assert_eq!(over.verdict, Verdict::Deny);

    let intern_denied = engine.submit_action(
        &intern(),
        "propose_setpoint_change",
        json!({
            "tank": ids.tank1,
            "sensor": ids.sensor1,
            "permit": ids.permit,
            "target_do": 2.0,
            "rationale": "intern"
        }),
    );
    assert!(intern_denied.is_err(), "T1 cannot submit_action");

    let no_role = Session::new(Actor::consumer("ops.norole", &[], AgentTier::T2), "test");
    let unauth = engine
        .submit_action(
            &no_role,
            "propose_setpoint_change",
            json!({
                "tank": ids.tank1,
                "sensor": ids.sensor1,
                "permit": ids.permit,
                "target_do": 2.0,
                "rationale": "no role"
            }),
        )
        .unwrap();
    assert_eq!(unauth.verdict, Verdict::Deny);
}

#[test]
fn does_seal_replayable_override_dossier() {
    let engine = Engine::memory().unwrap();
    let ids = install(&engine).unwrap();
    let proposed = engine
        .submit_action(
            &operator(),
            "propose_setpoint_change",
            json!({
                "tank": ids.tank1,
                "sensor": ids.sensor1,
                "permit": ids.permit,
                "target_do": 2.4,
                "rationale": "override path"
            }),
        )
        .unwrap();
    let inbox = proposed.inbox_id.unwrap();
    let over = engine
        .override_action(&supervisor(), &inbox, "process_exception", "foam event")
        .unwrap();
    assert_eq!(over.verdict, Verdict::Allow);
    let rec = engine
        .get_decision_record(&operator(), over.decision_record_id.as_deref().unwrap())
        .unwrap();
    assert_eq!(rec.action_name, "override_setpoint");
    let created = engine
        .search_objects(
            &operator(),
            Query {
                type_name: Some("Override".into()),
                ..Query::default()
            },
        )
        .unwrap();
    assert!(!created.is_empty());
}

#[test]
fn does_keep_action_written_if_funnel_ingests() {
    let engine = Engine::memory().unwrap();
    let ids = install(&engine).unwrap();
    let proposed = engine
        .submit_action(
            &operator(),
            "propose_setpoint_change",
            json!({
                "tank": ids.tank1,
                "sensor": ids.sensor1,
                "permit": ids.permit,
                "target_do": 3.1,
                "rationale": "set"
            }),
        )
        .unwrap();
    engine
        .confirm_action(&supervisor(), proposed.inbox_id.as_deref().unwrap())
        .unwrap();
    engine
        .funnel_ingest(
            &operator(),
            vec![onto::IngestRecord {
                type_name: "AerationTank".into(),
                id: Some(ids.tank1.clone()),
                properties: [
                    ("name".into(), json!("Basin 1")),
                    ("current_do".into(), json!(1.1)),
                    ("target_do".into(), json!(0.1)),
                ]
                .into_iter()
                .collect(),
                as_of: Some(engine.now().to_string()),
                provenance: Some("nightly".into()),
            }],
        )
        .unwrap();
    let tank = engine
        .get_object(&operator(), &ids.tank1, onto::AsOf::Current)
        .unwrap();
    assert_eq!(tank.properties["target_do"].value, json!(3.1));
    assert_eq!(
        tank.properties["target_do"].source,
        onto::PropertySource::ActionWritten,
        "Funnel must not overwrite ActionWritten target_do"
    );
    assert_eq!(tank.properties["current_do"].value, json!(1.1));
    assert_eq!(
        tank.properties["current_do"].source,
        onto::PropertySource::Mapped,
        "Mapped current_do must still update so the skip is not a total no-op"
    );
}

#[test]
fn does_leave_production_unchanged_if_branch_is_unmerged() {
    let engine = Engine::memory().unwrap();
    install(&engine).unwrap();
    let count_before = engine
        .search_objects(
            &operator(),
            Query {
                type_name: Some("AerationTank".into()),
                ..Query::default()
            },
        )
        .unwrap()
        .len();
    let b = engine.open_branch(&modeller(), "ghost-type").unwrap();
    engine
        .create_object_type(
            &modeller(),
            &b,
            onto::ObjectTypeSpec {
                name: "GhostAsset".into(),
                typology: onto::Typology::Entity,
                title_prop: Some("name".into()),
                interfaces: vec![],
                freshness_budget_secs: None,
                properties: vec![onto::PropertySpec {
                    name: "name".into(),
                    value_type: "Text".into(),
                    source: onto::PropertySource::Mapped,
                    nullable: false,
                    function: None,
                }],
            },
        )
        .unwrap();
    let schema = engine.get_schema(&operator(), None).unwrap();
    assert!(!schema.object_types.iter().any(|t| t.name == "GhostAsset"));
    let count_after = engine
        .search_objects(
            &operator(),
            Query {
                type_name: Some("AerationTank".into()),
                ..Query::default()
            },
        )
        .unwrap()
        .len();
    assert_eq!(count_before, count_after);
}

#[test]
fn does_deny_consumer_working_branch_schema() {
    let engine = Engine::memory().unwrap();
    install(&engine).unwrap();
    let b = engine.open_branch(&modeller(), "secret").unwrap();
    let err = engine.get_schema(&operator(), Some(&b));
    assert!(err.is_err());
}

#[test]
fn does_keep_builder_and_consumer_keys_distinct() {
    assert_ne!(KeyKind::Builder, KeyKind::Consumer);
}

#[test]
fn does_walk_write_path_seven_steps_in_order() {
    let engine = Engine::memory().unwrap();
    let ids = install(&engine).unwrap();
    let proposed = engine
        .submit_action(
            &operator(),
            "propose_setpoint_change",
            json!({
                "tank": ids.tank1,
                "sensor": ids.sensor1,
                "permit": ids.permit,
                "target_do": 2.5,
                "rationale": "seven steps"
            }),
        )
        .unwrap();
    assert_eq!(proposed.verdict, Verdict::Allow);
    assert!(
        proposed.inbox_id.is_some(),
        "Reviewable Propose must mint an inbox on the write path"
    );
    let rec = engine
        .get_decision_record(&operator(), proposed.decision_record_id.as_deref().unwrap())
        .unwrap();
    assert_eq!(
        rec.proof_trace,
        vec![
            WritePathStep::Submit,
            WritePathStep::ParamAndPermission,
            WritePathStep::SubmissionCriteria,
            WritePathStep::StagedEdits,
            WritePathStep::Commit,
            WritePathStep::SealDecisionRecord,
            WritePathStep::DeclareSideEffects,
        ]
    );
    assert_eq!(rec.proof_trace, WritePathStep::ALL.to_vec());
    assert_eq!(rec.action_name, "propose_setpoint_change");
    assert_eq!(rec.verdict, Verdict::Allow);
    assert!(!rec.proof_trace.is_empty());
    assert_ne!(
        rec.proof_trace,
        vec![
            WritePathStep::Submit,
            WritePathStep::ParamAndPermission,
            WritePathStep::SubmissionCriteria,
            WritePathStep::SealDecisionRecord,
        ],
        "Allow must not use the discarded-stage trace"
    );
}

#[test]
fn does_discard_stage_if_guard_fails_at_criteria() {
    let engine = Engine::memory().unwrap();
    let ids = install(&engine).unwrap();
    let tank_before = engine
        .get_object(&operator(), &ids.tank1, onto::AsOf::Current)
        .unwrap();
    let objects_before = engine
        .search_objects(
            &operator(),
            Query {
                type_name: Some("AerationTank".into()),
                ..Query::default()
            },
        )
        .unwrap();
    let over = engine
        .submit_action(
            &operator(),
            "propose_setpoint_change",
            json!({
                "tank": ids.tank1,
                "sensor": ids.sensor1,
                "permit": ids.permit,
                "target_do": 9.0,
                "rationale": "exceeds permit"
            }),
        )
        .unwrap();
    assert_eq!(over.verdict, Verdict::Deny);
    assert!(over.inbox_id.is_none());
    assert!(over.created_ids.is_empty());
    assert!(engine.list_inbox(&supervisor()).unwrap().is_empty());
    let tank_after = engine
        .get_object(&operator(), &ids.tank1, onto::AsOf::Current)
        .unwrap();
    assert_eq!(
        serde_json::to_value(&tank_before.properties).unwrap(),
        serde_json::to_value(&tank_after.properties).unwrap()
    );
    let objects_after = engine
        .search_objects(
            &operator(),
            Query {
                type_name: Some("AerationTank".into()),
                ..Query::default()
            },
        )
        .unwrap();
    assert_eq!(objects_before.len(), objects_after.len());
    let rec = engine
        .get_decision_record(&operator(), over.decision_record_id.as_deref().unwrap())
        .unwrap();
    assert_eq!(
        rec.proof_trace,
        vec![
            WritePathStep::Submit,
            WritePathStep::ParamAndPermission,
            WritePathStep::SubmissionCriteria,
            WritePathStep::SealDecisionRecord,
        ]
    );
}

#[test]
fn does_pin_reads_and_versions_on_decision_snapshot() {
    let engine = Engine::memory().unwrap();
    let ids = install(&engine).unwrap();
    let proposed = engine
        .submit_action(
            &operator(),
            "propose_setpoint_change",
            json!({
                "tank": ids.tank1,
                "sensor": ids.sensor1,
                "permit": ids.permit,
                "target_do": 2.3,
                "rationale": "snapshot"
            }),
        )
        .unwrap();
    let rec = engine
        .get_decision_record(&operator(), proposed.decision_record_id.as_deref().unwrap())
        .unwrap();
    let objects = rec.data_snapshot["objects"]
        .as_array()
        .expect("snapshot objects");
    let by_id = |want: &str| {
        objects
            .iter()
            .find(|o| o["id"] == want)
            .unwrap_or_else(|| panic!("missing {want} in snapshot"))
    };
    let tank = by_id(&ids.tank1);
    assert_eq!(tank["type_name"], "AerationTank");
    assert_eq!(tank["properties"]["name"], "Basin 1");
    let sensor = by_id(&ids.sensor1);
    assert_eq!(sensor["type_name"], "DO_Sensor");
    assert!(sensor["properties"].get("last_reading_at").is_some());
    let permit = by_id(&ids.permit);
    assert_eq!(permit["properties"]["do_max"], 4.0);
    assert_eq!(rec.data_snapshot["engine_version"], onto::ENGINE_VERSION);
    let cal = engine
        .get_schema(&operator(), None)
        .unwrap()
        .functions
        .iter()
        .find(|f| f.name == "days_since_calibration")
        .expect("calibration function is an OMS record")
        .pin();
    assert_eq!(rec.data_snapshot["function_version"], cal);
    assert_ne!(cal, "days_since_calibration:0.1");
    let rule = rec.data_snapshot["rule_version"]
        .as_str()
        .expect("rule_version");
    assert!(
        rule.starts_with("propose_setpoint_change:"),
        "rule_version pins the action spec, got {rule}"
    );
    assert_eq!(rec.engine_version, onto::ENGINE_VERSION);
    assert_eq!(rec.function_version, cal);
}

#[test]
fn does_not_double_apply_if_idempotency_key_repeats() {
    let engine = Engine::memory().unwrap();
    let ids = install(&engine).unwrap();
    let params = json!({
        "tank": ids.tank1,
        "sensor": ids.sensor1,
        "permit": ids.permit,
        "target_do": 2.6,
        "rationale": "idempotent",
        "idempotency_key": "setpoint:tank-1:2.6"
    });
    let first = commit_setpoint(&engine, params.clone());
    assert_eq!(first.verdict, Verdict::Allow);
    let second = engine
        .confirm_action(&supervisor(), first.inbox_id.as_deref().unwrap())
        .unwrap();
    assert_eq!(first.decision_record_id, second.decision_record_id);
    assert_eq!(first.created_ids, second.created_ids);
    let tank = engine
        .get_object(&operator(), &ids.tank1, onto::AsOf::Current)
        .unwrap();
    assert_eq!(tank.properties["target_do"].value, json!(2.6));
    let approvals = engine
        .search_objects(
            &operator(),
            Query {
                type_name: Some("ApprovalRecord".into()),
                ..Query::default()
            },
        )
        .unwrap();
    assert_eq!(approvals.len(), 1);
    let rec = engine
        .get_decision_record(&operator(), first.decision_record_id.as_deref().unwrap())
        .unwrap();
    assert_eq!(
        rec.effects["idempotency_key"],
        format!(
            "approve_setpoint_change:ops.chen:{}:setpoint:tank-1:2.6",
            first.inbox_id.as_deref().unwrap()
        )
    );
    assert_eq!(rec.proof_trace, WritePathStep::ALL.to_vec());
}

#[test]
fn does_derive_days_since_calibration_from_clock() {
    let engine = Engine::memory().unwrap();
    let ids = install(&engine).unwrap();
    let schema = engine.get_schema(&operator(), None).unwrap();
    let cal = schema
        .functions
        .iter()
        .find(|f| f.name == "days_since_calibration")
        .expect("function is an OMS record");
    assert_eq!(cal.inputs, vec!["calibration_date"]);
    let sensor_type = schema
        .object_types
        .iter()
        .find(|t| t.name == "DO_Sensor")
        .unwrap();
    let derived = sensor_type
        .properties
        .iter()
        .find(|p| p.name == "days_since_calibration")
        .unwrap();
    assert_eq!(derived.source, onto::PropertySource::Derived);
    assert_eq!(derived.function.as_deref(), Some("days_since_calibration"));

    let sensor = engine
        .get_object(&operator(), &ids.sensor1, AsOf::Current)
        .unwrap();
    assert_eq!(sensor.properties["days_since_calibration"].value, json!(10));
    assert_eq!(
        sensor.properties["days_since_calibration"].source,
        onto::PropertySource::Derived
    );
    engine.set_clock(engine.now() + 86_400);
    let later = engine
        .get_object(&operator(), &ids.sensor1, AsOf::Current)
        .unwrap();
    assert_eq!(later.properties["days_since_calibration"].value, json!(11));
}

#[test]
#[allow(clippy::too_many_lines)] // function registry merge is one teaching case
fn does_evaluate_runtime_function_after_merge() {
    let engine = Engine::memory().unwrap();
    install(&engine).unwrap();

    let b = engine.open_branch(&modeller(), "fn-double").unwrap();
    engine
        .create_function(
            &modeller(),
            &b,
            onto::FunctionSpec {
                name: "double_of".into(),
                inputs: vec!["n".into()],
                kind: onto::FunctionKind::DoubleInteger,
            },
        )
        .unwrap();
    engine
        .create_object_type(
            &modeller(),
            &b,
            onto::ObjectTypeSpec {
                name: "Gauge".into(),
                typology: onto::Typology::Entity,
                title_prop: Some("name".into()),
                interfaces: vec![],
                freshness_budget_secs: None,
                properties: vec![
                    onto::PropertySpec {
                        name: "name".into(),
                        value_type: "Text".into(),
                        source: onto::PropertySource::Mapped,
                        nullable: false,
                        function: None,
                    },
                    onto::PropertySpec {
                        name: "n".into(),
                        value_type: "Text".into(),
                        source: onto::PropertySource::Mapped,
                        nullable: false,
                        function: None,
                    },
                    onto::PropertySpec {
                        name: "twice".into(),
                        value_type: "Text".into(),
                        source: onto::PropertySource::Derived,
                        nullable: true,
                        function: Some("double_of".into()),
                    },
                ],
            },
        )
        .unwrap();

    let main_mid = engine.get_schema(&operator(), None).unwrap();
    assert!(
        !main_mid.functions.iter().any(|f| f.name == "double_of"),
        "unmerged function must not appear on production schema"
    );
    assert!(!main_mid.object_types.iter().any(|t| t.name == "Gauge"));
    let ingest_denied = engine.funnel_ingest(
        &operator(),
        vec![onto::IngestRecord {
            type_name: "Gauge".into(),
            id: Some("gauge-1".into()),
            properties: [("name".into(), json!("g1")), ("n".into(), json!(21))]
                .into_iter()
                .collect(),
            as_of: Some(engine.now().to_string()),
            provenance: Some("test".into()),
        }],
    );
    assert!(
        ingest_denied.is_err(),
        "unmerged type must have zero effect on production writes"
    );

    let proposal = engine.submit_proposal(&modeller(), &b).unwrap();
    engine
        .review_proposal(&reviewer(), &proposal, true)
        .unwrap();
    engine.merge_to_main(&reviewer(), &proposal).unwrap();

    engine
        .funnel_ingest(
            &operator(),
            vec![onto::IngestRecord {
                type_name: "Gauge".into(),
                id: Some("gauge-1".into()),
                properties: [("name".into(), json!("g1")), ("n".into(), json!(21))]
                    .into_iter()
                    .collect(),
                as_of: Some(engine.now().to_string()),
                provenance: Some("test".into()),
            }],
        )
        .unwrap();
    let gauge = engine
        .get_object(&operator(), "gauge-1", AsOf::Current)
        .unwrap();
    assert_eq!(gauge.properties["n"].value, json!(21));
    assert_eq!(gauge.properties["twice"].value, json!(42));
    assert_eq!(
        gauge.properties["twice"].source,
        onto::PropertySource::Derived
    );
    assert_eq!(
        gauge.properties["twice"].provenance.as_deref(),
        Some("function:double_of")
    );
}

#[test]
fn does_append_versions_and_read_history_as_of() {
    let engine = Engine::memory().unwrap();
    let ids = install(&engine).unwrap();
    let spans_seed = engine.object_spans(&ids.tank1).unwrap();
    assert_eq!(spans_seed.len(), 1, "funnel seed is the first version");
    assert!(spans_seed[0].is_open());

    let first = commit_setpoint(
        &engine,
        json!({
            "tank": ids.tank1,
            "sensor": ids.sensor1,
            "permit": ids.permit,
            "target_do": 2.5,
            "rationale": "first setpoint",
            "idempotency_key": "setpoint:tank-1:first"
        }),
    );
    assert_eq!(first.verdict, Verdict::Allow);
    let after_first = engine.now();
    let tank_first = engine
        .get_object(&operator(), &ids.tank1, AsOf::Current)
        .unwrap();
    assert_eq!(tank_first.properties["target_do"].value, json!(2.5));

    engine.set_clock(after_first + 10);
    let second = commit_setpoint(
        &engine,
        json!({
            "tank": ids.tank1,
            "sensor": ids.sensor1,
            "permit": ids.permit,
            "target_do": 3.0,
            "rationale": "second setpoint",
            "idempotency_key": "setpoint:tank-1:second"
        }),
    );
    assert_eq!(second.verdict, Verdict::Allow);

    let spans = engine.object_spans(&ids.tank1).unwrap();
    let action_versions = spans.len() - spans_seed.len();
    assert_eq!(
        action_versions, 2,
        "two sequential approve_setpoint_change writes yield two versions"
    );
    assert_eq!(spans.iter().filter(|s| s.is_open()).count(), 1);

    let current = engine
        .get_object(&operator(), &ids.tank1, AsOf::Current)
        .unwrap();
    assert_eq!(current.properties["target_do"].value, json!(3.0));

    let historical = engine
        .get_object(&operator(), &ids.tank1, AsOf::Valid(after_first))
        .unwrap();
    assert_eq!(
        historical.properties["target_do"].value,
        json!(2.5),
        "as_of before the second commit must show the first target_do"
    );

    let missing = engine.get_object(&operator(), &ids.tank1, AsOf::Valid(0));
    assert!(
        matches!(missing, Err(onto::OntoError::NotFound(_))),
        "missing coverage is NotFound, not a silent current row"
    );
}

#[test]
fn does_return_empty_vec_if_object_set_is_empty() {
    let engine = Engine::memory().unwrap();
    install(&engine).unwrap();
    let none = engine
        .search_objects(
            &operator(),
            Query {
                type_name: Some("GhostAsset".into()),
                ..Query::default()
            },
        )
        .unwrap();
    assert!(none.is_empty());
    let miss = engine
        .search_objects(
            &operator(),
            Query {
                type_name: Some("AerationTank".into()),
                equals: [("name".into(), json!("no such basin"))]
                    .into_iter()
                    .collect(),
                ..Query::default()
            },
        )
        .unwrap();
    assert!(miss.is_empty());
}

#[test]
fn does_hide_named_set_until_merge() {
    let engine = Engine::memory().unwrap();
    install(&engine).unwrap();
    let b = engine.open_branch(&modeller(), "set-aeration").unwrap();
    engine
        .create_object_set(
            &modeller(),
            &b,
            ObjectSetSpec {
                name: "aeration_tanks".into(),
                type_name: Some("AerationTank".into()),
                equals: BTreeMap::default(),
            },
        )
        .unwrap();
    let before = engine.search_objects(
        &operator(),
        Query {
            set_name: Some("aeration_tanks".into()),
            ..Query::default()
        },
    );
    assert!(
        matches!(before, Err(onto::OntoError::NotFound(_))),
        "unmerged named set must be invisible on main, got {before:?}"
    );

    let proposal = engine.submit_proposal(&modeller(), &b).unwrap();
    engine
        .review_proposal(&reviewer(), &proposal, true)
        .unwrap();
    engine.merge_to_main(&reviewer(), &proposal).unwrap();

    let after = engine
        .search_objects(
            &operator(),
            Query {
                set_name: Some("aeration_tanks".into()),
                ..Query::default()
            },
        )
        .unwrap();
    assert!(!after.is_empty());
    assert!(after.iter().all(|o| o.type_name == "AerationTank"));
}

#[test]
fn does_include_new_match_and_exclude_non_match() {
    let engine = Engine::memory().unwrap();
    let ids = install(&engine).unwrap();
    merge_object_set(
        &engine,
        "set-live-tanks",
        ObjectSetSpec {
            name: "aeration_tanks".into(),
            type_name: Some("AerationTank".into()),
            equals: BTreeMap::default(),
        },
    );
    let before = engine
        .search_objects(
            &operator(),
            Query {
                set_name: Some("aeration_tanks".into()),
                ..Query::default()
            },
        )
        .unwrap();
    let before_ids: Vec<&str> = before.iter().map(|o| o.id.as_str()).collect();
    assert!(before_ids.contains(&ids.tank1.as_str()));
    assert!(!before_ids.contains(&"tank-4"));

    engine
        .funnel_ingest(
            &operator(),
            vec![
                IngestRecord {
                    type_name: "AerationTank".into(),
                    id: Some("tank-4".into()),
                    properties: [
                        ("name".into(), json!("Basin 4")),
                        ("current_do".into(), json!(1.5)),
                    ]
                    .into_iter()
                    .collect(),
                    as_of: Some(engine.now().to_string()),
                    provenance: Some("test".into()),
                },
                IngestRecord {
                    type_name: "Blower".into(),
                    id: Some("blower-2".into()),
                    properties: [("name".into(), json!("B-2"))].into_iter().collect(),
                    as_of: Some(engine.now().to_string()),
                    provenance: Some("test".into()),
                },
            ],
        )
        .unwrap();

    let after = engine
        .search_objects(
            &operator(),
            Query {
                set_name: Some("aeration_tanks".into()),
                ..Query::default()
            },
        )
        .unwrap();
    let after_ids: Vec<&str> = after.iter().map(|o| o.id.as_str()).collect();
    assert!(
        after_ids.contains(&"tank-4"),
        "named set must include a newly created match, got {after_ids:?}"
    );
    assert!(
        !after_ids.contains(&"blower-2"),
        "named set must exclude a non-matching type, got {after_ids:?}"
    );
    assert_eq!(after.len(), before.len() + 1);
}

#[test]
fn does_hide_rationale_in_set_results_if_restricted() {
    let engine = Engine::memory().unwrap();
    install(&engine).unwrap();
    engine
        .funnel_ingest(
            &operator(),
            vec![IngestRecord {
                type_name: "LabMeasurement".into(),
                id: Some("lab-1".into()),
                properties: [
                    ("name".into(), json!("lab")),
                    ("value".into(), json!(2.2)),
                    ("rationale".into(), json!("hold the band")),
                ]
                .into_iter()
                .collect(),
                as_of: Some(engine.now().to_string()),
                provenance: Some("test".into()),
            }],
        )
        .unwrap();
    let open = engine
        .search_objects(
            &operator(),
            Query {
                type_name: Some("LabMeasurement".into()),
                ..Query::default()
            },
        )
        .unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(
        open[0].properties["rationale"].value,
        json!("hold the band")
    );
    let closed = engine
        .search_objects(
            &restricted(),
            Query {
                type_name: Some("LabMeasurement".into()),
                ..Query::default()
            },
        )
        .unwrap();
    assert_eq!(closed.len(), 1);
    assert!(
        !closed[0].properties.contains_key("rationale"),
        "restricted role must not see rationale in set results"
    );
}

#[test]
fn does_deny_intern_mutations_including_create_link() {
    let engine = Engine::memory().unwrap();
    let ids = install(&engine).unwrap();
    let before = engine
        .search_objects(
            &operator(),
            Query {
                type_name: Some("AerationTank".into()),
                ..Query::default()
            },
        )
        .unwrap()
        .len();

    let leftover = engine.funnel_ingest(
        &intern(),
        vec![IngestRecord {
            type_name: "AerationTank".into(),
            id: Some("tank-intern".into()),
            properties: [("name".into(), json!("Intern basin"))]
                .into_iter()
                .collect(),
            as_of: Some(engine.now().to_string()),
            provenance: Some("intern".into()),
        }],
    );
    assert!(
        matches!(leftover, Err(onto::OntoError::Denied(_))),
        "intern must not Funnel-write; missing grant is Deny, got {leftover:?}"
    );

    let tools = engine.list_tools(&intern()).unwrap();
    assert!(
        !tools.iter().any(|t| t.name == "create_link"),
        "create_link must not be a consumer tool"
    );
    let dispatched = dispatch(
        &engine,
        &intern(),
        "create_link",
        json!({
            "type_name": "contains",
            "from_id": ids.plant,
            "to_id": ids.tank1
        }),
    );
    assert!(
        matches!(dispatched, Err(onto::OntoError::Denied(_))),
        "create_link must not be a leftover public write, got {dispatched:?}"
    );

    let denied_action = engine.submit_action(
        &intern(),
        "propose_setpoint_change",
        json!({
            "tank": ids.tank1,
            "sensor": ids.sensor1,
            "permit": ids.permit,
            "target_do": 2.0,
            "rationale": "intern leftover"
        }),
    );
    assert!(
        action_is_blocked(&denied_action),
        "intern must not mutate via propose_setpoint_change, got {denied_action:?}"
    );
    let link_denied = engine.submit_action(
        &intern(),
        "assert_link",
        json!({
            "link_type": "contains",
            "from": ids.plant,
            "to": ids.tank1
        }),
    );
    assert!(
        action_is_blocked(&link_denied),
        "intern must not mutate via assert_link, got {link_denied:?}"
    );

    let after = engine
        .search_objects(
            &operator(),
            Query {
                type_name: Some("AerationTank".into()),
                ..Query::default()
            },
        )
        .unwrap();
    assert_eq!(before, after.len());
    assert!(!after.iter().any(|o| o.id == "tank-intern"));
}

#[test]
fn does_hide_denied_properties_on_get_and_sets_if_restricted() {
    let engine = Engine::memory().unwrap();
    let ids = install(&engine).unwrap();
    engine
        .funnel_ingest(
            &operator(),
            vec![IngestRecord {
                type_name: "LabMeasurement".into(),
                id: Some("lab-deny".into()),
                properties: [
                    ("name".into(), json!("lab")),
                    ("value".into(), json!(2.2)),
                    ("rationale".into(), json!("hold the band")),
                ]
                .into_iter()
                .collect(),
                as_of: Some(engine.now().to_string()),
                provenance: Some("test".into()),
            }],
        )
        .unwrap();

    let open = engine
        .get_object(&operator(), "lab-deny", AsOf::Current)
        .unwrap();
    assert_eq!(open.properties["rationale"].value, json!("hold the band"));
    let closed = engine
        .get_object(&restricted(), "lab-deny", AsOf::Current)
        .unwrap();
    assert!(
        !closed.properties.contains_key("rationale"),
        "restricted must not see denied rationale on get_object"
    );
    assert!(closed.properties.contains_key("value"));

    let permit_open = engine
        .get_object(&operator(), &ids.permit, AsOf::Current)
        .unwrap();
    assert_eq!(permit_open.properties["do_max"].value, json!(4.0));
    let permit_closed = engine
        .get_object(&restricted(), &ids.permit, AsOf::Current)
        .unwrap();
    assert!(
        !permit_closed.properties.contains_key("do_max"),
        "property-level Deny hides do_max, not just rationale"
    );
    assert!(permit_closed.properties.contains_key("name"));

    let set = engine
        .search_objects(
            &restricted(),
            Query {
                type_name: Some("LabMeasurement".into()),
                ..Query::default()
            },
        )
        .unwrap();
    let lab = set.iter().find(|o| o.id == "lab-deny").expect("lab-deny");
    assert!(!lab.properties.contains_key("rationale"));
    let permits = engine
        .search_objects(
            &restricted(),
            Query {
                type_name: Some("PermitVersion".into()),
                ..Query::default()
            },
        )
        .unwrap();
    assert_eq!(permits.len(), 1);
    assert!(!permits[0].properties.contains_key("do_max"));
}

#[test]
fn does_count_filtered_set_not_whole_type() {
    let engine = Engine::memory().unwrap();
    install(&engine).unwrap();
    let whole = engine.aggregate(&operator(), "AerationTank").unwrap();
    assert_eq!(whole["count"], 3);
    let filtered = engine
        .aggregate_set(
            &operator(),
            ObjectSet::inline(
                Some("AerationTank".into()),
                ObjectSetFilter {
                    equals: [("name".into(), json!("Basin 1"))].into_iter().collect(),
                },
                50,
            ),
        )
        .unwrap();
    assert_eq!(filtered["count"], 1);
    assert_ne!(filtered["count"], whole["count"]);
}

#[test]
fn does_compensate_allow_as_inverse_action_not_rollback() {
    let engine = Engine::memory().unwrap();
    let ids = install(&engine).unwrap();
    let approved = commit_setpoint(
        &engine,
        json!({
            "tank": ids.tank1,
            "sensor": ids.sensor1,
            "permit": ids.permit,
            "target_do": 2.5,
            "rationale": "approve then compensate",
            "idempotency_key": "setpoint:tank-1:approve"
        }),
    );
    assert_eq!(approved.verdict, Verdict::Allow);
    let original_id = approved
        .decision_record_id
        .clone()
        .expect("original record");
    let original = engine
        .get_decision_record(&operator(), &original_id)
        .unwrap();
    assert_eq!(original.action_name, "approve_setpoint_change");
    let after_approve = engine.now();
    let spans_after_approve = engine.object_spans(&ids.tank1).unwrap();

    engine.set_clock(after_approve + 10);
    let compensated = engine
        .compensate_action(
            &supervisor(),
            &original_id,
            json!({
                "target_do": 1.8,
                "rationale": "forward inverse",
                "idempotency_key": "compensate:tank-1:once"
            }),
        )
        .unwrap();
    assert_eq!(compensated.verdict, Verdict::Allow);
    let compensation_id = compensated
        .decision_record_id
        .clone()
        .expect("compensation record");
    assert_ne!(
        compensation_id, original_id,
        "compensation must seal a new DecisionRecord"
    );

    let still_original = engine
        .get_decision_record(&operator(), &original_id)
        .unwrap();
    assert_eq!(still_original.id, original.id);
    assert_eq!(still_original.action_name, original.action_name);
    assert_eq!(still_original.verdict, Verdict::Allow);
    assert_eq!(still_original.created_at, original.created_at);
    assert_eq!(still_original.params, original.params);
    assert_eq!(still_original.proof_trace, original.proof_trace);

    let inverse = engine
        .get_decision_record(&operator(), &compensation_id)
        .unwrap();
    assert_eq!(inverse.action_name, "revert_setpoint_change");
    assert_eq!(inverse.proof_trace, WritePathStep::ALL.to_vec());
    assert_eq!(inverse.verdict, Verdict::Allow);

    let current = engine
        .get_object(&operator(), &ids.tank1, AsOf::Current)
        .unwrap();
    assert_eq!(
        current.properties["target_do"].value,
        json!(1.8),
        "current target_do is the compensated value"
    );
    let historical = engine
        .get_object(&operator(), &ids.tank1, AsOf::Valid(after_approve))
        .unwrap();
    assert_eq!(
        historical.properties["target_do"].value,
        json!(2.5),
        "as_of before compensate still sees the approved value"
    );

    let spans = engine.object_spans(&ids.tank1).unwrap();
    assert_eq!(
        spans.len(),
        spans_after_approve.len() + 1,
        "compensation appends a version; it does not rewrite history"
    );
}

#[test]
fn does_error_if_compensation_name_is_missing() {
    let engine = Engine::memory().unwrap();
    let ids = install(&engine).unwrap();
    let over = engine
        .submit_action(
            &supervisor(),
            "override_setpoint",
            json!({
                "tank": ids.tank1,
                "override_category": "process_exception",
                "override_reason": "no inverse named"
            }),
        )
        .unwrap();
    assert_eq!(over.verdict, Verdict::Allow);
    let rec_id = over.decision_record_id.clone().expect("override record");
    let err = engine
        .compensate_action(&supervisor(), &rec_id, json!({}))
        .unwrap_err();
    assert!(
        matches!(err, onto::OntoError::NoCompensation(ref name) if name == "override_setpoint"),
        "missing compensation must be typed, got {err:?}"
    );
    let still = engine.get_decision_record(&operator(), &rec_id).unwrap();
    assert_eq!(still.action_name, "override_setpoint");
    assert_eq!(still.verdict, Verdict::Allow);
}

#[test]
fn does_not_double_apply_if_compensate_key_repeats() {
    let engine = Engine::memory().unwrap();
    let ids = install(&engine).unwrap();
    let approved = commit_setpoint(
        &engine,
        json!({
            "tank": ids.tank1,
            "sensor": ids.sensor1,
            "permit": ids.permit,
            "target_do": 2.5,
            "rationale": "idempotent compensate",
            "idempotency_key": "setpoint:tank-1:comp-src"
        }),
    );
    let original_id = approved.decision_record_id.clone().expect("original");
    engine.set_clock(engine.now() + 10);
    let overlay = json!({
        "target_do": 1.8,
        "rationale": "forward inverse",
        "idempotency_key": "compensate:tank-1:same"
    });
    let first = engine
        .compensate_action(&supervisor(), &original_id, overlay.clone())
        .unwrap();
    assert_eq!(first.verdict, Verdict::Allow);
    let spans_after = engine.object_spans(&ids.tank1).unwrap();
    let second = engine
        .compensate_action(&supervisor(), &original_id, overlay)
        .unwrap();
    assert_eq!(first.decision_record_id, second.decision_record_id);
    assert_eq!(
        engine.object_spans(&ids.tank1).unwrap().len(),
        spans_after.len(),
        "retry with the same compensation idempotency key must not append another version"
    );
    let tank = engine
        .get_object(&operator(), &ids.tank1, AsOf::Current)
        .unwrap();
    assert_eq!(tank.properties["target_do"].value, json!(1.8));
}

#[test]
fn does_refuse_compensate_if_verdict_is_deny() {
    let engine = Engine::memory().unwrap();
    let ids = install(&engine).unwrap();
    let denied = engine
        .submit_action(
            &operator(),
            "propose_setpoint_change",
            json!({
                "tank": ids.tank1,
                "sensor": ids.sensor1,
                "permit": ids.permit,
                "target_do": 9.0,
                "rationale": "over permit"
            }),
        )
        .unwrap();
    assert_eq!(denied.verdict, Verdict::Deny);
    let rec_id = denied.decision_record_id.clone().expect("deny record");
    let err = engine
        .compensate_action(&supervisor(), &rec_id, json!({}))
        .unwrap_err();
    assert!(
        matches!(err, onto::OntoError::NotCompensable(ref id) if id.as_str() == rec_id),
        "Deny must not compensate, got {err:?}"
    );
    let still = engine.get_decision_record(&operator(), &rec_id).unwrap();
    assert_eq!(still.verdict, Verdict::Deny);
    assert_eq!(still.action_name, "propose_setpoint_change");
}

fn setpoint_params(ids: &WastewaterIds, target_do: f64, rationale: &str) -> serde_json::Value {
    json!({
        "tank": ids.tank1,
        "sensor": ids.sensor1,
        "permit": ids.permit,
        "target_do": target_do,
        "rationale": rationale
    })
}

fn commit_setpoint(engine: &Engine, params: serde_json::Value) -> ActionOutcome {
    let proposed = engine
        .submit_action(&operator(), "propose_setpoint_change", params)
        .unwrap();
    assert_eq!(proposed.verdict, Verdict::Allow);
    engine
        .confirm_action(&supervisor(), proposed.inbox_id.as_deref().unwrap())
        .unwrap()
}

#[test]
fn does_let_t1_observe_and_refuse_submit() {
    let engine = Engine::memory().unwrap();
    let ids = install(&engine).unwrap();
    let tools = engine.list_tools(&intern()).unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"get_object"), "T1 must see get_object");
    assert!(
        names.contains(&"search_objects"),
        "T1 must see search_objects"
    );
    assert!(names.contains(&"traverse_links"));
    assert!(names.contains(&"aggregate"));
    assert!(names.contains(&"list_missing_evidence"));
    assert!(
        !names.contains(&"submit_action"),
        "T1 must not see submit_action"
    );
    assert!(
        !names.contains(&"confirm_action"),
        "T1 must not see confirm_action"
    );
    assert!(
        !names.iter().any(|n| n.starts_with("action.")),
        "T1 must not see action.* syscalls"
    );

    engine
        .get_object(&intern(), &ids.tank1, AsOf::Current)
        .expect("T1 can get_object");
    engine
        .search_objects(
            &intern(),
            Query {
                type_name: Some("AerationTank".into()),
                ..Query::default()
            },
        )
        .expect("T1 can search_objects");

    let denied = engine.submit_action(
        &intern(),
        "propose_setpoint_change",
        setpoint_params(&ids, 2.0, "intern"),
    );
    assert!(denied.is_err(), "T1 cannot submit_action");
    assert!(dispatch(
        &engine,
        &intern(),
        "submit_action",
        json!({
            "action": "propose_setpoint_change",
            "params": setpoint_params(&ids, 2.0, "intern-dispatch")
        }),
    )
    .is_err());
    assert!(dispatch(
        &engine,
        &intern(),
        "confirm_action",
        json!({ "inbox_id": "nope" }),
    )
    .is_err());
}

#[test]
fn does_let_t2_propose_and_refuse_confirm_or_override() {
    let engine = Engine::memory().unwrap();
    let ids = install(&engine).unwrap();
    let tools = engine.list_tools(&operator()).unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"submit_action"));
    assert!(names.contains(&"describe_action"));
    assert!(
        !names.contains(&"confirm_action"),
        "T2 must not see confirm_action"
    );
    assert!(
        !names.contains(&"override_action"),
        "T2 must not see override_action"
    );
    assert!(!names.contains(&"auto_action"));

    let proposed = engine
        .submit_action(
            &operator(),
            "propose_setpoint_change",
            setpoint_params(&ids, 2.3, "operator propose"),
        )
        .unwrap();
    assert_eq!(proposed.verdict, Verdict::Allow);
    let inbox = proposed.inbox_id.expect("inbox");
    assert!(engine.confirm_action(&operator(), &inbox).is_err());
    assert!(engine
        .override_action(&operator(), &inbox, "process_exception", "no")
        .is_err());
    assert!(dispatch(
        &engine,
        &operator(),
        "override_action",
        json!({
            "inbox_id": inbox,
            "category": "process_exception",
            "reason": "dispatch"
        }),
    )
    .is_err());
}

#[test]
fn does_let_t3_confirm_other_actor_not_self() {
    let engine = Engine::memory().unwrap();
    let ids = install(&engine).unwrap();
    let tools = engine.list_tools(&supervisor()).unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"confirm_action"));
    assert!(names.contains(&"override_action"));
    assert!(names.contains(&"list_inbox"));
    assert!(!names.contains(&"auto_action"));

    let proposed = engine
        .submit_action(
            &operator(),
            "propose_setpoint_change",
            setpoint_params(&ids, 2.4, "for chen"),
        )
        .unwrap();
    let inbox = proposed.inbox_id.expect("inbox");
    let confirmed = engine.confirm_action(&supervisor(), &inbox).unwrap();
    assert_eq!(confirmed.verdict, Verdict::Allow);

    let own = engine
        .submit_action(
            &supervisor(),
            "propose_setpoint_change",
            setpoint_params(&ids, 2.1, "self"),
        )
        .unwrap();
    assert_eq!(own.verdict, Verdict::Allow);
    let own_inbox = own.inbox_id.expect("own inbox");
    let self_confirm = engine.confirm_action(&supervisor(), &own_inbox);
    assert!(self_confirm.is_err(), "confirmer must not be the proposer");
}

#[test]
fn does_deny_t4_auto_if_bound_is_empty() {
    let engine = Engine::memory().unwrap();
    let ids = install(&engine).unwrap();
    let tools = engine.list_tools(&automator()).unwrap();
    assert!(
        tools.iter().any(|t| t.name == "auto_action"),
        "T4 must see auto_action"
    );
    let denied = engine.auto_action(
        &automator(),
        "request_sensor_calibration",
        "aeration_tanks",
        RiskBand::Low,
        json!({ "sensor": ids.sensor1 }),
    );
    assert!(denied.is_err(), "empty bound = no auto");
    assert!(dispatch(
        &engine,
        &automator(),
        "auto_action",
        json!({
            "action": "request_sensor_calibration",
            "object_set": "aeration_tanks",
            "risk_band": "low",
            "params": { "sensor": ids.sensor1 }
        }),
    )
    .is_err());
    assert!(dispatch(
        &engine,
        &supervisor(),
        "auto_action",
        json!({
            "action": "request_sensor_calibration",
            "object_set": "aeration_tanks",
            "risk_band": "low",
            "params": { "sensor": ids.sensor1 }
        }),
    )
    .is_err());
}

fn merge_branch(engine: &Engine, branch: &str) {
    let proposal = engine.submit_proposal(&modeller(), branch).unwrap();
    engine
        .review_proposal(&reviewer(), &proposal, true)
        .unwrap();
    engine.merge_to_main(&reviewer(), &proposal).unwrap();
}

fn propose_action(name: &str, interfaces: Vec<String>) -> ActionTypeSpec {
    ActionTypeSpec {
        name: name.into(),
        mode: ExecutionMode::Propose,
        parameters: vec![],
        guards: json!([]),
        required_roles: vec!["operator".into()],
        required_tier: AgentTier::T2,
        effects: json!([]),
        compensation: None,
        side_effects: json!({}),
        on_review: None,
        interfaces,
    }
}

#[test]
fn does_expose_kernel_types_as_queryable_objecttype_records() {
    let engine = Engine::memory().unwrap();
    let found = engine
        .search_objects(
            &intern(),
            Query {
                type_name: Some("ObjectType".into()),
                ..Query::default()
            },
        )
        .unwrap();
    let names: Vec<String> = found
        .iter()
        .map(|v| {
            v.properties
                .get("name")
                .and_then(|p| p.value.as_str())
                .unwrap_or(&v.id)
                .to_string()
        })
        .collect();
    for k in Engine::kernel_types() {
        assert!(
            names.iter().any(|n| n == k),
            "kernel type {k} must be a queryable ObjectType record after Engine::memory, got {names:?}"
        );
    }
}

#[test]
fn does_create_inbox_if_reviewable_and_ignore_unmerged_attach() {
    let engine = Engine::memory().unwrap();
    let b = engine
        .open_branch(&modeller(), "kernel-reviewable")
        .unwrap();
    engine
        .create_object_type(
            &modeller(),
            &b,
            ObjectTypeSpec {
                name: "SampleAsset".into(),
                typology: Typology::Entity,
                title_prop: Some("name".into()),
                interfaces: vec![],
                freshness_budget_secs: None,
                properties: vec![PropertySpec {
                    name: "name".into(),
                    value_type: "Text".into(),
                    source: PropertySource::Mapped,
                    nullable: true,
                    function: None,
                }],
            },
        )
        .unwrap();
    engine
        .create_action_type(&modeller(), &b, propose_action("propose_sample", vec![]))
        .unwrap();
    engine
        .attach_interface(&modeller(), &b, "propose_sample", "Reviewable")
        .unwrap();
    engine
        .create_action_type(&modeller(), &b, propose_action("propose_plain", vec![]))
        .unwrap();
    merge_branch(&engine, &b);

    let with_iface = engine
        .submit_action(&operator(), "propose_sample", json!({}))
        .unwrap();
    assert_eq!(with_iface.verdict, Verdict::Allow);
    let inbox = with_iface
        .inbox_id
        .expect("Reviewable Propose must create an inbox object");
    let pending = engine.list_inbox(&supervisor()).unwrap();
    assert!(
        pending.iter().any(|i| i.id == inbox),
        "inbox object must be listable"
    );

    let before = engine
        .submit_action(&operator(), "propose_plain", json!({ "n": 1 }))
        .unwrap();
    let before_inbox = &before.inbox_id;
    assert!(
        before.inbox_id.is_none(),
        "Propose without Reviewable on main must not mint inbox, got {before_inbox:?}"
    );

    let pending_b = engine
        .open_branch(&modeller(), "unmerged-reviewable")
        .unwrap();
    engine
        .attach_interface(&modeller(), &pending_b, "propose_plain", "Reviewable")
        .unwrap();
    let still = engine
        .submit_action(&operator(), "propose_plain", json!({ "n": 2 }))
        .unwrap();
    let still_inbox = &still.inbox_id;
    assert!(
        still.inbox_id.is_none(),
        "unmerged Reviewable attach must have zero effect on main, got {still_inbox:?}"
    );
}

#[test]
fn does_review_if_evidenced_submit_lacks_evidence() {
    let engine = Engine::memory().unwrap();
    install(&engine).unwrap();
    let b = engine.open_branch(&modeller(), "kernel-evidenced").unwrap();
    engine
        .create_object_type(
            &modeller(),
            &b,
            ObjectTypeSpec {
                name: "LabNote".into(),
                typology: Typology::Entity,
                title_prop: Some("name".into()),
                interfaces: vec!["Evidenced".into()],
                freshness_budget_secs: None,
                properties: vec![
                    PropertySpec {
                        name: "name".into(),
                        value_type: "Text".into(),
                        source: PropertySource::Mapped,
                        nullable: false,
                        function: None,
                    },
                    PropertySpec {
                        name: "rationale".into(),
                        value_type: "Text".into(),
                        source: PropertySource::Mapped,
                        nullable: true,
                        function: None,
                    },
                ],
            },
        )
        .unwrap();
    engine
        .create_action_type(
            &modeller(),
            &b,
            ActionTypeSpec {
                name: "inspect_note".into(),
                mode: ExecutionMode::Auto,
                parameters: vec![ParamSpec {
                    name: "note".into(),
                    value_type: "Text".into(),
                    object_type: Some("LabNote".into()),
                    required: true,
                }],
                guards: json!([]),
                required_roles: vec!["operator".into()],
                required_tier: AgentTier::T2,
                effects: json!([]),
                compensation: None,
                side_effects: json!({}),
                on_review: None,
                interfaces: vec![],
            },
        )
        .unwrap();
    merge_branch(&engine, &b);

    engine
        .funnel_ingest(
            &operator(),
            vec![IngestRecord {
                type_name: "LabNote".into(),
                id: Some("note-1".into()),
                properties: [("name".into(), json!("lab"))].into_iter().collect(),
                as_of: None,
                provenance: Some("test".into()),
            }],
        )
        .unwrap();

    let missing = engine.list_missing_evidence(&operator(), "note-1").unwrap();
    let missing_fields = missing
        .get("missing")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(
        missing_fields
            .iter()
            .any(|v| v.as_str() == Some("rationale")),
        "Evidenced required property must show in list_missing_evidence, got {missing}"
    );

    let out = engine
        .submit_action(&operator(), "inspect_note", json!({ "note": "note-1" }))
        .unwrap();
    assert_eq!(
        out.verdict,
        Verdict::Review,
        "submit that reads Evidenced object with missing rationale must Complete-fail, got {out:?}"
    );
    let reason = &out.reason;
    assert!(
        out.reason.contains("Complete fail"),
        "expected Complete fail, got {reason}"
    );
}
