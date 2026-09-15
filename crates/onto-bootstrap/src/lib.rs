//! Install teaching cases by calling live OMS builder Actions, then Funnel.
//!
//! [`install`] is wastewater. [`install_highered`] is the higher-education case
//! (Zhang 2026). [`install_healthcare`] is the instructional healthcare case
//! (Zhang 2026). All use the same builder APIs; none is a second OMS.
//! Schema is created on a branch and merged. Instances arrive through Funnel.
//! `AerationTank.target_do` is `ActionWritten`: Funnel must not overwrite it.
//! Healthcare never computes a prescribed quantity.

#![allow(clippy::missing_errors_doc)] // OntoError is the public contract
#![allow(clippy::too_many_lines)] // teaching-case install is one schema dump

mod healthcare;
mod highered;

pub use healthcare::{define_healthcare, install_healthcare, HealthcareIds};
pub use highered::{define_highered, install_highered, HigheredIds};

use onto::{
    ActionTypeSpec, Actor, AgentTier, AuthzDecision, AuthzLevel, AuthzOp, Engine, ExecutionMode,
    FunctionKind, FunctionSpec, IngestRecord, InterfaceSpec, KeyKind, LinkTypeSpec, ObjectTypeSpec,
    ParamSpec, PolicySpec, PropertySource, PropertySpec, Result, Session, Typology, ValueTypeSpec,
};
use serde_json::json;
use std::collections::BTreeMap;

pub struct WastewaterIds {
    pub plant: String,
    pub tank1: String,
    pub tank2: String,
    pub tank3: String,
    pub sensor1: String,
    pub blower: String,
    pub doser: String,
    pub permit: String,
}

