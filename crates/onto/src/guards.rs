//! Typed Action guards. Unknown operators never become Allow (Zhang 2026).
//!
//! Publication parses JSON into [`Guard`]. Evaluation is fail-closed: a missing
//! bound is insufficient evidence, not infinity.

use crate::error::{OntoError, Result};
use crate::types::{GuardResult, ObjectView, SnapshotObject, Verdict};
use serde_json::Value;

/// Supported guard operators. A new variant is a compile break.
#[derive(Debug, Clone, PartialEq)]
pub enum Guard {
    Freshness {
        object: String,
        max_age_secs: i64,
    },
    ExistsField {
        object: String,
        field: String,
    },
    LteField {
        param: String,
        object: String,
        field: String,
    },
    Lte {
        param: String,
        max: f64,
    },
    Gt {
        param: String,
        min: f64,
    },
    GtField {
        object: String,
        field: String,
        min: f64,
    },
    MaxDaysSinceCalibration {
        object: String,
        days: i64,
    },
    EqField {
        param: String,
        object: String,
        field: String,
    },
    Linked {
        link: String,
        from: String,
        to: String,
    },
}

const OPERATORS: [&str; 9] = [
    "freshness",
    "exists_field",
    "lte_field",
    "lte",
    "gt",
    "gt_field",
    "max_days_since_calibration",
    "eq_field",
    "linked",
];

/// Parse published guards. Empty list or `{}` is an intentional empty rule.
#[allow(clippy::module_name_repetitions)] // public parse entry for Guard JSON
pub fn parse_guards(value: &Value) -> Result<Vec<Guard>> {
    if value.is_null() || value == &Value::Object(serde_json::Map::new()) {
        return Ok(Vec::new());
    }
    if let Some(arr) = value.as_array() {
        return arr.iter().map(parse_one).collect();
    }
    Ok(vec![parse_one(value)?])
}

fn parse_one(value: &Value) -> Result<Guard> {
    let obj = value
        .as_object()
        .ok_or_else(|| OntoError::Invalid("guard must be object".into()))?;
    let present: Vec<&str> = OPERATORS
        .iter()
        .copied()
        .filter(|k| obj.contains_key(*k))
        .collect();
    if present.len() != 1 {
        return Err(OntoError::Invalid(
            "unrecognized or compound guard (fail closed)".into(),
        ));
    }
    match present.first().copied() {
        None => Err(OntoError::Invalid("unrecognized guard".into())),
        Some(op) => match op {
            "freshness" => {
                let object = obj
                    .get("freshness")
                    .and_then(Value::as_str)
                    .ok_or_else(|| OntoError::Invalid("freshness needs an object param".into()))?
                    .to_string();
                let max_age_secs = obj
                    .get("max_age_secs")
                    .and_then(Value::as_i64)
                    .ok_or_else(|| OntoError::Invalid("freshness needs max_age_secs".into()))?;
                Ok(Guard::Freshness {
                    object,
                    max_age_secs,
                })
            }
            "exists_field" => {
                let field = obj
                    .get("exists_field")
                    .and_then(Value::as_str)
                    .ok_or_else(|| OntoError::Invalid("exists_field needs a field".into()))?
                    .to_string();
                let object = required_str(obj, "object")?;
                Ok(Guard::ExistsField { object, field })
            }
            "lte_field" => {
                let param = obj
                    .get("lte_field")
                    .and_then(Value::as_str)
                    .ok_or_else(|| OntoError::Invalid("lte_field needs a param".into()))?
                    .to_string();
                let object = required_str(obj, "object")?;
                let field = required_str(obj, "field")?;
                Ok(Guard::LteField {
                    param,
                    object,
                    field,
                })
            }
            "lte" => {
                let max = obj
                    .get("lte")
                    .and_then(Value::as_f64)
                    .ok_or_else(|| OntoError::Invalid("lte needs a number".into()))?;
                let param = required_str(obj, "param")?;
                Ok(Guard::Lte { param, max })
            }
            "gt" => {
                let min = obj
                    .get("gt")
                    .and_then(Value::as_f64)
                    .ok_or_else(|| OntoError::Invalid("gt needs a number".into()))?;
                let param = required_str(obj, "param")?;
                Ok(Guard::Gt { param, min })
            }
            "gt_field" => {
                let min = obj
                    .get("gt_field")
                    .and_then(Value::as_f64)
                    .ok_or_else(|| OntoError::Invalid("gt_field needs a number".into()))?;
                let object = required_str(obj, "object")?;
                let field = required_str(obj, "field")?;
                Ok(Guard::GtField { object, field, min })
            }
            "max_days_since_calibration" => {
                let days = obj
                    .get("max_days_since_calibration")
                    .and_then(Value::as_i64)
                    .ok_or_else(|| {
                        OntoError::Invalid("max_days_since_calibration needs days".into())
                    })?;
                let object = required_str(obj, "object")?;
                Ok(Guard::MaxDaysSinceCalibration { object, days })
            }
            "eq_field" => parse_eq_field(obj),
            "linked" => parse_linked(obj),
            other => {
                let _ = other;
                Err(OntoError::Invalid("unrecognized guard".into()))
            }
        },
    }
}

