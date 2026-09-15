//! Higher-education teaching ontology (Zhang 2026). Installed at runtime.
//!
//! Course, Section, Student, Seat, Waitlist are OMS object types. Enrollment
//! uses the same dual keys and seven-step write path as wastewater. A full
//! section cannot Auto-enroll: `propose_enroll` is Propose, so it lands in the
//! Review inbox. Supervisor confirm runs `approve_enroll`, which assigns the
//! Seat or Denies if `slots` is already 0. A second confirm on the same seat
//! therefore cannot double-book. Compensation is `release_seat` (forward inverse).
//!
//! # Context
//! Teaching case only. Cite Zhang 2026; do not copy. No healthcare types.
//!
//! # Inputs
//! A live [`Engine`]. Builder sessions open a branch, create types/links/actions,
//! submit, review, merge. Consumer Funnel seeds instances. Links go through
//! Action `assert_link`, never a public `create_link`.
//!
//! # Outputs
//! [`HigheredIds`] of the seeded Course, Section, Seat, Students, Waitlist.
//! After merge, a consumer can `search_objects` those types.
//!
//! # Side effects
//! Mutates OMS schema on a working branch, then production instances after merge.
//! Unmerged, the branch has zero effect on types already on main (e.g. wastewater).
//!
//! # Example
//! ```
//! use onto::Engine;
//! use onto_bootstrap::install_highered;
//!
//! let engine = Engine::memory().unwrap();
//! let ids = install_highered(&engine).unwrap();
//! assert_eq!(ids.course, "course-cs101");
//! ```
//!
//! # Relations
//! [`crate::install`] is the wastewater case. Both may live in one engine:
//! install wastewater first, then this module on a branch until merge.

use onto::{
    ActionTypeSpec, Actor, AgentTier, AuthzDecision, AuthzLevel, AuthzOp, Engine, ExecutionMode,
    IngestRecord, KeyKind, LinkTypeSpec, ObjectTypeSpec, ParamSpec, PolicySpec, PropertySource,
    PropertySpec, Result, Session, Typology, ValueTypeSpec,
};
use serde_json::json;
use std::collections::BTreeMap;

/// Seeded identities after [`install_highered`].
pub struct HigheredIds {
    pub course: String,
    pub section: String,
    pub seat: String,
    pub ana: String,
    pub bruno: String,
    pub waitlist_ana: String,
    pub waitlist_bruno: String,
}

/// Install Course/Section/Student/Seat/Waitlist via builder APIs, then Funnel.
///
/// Consumer keys cannot call this. Schema is created on `highered-v1`, reviewed,
/// and merged. Instances are Funnel-written. Links are Action-written.
pub fn install_highered(engine: &Engine) -> Result<HigheredIds> {
    let modeller = Session::new(Actor::builder("human.modeler", &["modeler"]), "bootstrap");
    let reviewer = Session::new(Actor::builder("human.reviewer", &["reviewer"]), "bootstrap");
    let ops = Session::new(
        Actor::consumer("pipeline.funnel", &["operator"], AgentTier::T2),
        "ingest",
    );

    let branch = engine.open_branch(&modeller, "highered-v1")?;
    define_highered(engine, &modeller, &branch)?;
    let proposal = engine.submit_proposal(&modeller, &branch)?;
    engine.review_proposal(&reviewer, &proposal, true)?;
    engine.merge_to_main(&reviewer, &proposal)?;
    seed_world(engine, &ops)
}

