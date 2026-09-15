//! Spec for the five leftover gates. Names fail if the mechanism is deleted.

use onto::{
    dispatch, ActionTypeSpec, Actor, AgentTier, AsOf, Engine, ExecutionMode, IngestRecord, KeyKind,
    Session, Verdict,
};
use rusqlite::Connection;
use serde_json::json;
use std::time::{SystemTime, UNIX_EPOCH};

fn modeller() -> Session {
    Session::new(Actor::builder("human.modeler", &["modeler"]), "leftover")
}
fn reviewer() -> Session {
    Session::new(Actor::builder("human.reviewer", &["reviewer"]), "leftover")
}
fn operator() -> Session {
    Session::new(
        Actor::consumer("ops.maya", &["operator"], AgentTier::T2),
        "leftover",
    )
}
fn supervisor() -> Session {
    Session::new(
        Actor::consumer("ops.chen", &["supervisor", "operator"], AgentTier::T3),
        "leftover",
    )
}

fn enroll(student: &str, section: &str, seat: &str, enrolled: i64) -> serde_json::Value {
    json!({
        "student": student,
        "section": section,
        "seat": seat,
        "claim": 1,
        "enrolled": enrolled
    })
}

fn temp_db() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let path =
        std::env::temp_dir().join(format!("onto-leftover-{}-{nanos}.db", std::process::id()));
    path.to_string_lossy().into_owned()
}

#[test]
fn does_reject_unknown_effect_verb_at_publish() {
    let engine = Engine::memory().unwrap();
    let branch = engine.open_branch(&modeller(), "bad-effect").unwrap();
    let err = engine
        .create_action_type(
            &modeller(),
            &branch,
            ActionTypeSpec {
                name: "explode".into(),
                mode: ExecutionMode::Auto,
                parameters: vec![],
                guards: json!([]),
                required_roles: vec![],
                required_tier: AgentTier::T2,
                effects: json!([{ "explode": true }]),
                compensation: None,
                side_effects: json!({}),
                on_review: None,
                interfaces: vec![],
            },
        )
        .unwrap_err();
    assert!(matches!(err, onto::OntoError::Invalid(_)));
}

#[test]
fn does_deny_second_occupies_if_link_is_one_to_one() {
    let engine = Engine::memory().unwrap();
    let ids = onto_bootstrap::install_highered(&engine).unwrap();
    let proposed = engine
        .submit_action(
            &operator(),
            "propose_enroll",
            enroll(&ids.ana, &ids.section, &ids.seat, 1),
        )
        .unwrap();
    assert_eq!(
        engine
            .confirm_action(&supervisor(), proposed.inbox_id.as_deref().unwrap())
            .unwrap()
            .verdict,
        Verdict::Allow
    );
    engine
        .funnel_ingest(
            &operator(),
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
    let second = engine
        .submit_action(
            &operator(),
            "assert_link",
            json!({
                "link_type": "occupies",
                "from": ids.ana,
                "to": "seat-other"
            }),
        )
        .unwrap();
    assert_eq!(
        second.verdict,
        Verdict::Deny,
        "second occupies on a 1:1 link must Deny, got {}",
        second.reason
    );
    let held = engine
        .traverse_links(&operator(), &ids.ana, "occupies")
        .unwrap();
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].id, ids.seat);
}

#[test]
fn does_ignore_caller_enrolled_if_occupies_count_is_one() {
    let engine = Engine::memory().unwrap();
    let ids = onto_bootstrap::install_highered(&engine).unwrap();
    let proposed = engine
        .submit_action(
            &operator(),
            "propose_enroll",
            enroll(&ids.ana, &ids.section, &ids.seat, 99),
        )
        .unwrap();
    assert_eq!(
        engine
            .confirm_action(&supervisor(), proposed.inbox_id.as_deref().unwrap())
            .unwrap()
            .verdict,
        Verdict::Allow
    );
    let section = engine
        .get_object(&operator(), &ids.section, AsOf::Current)
        .unwrap();
    assert_eq!(
        section.properties["enrolled"].value,
        json!(1),
        "enrolled must be occupies count, not caller 99"
    );
}

#[test]
fn does_write_zero_enrolled_if_occupies_is_closed() {
    let engine = Engine::memory().unwrap();
    let ids = onto_bootstrap::install_highered(&engine).unwrap();
    let proposed = engine
        .submit_action(
            &operator(),
            "propose_enroll",
            enroll(&ids.ana, &ids.section, &ids.seat, 7),
        )
        .unwrap();
    let confirmed = engine
        .confirm_action(&supervisor(), proposed.inbox_id.as_deref().unwrap())
        .unwrap();
    engine
        .compensate_action(
            &supervisor(),
            confirmed.decision_record_id.as_deref().unwrap(),
            json!({}),
        )
        .unwrap();
    let section = engine
        .get_object(&operator(), &ids.section, AsOf::Current)
        .unwrap();
    assert_eq!(section.properties["enrolled"].value, json!(0));
}