fn required_str(obj: &serde_json::Map<String, Value>, key: &str) -> Result<String> {
    obj.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| OntoError::Invalid(format!("guard needs {key}")))
}

fn parse_eq_field(obj: &serde_json::Map<String, Value>) -> Result<Guard> {
    let param = obj
        .get("eq_field")
        .and_then(Value::as_str)
        .ok_or_else(|| OntoError::Invalid("eq_field needs a param".into()))?
        .to_string();
    Ok(Guard::EqField {
        param,
        object: required_str(obj, "object")?,
        field: required_str(obj, "field")?,
    })
}

fn parse_linked(obj: &serde_json::Map<String, Value>) -> Result<Guard> {
    let link = obj
        .get("linked")
        .and_then(Value::as_str)
        .ok_or_else(|| OntoError::Invalid("linked needs a link type".into()))?
        .to_string();
    Ok(Guard::Linked {
        link,
        from: required_str(obj, "from")?,
        to: required_str(obj, "to")?,
    })
}

/// Evaluate parsed guards. Empty set is an intentional pass.
pub fn evaluate<F, L>(
    guards: &[Guard],
    params: &Value,
    now: i64,
    mut read: F,
    mut linked: L,
) -> Result<(Vec<GuardResult>, Vec<SnapshotObject>)>
where
    F: FnMut(&str) -> Result<ObjectView>,
    L: FnMut(&str, &str, &str) -> Result<bool>,
{
    let mut reads = Vec::new();
    if guards.is_empty() {
        return Ok((
            vec![GuardResult {
                name: "empty".into(),
                verdict: Verdict::Allow,
                reason: "no guards".into(),
            }],
            reads,
        ));
    }
    let mut out = Vec::new();
    for guard in guards {
        out.push(eval_one(
            guard,
            params,
            now,
            &mut read,
            &mut linked,
            &mut reads,
        )?);
    }
    Ok((out, reads))
}