/// Create higher-ed types, links, actions, and policies on an open branch.
///
/// Does not merge. Used by [`install_highered`] and by isolation tests that
/// leave the branch unmerged on top of wastewater.
pub fn define_highered(engine: &Engine, s: &Session, branch: &str) -> Result<()> {
    engine.create_value_type(s, branch, vt("Text", "string", None, None, None))?;
    engine.create_value_type(s, branch, vt("Count", "number", Some(0.0), None, None))?;

    engine.create_object_type(
        s,
        branch,
        obj(
            "Course",
            Typology::Entity,
            "name",
            vec![prop("name", "Text", PropertySource::Mapped, false)],
        ),
    )?;
    engine.create_object_type(
        s,
        branch,
        obj(
            "Section",
            Typology::Entity,
            "name",
            vec![
                prop("name", "Text", PropertySource::Mapped, false),
                prop("capacity", "Count", PropertySource::Mapped, false),
                prop("enrolled", "Count", PropertySource::ActionWritten, true),
                prop("status", "Text", PropertySource::Mapped, false),
            ],
        ),
    )?;
    engine.create_object_type(
        s,
        branch,
        obj(
            "Student",
            Typology::Entity,
            "name",
            vec![prop("name", "Text", PropertySource::Mapped, false)],
        ),
    )?;
    engine.create_object_type(
        s,
        branch,
        obj(
            "Seat",
            Typology::Entity,
            "name",
            vec![
                prop("name", "Text", PropertySource::Mapped, false),
                prop("slots", "Count", PropertySource::Mapped, false),
                prop("occupant", "Text", PropertySource::ActionWritten, true),
            ],
        ),
    )?;
    engine.create_object_type(
        s,
        branch,
        obj(
            "Waitlist",
            Typology::Entity,
            "name",
            vec![
                prop("name", "Text", PropertySource::Mapped, false),
                prop("status", "Text", PropertySource::Mapped, false),
            ],
        ),
    )?;

    for (name, from, to, card) in [
        ("course_has_section", "Course", "Section", "1:n"),
        ("section_has_seat", "Section", "Seat", "1:n"),
        ("waitlist_for", "Waitlist", "Section", "n:1"),
        ("waitlist_student", "Waitlist", "Student", "n:1"),
        ("occupies", "Student", "Seat", "1:1"),
    ] {
        engine.create_link_type(
            s,
            branch,
            LinkTypeSpec {
                name: name.into(),
                from_type: from.into(),
                to_type: to.into(),
                cardinality: card.into(),
                allow_cycles: false,
            },
        )?;
    }

    let enroll_params = vec![
        ParamSpec {
            name: "student".into(),
            value_type: "Text".into(),
            object_type: Some("Student".into()),
            required: true,
        },
        ParamSpec {
            name: "section".into(),
            value_type: "Text".into(),
            object_type: Some("Section".into()),
            required: true,
        },
        ParamSpec {
            name: "seat".into(),
            value_type: "Text".into(),
            object_type: Some("Seat".into()),
            required: true,
        },
        ParamSpec {
            name: "claim".into(),
            value_type: "Count".into(),
            object_type: None,
            required: true,
        },
        ParamSpec {
            name: "enrolled".into(),
            value_type: "Count".into(),
            object_type: None,
            required: true,
        },
    ];

    // Full section: Propose, not Auto. Lands in the Review inbox.
    engine.create_action_type(
        s,
        branch,
        ActionTypeSpec {
            name: "propose_enroll".into(),
            mode: ExecutionMode::Propose,
            parameters: enroll_params.clone(),
            guards: json!([]),
            required_roles: vec!["operator".into()],
            required_tier: AgentTier::T2,
            effects: json!([]),
            compensation: Some("release_seat".into()),
            side_effects: json!({}),
            on_review: Some("join_waitlist".into()),
        },
    )?;

    // Confirm assigns the seat. lte_field claim vs seat.slots Denies a double-book.
    engine.create_action_type(
        s,
        branch,
        ActionTypeSpec {
            name: "approve_enroll".into(),
            mode: ExecutionMode::Auto,
            parameters: enroll_params.clone(),
            guards: json!([
                { "lte_field": "claim", "object": "seat", "field": "slots" }
            ]),
            required_roles: vec!["supervisor".into()],
            required_tier: AgentTier::T3,
            effects: json!([
                {
                    "update": "seat",
                    "properties": { "occupant": "$student", "slots": 0 }
                },
                {
                    "update": "section",
                    "properties": { "enrolled": "$enrolled" }
                },
                {
                    "link": "occupies",
                    "from": "student",
                    "to": "seat"
                }
            ]),
            compensation: Some("release_seat".into()),
            side_effects: json!({ "registrar": "assign_seat", "idempotent": true }),
            on_review: Some("join_waitlist".into()),
        },
    )?;

    engine.create_action_type(
        s,
        branch,
        ActionTypeSpec {
            name: "release_seat".into(),
            mode: ExecutionMode::Auto,
            parameters: enroll_params,
            guards: json!([]),
            required_roles: vec!["supervisor".into()],
            required_tier: AgentTier::T3,
            effects: json!([{
                "update": "seat",
                "properties": { "occupant": "", "slots": 1 }
            }]),
            compensation: None,
            side_effects: json!({ "registrar": "release_seat", "idempotent": true }),
            on_review: None,
        },
    )?;

    engine.create_action_type(
        s,
        branch,
        ActionTypeSpec {
            name: "join_waitlist".into(),
            mode: ExecutionMode::Auto,
            parameters: vec![
                ParamSpec {
                    name: "student".into(),
                    value_type: "Text".into(),
                    object_type: Some("Student".into()),
                    required: true,
                },
                ParamSpec {
                    name: "section".into(),
                    value_type: "Text".into(),
                    object_type: Some("Section".into()),
                    required: true,
                },
            ],
            guards: json!([]),
            required_roles: vec!["operator".into()],
            required_tier: AgentTier::T2,
            effects: json!([{
                "create": "Waitlist",
                "properties": { "name": "$student", "status": "waiting" }
            }]),
            compensation: None,
            side_effects: json!({}),
            on_review: None,
        },
    )?;

    engine.create_action_type(
        s,
        branch,
        ActionTypeSpec {
            name: "assert_link".into(),
            mode: ExecutionMode::Auto,
            parameters: vec![
                ParamSpec {
                    name: "link_type".into(),
                    value_type: "Text".into(),
                    object_type: None,
                    required: true,
                },
                ParamSpec {
                    name: "from".into(),
                    value_type: "Text".into(),
                    object_type: None,
                    required: true,
                },
                ParamSpec {
                    name: "to".into(),
                    value_type: "Text".into(),
                    object_type: None,
                    required: true,
                },
            ],
            guards: json!([]),
            required_roles: vec!["operator".into()],
            required_tier: AgentTier::T2,
            effects: json!([{
                "link": "$link_type",
                "from": "from",
                "to": "to"
            }]),
            compensation: None,
            side_effects: json!({}),
            on_review: None,
        },
    )?;

    seed_policies(engine, s, branch)
}

