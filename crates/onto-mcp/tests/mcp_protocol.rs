use onto::{dispatch, Actor, AgentTier, Engine, Session};
use onto_bootstrap::install;
use serde_json::json;

#[test]
fn does_project_consumer_tools_from_live_oms() {
    let engine = Engine::memory().unwrap();
    install(&engine).unwrap();
    let session = Session::new(
        Actor::consumer("agent", &["operator"], AgentTier::T2),
        "mcp",
    );
    let listed = dispatch(&engine, &session, "list_tools", json!({})).unwrap();
    let names: Vec<String> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert!(names.contains(&"get_object".into()));
    assert!(names.contains(&"action.propose_setpoint_change".into()));
    assert!(!names.contains(&"create_object_type".into()));
    assert!(!names.contains(&"create_link".into()));
}

#[test]
fn does_exclude_production_reads_from_builder_tools() {
    let engine = Engine::memory().unwrap();
    let session = Session::new(Actor::builder("ke", &["modeler"]), "mcp");
    let listed = dispatch(&engine, &session, "list_tools", json!({})).unwrap();
    let names: Vec<String> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert!(names.contains(&"create_object_type".into()));
    assert!(names.contains(&"create_function".into()));
    assert!(names.contains(&"merge_to_main".into()));
    assert!(!names.contains(&"get_object".into()));
}

#[test]
fn does_propose_via_mcp_dispatch() {
    let engine = Engine::memory().unwrap();
    let ids = install(&engine).unwrap();
    let session = Session::new(
        Actor::consumer("agent", &["operator"], AgentTier::T2),
        "mcp",
    );
    let out = dispatch(
        &engine,
        &session,
        "submit_action",
        json!({
            "action": "propose_setpoint_change",
            "params": {
                "tank": ids.tank1,
                "sensor": ids.sensor1,
                "permit": ids.permit,
                "target_do": 2.0,
                "rationale": "mcp"
            }
        }),
    )
    .unwrap();
    assert_eq!(out["verdict"], "allow");
    assert!(out["inbox_id"].as_str().is_some());
}

#[test]
fn does_include_write_path_trace_on_mcp_decision_record() {
    let engine = Engine::memory().unwrap();
    let ids = install(&engine).unwrap();
    let session = Session::new(
        Actor::consumer("agent", &["operator"], AgentTier::T2),
        "mcp",
    );
    let out = dispatch(
        &engine,
        &session,
        "submit_action",
        json!({
            "action": "propose_setpoint_change",
            "params": {
                "tank": ids.tank1,
                "sensor": ids.sensor1,
                "permit": ids.permit,
                "target_do": 2.0,
                "rationale": "mcp-trace"
            }
        }),
    )
    .unwrap();
    let rec = dispatch(
        &engine,
        &session,
        "get_decision_record",
        json!({ "id": out["decision_record_id"] }),
    )
    .unwrap();
    let trace = rec["proof_trace"].as_array().expect("proof_trace");
    assert_eq!(
        serde_json::Value::Array(trace.clone()),
        serde_json::json!([
            "submit",
            "param_and_permission",
            "submission_criteria",
            "staged_edits",
            "commit",
            "seal_decision_record",
            "declare_side_effects"
        ])
    );
    assert!(rec["data_snapshot"]["objects"]
        .as_array()
        .unwrap()
        .iter()
        .any(|o| { o["id"] == ids.sensor1 }));
    assert_eq!(rec["data_snapshot"]["engine_version"], onto::ENGINE_VERSION);
}

#[test]
fn does_separate_mcp_keys_on_write_path() {
    let engine = Engine::memory().unwrap();
    install(&engine).unwrap();
    let builder = Session::new(Actor::builder("ke", &["modeler"]), "mcp");
    let denied = dispatch(
        &engine,
        &builder,
        "submit_action",
        json!({
            "action": "propose_setpoint_change",
            "params": { "tank": "tank-1" }
        }),
    );
    assert!(
        denied.is_err(),
        "builder key cannot submit production actions"
    );
}

fn operator() -> Session {
    Session::new(
        Actor::consumer("ops.maya", &["operator"], AgentTier::T2),
        "mcp",
    )
}

fn supervisor() -> Session {
    Session::new(
        Actor::consumer("ops.chen", &["supervisor", "operator"], AgentTier::T3),
        "mcp",
    )
}