#[allow(clippy::too_many_lines)] // one arm per Guard variant
fn eval_one<F, L>(
    guard: &Guard,
    params: &Value,
    now: i64,
    read: &mut F,
    linked: &mut L,
    reads: &mut Vec<SnapshotObject>,
) -> Result<GuardResult>
where
    F: FnMut(&str) -> Result<ObjectView>,
    L: FnMut(&str, &str, &str) -> Result<bool>,
{
    match guard {
        Guard::Freshness {
            object,
            max_age_secs,
        } => {
            let id = param_str(params, object)?;
            let view = read_view(&id, read, reads)?;
            let as_of = freshness_stamp(&view);
            let Some(as_of) = as_of else {
                return Ok(GuardResult {
                    name: "freshness".into(),
                    verdict: Verdict::Review,
                    reason: format!("no freshness stamp on {object} (Complete fail)"),
                });
            };
            let Ok(ts) = as_of.parse::<i64>() else {
                return Ok(GuardResult {
                    name: "freshness".into(),
                    verdict: Verdict::Review,
                    reason: format!("unparseable freshness stamp on {object}"),
                });
            };
            if now - ts > *max_age_secs {
                return Ok(GuardResult {
                    name: "freshness".into(),
                    verdict: Verdict::Review,
                    reason: format!(
                        "stale {object}: age {}s > {max_age_secs}s (Current fail)",
                        now - ts
                    ),
                });
            }
            Ok(GuardResult {
                name: "freshness".into(),
                verdict: Verdict::Allow,
                reason: "fresh".into(),
            })
        }
        Guard::ExistsField { object, field } => {
            let id = param_str(params, object)?;
            let view = read_view(&id, read, reads)?;
            if view.properties.contains_key(field) {
                Ok(GuardResult {
                    name: "complete".into(),
                    verdict: Verdict::Allow,
                    reason: "present".into(),
                })
            } else {
                Ok(GuardResult {
                    name: "complete".into(),
                    verdict: Verdict::Review,
                    reason: format!("missing {field} on {object} (Complete fail)"),
                })
            }
        }
        Guard::LteField {
            param,
            object,
            field,
        } => {
            let n = param_f64(params, param)?;
            let id = param_str(params, object)?;
            let view = read_view(&id, read, reads)?;
            let Some(bound) = view.properties.get(field).and_then(|p| p.value.as_f64()) else {
                return Ok(GuardResult {
                    name: "insufficient_evidence".into(),
                    verdict: Verdict::Review,
                    reason: format!("missing numeric bound {field} on {object}"),
                });
            };
            if n > bound {
                return Ok(GuardResult {
                    name: "permit_limit".into(),
                    verdict: Verdict::Deny,
                    reason: format!("{param}={n} exceeds {field}={bound}"),
                });
            }
            Ok(GuardResult {
                name: "permit_limit".into(),
                verdict: Verdict::Allow,
                reason: "within permit".into(),
            })
        }
        Guard::Lte { param, max } => {
            let n = param_f64(params, param)?;
            if n > *max {
                return Ok(GuardResult {
                    name: "lte".into(),
                    verdict: Verdict::Deny,
                    reason: format!("{param}={n} > {max}"),
                });
            }
            Ok(GuardResult {
                name: "lte".into(),
                verdict: Verdict::Allow,
                reason: "ok".into(),
            })
        }
        Guard::Gt { param, min } => {
            let n = param_f64(params, param)?;
            if n > *min {
                Ok(GuardResult {
                    name: "gt".into(),
                    verdict: Verdict::Allow,
                    reason: "ok".into(),
                })
            } else {
                Ok(GuardResult {
                    name: "gt".into(),
                    verdict: Verdict::Deny,
                    reason: format!("{param}={n} is not greater than {min}"),
                })
            }
        }
        Guard::GtField { object, field, min } => {
            let id = param_str(params, object)?;
            let view = read_view(&id, read, reads)?;
            let Some(n) = view.properties.get(field).and_then(|p| p.value.as_f64()) else {
                return Ok(GuardResult {
                    name: "insufficient_evidence".into(),
                    verdict: Verdict::Review,
                    reason: format!("missing numeric field {field} on {object}"),
                });
            };
            if n > *min {
                Ok(GuardResult {
                    name: "gt_field".into(),
                    verdict: Verdict::Allow,
                    reason: "ok".into(),
                })
            } else {
                Ok(GuardResult {
                    name: "gt_field".into(),
                    verdict: Verdict::Deny,
                    reason: format!("{field}={n} is not greater than {min}"),
                })
            }
        }
        Guard::MaxDaysSinceCalibration { object, days } => {
            let id = param_str(params, object)?;
            let view = read_view(&id, read, reads)?;
            let Some(cal) = view
                .properties
                .get("calibration_date")
                .and_then(|p| p.value.as_i64().or_else(|| p.value.as_str()?.parse().ok()))
            else {
                return Ok(GuardResult {
                    name: "calibration".into(),
                    verdict: Verdict::Review,
                    reason: format!("missing calibration_date on {object}"),
                });
            };
            let elapsed = (now - cal) / 86_400;
            if elapsed > *days {
                return Ok(GuardResult {
                    name: "calibration".into(),
                    verdict: Verdict::Review,
                    reason: format!("sensor calibration is {elapsed} days old"),
                });
            }
            Ok(GuardResult {
                name: "calibration".into(),
                verdict: Verdict::Allow,
                reason: "calibrated".into(),
            })
        }
        Guard::EqField {
            param,
            object,
            field,
        } => {
            let expected = param_str(params, param)?;
            let id = param_str(params, object)?;
            let view = read_view(&id, read, reads)?;
            let Some(actual) = view.properties.get(field).and_then(|p| p.value.as_str()) else {
                return Ok(GuardResult {
                    name: "eq_field".into(),
                    verdict: Verdict::Review,
                    reason: format!("missing {field} on {object}"),
                });
            };
            if actual == expected {
                Ok(GuardResult {
                    name: "eq_field".into(),
                    verdict: Verdict::Allow,
                    reason: "bound".into(),
                })
            } else {
                Ok(GuardResult {
                    name: "eq_field".into(),
                    verdict: Verdict::Deny,
                    reason: format!("{param} is not the committed {field}"),
                })
            }
        }
        Guard::Linked { link, from, to } => {
            let from_id = param_str(params, from)?;
            let to_id = param_str(params, to)?;
            if linked(link, &from_id, &to_id)? {
                Ok(GuardResult {
                    name: "linked".into(),
                    verdict: Verdict::Allow,
                    reason: "bound".into(),
                })
            } else {
                Ok(GuardResult {
                    name: "linked".into(),
                    verdict: Verdict::Deny,
                    reason: format!("{from} is not linked to {to} by {link}"),
                })
            }
        }
    }
}