fn seed_world(engine: &Engine, ops: &Session) -> Result<HigheredIds> {
    let t = engine.now();
    let ids = HigheredIds {
        course: "course-cs101".into(),
        section: "section-001".into(),
        seat: "seat-1".into(),
        ana: "student-ana".into(),
        bruno: "student-bruno".into(),
        waitlist_ana: "waitlist-ana".into(),
        waitlist_bruno: "waitlist-bruno".into(),
    };
    engine.funnel_ingest(
        ops,
        vec![
            rec(
                "Course",
                &ids.course,
                &[("name", json!("Introdução à Ontologia Operacional"))],
                t,
            ),
            rec(
                "Section",
                &ids.section,
                &[
                    ("name", json!("Turma 001")),
                    ("capacity", json!(1)),
                    ("status", json!("full")),
                ],
                t,
            ),
            rec(
                "Seat",
                &ids.seat,
                &[("name", json!("Assento 1")), ("slots", json!(1))],
                t,
            ),
            rec("Student", &ids.ana, &[("name", json!("Ana Silva"))], t),
            rec("Student", &ids.bruno, &[("name", json!("Bruno Costa"))], t),
            rec(
                "Waitlist",
                &ids.waitlist_ana,
                &[("name", json!("Fila Ana")), ("status", json!("waiting"))],
                t,
            ),
            rec(
                "Waitlist",
                &ids.waitlist_bruno,
                &[("name", json!("Fila Bruno")), ("status", json!("waiting"))],
                t,
            ),
        ],
    )?;
    seed_links(engine, ops, &ids)?;
    Ok(ids)
}

fn seed_links(engine: &Engine, ops: &Session, ids: &HigheredIds) -> Result<()> {
    for (link_type, from, to) in [
        (
            "course_has_section",
            ids.course.as_str(),
            ids.section.as_str(),
        ),
        ("section_has_seat", ids.section.as_str(), ids.seat.as_str()),
        (
            "waitlist_for",
            ids.waitlist_ana.as_str(),
            ids.section.as_str(),
        ),
        (
            "waitlist_for",
            ids.waitlist_bruno.as_str(),
            ids.section.as_str(),
        ),
        (
            "waitlist_student",
            ids.waitlist_ana.as_str(),
            ids.ana.as_str(),
        ),
        (
            "waitlist_student",
            ids.waitlist_bruno.as_str(),
            ids.bruno.as_str(),
        ),
    ] {
        let out = engine.submit_action(
            ops,
            "assert_link",
            json!({
                "link_type": link_type,
                "from": from,
                "to": to,
            }),
        )?;
        match out.verdict {
            onto::Verdict::Allow => {}
            onto::Verdict::Deny | onto::Verdict::Review => {
                return Err(onto::OntoError::Denied(format!(
                    "assert_link {link_type} {from}->{to}: {}",
                    out.reason
                )));
            }
        }
    }
    Ok(())
}

