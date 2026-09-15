//! Functions as versioned OMS records (Zhang 2026, Ch. 7).
//!
//! Derived properties name a [`FunctionSpec`]. The engine looks that record up
//! on the read branch (production: main) and evaluates [`FunctionKind`]. There
//! is no match on property name. A new function is registered with
//! `create_function` on a working branch, then merged.
//!
//! # Context
//! Functions compute derived properties and may later be used in guards. They
//! are ontology objects, not engine `if` arms.
//!
//! # Inputs / outputs
//! [`apply`] takes a spec, the object's stored properties, and the engine clock.
//! It returns a derived [`PropertyView`] or `None` when inputs are missing.
//!
//! # Side effects
//! None. Evaluation is pure given `(spec, properties, now)`.
//!
//! # Relations
//! [`crate::Engine::create_function`] stores the spec. [`crate::PropertySpec::function`]
//! names it. DecisionRecord `function_version` is [`digest`] of invoked pins.

use crate::types::{PropertySource, PropertyView};
use crate::write_path::pin_version;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// Body key for a registered function. Exhaustive: a new variant is a compile break.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FunctionKind {
    /// `(now - timestamp_input) / 86400`, clamped at zero.
    DaysSinceTimestamp,
    /// Integer input multiplied by two.
    DoubleInteger,
}

/// OMS record: named function, input property names, and a kind/body key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionSpec {
    pub name: String,
    pub inputs: Vec<String>,
    pub kind: FunctionKind,
}

impl FunctionSpec {
    /// Stable pin `name:hash` over kind + inputs. Used on DecisionRecord.
    pub fn pin(&self) -> String {
        pin_version(&self.name, &format!("{}:{}", kind_key(self.kind), self.inputs.join(",")))
    }
}

fn kind_key(kind: FunctionKind) -> &'static str {
    match kind {
        FunctionKind::DaysSinceTimestamp => "days_since_timestamp",
        FunctionKind::DoubleInteger => "double_integer",
    }
}

/// Evaluate `spec` against stored properties at engine clock `now`.
pub fn apply(
    spec: &FunctionSpec,
    properties: &BTreeMap<String, PropertyView>,
    now: i64,
) -> Option<PropertyView> {
    let value = eval_kind(spec.kind, &spec.inputs, properties, now)?;
    Some(PropertyView {
        value,
        source: PropertySource::Derived,
        as_of: Some(now.to_string()),
        provenance: Some(format!("function:{}", spec.name)),
    })
}

fn eval_kind(
    kind: FunctionKind,
    inputs: &[String],
    properties: &BTreeMap<String, PropertyView>,
    now: i64,
) -> Option<Value> {
    match kind {
        FunctionKind::DaysSinceTimestamp => {
            let name = inputs.first()?;
            let cal = properties.get(name)?;
            let as_of = cal
                .value
                .as_i64()
                .or_else(|| cal.value.as_str().and_then(|s| s.parse::<i64>().ok()))?;
            Some(json!(((now - as_of).max(0)) / 86_400))
        }
        FunctionKind::DoubleInteger => {
            let name = inputs.first()?;
            let src = properties.get(name)?;
            let n = integer_input(&src.value)?;
            Some(json!(n.saturating_mul(2)))
        }
    }
}

fn integer_input(value: &Value) -> Option<i64> {
    if let Some(n) = value.as_i64() {
        return Some(n);
    }
    if let Some(s) = value.as_str() {
        return s.parse().ok();
    }
    None
}

/// Digest of invoked function pins (sorted, unique). Empty reads pin `"none"`.
pub fn digest(pins: &[String]) -> String {
    let mut sorted = pins.to_vec();
    sorted.sort();
    sorted.dedup();
    if sorted.is_empty() {
        "none".into()
    } else {
        sorted.join("+")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapped(v: Value) -> PropertyView {
        PropertyView {
            value: v,
            source: PropertySource::Mapped,
            as_of: Some("1".into()),
            provenance: None,
        }
    }

    #[test]
    fn days_since_timestamp_uses_clock_not_property_name() {
        let spec = FunctionSpec {
            name: "age_days".into(),
            inputs: vec!["when".into()],
            kind: FunctionKind::DaysSinceTimestamp,
        };
        let mut props = BTreeMap::new();
        props.insert("when".into(), mapped(json!(100)));
        let view = apply(&spec, &props, 100 + 5 * 86_400).expect("apply");
        assert_eq!(view.value, json!(5));
        assert_eq!(view.source, PropertySource::Derived);
        assert_eq!(view.provenance.as_deref(), Some("function:age_days"));
    }

    #[test]
    fn double_integer_doubles_named_input() {
        let spec = FunctionSpec {
            name: "double_of".into(),
            inputs: vec!["n".into()],
            kind: FunctionKind::DoubleInteger,
        };
        let mut props = BTreeMap::new();
        props.insert("n".into(), mapped(json!(21)));
        let view = apply(&spec, &props, 0).expect("apply");
        assert_eq!(view.value, json!(42));
    }

    #[test]
    fn missing_input_yields_none() {
        let spec = FunctionSpec {
            name: "double_of".into(),
            inputs: vec!["n".into()],
            kind: FunctionKind::DoubleInteger,
        };
        assert!(apply(&spec, &BTreeMap::new(), 0).is_none());
    }

    #[test]
    fn digest_sorts_and_dedups_pins() {
        assert_eq!(digest(&[]), "none");
        assert_eq!(
            digest(&["b:1".into(), "a:1".into(), "b:1".into()]),
            "a:1+b:1"
        );
    }

    #[test]
    fn pin_changes_when_kind_or_inputs_change() {
        let a = FunctionSpec {
            name: "f".into(),
            inputs: vec!["x".into()],
            kind: FunctionKind::DoubleInteger,
        };
        let b = FunctionSpec {
            name: "f".into(),
            inputs: vec!["y".into()],
            kind: FunctionKind::DoubleInteger,
        };
        assert_ne!(a.pin(), b.pin());
        assert!(a.pin().starts_with("f:"));
    }
}