#[test]
fn does_review_via_mcp_if_sensor_is_stale() {
    let engine = Engine::memory().unwrap();
    let ids = install(&engine).unwrap();
    engine.set_clock(engine.now() + 10_000);
    let out = dispatch(
        &engine,
        &operator(),
        "submit_action",
        json!({
            "action": "propose_setpoint_change",
            "params": {
                "tank": ids.tank1,
                "sensor": ids.sensor1,
                "permit": ids.permit,
                "target_do": 2.2,
                "rationale": "mcp-stale"
            }
        }),
    )
    .unwrap();
    assert_eq!(out["verdict"], "review");
    assert_eq!(out["alternative"], "request_sensor_calibration");
    assert!(out["inbox_id"].is_null());
}

#[test]
fn does_deny_via_mcp_if_permit_exceeded() {
    let engine = Engine::memory().unwrap();
    let ids = install(&engine).unwrap();
    let out = dispatch(
        &engine,
        &operator(),
        "submit_action",
        json!({
            "action": "propose_setpoint_change",
            "params": {
                "tank": ids.tank1,
                "sensor": ids.sensor1,
                "permit": ids.permit,
                "target_do": 9.0,
                "rationale": "mcp-permit"
            }
        }),
    )
    .unwrap();
    assert_eq!(out["verdict"], "deny");
    assert!(out["inbox_id"].is_null());
}

#[test]
fn does_reject_self_override_and_replay_dossier_via_mcp() {
    let engine = Engine::memory().unwrap();
    let ids = install(&engine).unwrap();
    let proposed = dispatch(
        &engine,
        &operator(),
        "submit_action",
        json!({
            "action": "propose_setpoint_change",
            "params": {
                "tank": ids.tank1,
                "sensor": ids.sensor1,
                "permit": ids.permit,
                "target_do": 2.4,
                "rationale": "mcp-override"
            }
        }),
    )
    .unwrap();
    let inbox = proposed["inbox_id"].as_str().unwrap().to_string();
    let own = dispatch(
        &engine,
        &supervisor(),
        "submit_action",
        json!({
            "action": "propose_setpoint_change",
            "params": {
                "tank": ids.tank1,
                "sensor": ids.sensor1,
                "permit": ids.permit,
                "target_do": 2.1,
                "rationale": "mcp-self"
            }
        }),
    )
    .unwrap();
    let own_inbox = own["inbox_id"].as_str().unwrap();
    let denied = dispatch(
        &engine,
        &supervisor(),
        "override_action",
        json!({
            "inbox_id": own_inbox,
            "category": "process_exception",
            "reason": "same actor"
        }),
    );
    assert!(
        denied.is_err(),
        "MCP override must refuse confirmer == proposer"
    );

    let over = dispatch(
        &engine,
        &supervisor(),
        "override_action",
        json!({
            "inbox_id": inbox,
            "category": "process_exception",
            "reason": "foam event"
        }),
    )
    .unwrap();
    assert_eq!(over["verdict"], "allow");
    let rec = dispatch(
        &engine,
        &operator(),
        "get_decision_record",
        json!({ "id": over["decision_record_id"] }),
    )
    .unwrap();
    assert_eq!(rec["action_name"], "override_setpoint");
    assert_eq!(rec["actor"], "ops.chen");
    assert_eq!(rec["confirmer"], "ops.chen");
    assert_eq!(rec["params"]["proposed_by"], "ops.maya");
}

#[test]
fn does_keep_action_written_if_mcp_funnel_ingests() {
    let engine = Engine::memory().unwrap();
    let ids = install(&engine).unwrap();
    let proposed = dispatch(
        &engine,
        &operator(),
        "submit_action",
        json!({
            "action": "propose_setpoint_change",
            "params": {
                "tank": ids.tank1,
                "sensor": ids.sensor1,
                "permit": ids.permit,
                "target_do": 3.1,
                "rationale": "mcp-funnel"
            }
        }),
    )
    .unwrap();
    dispatch(
        &engine,
        &supervisor(),
        "confirm_action",
        json!({ "inbox_id": proposed["inbox_id"] }),
    )
    .unwrap();
    dispatch(
        &engine,
        &operator(),
        "funnel_ingest",
        json!({
            "records": [{
                "type_name": "AerationTank",
                "id": ids.tank1,
                "properties": {
                    "name": "Basin 1",
                    "current_do": 1.1,
                    "target_do": 0.1
                },
                "as_of": engine.now().to_string(),
                "provenance": "mcp-nightly"
            }]
        }),
    )
    .unwrap();
    let tank = dispatch(
        &engine,
        &operator(),
        "get_object",
        json!({ "id": ids.tank1 }),
    )
    .unwrap();
    assert_eq!(tank["properties"]["target_do"]["value"], 3.1);
    assert_eq!(tank["properties"]["target_do"]["source"], "action_written");
    assert_eq!(tank["properties"]["current_do"]["value"], 1.1);
}
