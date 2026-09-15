//! Compiled safety regressions from the kernel review pack.
//! Names stay `r0x_*` so a deleted mechanism fails this file, not a rename.

use onto::{
    dispatch, ActionOutcome, ActionTypeSpec, Actor, AgentTier, AsOf, AuthzDecision, AuthzLevel,
    AuthzOp, Engine, ExecutionMode, IngestRecord, KeyKind, ObjectTypeSpec, ParamSpec, PolicySpec,
    PropertySource, PropertySpec, Query, Session, Typology, Verdict,
};
use serde_json::{json, Value};

fn builder() -> Session {
    Session::new(Actor::builder("review.builder", &["modeler"]), "review")
}
fn reviewer() -> Session {
    Session::new(Actor::builder("review.reviewer", &["reviewer"]), "review")
}
fn ops() -> Session {
    Session::new(
        Actor::consumer("review.operator", &["operator"], AgentTier::T2),
        "review",
    )
}
fn boss() -> Session {
    Session::new(
        Actor::consumer(
            "review.supervisor",
            &["supervisor", "operator"],
            AgentTier::T3,
        ),
        "review",
    )
}
fn blocked(result: onto::Result<ActionOutcome>) -> bool {
    match result {
        Err(_) => true,
        Ok(out) => out.verdict != Verdict::Allow,
    }
}
fn merge(e: &Engine, branch: &str) -> onto::Result<()> {
    let p = e.submit_proposal(&builder(), branch)?;
    e.review_proposal(&reviewer(), &p, true)?;
    e.merge_to_main(&reviewer(), &p)
}
fn spec(name: &str, mode: ExecutionMode) -> ActionTypeSpec {
    ActionTypeSpec {
        name: name.into(),
        mode,
        parameters: vec![],
        guards: json!([]),
        required_roles: vec![],
        required_tier: AgentTier::T2,
        effects: json!([]),
        compensation: None,
        side_effects: json!({}),
        on_review: None,
        interfaces: vec![],
    }
}
fn register(e: &Engine, action: ActionTypeSpec) -> onto::Result<()> {
    let branch = format!("review-{}", action.name);
    e.open_branch(&builder(), &branch)?;
    e.create_action_type(&builder(), &branch, action)?;
    merge(e, &branch)
}
fn object_type(name: &str) -> ObjectTypeSpec {
    ObjectTypeSpec {
        name: name.into(),
        typology: Typology::Entity,
        title_prop: None,
        interfaces: vec![],
        freshness_budget_secs: None,
        properties: vec![],
    }
}
fn setpoint(ids: &onto_bootstrap::WastewaterIds) -> Value {
    json!({
        "tank": ids.tank1,
        "sensor": ids.sensor1,
        "permit": ids.permit,
        "target_do": 2.0,
        "rationale": "review"
    })
}

#[test]
fn r01_unknown_guard_cannot_become_allow() {
    let e = Engine::memory().unwrap();
    let mut action = spec("unrecognized_guard", ExecutionMode::Propose);
    action.guards = json!([{ "must_never_be_accepted": true }]);
    if register(&e, action).is_err() {
        return;
    }
    assert!(
        blocked(e.submit_action(&ops(), "unrecognized_guard", json!({}))),
        "Unknown guards must fail closed"
    );
}

#[test]
fn r01_missing_bound_cannot_mean_infinity() {
    let e = Engine::memory().unwrap();
    let ids = onto_bootstrap::install(&e).unwrap();
    let mut action = spec("missing_bound", ExecutionMode::Propose);
    action.parameters = vec![
        ParamSpec {
            name: "permit".into(),
            value_type: "Text".into(),
            object_type: Some("PermitVersion".into()),
            required: true,
        },
        ParamSpec {
            name: "n".into(),
            value_type: "DOConcentration".into(),
            object_type: None,
            required: true,
        },
    ];
    action.guards = json!([{ "lte_field": "n", "object": "permit", "field": "unrecorded_limit" }]);
    if register(&e, action).is_err() {
        return;
    }
    assert!(blocked(e.submit_action(
        &ops(),
        "missing_bound",
        json!({ "permit": ids.permit, "n": 2.0 })
    )));
}