fn freshness_stamp(view: &ObjectView) -> Option<String> {
    view.properties
        .get("last_reading_at")
        .or_else(|| view.properties.get("observed_at"))
        .and_then(|p| {
            p.as_of
                .clone()
                .or_else(|| p.value.as_str().map(str::to_string))
                .or_else(|| p.value.as_i64().map(|n| n.to_string()))
        })
}

fn read_view<F>(id: &str, read: &mut F, reads: &mut Vec<SnapshotObject>) -> Result<ObjectView>
where
    F: FnMut(&str) -> Result<ObjectView>,
{
    let view = read(id)?;
    if let Some(existing) = reads.iter_mut().find(|s| s.id == id) {
        existing.delegated = true;
    } else {
        let mut snap = crate::types::snapshot_from_view(&view);
        snap.delegated = true;
        reads.push(snap);
    }
    Ok(view)
}

fn param_str(params: &Value, name: &str) -> Result<String> {
    params
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| OntoError::Invalid(format!("param {name} must be a string id")))
}

fn param_f64(params: &Value, name: &str) -> Result<f64> {
    params
        .get(name)
        .and_then(Value::as_f64)
        .ok_or_else(|| OntoError::Invalid(format!("param {name} must be a number")))
}

/// Map an OMS value type name onto a JSON Schema `type` string.
#[must_use]
pub fn json_schema_type(value_type: &str) -> &'static str {
    match value_type {
        "Integer" | "Count" | "Number" | "DOConcentration" => "number",
        "Boolean" => "boolean",
        _ => "string",
    }
}

