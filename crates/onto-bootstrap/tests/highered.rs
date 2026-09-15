use onto::{dispatch, Actor, AgentTier, AsOf, Engine, KeyKind, Query, Session, Verdict};
use onto_bootstrap::{define_highered, install, install_highered};
use serde_json::json;

fn modeller() -> Session {
    Session::new(Actor::builder("human.modeler", &["modeler"]), "test")
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

fn enroll_params(student: &str, section: &str, seat: &str) -> serde_json::Value {
    json!({
        "student": student,
        "section": section,
        "seat": seat,
        "claim": 1,
        "enrolled": 1
    })
}

fn search_type(engine: &Engine, type_name: &str) -> Vec<onto::ObjectView> {
    engine
        .search_objects(
            &operator(),
            Query {
                type_name: Some(type_name.into()),
                ..Query::default()
            },
        )
        .unwrap()
}

#[test]
fn consumer_can_search_course_section_student_after_install() {
    let engine = Engine::memory().unwrap();
    let ids = install_highered(&engine).unwrap();
    let courses = search_type(&engine, "Course");
    assert!(
        courses.iter().any(|o| o.id == ids.course),
        "Course must be searchable after install_highered"
    );
    let sections = search_type(&engine, "Section");
    assert!(sections.iter().any(|o| o.id == ids.section));
    assert_eq!(
        sections[0].properties["status"].value,
        json!("full"),
        "seeded section is full (waitlist path)"
    );
    let students = search_type(&engine, "Student");
    assert!(students.iter().any(|o| o.id == ids.ana));
    assert!(students.iter().any(|o| o.id == ids.bruno));
    let waitlist = search_type(&engine, "Waitlist");
    assert_eq!(waitlist.len(), 2);
}

#[test]
fn propose_enroll_on_full_section_goes_review_inbox() {
    let engine = Engine::memory().unwrap();
    let ids = install_highered(&engine).unwrap();
    let proposed = engine
        .submit_action(
            &operator(),
            "propose_enroll",
            enroll_params(&ids.ana, &ids.section, &ids.seat),
        )
        .unwrap();
    assert_eq!(
        proposed.verdict,
        Verdict::Allow,
        "Propose on a full section is the Review inbox, not Auto commit"
    );
    let inbox = proposed
        .inbox_id
        .expect("full section enroll goes to inbox");
    let items = engine.list_inbox(&supervisor()).unwrap();
    assert!(
        items
            .iter()
            .any(|i| i.id == inbox && i.action_name == "propose_enroll"),
        "Review inbox must hold propose_enroll"
    );
    let seat = engine
        .get_object(&operator(), &ids.seat, AsOf::Current)
        .unwrap();
    assert!(
        !seat.properties.contains_key("occupant"),
        "Propose must not assign a seat"
    );
}

#[test]
fn supervisor_confirm_assigns_seat() {
    let engine = Engine::memory().unwrap();
    let ids = install_highered(&engine).unwrap();
    let proposed = engine
        .submit_action(
            &operator(),
            "propose_enroll",
            enroll_params(&ids.ana, &ids.section, &ids.seat),
        )
        .unwrap();
    let confirmed = engine
        .confirm_action(&supervisor(), proposed.inbox_id.as_deref().unwrap())
        .unwrap();
    assert_eq!(confirmed.verdict, Verdict::Allow);
    let seat = engine
        .get_object(&operator(), &ids.seat, AsOf::Current)
        .unwrap();
    assert_eq!(seat.properties["occupant"].value, json!(ids.ana));
    assert_eq!(
        seat.properties["occupant"].source,
        onto::PropertySource::ActionWritten
    );
    assert_eq!(seat.properties["slots"].value, json!(0));
    let section = engine
        .get_object(&operator(), &ids.section, AsOf::Current)
        .unwrap();
    assert_eq!(section.properties["enrolled"].value, json!(1));
    let held = engine
        .traverse_links(&operator(), &ids.ana, "occupies")
        .unwrap();
    assert!(held.iter().any(|o| o.id == ids.seat));
}

#[test]
fn second_confirm_same_seat_denies_no_double_book() {
    let engine = Engine::memory().unwrap();
    let ids = install_highered(&engine).unwrap();
    let ana = engine
        .submit_action(
            &operator(),
            "propose_enroll",
            enroll_params(&ids.ana, &ids.section, &ids.seat),
        )
        .unwrap();
    let bruno = engine
        .submit_action(
            &operator(),
            "propose_enroll",
            enroll_params(&ids.bruno, &ids.section, &ids.seat),
        )
        .unwrap();
    let first = engine
        .confirm_action(&supervisor(), ana.inbox_id.as_deref().unwrap())
        .unwrap();
    assert_eq!(first.verdict, Verdict::Allow);
    let second = engine
        .confirm_action(&supervisor(), bruno.inbox_id.as_deref().unwrap())
        .unwrap();
    assert_eq!(
        second.verdict,
        Verdict::Deny,
        "second confirm on the same seat must Deny, not double-book"
    );
    let seat = engine
        .get_object(&operator(), &ids.seat, AsOf::Current)
        .unwrap();
    assert_eq!(
        seat.properties["occupant"].value,
        json!(ids.ana),
        "occupant must stay the first student"
    );
    assert_eq!(seat.properties["slots"].value, json!(0));
    let ana_held = engine
        .traverse_links(&operator(), &ids.ana, "occupies")
        .unwrap();
    assert_eq!(ana_held.len(), 1);
    let bruno_held = engine
        .traverse_links(&operator(), &ids.bruno, "occupies")
        .unwrap();
    assert!(
        bruno_held.is_empty(),
        "Bruno must not occupy the same seat, got {bruno_held:?}"
    );
}

#[test]
fn compensate_releases_seat_instead_of_double_book() {
    let engine = Engine::memory().unwrap();
    let ids = install_highered(&engine).unwrap();
    let proposed = engine
        .submit_action(
            &operator(),
            "propose_enroll",
            enroll_params(&ids.ana, &ids.section, &ids.seat),
        )
        .unwrap();
    let assigned = engine
        .confirm_action(&supervisor(), proposed.inbox_id.as_deref().unwrap())
        .unwrap();
    let rec_id = assigned.decision_record_id.clone().expect("assign record");
    engine.set_clock(engine.now() + 10);
    let released = engine
        .compensate_action(
            &supervisor(),
            &rec_id,
            json!({ "idempotency_key": "compensate:seat-1:once" }),
        )
        .unwrap();
    assert_eq!(released.verdict, Verdict::Allow);
    let inverse = engine
        .get_decision_record(&operator(), released.decision_record_id.as_deref().unwrap())
        .unwrap();
    assert_eq!(inverse.action_name, "release_seat");
    let seat = engine
        .get_object(&operator(), &ids.seat, AsOf::Current)
        .unwrap();
    assert_eq!(seat.properties["slots"].value, json!(1));
    let bruno = engine
        .submit_action(
            &operator(),
            "propose_enroll",
            enroll_params(&ids.bruno, &ids.section, &ids.seat),
        )
        .unwrap();
    let second = engine
        .confirm_action(&supervisor(), bruno.inbox_id.as_deref().unwrap())
        .unwrap();
    assert_eq!(second.verdict, Verdict::Allow);
    let seat = engine
        .get_object(&operator(), &ids.seat, AsOf::Current)
        .unwrap();
    assert_eq!(seat.properties["occupant"].value, json!(ids.bruno));
}

#[test]
fn consumer_cannot_create_object_type() {
    let engine = Engine::memory().unwrap();
    install_highered(&engine).unwrap();
    let denied = dispatch(
        &engine,
        &operator(),
        "create_object_type",
        json!({
            "branch": "x",
            "spec": {
                "name": "Clinic",
                "typology": "entity",
                "title_prop": "name",
                "interfaces": [],
                "properties": []
            }
        }),
    );
    assert!(denied.is_err(), "consumer cannot mutate schema");
    assert_ne!(KeyKind::Builder, KeyKind::Consumer);
}

#[test]
fn unmerged_highered_does_not_affect_wastewater() {
    let engine = Engine::memory().unwrap();
    install(&engine).unwrap();
    let tanks_before = search_type(&engine, "AerationTank").len();
    assert!(tanks_before > 0);

    let b = engine.open_branch(&modeller(), "highered-v1").unwrap();
    define_highered(&engine, &modeller(), &b).unwrap();

    let schema = engine.get_schema(&operator(), None).unwrap();
    assert!(
        !schema.object_types.iter().any(|t| t.name == "Course"),
        "unmerged higher-ed must not appear on main"
    );
    assert!(schema.object_types.iter().any(|t| t.name == "AerationTank"));
    assert!(!schema.object_types.iter().any(|t| t.name == "Course"));
    let tanks_after = search_type(&engine, "AerationTank").len();
    assert_eq!(tanks_before, tanks_after);
    let courses = search_type(&engine, "Course");
    assert!(courses.is_empty());
    let tools = engine.list_tools(&operator()).unwrap();
    assert!(
        !tools.iter().any(|t| t.name == "action.propose_enroll"),
        "unmerged propose_enroll must not project"
    );
    assert!(tools
        .iter()
        .any(|t| t.name == "action.propose_setpoint_change"));
}

#[test]
fn merged_highered_after_wastewater_keeps_both() {
    let engine = Engine::memory().unwrap();
    install(&engine).unwrap();
    install_highered(&engine).unwrap();
    assert!(!search_type(&engine, "AerationTank").is_empty());
    assert!(!search_type(&engine, "Course").is_empty());
    let schema = engine.get_schema(&operator(), None).unwrap();
    assert!(schema.object_types.iter().any(|t| t.name == "AerationTank"));
    assert!(schema.object_types.iter().any(|t| t.name == "Course"));
}