/// Install wastewater types, Actions, policies, and seed instances.
///
/// # Context
/// Teaching loop (Zhang 2026): propose setpoint → inbox → confirm or override.
/// Stale or missing sensor evidence is Review plus `request_sensor_calibration`.
/// Permit overshoot and missing role are Deny. Override seals a replayable
/// `DecisionRecord`. Funnel never overwrites `target_do`.
///
/// # Inputs
/// `engine` — live OMS. Types are defined on a working branch and merged to main.
///
/// # Outputs
/// Stable ids for plant, tanks, sensor, blower, doser, and permit.
///
/// # Side effects
/// Writes schema on a branch, merges to main, Funnel-ingests instances, asserts links.
///
/// # Example
/// ```
/// let engine = onto::Engine::memory().unwrap();
/// let ids = onto_bootstrap::install(&engine).unwrap();
/// assert_eq!(ids.tank1, "tank-1");
/// ```
pub fn install(engine: &Engine) -> Result<WastewaterIds> {
    let modeller = Session::new(Actor::builder("human.modeler", &["modeler"]), "bootstrap");
    let reviewer = Session::new(Actor::builder("human.reviewer", &["reviewer"]), "bootstrap");
    let ops = Session::new(
        Actor::consumer("pipeline.funnel", &["operator"], AgentTier::T2),
        "ingest",
    );

    let branch = engine.open_branch(&modeller, "wastewater-v1")?;
    define_language(engine, &modeller, &branch)?;
    let proposal = engine.submit_proposal(&modeller, &branch)?;
    engine.review_proposal(&reviewer, &proposal, true)?;
    engine.merge_to_main(&reviewer, &proposal)?;
    seed_world(engine, &ops)
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
        unit: unit.map(str::to_string),
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

fn derived(name: &str, value_type: &str, function: &str) -> PropertySpec {
    PropertySpec {
        name: name.into(),
        value_type: value_type.into(),
        source: PropertySource::Derived,
        nullable: true,
        function: Some(function.into()),
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
        interfaces: interfaces.iter().copied().map(str::to_string).collect(),
        freshness_budget_secs: budget,
        properties,
    }
}

fn define_language(engine: &Engine, s: &Session, branch: &str) -> Result<()> {
    engine.create_value_type(s, branch, vt("Text", "string", None, None, None))?;
    engine.create_value_type(s, branch, vt("Timestamp", "number", None, None, Some("s")))?;
    engine.create_value_type(
        s,
        branch,
        vt(
            "DOConcentration",
            "number",
            Some(0.0),
            Some(15.0),
            Some("mg/L"),
        ),
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
            "TreatmentPlant",
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
            "AerationTank",
            Typology::Entity,
            "name",
            None,
            &[],
            vec![
                prop("name", "Text", PropertySource::Mapped, false),
                prop(
                    "current_do",
                    "DOConcentration",
                    PropertySource::Mapped,
                    true,
                ),
                prop(
                    "target_do",
                    "DOConcentration",
                    PropertySource::ActionWritten,
                    true,
                ),
            ],
        ),
    )?;
    engine.create_object_type(
        s,
        branch,
        obj(
            "Blower",
            Typology::Entity,
            "name",
            None,
            &[],
            vec![
                prop("name", "Text", PropertySource::Mapped, false),
                prop("status", "Text", PropertySource::Mapped, true),
            ],
        ),
    )?;
    engine.create_function(
        s,
        branch,
        FunctionSpec {
            name: "days_since_calibration".into(),
            inputs: vec!["calibration_date".into()],
            kind: FunctionKind::DaysSinceTimestamp,
        },
    )?;
    engine.create_object_type(
        s,
        branch,
        obj(
            "DO_Sensor",
            Typology::Entity,
            "name",
            Some(300),
            &[],
            vec![
                prop("name", "Text", PropertySource::Mapped, false),
                prop(
                    "calibration_date",
                    "Timestamp",
                    PropertySource::Mapped,
                    false,
                ),
                prop(
                    "last_reading_at",
                    "Timestamp",
                    PropertySource::Mapped,
                    false,
                ),
                derived(
                    "days_since_calibration",
                    "Timestamp",
                    "days_since_calibration",
                ),
            ],
        ),
    )?;
    engine.create_object_type(
        s,
        branch,
        obj(
            "ChemicalDoser",
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
            "TelemetryReading",
            Typology::Event,
            "name",
            Some(300),
            &[],
            vec![
                prop("name", "Text", PropertySource::Mapped, true),
                prop("do_value", "DOConcentration", PropertySource::Mapped, false),
                prop("observed_at", "Timestamp", PropertySource::Mapped, false),
            ],
        ),
    )?;
    engine.create_object_type(
        s,
        branch,
        obj(
            "LabMeasurement",
            Typology::Event,
            "name",
            None,
            &[],
            vec![
                prop("name", "Text", PropertySource::Mapped, true),
                prop("value", "DOConcentration", PropertySource::Mapped, false),
            ],
        ),
    )?;
    engine.create_object_type(
        s,
        branch,
        obj(
            "EffluentRecord",
            Typology::Event,
            "name",
            None,
            &[],
            vec![prop("name", "Text", PropertySource::Mapped, true)],
        ),
    )?;
    engine.create_object_type(
        s,
        branch,
        obj(
            "AlarmEvent",
            Typology::Event,
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
            "OperatingState",
            Typology::State,
            "name",
            None,
            &[],
            vec![
                prop("name", "Text", PropertySource::Mapped, true),
                prop("do", "DOConcentration", PropertySource::Mapped, true),
                prop("load_band", "Text", PropertySource::Mapped, true),
            ],
        ),
    )?;
    engine.create_object_type(
        s,
        branch,
        obj(
            "PermitVersion",
            Typology::Entity,
            "name",
            None,
            &[],
            vec![
                prop("name", "Text", PropertySource::Mapped, false),
                prop("do_max", "DOConcentration", PropertySource::Mapped, false),
            ],
        ),
    )?;
    engine.create_object_type(
        s,
        branch,
        obj(
            "ControlRecommendation",
            Typology::DecisionRecord,
            "name",
            None,
            &["Reviewable", "Evidenced"],
            vec![
                prop("name", "Text", PropertySource::ActionWritten, true),
                prop(
                    "target_do",
                    "DOConcentration",
                    PropertySource::ActionWritten,
                    false,
                ),
                prop("rationale", "Text", PropertySource::ActionWritten, false),
                prop("status", "Text", PropertySource::ActionWritten, false),
            ],
        ),
    )?;
    engine.create_object_type(
        s,
        branch,
        obj(
            "CompliancePrediction",
            Typology::DecisionRecord,
            "name",
            None,
            &[],
            vec![prop("name", "Text", PropertySource::ActionWritten, true)],
        ),
    )?;
    engine.create_object_type(
        s,
        branch,
        obj(
            "ApprovalRecord",
            Typology::DecisionRecord,
            "name",
            None,
            &[],
            vec![prop("name", "Text", PropertySource::ActionWritten, true)],
        ),
    )?;
    engine.create_object_type(
        s,
        branch,
        obj(
            "Override",
            Typology::DecisionRecord,
            "name",
            None,
            &[],
            vec![
                prop("category", "Text", PropertySource::ActionWritten, false),
                prop("reason", "Text", PropertySource::ActionWritten, false),
            ],
        ),
    )?;
    engine.create_object_type(
        s,
        branch,
        obj(
            "VerificationRecord",
            Typology::DecisionRecord,
            "name",
            None,
            &[],
            vec![prop("name", "Text", PropertySource::ActionWritten, true)],
        ),
    )?;
    engine.create_object_type(
        s,
        branch,
        obj(
            "CalibrationRequest",
            Typology::DecisionRecord,
            "name",
            None,
            &[],
            vec![prop("name", "Text", PropertySource::ActionWritten, true)],
        ),
    )?;

    for (name, from, to, card, cycles) in [
        ("contains", "TreatmentPlant", "AerationTank", "1:n", false),
        ("upstream_of", "AerationTank", "AerationTank", "n:n", true),
        ("monitors", "DO_Sensor", "AerationTank", "1:1", false),
        ("supplies_air", "Blower", "AerationTank", "1:n", false),
        ("treats", "ChemicalDoser", "AerationTank", "n:n", false),
        (
            "evidence_for",
            "TelemetryReading",
            "ControlRecommendation",
            "n:n",
            false,
        ),
    ] {
        engine.create_link_type(
            s,
            branch,
            LinkTypeSpec {
                name: name.into(),
                from_type: from.into(),
                to_type: to.into(),
                cardinality: card.into(),
                allow_cycles: cycles,
            },
        )?;
    }

    let propose = ActionTypeSpec {
        name: "propose_setpoint_change".into(),
        mode: ExecutionMode::Propose,
        parameters: vec![
            ParamSpec {
                name: "tank".into(),
                value_type: "Text".into(),
                object_type: Some("AerationTank".into()),
                required: true,
            },
            ParamSpec {
                name: "sensor".into(),
                value_type: "Text".into(),
                object_type: Some("DO_Sensor".into()),
                required: true,
            },
            ParamSpec {
                name: "permit".into(),
                value_type: "Text".into(),
                object_type: Some("PermitVersion".into()),
                required: true,
            },
            ParamSpec {
                name: "target_do".into(),
                value_type: "DOConcentration".into(),
                object_type: None,
                required: true,
            },
            ParamSpec {
                name: "rationale".into(),
                value_type: "Text".into(),
                object_type: None,
                required: true,
            },
        ],
        guards: json!([
            { "freshness": "sensor", "max_age_secs": 300 },
            { "exists_field": "last_reading_at", "object": "sensor" },
            { "lte_field": "target_do", "object": "permit", "field": "do_max" },
            { "max_days_since_calibration": 365, "object": "sensor" }
        ]),
        required_roles: vec!["operator".into()],
        required_tier: AgentTier::T2,
        effects: json!([]),
        compensation: Some("revert_setpoint_change".into()),
        side_effects: json!({}),
        on_review: Some("request_sensor_calibration".into()),
        interfaces: vec![],
    };
    engine.create_action_type(s, branch, propose)?;
    engine.attach_interface(s, branch, "propose_setpoint_change", "Reviewable")?;

    engine.create_action_type(
        s,
        branch,
        ActionTypeSpec {
            name: "approve_setpoint_change".into(),
            mode: ExecutionMode::Auto,
            parameters: vec![
                ParamSpec {
                    name: "tank".into(),
                    value_type: "Text".into(),
                    object_type: Some("AerationTank".into()),
                    required: true,
                },
                ParamSpec {
                    name: "sensor".into(),
                    value_type: "Text".into(),
                    object_type: Some("DO_Sensor".into()),
                    required: true,
                },
                ParamSpec {
                    name: "permit".into(),
                    value_type: "Text".into(),
                    object_type: Some("PermitVersion".into()),
                    required: true,
                },
                ParamSpec {
                    name: "target_do".into(),
                    value_type: "DOConcentration".into(),
                    object_type: None,
                    required: true,
                },
                ParamSpec {
                    name: "rationale".into(),
                    value_type: "Text".into(),
                    object_type: None,
                    required: true,
                },
            ],
            guards: json!([
                { "freshness": "sensor", "max_age_secs": 300 },
                { "lte_field": "target_do", "object": "permit", "field": "do_max" }
            ]),
            required_roles: vec!["supervisor".into()],
            required_tier: AgentTier::T3,
            effects: json!([
                {
                    "update": "tank",
                    "properties": { "target_do": "$target_do" }
                },
                {
                    "create": "ApprovalRecord",
                    "properties": { "name": "$rationale" }
                }
            ]),
            compensation: Some("revert_setpoint_change".into()),
            side_effects: json!({ "dcs": "apply_setpoint_change", "idempotent": true }),
            on_review: Some("request_sensor_calibration".into()),
            interfaces: vec![],
        },
    )?;

    engine.create_action_type(
        s,
        branch,
        ActionTypeSpec {
            name: "revert_setpoint_change".into(),
            mode: ExecutionMode::Auto,
            parameters: vec![
                ParamSpec {
                    name: "tank".into(),
                    value_type: "Text".into(),
                    object_type: Some("AerationTank".into()),
                    required: true,
                },
                ParamSpec {
                    name: "sensor".into(),
                    value_type: "Text".into(),
                    object_type: Some("DO_Sensor".into()),
                    required: true,
                },
                ParamSpec {
                    name: "permit".into(),
                    value_type: "Text".into(),
                    object_type: Some("PermitVersion".into()),
                    required: true,
                },
                ParamSpec {
                    name: "target_do".into(),
                    value_type: "DOConcentration".into(),
                    object_type: None,
                    required: true,
                },
                ParamSpec {
                    name: "rationale".into(),
                    value_type: "Text".into(),
                    object_type: None,
                    required: true,
                },
            ],
            guards: json!([
                { "freshness": "sensor", "max_age_secs": 300 },
                { "lte_field": "target_do", "object": "permit", "field": "do_max" }
            ]),
            required_roles: vec!["supervisor".into()],
            required_tier: AgentTier::T3,
            effects: json!([{
                "update": "tank",
                "properties": { "target_do": "$target_do" }
            }]),
            compensation: None,
            side_effects: json!({ "dcs": "revert_setpoint_change", "idempotent": true }),
            on_review: None,
            interfaces: vec![],
        },
    )?;

    engine.create_action_type(
        s,
        branch,
        ActionTypeSpec {
            name: "override_setpoint".into(),
            mode: ExecutionMode::Auto,
            parameters: vec![
                ParamSpec {
                    name: "tank".into(),
                    value_type: "Text".into(),
                    object_type: Some("AerationTank".into()),
                    required: true,
                },
                ParamSpec {
                    name: "override_category".into(),
                    value_type: "Text".into(),
                    object_type: None,
                    required: true,
                },
                ParamSpec {
                    name: "override_reason".into(),
                    value_type: "Text".into(),
                    object_type: None,
                    required: true,
                },
            ],
            guards: json!([]),
            required_roles: vec!["supervisor".into()],
            required_tier: AgentTier::T3,
            effects: json!([{
                "create": "Override",
                "properties": {
                    "category": "$override_category",
                    "reason": "$override_reason"
                }
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

    seed_policies(engine, s, branch)?;

    engine.create_action_type(
        s,
        branch,
        ActionTypeSpec {
            name: "request_sensor_calibration".into(),
            mode: ExecutionMode::Auto,
            parameters: vec![ParamSpec {
                name: "sensor".into(),
                value_type: "Text".into(),
                object_type: Some("DO_Sensor".into()),
                required: true,
            }],
            guards: json!([]),
            required_roles: vec!["operator".into()],
            required_tier: AgentTier::T2,
            effects: json!([{
                "create": "CalibrationRequest",
                "properties": { "name": "calibrate" }
            }]),
            compensation: None,
            side_effects: json!({}),
            on_review: None,
            interfaces: vec![],
        },
    )?;

    Ok(())
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

fn seed_world(engine: &Engine, ops: &Session) -> Result<WastewaterIds> {
    let t = engine.now();
    let ids = WastewaterIds {
        plant: "plant-1".into(),
        tank1: "tank-1".into(),
        tank2: "tank-2".into(),
        tank3: "tank-3".into(),
        sensor1: "sensor-1".into(),
        blower: "blower-1".into(),
        doser: "doser-1".into(),
        permit: "permit-2026".into(),
    };
    engine.funnel_ingest(
        ops,
        vec![
            rec(
                "TreatmentPlant",
                &ids.plant,
                &[("name", json!("Eastworks"))],
                t,
            ),
            rec(
                "AerationTank",
                &ids.tank1,
                &[("name", json!("Basin 1")), ("current_do", json!(1.8))],
                t,
            ),
            rec(
                "AerationTank",
                &ids.tank2,
                &[("name", json!("Basin 2")), ("current_do", json!(2.1))],
                t,
            ),
            rec(
                "AerationTank",
                &ids.tank3,
                &[("name", json!("Basin 3")), ("current_do", json!(1.9))],
                t,
            ),
            rec(
                "DO_Sensor",
                &ids.sensor1,
                &[
                    ("name", json!("DO-1")),
                    ("calibration_date", json!(t - 10 * 86_400)),
                    ("last_reading_at", json!(t)),
                ],
                t,
            ),
            rec(
                "Blower",
                &ids.blower,
                &[("name", json!("B-1")), ("status", json!("on"))],
                t,
            ),
            rec(
                "ChemicalDoser",
                &ids.doser,
                &[("name", json!("Carbon-1"))],
                t,
            ),
            rec(
                "PermitVersion",
                &ids.permit,
                &[("name", json!("2026-A")), ("do_max", json!(4.0))],
                t,
            ),
            rec(
                "TelemetryReading",
                "tele-1",
                &[
                    ("name", json!("t1")),
                    ("do_value", json!(1.8)),
                    ("observed_at", json!(t)),
                ],
                t,
            ),
        ],
    )?;
    seed_links(engine, ops, &ids)?;
    Ok(ids)
}

fn seed_links(engine: &Engine, ops: &Session, ids: &WastewaterIds) -> Result<()> {
    for (link_type, from, to) in [
        ("contains", ids.plant.as_str(), ids.tank1.as_str()),
        ("contains", ids.plant.as_str(), ids.tank2.as_str()),
        ("contains", ids.plant.as_str(), ids.tank3.as_str()),
        ("upstream_of", ids.tank1.as_str(), ids.tank2.as_str()),
        ("upstream_of", ids.tank2.as_str(), ids.tank3.as_str()),
        ("upstream_of", ids.tank3.as_str(), ids.tank1.as_str()),
        ("monitors", ids.sensor1.as_str(), ids.tank1.as_str()),
        ("supplies_air", ids.blower.as_str(), ids.tank1.as_str()),
        ("treats", ids.doser.as_str(), ids.tank1.as_str()),
        ("treats", ids.doser.as_str(), ids.tank2.as_str()),
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
        grant(
            "property_rationale_deny_restricted",
            AuthzLevel::Property,
            AuthzOp::Read,
            consumer,
            Some("*"),
            Some("*"),
            Some("rationale"),
            &["restricted"],
            1,
            AuthzDecision::Deny,
        ),
        grant(
            "property_do_max_deny_restricted",
            AuthzLevel::Property,
            AuthzOp::Read,
            consumer,
            Some("*"),
            Some("*"),
            Some("do_max"),
            &["restricted"],
            1,
            AuthzDecision::Deny,
        ),
    ] {
        engine.create_policy(s, branch, spec)?;
    }
    Ok(())
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
        type_name: type_name.map(str::to_string),
        instance_id: instance_id.map(str::to_string),
        property: property.map(str::to_string),
        roles: roles.iter().copied().map(str::to_string).collect(),
        min_tier,
        decision,
    }
}
