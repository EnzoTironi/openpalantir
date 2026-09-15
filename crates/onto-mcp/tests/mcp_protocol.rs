use onto::{dispatch, Actor, Engine, Session};
use onto_bootstrap::install;
use serde_json::json;

#[test]
fn consumer_tools_projected_from_live_oms() {
    let engine = Engine::memory().unwrap();
    install(&engine).unwrap();
    let session = Session::new(Actor::consumer("agent", &["operator"], 2), "mcp");
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
fn builder_tools_exclude_production_reads() {
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
fn mcp_style_call_propose() {
    let engine = Engine::memory().unwrap();
    let ids = install(&engine).unwrap();
    let session = Session::new(Actor::consumer("agent", &["operator"], 2), "mcp");
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
fn mcp_decision_record_has_write_path_trace() {
    let engine = Engine::memory().unwrap();
    let ids = install(&engine).unwrap();
    let session = Session::new(Actor::consumer("agent", &["operator"], 2), "mcp");
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
fn mcp_keys_stay_separated_on_write_path() {
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