/// Project a persisted [`ValueTypeSpec`] into the JSON Schema the runtime accepts.
#[must_use]
pub fn json_schema_from_value_type(spec: &crate::types::ValueTypeSpec) -> serde_json::Value {
    let type_name = match spec.base.as_str() {
        "number" => "number",
        "boolean" => "boolean",
        _ => "string",
    };
    let mut schema = serde_json::Map::new();
    schema.insert("type".into(), serde_json::json!(type_name));
    if let Some(min) = spec.min {
        schema.insert("minimum".into(), serde_json::json!(min));
    }
    if let Some(max) = spec.max {
        schema.insert("maximum".into(), serde_json::json!(max));
    }
    serde_json::Value::Object(schema)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ObjectView;
    use serde_json::json;
    use std::collections::BTreeMap;

    #[test]
    fn does_reject_unknown_guard_at_parse() {
        let err = parse_guards(&json!([{"must_never_be_accepted": true}])).unwrap_err();
        assert!(matches!(err, OntoError::Invalid(_)));
    }

    #[test]
    fn does_treat_missing_lte_bound_as_review() {
        let guards =
            parse_guards(&json!([{"lte_field":"n","object":"permit","field":"missing"}])).unwrap();
        let view = ObjectView {
            id: "p".into(),
            type_name: "PermitVersion".into(),
            title: None,
            properties: BTreeMap::new(),
            missing: vec![],
            stale: vec![],
            version_id: String::new(),
        };
        let (results, _) = evaluate(
            &guards,
            &json!({"permit":"p","n":2.0}),
            1,
            |_| Ok(view.clone()),
            |_, _, _| Ok(false),
        )
        .unwrap();
        assert_eq!(results[0].verdict, Verdict::Review);
        assert_eq!(results[0].name, "insufficient_evidence");
    }

    #[test]
    fn does_map_text_to_json_schema_string() {
        assert_eq!(json_schema_type("Text"), "string");
        assert_eq!(json_schema_type("DOConcentration"), "number");
        let clearance = crate::types::ValueTypeSpec {
            name: "Clearance".into(),
            base: "number".into(),
            min: Some(1.0),
            max: Some(1.0),
            unit: None,
        };
        assert_eq!(
            json_schema_from_value_type(&clearance)["type"],
            serde_json::json!("number")
        );
    }

    #[test]
    fn does_deny_eq_field_if_value_differs() {
        let guards =
            parse_guards(&json!([{"eq_field":"student","object":"seat","field":"occupant"}]))
                .unwrap();
        let mut properties = BTreeMap::new();
        properties.insert(
            "occupant".into(),
            crate::types::PropertyView {
                value: json!("ana"),
                source: crate::types::PropertySource::ActionWritten,
                as_of: None,
                provenance: None,
            },
        );
        let view = ObjectView {
            id: "seat".into(),
            type_name: "Seat".into(),
            title: None,
            properties,
            missing: vec![],
            stale: vec![],
            version_id: String::new(),
        };
        let (results, reads) = evaluate(
            &guards,
            &json!({"student":"bruno","seat":"seat"}),
            1,
            |_| Ok(view.clone()),
            |_, _, _| Ok(false),
        )
        .unwrap();
        assert_eq!(results[0].verdict, Verdict::Deny);
        assert!(reads[0].delegated);
    }

    #[test]
    fn does_deny_linked_if_edge_is_missing() {
        let guards =
            parse_guards(&json!([{"linked":"observation_of","from":"observation","to":"patient"}]))
                .unwrap();
        let (results, _) = evaluate(
            &guards,
            &json!({"observation":"o1","patient":"p1"}),
            1,
            |_| {
                Ok(ObjectView {
                    id: "o1".into(),
                    type_name: "Observation".into(),
                    title: None,
                    properties: BTreeMap::new(),
                    missing: vec![],
                    stale: vec![],
                    version_id: String::new(),
                })
            },
            |_, _, _| Ok(false),
        )
        .unwrap();
        assert_eq!(results[0].verdict, Verdict::Deny);
    }
}