fn seed_policies(engine: &Engine, s: &Session, branch: &str) -> Result<()> {
    let consumer = Some(KeyKind::Consumer);
    let readers: &[&str] = &["operator", "supervisor", "restricted"];
    let writers: &[&str] = &["operator", "supervisor"];
    for spec in [
        grant(
            "platform_consumer_read",
            AuthzLevel::Platform,
            AuthzOp::Read,
            consumer,
            None,
            None,
            None,
            &[],
            0,
            AuthzDecision::Allow,
        ),
        grant(
            "platform_consumer_write",
            AuthzLevel::Platform,
            AuthzOp::Write,
            consumer,
            None,
            None,
            None,
            &[],
            0,
            AuthzDecision::Allow,
        ),
        grant(
            "type_star_read",
            AuthzLevel::Type,
            AuthzOp::Read,
            consumer,
            Some("*"),
            None,
            None,
            readers,
            1,
            AuthzDecision::Allow,
        ),
        grant(
            "type_t1_observe",
            AuthzLevel::Type,
            AuthzOp::Read,
            consumer,
            Some("*"),
            None,
            None,
            &[],
            1,
            AuthzDecision::Allow,
        ),
        grant(
            "type_star_write",
            AuthzLevel::Type,
            AuthzOp::Write,
            consumer,
            Some("*"),
            None,
            None,
            writers,
            2,
            AuthzDecision::Allow,
        ),
        grant(
            "instance_star_read",
            AuthzLevel::Instance,
            AuthzOp::Read,
            consumer,
            Some("*"),
            Some("*"),
            None,
            readers,
            1,
            AuthzDecision::Allow,
        ),
        grant(
            "instance_t1_observe",
            AuthzLevel::Instance,
            AuthzOp::Read,
            consumer,
            Some("*"),
            Some("*"),
            None,
            &[],
            1,
            AuthzDecision::Allow,
        ),
        grant(
            "instance_star_write",
            AuthzLevel::Instance,
            AuthzOp::Write,
            consumer,
            Some("*"),
            Some("*"),
            None,
            writers,
            2,
            AuthzDecision::Allow,
        ),
        grant(
            "property_star_read",
            AuthzLevel::Property,
            AuthzOp::Read,
            consumer,
            Some("*"),
            Some("*"),
            Some("*"),
            readers,
            1,
            AuthzDecision::Allow,
        ),
        grant(
            "property_t1_observe",
            AuthzLevel::Property,
            AuthzOp::Read,
            consumer,
            Some("*"),
            Some("*"),
            Some("*"),
            &[],
            1,
            AuthzDecision::Allow,
        ),
        grant(
            "property_star_write",
            AuthzLevel::Property,
            AuthzOp::Write,
            consumer,
            Some("*"),
            Some("*"),
            Some("*"),
            writers,
            2,
            AuthzDecision::Allow,
        ),
    ] {
        engine.create_policy(s, branch, spec)?;
    }
    Ok(())
}

fn vt(
    name: &str,
    base: &str,
    min: Option<f64>,
    max: Option<f64>,
    unit: Option<&str>,
) -> ValueTypeSpec {
    ValueTypeSpec {
        name: name.into(),
        base: base.into(),
        min,
        max,
        unit: unit.map(|s| s.into()),
    }
}

fn prop(name: &str, value_type: &str, source: PropertySource, nullable: bool) -> PropertySpec {
    PropertySpec {
        name: name.into(),
        value_type: value_type.into(),
        source,
        nullable,
        function: None,
    }
}

fn obj(
    name: &str,
    typology: Typology,
    title: &str,
    properties: Vec<PropertySpec>,
) -> ObjectTypeSpec {
    ObjectTypeSpec {
        name: name.into(),
        typology,
        title_prop: Some(title.into()),
        interfaces: vec![],
        freshness_budget_secs: None,
        properties,
    }
}

fn rec(type_name: &str, id: &str, pairs: &[(&str, serde_json::Value)], as_of: i64) -> IngestRecord {
    IngestRecord {
        type_name: type_name.into(),
        id: Some(id.into()),
        properties: pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect::<BTreeMap<_, _>>(),
        as_of: Some(as_of.to_string()),
        provenance: Some("funnel:seed".into()),
    }
}

fn grant(
    name: &str,
    level: AuthzLevel,
    op: AuthzOp,
    key: Option<KeyKind>,
    type_name: Option<&str>,
    instance_id: Option<&str>,
    property: Option<&str>,
    roles: &[&str],
    min_tier: u8,
    decision: AuthzDecision,
) -> PolicySpec {
    PolicySpec {
        name: name.into(),
        level,
        op,
        key,
        type_name: type_name.map(|s| s.into()),
        instance_id: instance_id.map(|s| s.into()),
        property: property.map(|s| s.into()),
        roles: roles.iter().map(|s| (*s).to_string()).collect(),
        min_tier,
        decision,
    }
}
