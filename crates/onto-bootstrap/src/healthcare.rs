//! Instructional healthcare ontology (Zhang 2026). Installed at runtime.
//!
//! Patient, Order, Observation, and DecisionRecord are OMS object types.
//! `propose_order` is Propose: complete evidence lands in the inbox; missing or
//! stale Observation evidence is `Verdict::Review` (Evidenced / Complete); a
//! coded contraindication flag is permit-like `Verdict::Deny`. Confirm runs
//! `approve_order` and writes a DecisionRecord. This case never computes a
//! prescribed quantity. Standing order 9: instructional only.
//!
//! Cite Zhang 2026; do not copy. No YAML constitution. No UI.
//!
//! # Context
//! Teaching loop only. Evidence completeness → Review or Deny. Confirm records
//! a DecisionRecord. There is no derived property and no function that yields a
//! prescribed quantity.
//!
//! # Inputs
//! A live [`Engine`]. Builder sessions open a branch, create types/links/actions,
//! submit, review, merge. Consumer Funnel seeds instances. Links go through
//! Action `assert_link`, never a public `create_link`.
//!
//! # Outputs
//! [`HealthcareIds`] of the seeded Patient, Order, and Observation variants
//! (complete, missing evidence, contraindication flag).
//!
//! # Side effects
//! Mutates OMS schema on a working branch, then production instances after merge.
//! Unmerged, the branch has zero effect on types already on main (e.g. wastewater).
//!
//! # Example
//! ```
//! use onto::Engine;
//! use onto_bootstrap::install_healthcare;
//!
//! let engine = Engine::memory().unwrap();
//! let ids = install_healthcare(&engine).unwrap();
//! assert_eq!(ids.patient, "patient-1");
//! ```
//!
//! # Relations
//! [`crate::install`] is the wastewater case. Both may live in one engine:
//! install wastewater first, then this module on a branch until merge.

#![allow(clippy::module_name_repetitions)] // public Healthcare* names match the case
#![allow(clippy::too_many_lines)] // teaching-case install is one schema dump

use onto::{
    ActionTypeSpec, Actor, AgentTier, AuthzDecision, AuthzLevel, AuthzOp, Engine, ExecutionMode,
    IngestRecord, InterfaceSpec, KeyKind, LinkTypeSpec, ObjectTypeSpec, ParamSpec, PolicySpec,
    PropertySource, PropertySpec, Result, Session, Typology, ValueTypeSpec,
};
use serde_json::json;
use std::collections::BTreeMap;

/// Seeded identities after [`install_healthcare`].
pub struct HealthcareIds {
    pub patient: String,
    pub order: String,
    pub observation_complete: String,
    pub observation_missing: String,
    pub observation_flagged: String,
}

/// Install Patient/Order/Observation/DecisionRecord via builder APIs, then Funnel.
///
/// Consumer keys cannot call this. Schema is created on `healthcare-v1`, reviewed,
/// and merged. Instances are Funnel-written. Links are Action-written.
pub fn install_healthcare(engine: &Engine) -> Result<HealthcareIds> {
    let modeller = Session::new(Actor::builder("human.modeler", &["modeler"]), "bootstrap");
    let reviewer = Session::new(Actor::builder("human.reviewer", &["reviewer"]), "bootstrap");
    let ops = Session::new(
        Actor::consumer("pipeline.funnel", &["operator"], AgentTier::T2),
        "ingest",
    );

    let branch = engine.open_branch(&modeller, "healthcare-v1")?;
    define_healthcare(engine, &modeller, &branch)?;
    let proposal = engine.submit_proposal(&modeller, &branch)?;
    engine.review_proposal(&reviewer, &proposal, true)?;
    engine.merge_to_main(&reviewer, &proposal)?;
    seed_world(engine, &ops)
}

