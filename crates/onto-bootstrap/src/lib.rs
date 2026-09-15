//! Install the wastewater case by calling live OMS builder Actions, then Funnel.

use onto::{
    ActionTypeSpec, Actor, AgentTier, Engine, ExecutionMode, FunctionKind, FunctionSpec,
    IngestRecord, InterfaceSpec, LinkTypeSpec, ObjectTypeSpec, ParamSpec, PropertySource,
    PropertySpec, Result, Session, Typology, ValueTypeSpec,
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
        interfaces: interfaces.iter().map(|s| (*s).to_string()).collect(),
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
                prop("last_reading_at", "Timestamp", PropertySource::Mapped, true),
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
    };
    engine.create_action_type(s, branch, propose)?;

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
        },
    )?;

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
    engine.create_link(ops, "contains", &ids.plant, &ids.tank1)?;
    engine.create_link(ops, "contains", &ids.plant, &ids.tank2)?;
    engine.create_link(ops, "contains", &ids.plant, &ids.tank3)?;
    engine.create_link(ops, "upstream_of", &ids.tank1, &ids.tank2)?;
    engine.create_link(ops, "upstream_of", &ids.tank2, &ids.tank3)?;
    engine.create_link(ops, "upstream_of", &ids.tank3, &ids.tank1)?;
    engine.create_link(ops, "monitors", &ids.sensor1, &ids.tank1)?;
    engine.create_link(ops, "supplies_air", &ids.blower, &ids.tank1)?;
    engine.create_link(ops, "treats", &ids.doser, &ids.tank1)?;
    engine.create_link(ops, "treats", &ids.doser, &ids.tank2)?;
    Ok(ids)
}