#[test]
fn r03_explicit_instance_write_deny_is_enforced_by_actions() {
    let e = Engine::memory().unwrap();
    let ids = onto_bootstrap::install(&e).unwrap();
    let b = e.open_branch(&builder(), "deny-write").unwrap();
    e.create_policy(
        &builder(),
        &b,
        PolicySpec {
            name: "review_deny_tank".into(),
            level: AuthzLevel::Instance,
            op: AuthzOp::Write,
            key: Some(KeyKind::Consumer),
            type_name: Some("AerationTank".into()),
            instance_id: Some(ids.tank1.clone()),
            property: None,
            roles: vec!["supervisor".into()],
            min_tier: 0,
            decision: AuthzDecision::Deny,
        },
    )
    .unwrap();
    merge(&e, &b).unwrap();
    let proposed = e
        .submit_action(&ops(), "propose_setpoint_change", setpoint(&ids))
        .unwrap();
    assert_eq!(
        proposed.verdict,
        Verdict::Allow,
        "proposal must be created so confirm exercises write policy"
    );
    assert!(
        blocked(e.confirm_action(&boss(), proposed.inbox_id.as_deref().unwrap())),
        "A pending proposal must not bypass an instance write deny"
    );
}

#[test]
fn r03_decision_snapshot_must_not_reveal_denied_property() {
    let e = Engine::memory().unwrap();
    let ids = onto_bootstrap::install(&e).unwrap();
    let restricted = Session::new(
        Actor::consumer("restricted", &["restricted"], AgentTier::T2),
        "review",
    );
    let direct = e
        .get_object(&restricted, &ids.permit, AsOf::Current)
        .unwrap();
    assert!(
        !direct.properties.contains_key("do_max"),
        "control: property must be denied"
    );
    let out = e
        .submit_action(&ops(), "propose_setpoint_change", setpoint(&ids))
        .unwrap();
    let rec_id = out.decision_record_id.unwrap();
    match e.get_decision_record(&restricted, &rec_id) {
        Err(_) => {}
        Ok(rec) => {
            let leaked = rec.data_snapshot["objects"]
                .as_array()
                .is_some_and(|objects| {
                    objects.iter().any(|o| {
                        o["id"] == json!(ids.permit) && o["properties"].get("do_max").is_some()
                    })
                });
            assert!(!leaked, "Denied do_max cannot escape through a dossier");
        }
    }
}

#[test]
fn r05_explicit_idempotency_key_cannot_bypass_authority() {
    let e = Engine::memory().unwrap();
    let mut action = spec("role_protected", ExecutionMode::Propose);
    action.required_roles = vec!["operator".into()];
    register(&e, action).unwrap();
    let params = json!({ "idempotency_key": "review-global-collision" });
    let first = e
        .submit_action(&ops(), "role_protected", params.clone())
        .unwrap();
    assert_eq!(first.verdict, Verdict::Allow);
    let outsider = Session::new(Actor::consumer("outsider", &[], AgentTier::T2), "review");
    assert!(
        blocked(e.submit_action(&outsider, "role_protected", params)),
        "Cached response requires authorized scoped lookup"
    );
}

#[test]
fn r06_approve_action_requires_the_pending_proposal() {
    let e = Engine::memory().unwrap();
    let ids = onto_bootstrap::install(&e).unwrap();
    assert!(
        blocked(e.submit_action(&boss(), "approve_setpoint_change", setpoint(&ids))),
        "T3 direct submission must not simulate prior approval"
    );
}

#[test]
fn r06_changed_effect_invalidates_prepared_proposal() {
    let e = Engine::memory().unwrap();
    let ids = onto_bootstrap::install(&e).unwrap();
    let out = e
        .submit_action(&ops(), "propose_setpoint_change", setpoint(&ids))
        .unwrap();
    let mut changed = e
        .describe_action(&ops(), "approve_setpoint_change")
        .unwrap();
    changed.effects = json!([{ "update": "tank", "properties": { "target_do": 99.0 } }]);
    if register(&e, changed).is_err() {
        return;
    }
    assert!(
        blocked(e.confirm_action(&boss(), out.inbox_id.as_deref().unwrap())),
        "Old proposal cannot approve newly substituted effects"
    );
}