/// Create healthcare types, links, actions, and policies on an open branch.
///
/// Does not merge. Used by [`install_healthcare`] and by isolation tests that
/// leave the branch unmerged on top of wastewater.
pub fn define_healthcare(engine: &Engine, s: &Session, branch: &str) -> Result<()> {
    engine.create_value_type(s, branch, vt("Text", "string", None, None, None))?;
    engine.create_value_type(s, branch, vt("Timestamp", "number", None, None, Some("s")))?;
    // Permit-like bound: 1 = no coded flag, 0 = contraindication flag present.
    engine.create_value_type(
        s,
        branch,
        vt("Clearance", "number", Some(0.0), Some(1.0), None),
    )?;

    engine.create_interface(
        s,
        branch,
        InterfaceSpec {
            name: "Reviewable".into(),
            required_properties: vec!["status".into()],
        },
    )?;
    engine.create_interface(
        s,
        branch,
        InterfaceSpec {
            name: "Evidenced".into(),
            required_properties: vec!["rationale".into()],
        },
    )?;

    engine.create_object_type(
        s,
        branch,
        obj(
            "Patient",
            Typology::Entity,
            "name",
            None,
            &[],
            vec![prop("name", "Text", PropertySource::Mapped, false)],
        ),
    )?;
    engine.create_object_type(
        s,
        branch,
        obj(
            "Order",
            Typology::Entity,
            "name",
            None,
            &[],
            vec![
                prop("name", "Text", PropertySource::Mapped, false),
                prop("kind", "Text", PropertySource::Mapped, false),
                prop("status", "Text", PropertySource::ActionWritten, true),
            ],
        ),
    )?;
    engine.create_object_type(
        s,
        branch,
        obj(
            "Observation",
            Typology::Event,
            "name",
            Some(300),
            &["Evidenced"],
            vec![
                prop("name", "Text", PropertySource::Mapped, false),
                prop("observed_at", "Timestamp", PropertySource::Mapped, true),
                prop(
                    "last_reading_at",
                    "Timestamp",
                    PropertySource::Mapped,
                    false,
                ),
                prop("coded_clearance", "Clearance", PropertySource::Mapped, true),
                prop("rationale", "Text", PropertySource::Mapped, true),
            ],
        ),
    )?;
    engine.create_object_type(
        s,
        branch,
        obj(
            "DecisionRecord",
            Typology::DecisionRecord,
            "name",
            None,
            &["Reviewable", "Evidenced"],
            vec![
                prop("name", "Text", PropertySource::ActionWritten, true),
                prop("rationale", "Text", PropertySource::ActionWritten, false),
                prop("status", "Text", PropertySource::ActionWritten, false),
            ],
        ),
    )?;
    engine.create_object_type(
        s,
        branch,
        obj(
            "ObservationRequest",
            Typology::DecisionRecord,
            "name",
            None,
            &[],
            vec![prop("name", "Text", PropertySource::ActionWritten, true)],
        ),
    )?;

    for (name, from, to, card) in [
        ("observation_of", "Observation", "Patient", "n:1"),
        ("order_for", "Order", "Patient", "n:1"),
        ("evidence_for", "Observation", "DecisionRecord", "n:n"),
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

    let order_params = vec![
        ParamSpec {
            name: "patient".into(),
            value_type: "Text".into(),
            object_type: Some("Patient".into()),
            required: true,
        },
        ParamSpec {
            name: "observation".into(),
            value_type: "Text".into(),
            object_type: Some("Observation".into()),
            required: true,
        },
        ParamSpec {
            name: "order".into(),
            value_type: "Text".into(),
            object_type: Some("Order".into()),
            required: true,
        },
        ParamSpec {
            name: "need_clearance".into(),
            value_type: "Clearance".into(),
            object_type: None,
            required: true,
        },
        ParamSpec {
            name: "rationale".into(),
            value_type: "Text".into(),
            object_type: None,
            required: true,
        },
    ];

    // Completeness guards Review; coded flag is permit-like Deny.
    // Propose: complete evidence opens the inbox. Missing evidence does not Allow.
    engine.create_action_type(
        s,
        branch,
        ActionTypeSpec {
            name: "propose_order".into(),
            mode: ExecutionMode::Propose,
            parameters: order_params.clone(),
            guards: json!([
                { "freshness": "observation", "max_age_secs": 300 },
                { "exists_field": "last_reading_at", "object": "observation" },
                { "lte_field": "need_clearance", "object": "observation", "field": "coded_clearance" }
            ]),
            required_roles: vec!["operator".into()],
            required_tier: AgentTier::T2,
            effects: json!([]),
            compensation: None,
            side_effects: json!({}),
            on_review: Some("request_observation".into()),
            interfaces: vec![],
        },
    )?;
    engine.attach_interface(s, branch, "propose_order", "Reviewable")?;

    engine.create_action_type(
        s,
        branch,
        ActionTypeSpec {
            name: "approve_order".into(),
            mode: ExecutionMode::Auto,
            parameters: order_params,
            guards: json!([
                { "freshness": "observation", "max_age_secs": 300 },
                { "exists_field": "last_reading_at", "object": "observation" },
                { "lte_field": "need_clearance", "object": "observation", "field": "coded_clearance" }
            ]),
            required_roles: vec!["supervisor".into()],
            required_tier: AgentTier::T3,
            effects: json!([
                {
                    "update": "order",
                    "properties": { "status": "confirmed" }
                },
                {
                    "create": "DecisionRecord",
                    "properties": {
                        "name": "$rationale",
                        "rationale": "$rationale",
                        "status": "recorded"
                    }
                }
            ]),
            compensation: None,
            side_effects: json!({ "chart": "record_decision", "idempotent": true }),
            on_review: Some("request_observation".into()),
            interfaces: vec![],
        },
    )?;

    engine.create_action_type(
        s,
        branch,
        ActionTypeSpec {
            name: "request_observation".into(),
            mode: ExecutionMode::Auto,
            parameters: vec![ParamSpec {
                name: "observation".into(),
                value_type: "Text".into(),
                object_type: Some("Observation".into()),
                required: true,
            }],
            guards: json!([]),
            required_roles: vec!["operator".into()],
            required_tier: AgentTier::T2,
            effects: json!([{
                "create": "ObservationRequest",
                "properties": { "name": "completar evidencia" }
            }]),
            compensation: None,
            side_effects: json!({}),
            on_review: None,
            interfaces: vec![],
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
            interfaces: vec![],
        },
    )?;

    seed_policies(engine, s, branch)
}

fn seed_world(engine: &Engine, ops: &Session) -> Result<HealthcareIds> {
    let t = engine.now();
    let ids = HealthcareIds {
        patient: "patient-1".into(),
        order: "order-1".into(),
        observation_complete: "observation-complete".into(),
        observation_missing: "observation-missing".into(),
        observation_flagged: "observation-flagged".into(),
    };
    engine.funnel_ingest(
        ops,
        vec![
            rec("Patient", &ids.patient, &[("name", json!("Paciente 1"))], t),
            rec(
                "Order",
                &ids.order,
                &[
                    ("name", json!("Pedido instrucional 1")),
                    ("kind", json!("instructional")),
                ],
                t,
            ),
            rec(
                "Observation",
                &ids.observation_complete,
                &[
                    ("name", json!("Evidencia completa")),
                    ("observed_at", json!(t)),
                    ("last_reading_at", json!(t)),
                    ("coded_clearance", json!(1)),
                    ("rationale", json!("painel atual")),
                ],
                t,
            ),
            rec(
                "Observation",
                &ids.observation_missing,
                &[
                    ("name", json!("Evidencia ausente")),
                    ("coded_clearance", json!(1)),
                    ("rationale", json!("aguardando coleta")),
                ],
                t,
            ),
            rec(
                "Observation",
                &ids.observation_flagged,
                &[
                    ("name", json!("Sinal contraindicado")),
                    ("observed_at", json!(t)),
                    ("last_reading_at", json!(t)),
                    ("coded_clearance", json!(0)),
                    ("rationale", json!("flag codificada")),
                ],
                t,
            ),
        ],
    )?;
    seed_links(engine, ops, &ids)?;
    Ok(ids)
}

fn seed_links(engine: &Engine, ops: &Session, ids: &HealthcareIds) -> Result<()> {
    for (link_type, from, to) in [
        (
            "observation_of",
            ids.observation_complete.as_str(),
            ids.patient.as_str(),
        ),
        (
            "observation_of",
            ids.observation_missing.as_str(),
            ids.patient.as_str(),
        ),
        (
            "observation_of",
            ids.observation_flagged.as_str(),
            ids.patient.as_str(),
        ),
        ("order_for", ids.order.as_str(), ids.patient.as_str()),
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
    budget: Option<i64>,
    interfaces: &[&str],
    properties: Vec<PropertySpec>,
) -> ObjectTypeSpec {
    ObjectTypeSpec {
        name: name.into(),
        typology,
        title_prop: Some(title.into()),
        interfaces: interfaces.iter().map(|s| (*s).to_string()).collect(),
        freshness_budget_secs: budget,
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

#[allow(clippy::too_many_arguments)] // policy grant fixture matches PolicySpec columns
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
