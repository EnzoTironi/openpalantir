//! Named Zhang 2026 wastewater teaching cases.
//!
//! Each test fails if the mechanism is deleted or stubbed (always-Allow,
//! Funnel overwrite, self-override, unmerged schema leak).

use onto::{
    Actor, AgentTier, AsOf, Engine, IngestRecord, ObjectTypeSpec, PropertySource, PropertySpec,
    Query, Result, Session, Typology, Verdict, WritePathStep,
};
use onto_bootstrap::{install, WastewaterIds};
use serde_json::json;

fn operator() -> Session {
    Session::new(
        Actor::consumer("ops.maya", &["operator"], AgentTier::T2),
        "case",
    )
}

fn supervisor() -> Session {
    Session::new(
        Actor::consumer("ops.chen", &["supervisor", "operator"], AgentTier::T3),
        "case",
    )
}

fn setpoint(
    ids: &WastewaterIds,
    sensor: &str,
    target_do: f64,
    rationale: &str,
) -> serde_json::Value {
    json!({
        "tank": ids.tank1,
        "sensor": sensor,
        "permit": ids.permit,
        "target_do": target_do,
        "rationale": rationale
    })
}

fn seed_sensor_without_reading(engine: &Engine, id: &str) -> Result<()> {
    engine.funnel_ingest(
        &operator(),
        vec![IngestRecord {
            type_name: "DO_Sensor".into(),
            id: Some(id.into()),
            properties: [
                ("name".into(), json!("DO-missing")),
                ("calibration_date".into(), json!(engine.now() - 10 * 86_400)),
            ]
            .into_iter()
            .collect(),
            as_of: Some(engine.now().to_string()),
            provenance: Some("case:missing".into()),
        }],
    )?;
    Ok(())
}

#[test]
fn does_review_and_name_calibration_if_sensor_is_stale() {
    let engine = Engine::memory().unwrap();
    let ids = install(&engine).unwrap();
    engine.set_clock(engine.now() + 10_000);
    let evidence = engine
        .list_missing_evidence(&operator(), &ids.sensor1)
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
            "propose_setpoint_change",
            setpoint(&ids, &ids.sensor1, 2.2, "stale path"),
        )
        .unwrap();
    assert_eq!(out.verdict, Verdict::Review);
    assert!(out.inbox_id.is_none(), "Review must not open an inbox");
    assert_eq!(
        out.alternative.as_deref(),
        Some("request_sensor_calibration")
    );
    assert!(
        out.guard_results
            .iter()
            .any(|g| g.name == "freshness" && g.verdict == Verdict::Review),
        "stale path must be the freshness guard, got {:?}",
        out.guard_results
    );

    let alt = engine
        .submit_action(
            &operator(),
            "request_sensor_calibration",
            json!({ "sensor": ids.sensor1 }),
        )
        .unwrap();
    assert_eq!(alt.verdict, Verdict::Allow);
    let requests = engine
        .search_objects(
            &operator(),
            Query {
                type_name: Some("CalibrationRequest".into()),
                ..Query::default()
            },
        )
        .unwrap();
    assert!(
        !requests.is_empty(),
        "replacement proposal must create a CalibrationRequest, not a stub Allow"
    );
}

#[test]
fn does_review_and_name_calibration_if_sensor_reading_is_missing() {
    let engine = Engine::memory().unwrap();
    let ids = install(&engine).unwrap();
    seed_sensor_without_reading(&engine, "sensor-missing").unwrap();
    let view = engine
        .get_object(&operator(), "sensor-missing", AsOf::Current)
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

    let out = engine
        .submit_action(
            &operator(),
            "propose_setpoint_change",
            setpoint(&ids, "sensor-missing", 2.2, "missing reading"),
        )
        .unwrap();
    assert_eq!(out.verdict, Verdict::Review);
    assert!(out.inbox_id.is_none());
    assert_eq!(
        out.alternative.as_deref(),
        Some("request_sensor_calibration")
    );
    assert!(
        out.guard_results.iter().any(|g| {
            g.verdict == Verdict::Review && (g.name == "complete" || g.name == "freshness")
        }),
        "missing reading must Review on complete/freshness, got {:?}",
        out.guard_results
    );

    let alt = engine
        .submit_action(
            &operator(),
            "request_sensor_calibration",
            json!({ "sensor": "sensor-missing" }),
        )
        .unwrap();
    assert_eq!(alt.verdict, Verdict::Allow);
    let requests = engine
        .search_objects(
            &operator(),
            Query {
                type_name: Some("CalibrationRequest".into()),
                ..Query::default()
            },
        )
        .unwrap();
    assert!(!requests.is_empty());
}