#[test]
fn r06_shadow_cannot_enter_real_confirmation() {
    let e = Engine::memory().unwrap();
    register(&e, spec("shadow_probe", ExecutionMode::Shadow)).unwrap();
    let out = e.submit_action(&ops(), "shadow_probe", json!({})).unwrap();
    if let Some(inbox) = out.inbox_id {
        assert!(
            blocked(e.confirm_action(&boss(), &inbox)),
            "Shadow output must not be executable as real approval"
        );
    }
}

#[test]
fn r07_stale_branch_merge_cannot_silently_delete_other_release() {
    let e = Engine::memory().unwrap();
    let a = e.open_branch(&builder(), "branch-a").unwrap();
    let b = e.open_branch(&builder(), "branch-b").unwrap();
    e.create_object_type(&builder(), &a, object_type("AddedA"))
        .unwrap();
    e.create_object_type(&builder(), &b, object_type("AddedB"))
        .unwrap();
    merge(&e, &a).unwrap();
    if merge(&e, &b).is_err() {
        return;
    }
    let schema = e.get_schema(&ops(), None).unwrap();
    assert!(schema.object_types.iter().any(|t| t.name == "AddedA"));
    assert!(schema.object_types.iter().any(|t| t.name == "AddedB"));
}

#[test]
fn r08_link_endpoints_must_exist() {
    let e = Engine::memory().unwrap();
    let ids = onto_bootstrap::install(&e).unwrap();
    assert!(
        blocked(e.submit_action(
            &ops(),
            "assert_link",
            json!({
                "link_type": "monitors",
                "from": ids.sensor1,
                "to": "missing-tank"
            })
        )),
        "declared monitors with one missing endpoint must fail"
    );
}

#[test]
fn r08_string_parameter_must_reject_number() {
    let e = Engine::memory().unwrap();
    onto_bootstrap::install(&e).unwrap();
    let mut action = spec("string_check", ExecutionMode::Propose);
    action.parameters = vec![ParamSpec {
        name: "text".into(),
        value_type: "Text".into(),
        object_type: None,
        required: true,
    }];
    register(&e, action).unwrap();
    assert!(blocked(e.submit_action(
        &ops(),
        "string_check",
        json!({ "text": 123 })
    )));
}

#[test]
fn r09_late_record_has_distinct_recorded_time() {
    let e = Engine::memory().unwrap();
    let ids = onto_bootstrap::install(&e).unwrap();
    let t = e.now();
    e.funnel_ingest(
        &ops(),
        vec![IngestRecord {
            type_name: "DO_Sensor".into(),
            id: Some(ids.sensor1.clone()),
            properties: [("last_reading_at".into(), json!(999))]
                .into_iter()
                .collect(),
            as_of: Some((t + 100).to_string()),
            provenance: Some("newer-effective".into()),
        }],
    )
    .unwrap();
    e.set_clock(t + 200);
    e.funnel_ingest(
        &ops(),
        vec![IngestRecord {
            type_name: "DO_Sensor".into(),
            id: Some(ids.sensor1.clone()),
            properties: [("last_reading_at".into(), json!(111))]
                .into_iter()
                .collect(),
            as_of: Some((t + 20).to_string()),
            provenance: Some("older-effective".into()),
        }],
    )
    .unwrap();
    let current = e.get_object(&ops(), &ids.sensor1, AsOf::Current).unwrap();
    assert_eq!(
        current.properties["last_reading_at"].value,
        json!(999),
        "older effective event must not replace the current version"
    );
    let historical = e
        .get_object(&ops(), &ids.sensor1, AsOf::Valid(t + 30))
        .unwrap();
    assert_eq!(
        historical.properties["last_reading_at"].value,
        json!(111),
        "valid-time axis must reconstruct the late older interval"
    );
    let spans = e.object_spans(&ids.sensor1).unwrap();
    let historical_span = spans
        .iter()
        .find(|s| s.valid_from == t + 20 && s.tx_from == t + 200)
        .expect("late older interval");
    assert_eq!(historical_span.valid_to, Some(t + 100));
    let current_span = spans
        .iter()
        .find(|s| s.valid_to.is_none() && s.tx_to.is_none())
        .expect("open current");
    assert_eq!(current_span.valid_from, t + 100);
}