#[test]
fn does_keep_fail_closed_if_legacy_inbox_is_unmigrated() {
    let path = temp_db();
    let engine = Engine::open(&path).unwrap();
    let ids = onto_bootstrap::install_highered(&engine).unwrap();
    let proposed = engine
        .submit_action(
            &operator(),
            "propose_enroll",
            enroll(&ids.ana, &ids.section, &ids.seat, 1),
        )
        .unwrap();
    let inbox = proposed.inbox_id.expect("inbox");
    drop(engine);
    let db = Connection::open(&path).unwrap();
    db.execute("UPDATE inbox SET apply_action = '' WHERE id = ?1", [&inbox])
        .unwrap();
    drop(db);
    let engine = Engine::open(&path).unwrap();
    let err = engine.confirm_action(&supervisor(), &inbox).unwrap_err();
    assert!(
        matches!(err, onto::OntoError::Invalid(_)),
        "unmigrated empty apply_action must stay fail-closed, got {err}"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn does_cancel_inbox_if_apply_action_is_empty() {
    let path = temp_db();
    let engine = Engine::open(&path).unwrap();
    let ids = onto_bootstrap::install_highered(&engine).unwrap();
    let proposed = engine
        .submit_action(
            &operator(),
            "propose_enroll",
            enroll(&ids.ana, &ids.section, &ids.seat, 1),
        )
        .unwrap();
    let inbox = proposed.inbox_id.expect("inbox");
    drop(engine);
    let db = Connection::open(&path).unwrap();
    db.execute("UPDATE inbox SET apply_action = '' WHERE id = ?1", [&inbox])
        .unwrap();
    drop(db);
    let engine = Engine::open(&path).unwrap();
    let report = engine.migrate_legacy(&modeller()).unwrap();
    assert!(
        report.cancelled_inbox.iter().any(|id| id == &inbox),
        "migrator must cancel empty apply_action, got {report:?}"
    );
    let err = engine.confirm_action(&supervisor(), &inbox).unwrap_err();
    assert!(
        matches!(err, onto::OntoError::Conflict(_)),
        "cancelled inbox must not confirm, got {err}"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn does_reject_branch_if_base_revision_is_empty() {
    let path = temp_db();
    let engine = Engine::open(&path).unwrap();
    let branch = engine.open_branch(&modeller(), "stale-unpin").unwrap();
    let proposal = engine.submit_proposal(&modeller(), &branch).unwrap();
    engine
        .review_proposal(&reviewer(), &proposal, true)
        .unwrap();
    drop(engine);
    let db = Connection::open(&path).unwrap();
    db.execute(
        "UPDATE branches SET base_revision = '' WHERE name = ?1",
        [&branch],
    )
    .unwrap();
    drop(db);
    let engine = Engine::open(&path).unwrap();
    let err = engine.merge_to_main(&reviewer(), &proposal).unwrap_err();
    assert!(
        matches!(err, onto::OntoError::Invalid(_)),
        "unmigrated empty base_revision must stay fail-closed, got {err}"
    );
    let report = engine.migrate_legacy(&modeller()).unwrap();
    assert!(
        report.rejected_branches.iter().any(|n| n == &branch),
        "migrator must reject unpinned branch, got {report:?}"
    );
    let err = engine.merge_to_main(&reviewer(), &proposal).unwrap_err();
    assert!(
        matches!(err, onto::OntoError::Conflict(_)),
        "rejected proposal must not merge, got {err}"
    );
    let consumer = engine.migrate_legacy(&operator());
    assert!(consumer.is_err(), "consumer cannot migrate_legacy");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn does_list_claim_ack_reconcile_effect_intentions() {
    let engine = Engine::memory().unwrap();
    let ids = onto_bootstrap::install_highered(&engine).unwrap();
    let proposed = engine
        .submit_action(
            &operator(),
            "propose_enroll",
            enroll(&ids.ana, &ids.section, &ids.seat, 1),
        )
        .unwrap();
    let confirmed = engine
        .confirm_action(&supervisor(), proposed.inbox_id.as_deref().unwrap())
        .unwrap();
    let rec = confirmed.decision_record_id.expect("decision");
    let listed = engine.list_effect_intentions(&supervisor(), None).unwrap();
    assert!(
        listed
            .iter()
            .any(|e| e.decision_record_id == rec && e.status == onto::EffectStatus::Declared),
        "Allow must declare, not deliver: {listed:?}"
    );
    assert!(engine.claim_effect(&operator(), &rec).is_err());
    assert!(dispatch(
        &engine,
        &operator(),
        "claim_effect",
        json!({ "decision_record_id": rec }),
    )
    .is_err());
    engine.claim_effect(&supervisor(), &rec).unwrap();
    let claimed = engine
        .list_effect_intentions(&supervisor(), Some(onto::EffectStatus::Claimed))
        .unwrap();
    assert!(claimed.iter().any(|e| e.decision_record_id == rec));
    let open = engine.reconcile_effects(&supervisor()).unwrap();
    assert!(open.iter().any(|e| e.decision_record_id == rec));
    assert!(engine.claim_effect(&supervisor(), &rec).is_err());
    engine.ack_effect(&supervisor(), &rec).unwrap();
    let after = engine.reconcile_effects(&supervisor()).unwrap();
    assert!(
        !after.iter().any(|e| e.decision_record_id == rec),
        "acked intention is no longer open custody"
    );
    assert!(engine.ack_effect(&supervisor(), &rec).is_err());
    assert_ne!(KeyKind::Builder, KeyKind::Consumer);
}

#[test]
fn does_hide_effect_claim_if_actor_is_t2() {
    let engine = Engine::memory().unwrap();
    onto_bootstrap::install_highered(&engine).unwrap();
    let tools = engine.list_tools(&operator()).unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    assert!(!names.contains(&"claim_effect"));
    assert!(!names.contains(&"ack_effect"));
    assert!(!names.contains(&"reconcile_effects"));
    let boss_tools = engine.list_tools(&supervisor()).unwrap();
    let boss: Vec<&str> = boss_tools.iter().map(|t| t.name.as_str()).collect();
    assert!(boss.contains(&"claim_effect"));
    assert!(boss.contains(&"list_effect_intentions"));
}