#[test]
fn does_deny_if_target_do_exceeds_permit() {
    let engine = Engine::memory().unwrap();
    let ids = install(&engine).unwrap();
    let tank_before = engine
        .get_object(&operator(), &ids.tank1, AsOf::Current)
        .unwrap();
    let over = engine
        .submit_action(
            &operator(),
            "propose_setpoint_change",
            setpoint(&ids, &ids.sensor1, 9.0, "too high"),
        )
        .unwrap();
    assert_eq!(over.verdict, Verdict::Deny);
    assert!(over.inbox_id.is_none());
    assert!(over.created_ids.is_empty());
    assert!(
        over.guard_results
            .iter()
            .any(|g| g.name == "permit_limit" && g.verdict == Verdict::Deny),
        "permit-limit Deny must come from the permit_limit guard, got {:?}",
        over.guard_results
    );
    let rec = engine
        .get_decision_record(&operator(), over.decision_record_id.as_deref().unwrap())
        .unwrap();
    assert_eq!(rec.verdict, Verdict::Deny);
    assert_eq!(rec.action_name, "propose_setpoint_change");
    let tank_after = engine
        .get_object(&operator(), &ids.tank1, AsOf::Current)
        .unwrap();
    assert_eq!(
        tank_before.properties.get("target_do").map(|p| &p.value),
        tank_after.properties.get("target_do").map(|p| &p.value),
        "Deny must not write target_do"
    );
}

#[test]
fn does_deny_if_actor_lacks_role() {
    let engine = Engine::memory().unwrap();
    let ids = install(&engine).unwrap();
    let no_role = Session::new(Actor::consumer("ops.norole", &[], AgentTier::T2), "case");
    let unauth = engine
        .submit_action(
            &no_role,
            "propose_setpoint_change",
            setpoint(&ids, &ids.sensor1, 2.0, "no role"),
        )
        .unwrap();
    assert_eq!(unauth.verdict, Verdict::Deny);
    assert!(unauth.inbox_id.is_none());
    assert!(
        unauth
            .guard_results
            .iter()
            .any(|g| g.name == "authorization" && g.verdict == Verdict::Deny),
        "unauthorized Deny must be the authorization guard, got {:?}",
        unauth.guard_results
    );
    let rec = engine
        .get_decision_record(&operator(), unauth.decision_record_id.as_deref().unwrap())
        .unwrap();
    assert_eq!(rec.verdict, Verdict::Deny);
}

#[test]
#[allow(clippy::too_many_lines)] // dossier replay is one end-to-end teaching case
fn does_replay_override_dossier_if_confirmer_is_not_executor() {
    let engine = Engine::memory().unwrap();
    let ids = install(&engine).unwrap();
    let proposed = engine
        .submit_action(
            &operator(),
            "propose_setpoint_change",
            setpoint(&ids, &ids.sensor1, 2.4, "override path"),
        )
        .unwrap();
    assert_eq!(proposed.verdict, Verdict::Allow);
    let inbox = proposed.inbox_id.clone().expect("inbox");
    let proposer_id = operator().actor.id;

    let self_over = engine.override_action(
        &operator(),
        &inbox,
        "process_exception",
        "operator cannot override",
    );
    assert!(
        self_over.is_err(),
        "T2 executor must not override their own proposal"
    );

    let own = engine
        .submit_action(
            &supervisor(),
            "propose_setpoint_change",
            setpoint(&ids, &ids.sensor1, 2.1, "self override"),
        )
        .unwrap();
    let own_inbox = own.inbox_id.expect("own inbox");
    let self_supervisor =
        engine.override_action(&supervisor(), &own_inbox, "process_exception", "same actor");
    assert!(
        self_supervisor.is_err(),
        "confirmer must not be the executor/proposer, got {self_supervisor:?}"
    );

    let over = engine
        .override_action(&supervisor(), &inbox, "process_exception", "foam event")
        .unwrap();
    assert_eq!(over.verdict, Verdict::Allow);
    let rec_id = over.decision_record_id.clone().expect("override record");
    let rec = engine.get_decision_record(&operator(), &rec_id).unwrap();
    let supervisor_id = supervisor().actor.id;
    assert_eq!(rec.action_name, "override_setpoint");
    assert_eq!(rec.verdict, Verdict::Allow);
    assert_eq!(rec.actor, supervisor_id);
    assert_eq!(rec.confirmer.as_deref(), Some(supervisor_id.as_str()));
    assert_ne!(
        rec.confirmer.as_deref(),
        Some(proposer_id.as_str()),
        "confirmer must differ from the original executor"
    );
    assert_eq!(rec.params["override_category"], "process_exception");
    assert_eq!(rec.params["override_reason"], "foam event");
    assert_eq!(rec.params["proposed_by"], proposer_id);
    assert_eq!(rec.params["source_action"], "propose_setpoint_change");
    assert_eq!(rec.proof_trace, WritePathStep::ALL.to_vec());
    assert_eq!(rec.engine_version, onto::ENGINE_VERSION);

    let created = engine
        .search_objects(
            &operator(),
            Query {
                type_name: Some("Override".into()),
                ..Query::default()
            },
        )
        .unwrap();
    assert!(
        !created.is_empty(),
        "override must create an Override object through the write path"
    );
    assert_eq!(
        created[0].properties["category"].value,
        json!("process_exception")
    );
    assert_eq!(created[0].properties["reason"].value, json!("foam event"));
    assert_eq!(
        created[0].properties["category"].source,
        PropertySource::ActionWritten
    );

    engine.set_clock(engine.now() + 60);
    engine
        .funnel_ingest(
            &operator(),
            vec![IngestRecord {
                type_name: "AerationTank".into(),
                id: Some(ids.tank1.clone()),
                properties: [("current_do".into(), json!(0.4))].into_iter().collect(),
                as_of: Some(engine.now().to_string()),
                provenance: Some("after-override".into()),
            }],
        )
        .unwrap();
    let replayed = engine.get_decision_record(&operator(), &rec_id).unwrap();
    assert_eq!(replayed.id, rec.id);
    assert_eq!(replayed.params, rec.params);
    assert_eq!(replayed.verdict, rec.verdict);
    assert_eq!(replayed.data_snapshot, rec.data_snapshot);
    assert_eq!(replayed.actor, rec.actor);
    assert_eq!(replayed.confirmer, rec.confirmer);
    assert_eq!(replayed.created_at, rec.created_at);
}