#[test]
fn r10_listed_named_set_tool_is_callable() {
    let e = Engine::memory().unwrap();
    let b = e.open_branch(&builder(), "set-branch").unwrap();
    let tools = e.list_tools(&builder()).unwrap();
    assert!(tools.iter().any(|t| t.name == "create_object_set"));
    assert!(dispatch(
        &e,
        &builder(),
        "create_object_set",
        json!({
            "branch": b,
            "spec": { "name": "all", "type_name": null, "equals": {} }
        })
    )
    .is_ok());
}

#[test]
fn r12_example_cannot_claim_zero_to_take_occupied_seat() {
    let e = Engine::memory().unwrap();
    let ids = onto_bootstrap::install_highered(&e).unwrap();
    let params = |student: &str, claim: i64| {
        json!({
            "student": student,
            "section": ids.section,
            "seat": ids.seat,
            "claim": claim,
            "enrolled": 1
        })
    };
    let first = e
        .submit_action(&ops(), "propose_enroll", params(&ids.ana, 1))
        .unwrap();
    assert_eq!(
        e.confirm_action(&boss(), first.inbox_id.as_deref().unwrap())
            .unwrap()
            .verdict,
        Verdict::Allow
    );
    let second = e.submit_action(&ops(), "propose_enroll", params(&ids.bruno, 0));
    if let Ok(second) = second {
        if let Some(id) = second.inbox_id {
            assert!(
                blocked(e.confirm_action(&boss(), &id)),
                "claim=0 must not bypass a full seat"
            );
        } else {
            assert_ne!(second.verdict, Verdict::Allow);
        }
    }
}

#[test]
fn r12_compensation_must_close_the_active_occupies_relation() {
    let e = Engine::memory().unwrap();
    let ids = onto_bootstrap::install_highered(&e).unwrap();
    let params = json!({
        "student": ids.ana,
        "section": ids.section,
        "seat": ids.seat,
        "claim": 1,
        "enrolled": 1
    });
    let proposed = e.submit_action(&ops(), "propose_enroll", params).unwrap();
    let confirmed = e
        .confirm_action(&boss(), proposed.inbox_id.as_deref().unwrap())
        .unwrap();
    e.funnel_ingest(
        &ops(),
        vec![IngestRecord {
            type_name: "Seat".into(),
            id: Some("seat-other".into()),
            properties: [
                ("name".into(), json!("Assento 2")),
                ("slots".into(), json!(1)),
            ]
            .into_iter()
            .collect(),
            as_of: None,
            provenance: None,
        }],
    )
    .unwrap();
    assert_eq!(
        e.submit_action(
            &ops(),
            "assert_link",
            json!({ "link_type": "occupies", "from": ids.ana, "to": "seat-other" })
        )
        .unwrap()
        .verdict,
        Verdict::Allow
    );
    let compensated = e
        .compensate_action(
            &boss(),
            confirmed.decision_record_id.as_deref().unwrap(),
            json!({}),
        )
        .unwrap();
    assert_eq!(compensated.verdict, Verdict::Allow);
    let left = e.traverse_links(&ops(), &ids.ana, "occupies").unwrap();
    assert_eq!(left.len(), 1, "unrelated occupancy must survive");
    assert_eq!(left[0].id, "seat-other");
}

