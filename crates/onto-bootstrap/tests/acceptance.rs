use onto::{
    dispatch, Actor, AsOf, Engine, IngestRecord, KeyKind, ObjectSet, ObjectSetFilter,
    ObjectSetSpec, Query, Session, Verdict, WritePathStep,
};
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
fn restricted() -> Session {
    Session::new(
        Actor::consumer("ops.restricted", &["restricted"], 2),
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
fn kernel_types_exist_before_user_work() {
    assert!(Engine::kernel_types().contains(&"ObjectType"));
    assert!(Engine::kernel_types().contains(&"OntologyProposal"));
    assert!(Engine::kernel_types().contains(&"FunctionType"));
}

#[test]
fn runtime_create_type_then_consumer_tools_update() {
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
    let tank = engine
        .get_object(&operator(), &ids.tank1, onto::AsOf::Current)
        .unwrap();
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
}

#[test]
fn guard_fail_at_step_three_discards_stage() {
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
    assert!(engine.list_inbox(&operator()).unwrap().is_empty());
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
    assert_eq!(rec.effects["idempotency_key"], "setpoint:tank-1:2.6");
    assert_eq!(rec.proof_trace, WritePathStep::ALL.to_vec());
}

#[test]
fn wastewater_sensor_exposes_days_since_calibration() {
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
fn runtime_registered_function_visible_after_merge() {
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

#[test]
fn empty_object_set_is_ok_empty_vec() {
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
fn named_set_invisible_on_main_until_merge() {
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
                equals: Default::default(),
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
fn named_set_includes_new_match_excludes_non_match() {
    let engine = Engine::memory().unwrap();
    let ids = install(&engine).unwrap();
    merge_object_set(
        &engine,
        "set-live-tanks",
        ObjectSetSpec {
            name: "aeration_tanks".into(),
            type_name: Some("AerationTank".into()),
            equals: Default::default(),
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
fn restricted_cannot_see_rationale_in_set_results() {
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
fn aggregate_counts_filtered_set_not_whole_type() {
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
fn compensate_allow_is_inverse_action_not_rollback() {
    let engine = Engine::memory().unwrap();
    let ids = install(&engine).unwrap();
    let approved = engine
        .submit_action(
            &supervisor(),
            "approve_setpoint_change",
            json!({
                "tank": ids.tank1,
                "sensor": ids.sensor1,
                "permit": ids.permit,
                "target_do": 2.5,
                "rationale": "approve then compensate",
                "idempotency_key": "setpoint:tank-1:approve"
            }),
        )
        .unwrap();
    assert_eq!(approved.verdict, Verdict::Allow);
    let original_id = approved.decision_record_id.clone().expect("original record");
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
fn missing_compensation_name_is_error_not_silent_success() {
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
fn compensate_retry_same_key_does_not_double_apply() {
    let engine = Engine::memory().unwrap();
    let ids = install(&engine).unwrap();
    let approved = engine
        .submit_action(
            &supervisor(),
            "approve_setpoint_change",
            json!({
                "tank": ids.tank1,
                "sensor": ids.sensor1,
                "permit": ids.permit,
                "target_do": 2.5,
                "rationale": "idempotent compensate",
                "idempotency_key": "setpoint:tank-1:comp-src"
            }),
        )
        .unwrap();
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
fn compensate_deny_is_not_compensable() {
    let engine = Engine::memory().unwrap();
    let ids = install(&engine).unwrap();
    let denied = engine
        .submit_action(
            &supervisor(),
            "approve_setpoint_change",
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
    assert_eq!(still.action_name, "approve_setpoint_change");
}
