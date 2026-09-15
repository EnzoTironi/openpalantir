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