#[test]
fn does_keep_action_written_target_do_if_funnel_ingests() {
    let engine = Engine::memory().unwrap();
    let ids = install(&engine).unwrap();
    let proposed = engine
        .submit_action(
            &operator(),
            "propose_setpoint_change",
            setpoint(&ids, &ids.sensor1, 3.1, "set"),
        )
        .unwrap();
    engine
        .confirm_action(&supervisor(), proposed.inbox_id.as_deref().unwrap())
        .unwrap();
    let after_action = engine
        .get_object(&operator(), &ids.tank1, AsOf::Current)
        .unwrap();
    assert_eq!(after_action.properties["target_do"].value, json!(3.1));
    assert_eq!(
        after_action.properties["target_do"].source,
        PropertySource::ActionWritten
    );

    engine
        .funnel_ingest(
            &operator(),
            vec![IngestRecord {
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
        .get_object(&operator(), &ids.tank1, AsOf::Current)
        .unwrap();
    assert_eq!(
        tank.properties["target_do"].value,
        json!(3.1),
        "Funnel must not overwrite ActionWritten target_do"
    );
    assert_eq!(
        tank.properties["target_do"].source,
        PropertySource::ActionWritten
    );
    assert_eq!(tank.properties["current_do"].value, json!(1.1));
    assert_eq!(
        tank.properties["current_do"].source,
        PropertySource::Mapped,
        "Mapped current_do must still update so the skip is not a total no-op"
    );
}

#[test]
fn does_leave_production_instances_unchanged_if_schema_is_unmerged() {
    let engine = Engine::memory().unwrap();
    let ids = install(&engine).unwrap();
    let modeller = Session::new(Actor::builder("human.modeler", &["modeler"]), "case");
    let tanks_before = engine
        .search_objects(
            &operator(),
            Query {
                type_name: Some("AerationTank".into()),
                ..Query::default()
            },
        )
        .unwrap()
        .len();
    let instance_before = engine
        .get_object(&operator(), &ids.tank1, AsOf::Current)
        .unwrap();

    let b = engine.open_branch(&modeller, "ghost-case").unwrap();
    engine
        .create_object_type(
            &modeller,
            &b,
            ObjectTypeSpec {
                name: "GhostAsset".into(),
                typology: Typology::Entity,
                title_prop: Some("name".into()),
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

    let schema = engine.get_schema(&operator(), None).unwrap();
    assert!(!schema.object_types.iter().any(|t| t.name == "GhostAsset"));
    let ingest = engine.funnel_ingest(
        &operator(),
        vec![IngestRecord {
            type_name: "GhostAsset".into(),
            id: Some("ghost-1".into()),
            properties: [("name".into(), json!("nope"))].into_iter().collect(),
            as_of: Some(engine.now().to_string()),
            provenance: Some("unmerged".into()),
        }],
    );
    assert!(
        ingest.is_err(),
        "unmerged type must not accept production Funnel writes, got {ingest:?}"
    );
    let tanks_after = engine
        .search_objects(
            &operator(),
            Query {
                type_name: Some("AerationTank".into()),
                ..Query::default()
            },
        )
        .unwrap();
    assert_eq!(tanks_before, tanks_after.len());
    let instance_after = engine
        .get_object(&operator(), &ids.tank1, AsOf::Current)
        .unwrap();
    assert_eq!(
        serde_json::to_value(&instance_before.properties).unwrap(),
        serde_json::to_value(&instance_after.properties).unwrap()
    );
    assert!(engine
        .get_object(&operator(), "ghost-1", AsOf::Current)
        .is_err());
}