#[test]
fn r02_two_staged_updates_to_one_object_compose() {
    let e = Engine::memory().unwrap();
    onto_bootstrap::install(&e).unwrap();
    let b = e.open_branch(&builder(), "pair-schema").unwrap();
    let mut ot = object_type("Pair");
    ot.properties = ["x", "y"]
        .iter()
        .map(|n| PropertySpec {
            name: (*n).into(),
            value_type: "Text".into(),
            source: PropertySource::Mapped,
            nullable: false,
            function: None,
        })
        .collect();
    e.create_object_type(&builder(), &b, ot).unwrap();
    merge(&e, &b).unwrap();
    e.funnel_ingest(
        &ops(),
        vec![IngestRecord {
            type_name: "Pair".into(),
            id: Some("pair".into()),
            properties: [("x".into(), json!("0")), ("y".into(), json!("0"))]
                .into_iter()
                .collect(),
            as_of: None,
            provenance: None,
        }],
    )
    .unwrap();
    let mut action = spec("update_pair", ExecutionMode::Auto);
    action.parameters = vec![ParamSpec {
        name: "thing".into(),
        value_type: "Text".into(),
        object_type: Some("Pair".into()),
        required: true,
    }];
    action.effects = json!([
        { "update": "thing", "properties": { "x": "1" } },
        { "update": "thing", "properties": { "y": "1" } }
    ]);
    register(&e, action).unwrap();
    e.submit_action(&ops(), "update_pair", json!({ "thing": "pair" }))
        .unwrap();
    let current = e.get_object(&ops(), "pair", AsOf::Current).unwrap();
    assert_eq!(current.properties["x"].value, json!("1"));
    assert_eq!(current.properties["y"].value, json!("1"));
}

#[test]
fn r03_title_must_not_leak_a_denied_title_property() {
    let e = Engine::memory().unwrap();
    let ids = onto_bootstrap::install(&e).unwrap();
    let b = e.open_branch(&builder(), "deny-title").unwrap();
    e.create_policy(
        &builder(),
        &b,
        PolicySpec {
            name: "deny_sensor_name".into(),
            level: AuthzLevel::Property,
            op: AuthzOp::Read,
            key: Some(KeyKind::Consumer),
            type_name: Some("DO_Sensor".into()),
            instance_id: Some(ids.sensor1.clone()),
            property: Some("name".into()),
            roles: vec!["restricted".into()],
            min_tier: 0,
            decision: AuthzDecision::Deny,
        },
    )
    .unwrap();
    merge(&e, &b).unwrap();
    let actor = Session::new(
        Actor::consumer("restricted", &["restricted"], AgentTier::T2),
        "review",
    );
    let view = e.get_object(&actor, &ids.sensor1, AsOf::Current).unwrap();
    assert!(!view.properties.contains_key("name"));
    assert!(
        view.title.is_none(),
        "Denied name is still exposed as title"
    );
}

#[test]
fn r10_zero_limit_means_no_results() {
    let e = Engine::memory().unwrap();
    onto_bootstrap::install(&e).unwrap();
    let result = e
        .search_objects(
            &ops(),
            Query {
                limit: 0,
                ..Query::default()
            },
        )
        .unwrap();
    assert!(result.is_empty());
}

#[test]
fn r07_restart_must_not_replace_a_released_interface() {
    let path = std::env::temp_dir().join(format!("onto-review-{}.db", onto::new_id()));
    let file = path.to_str().unwrap();
    {
        let e = Engine::open(file).unwrap();
        let b = e.open_branch(&builder(), "custom-evidence").unwrap();
        e.create_interface(
            &builder(),
            &b,
            onto::InterfaceSpec {
                name: "Evidenced".into(),
                required_properties: vec!["review_proof".into()],
            },
        )
        .unwrap();
        merge(&e, &b).unwrap();
        assert!(e
            .get_schema(&builder(), None)
            .unwrap()
            .interfaces
            .iter()
            .any(|i| i.name == "Evidenced"
                && i.required_properties == vec!["review_proof".to_string()]));
    }
    let retained = {
        let e = Engine::open(file).unwrap();
        e.get_schema(&builder(), None)
            .unwrap()
            .interfaces
            .iter()
            .any(|i| {
                i.name == "Evidenced" && i.required_properties == vec!["review_proof".to_string()]
            })
    };
    let _ = std::fs::remove_file(path);
    assert!(
        retained,
        "Opening the database replaced an accepted OMS interface"
    );
}
