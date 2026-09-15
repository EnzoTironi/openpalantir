use onto::{dispatch, Actor, AsOf, Engine, KeyKind, Query, Session, Verdict, WritePathStep};
use onto_bootstrap::install;
use serde_json::json;

fn modeller() -> Session {
    Session::new(Actor::builder("human.modeler", &["modeler"]), "test")
}
fn reviewer() -> Session {
    Session::new(Actor::builder("human.reviewer", &["reviewer"]), "test")
}
fn operator() -> Session {
    Session::new(Actor::consumer("ops.maya", &["operator"], 2), "test")
}
fn supervisor() -> Session {
    Session::new(
        Actor::consumer("ops.chen", &["supervisor", "operator"], 3),
        "test",
    )
}
fn intern() -> Session {
    Session::new(Actor::consumer("ops.intern", &[], 1), "test")
}

#[test]
fn kernel_types_exist_before_user_work() {
    assert!(Engine::kernel_types().contains(&"ObjectType"));
    assert!(Engine::kernel_types().contains(&"OntologyProposal"));
}

#[test]
fn runtime_create_type_then_consumer_tools_update() {
    let engine = Engine::memory().unwrap();
    let before = engine.list_tools(&operator()).unwrap();
    assert!(
        !before.iter().any(|t| t.name == "action.propose_setpoint_change"),
        "action must not exist before merge"
    );
    install(&engine).unwrap();
    let after = engine.list_tools(&operator()).unwrap();
    assert!(after
        .iter()
        .any(|t| t.name == "action.propose_setpoint_change"));
}

#[test]
fn branch_alter_invisible_until_merge() {
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
    assert_eq!(main_before, main_mid, "unmerged branch must not affect main");

    let proposal = engine.submit_proposal(&modeller(), &b).unwrap();
    engine.review_proposal(&reviewer(), &proposal, true).unwrap();
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
fn key_isolation() {
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
fn wastewater_happy_path() {
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
    let tank = engine.get_object(&operator(), &ids.tank1, onto::AsOf::Current).unwrap();
    assert_eq!(tank.properties["target_do"].value, json!(2.5));
    assert_eq!(
        tank.properties["target_do"].source,
        onto::PropertySource::ActionWritten
    );
    let rec = engine
        .get_decision_record(&operator(), confirmed.decision_record_id.as_deref().unwrap())
        .unwrap();
    assert_eq!(rec.verdict, Verdict::Allow);
    assert_eq!(rec.engine_version, onto::ENGINE_VERSION);
}

#[test]
fn stale_sensor_review_with_alternative() {
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
fn permit_limit_and_unauthorized_deny() {
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

    let unauth = engine
        .submit_action(
            &intern(),
            "propose_setpoint_change",
            json!({
                "tank": ids.tank1,
                "sensor": ids.sensor1,
                "permit": ids.permit,
                "target_do": 2.0,
                "rationale": "intern"
            }),
        )
        .unwrap();
    assert_eq!(unauth.verdict, Verdict::Deny);
}

#[test]
fn override_replayable_dossier() {
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
fn funnel_does_not_overwrite_action_written() {
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
    let tank = engine.get_object(&operator(), &ids.tank1, onto::AsOf::Current).unwrap();
    assert_eq!(tank.properties["target_do"].value, json!(3.1));
    assert_eq!(tank.properties["current_do"].value, json!(1.1));
}

#[test]
fn unmerged_branch_zero_effect_on_production() {
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
fn consumer_cannot_read_working_branch_schema() {
    let engine = Engine::memory().unwrap();
    install(&engine).unwrap();
    let b = engine.open_branch(&modeller(), "secret").unwrap();
    let err = engine.get_schema(&operator(), Some(&b));
    assert!(err.is_err());
}

#[test]
fn builder_and_consumer_keys_are_distinct() {
    assert_ne!(KeyKind::Builder, KeyKind::Consumer);
}

#[test]
fn write_path_seven_steps_in_order() {
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
    let rec = engine
        .get_decision_record(
            &operator(),
            proposed.decision_record_id.as_deref().unwrap(),
        )
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
}

#[test]
fn guard_fail_at_step_three_discards_stage() {
    let engine = Engine::memory().unwrap();
    let ids = install(&engine).unwrap();
    let tank_before = engine.get_object(&operator(), &ids.tank1, onto::AsOf::Current).unwrap();
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
    assert!(engine.list_inbox(&operator()).unwrap().is_empty());
    let tank_after = engine.get_object(&operator(), &ids.tank1, onto::AsOf::Current).unwrap();
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
fn decision_snapshot_pins_reads_and_versions() {
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
        .get_decision_record(
            &operator(),
            proposed.decision_record_id.as_deref().unwrap(),
        )
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
    assert_eq!(
        rec.data_snapshot["function_version"],
        onto::FUNCTION_VERSION
    );
    let rule = rec.data_snapshot["rule_version"]
        .as_str()
        .expect("rule_version");
    assert!(
        rule.starts_with("propose_setpoint_change:"),
        "rule_version pins the action spec, got {rule}"
    );
    assert_eq!(rec.engine_version, onto::ENGINE_VERSION);
    assert_eq!(rec.function_version, onto::FUNCTION_VERSION);
}

#[test]
fn idempotent_side_effect_key_does_not_double_apply() {
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
    let first = engine
        .submit_action(&supervisor(), "approve_setpoint_change", params.clone())
        .unwrap();
    assert_eq!(first.verdict, Verdict::Allow);
    let second = engine
        .submit_action(&supervisor(), "approve_setpoint_change", params)
        .unwrap();
    assert_eq!(first.decision_record_id, second.decision_record_id);
    assert_eq!(first.created_ids, second.created_ids);
    let tank = engine.get_object(&operator(), &ids.tank1, onto::AsOf::Current).unwrap();
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
        .get_decision_record(
            &operator(),
            first.decision_record_id.as_deref().unwrap(),
        )
        .unwrap();
    assert_eq!(
        rec.effects["idempotency_key"],
        "setpoint:tank-1:2.6"
    );
    assert_eq!(rec.proof_trace, WritePathStep::ALL.to_vec());
}

#[test]
fn sequential_setpoint_writes_append_versions_and_as_of_reads_history() {
    let engine = Engine::memory().unwrap();
    let ids = install(&engine).unwrap();
    let spans_seed = engine.object_spans(&ids.tank1).unwrap();
    assert_eq!(spans_seed.len(), 1, "funnel seed is the first version");
    assert!(spans_seed[0].is_open());

    let first = engine
        .submit_action(
            &supervisor(),
            "approve_setpoint_change",
            json!({
                "tank": ids.tank1,
                "sensor": ids.sensor1,
                "permit": ids.permit,
                "target_do": 2.5,
                "rationale": "first setpoint",
                "idempotency_key": "setpoint:tank-1:first"
            }),
        )
        .unwrap();
    assert_eq!(first.verdict, Verdict::Allow);
    let after_first = engine.now();
    let tank_first = engine
        .get_object(&operator(), &ids.tank1, AsOf::Current)
        .unwrap();
    assert_eq!(tank_first.properties["target_do"].value, json!(2.5));

    engine.set_clock(after_first + 10);
    let second = engine
        .submit_action(
            &supervisor(),
            "approve_setpoint_change",
            json!({
                "tank": ids.tank1,
                "sensor": ids.sensor1,
                "permit": ids.permit,
                "target_do": 3.0,
                "rationale": "second setpoint",
                "idempotency_key": "setpoint:tank-1:second"
            }),
        )
        .unwrap();
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
