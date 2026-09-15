//! Command-boundary proofs: one transaction, one seat.

use onto::{Actor, AgentTier, AsOf, Engine, Session, Verdict};
use serde_json::json;
use std::sync::Arc;
use std::thread;

fn ops() -> Session {
    Session::new(
        Actor::consumer("boundary.ops", &["operator"], AgentTier::T2),
        "boundary",
    )
}

fn boss() -> Session {
    Session::new(
        Actor::consumer("boundary.boss", &["supervisor", "operator"], AgentTier::T3),
        "boundary",
    )
}

fn enroll(student: &str, ids: &onto_bootstrap::HigheredIds) -> serde_json::Value {
    json!({
        "student": student,
        "section": ids.section,
        "seat": ids.seat,
        "claim": 1,
        "enrolled": 1
    })
}

#[test]
fn does_assign_one_seat_if_two_inboxes_compete() {
    let engine = Arc::new(Engine::memory().unwrap());
    let ids = onto_bootstrap::install_highered(&engine).unwrap();
    let first = engine
        .submit_action(&ops(), "propose_enroll", enroll(&ids.ana, &ids))
        .unwrap();
    let second = engine
        .submit_action(&ops(), "propose_enroll", enroll(&ids.bruno, &ids))
        .unwrap();
    let ia = first.inbox_id.unwrap();
    let ib = second.inbox_id.unwrap();
    assert_ne!(ia, ib);
    let a = Arc::clone(&engine);
    let b = Arc::clone(&engine);
    let inbox_ana = ia.clone();
    let inbox_bruno = ib.clone();
    thread::scope(|s| {
        s.spawn(|| a.confirm_action(&boss(), &inbox_ana));
        s.spawn(|| b.confirm_action(&boss(), &inbox_bruno));
    });
    let occupants = engine
        .traverse_links(&ops(), &ids.ana, "occupies")
        .unwrap()
        .len()
        + engine
            .traverse_links(&ops(), &ids.bruno, "occupies")
            .unwrap()
            .len();
    assert_eq!(
        occupants, 1,
        "two competing inboxes cannot assign the same seat"
    );
    let seat = engine.get_object(&ops(), &ids.seat, AsOf::Current).unwrap();
    let occupant = seat.properties["occupant"].value.as_str().unwrap_or("");
    assert!(occupant == ids.ana || occupant == ids.bruno);
}

#[test]
fn does_reevaluate_second_confirm_if_seat_already_taken() {
    let engine = Engine::memory().unwrap();
    let ids = onto_bootstrap::install_highered(&engine).unwrap();
    let first = engine
        .submit_action(&ops(), "propose_enroll", enroll(&ids.ana, &ids))
        .unwrap();
    let second = engine
        .submit_action(&ops(), "propose_enroll", enroll(&ids.bruno, &ids))
        .unwrap();
    assert_eq!(
        engine
            .confirm_action(&boss(), first.inbox_id.as_deref().unwrap())
            .unwrap()
            .verdict,
        Verdict::Allow
    );
    let again = engine.confirm_action(&boss(), second.inbox_id.as_deref().unwrap());
    match again {
        Err(_) => {}
        Ok(out) => assert_ne!(out.verdict, Verdict::Allow),
    }
    assert_eq!(
        engine
            .traverse_links(&ops(), &ids.bruno, "occupies")
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn does_use_frozen_clock_in_memory_and_live_clock_from_store() {
    let memory = Engine::memory().unwrap();
    assert!(memory.clock_is_frozen());
    assert_eq!(memory.now(), 1_700_000_000);
    let live = Engine::from_store(onto::SqliteStore::memory().unwrap());
    assert!(!live.clock_is_frozen());
    assert!(live.now() > 1_700_000_000);
}
