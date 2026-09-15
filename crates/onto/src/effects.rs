//! Typed Action effect plan. Parsed at publication; applied at commit.
//!
//! Unknown verbs fail closed. `$parameter` names must exist on the Action.
//! Link cardinality is checked against live and staged edges (Zhang 2026).

use crate::error::{OntoError, Result};
use crate::types::ParamSpec;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// One published write-set verb.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectOp {
    Create {
        type_name: String,
        properties: BTreeMap<String, Value>,
    },
    Update {
        target: String,
        properties: BTreeMap<String, Value>,
    },
    Link {
        link: Value,
        from: String,
        to: Option<String>,
    },
    CloseLink {
        link: Value,
        from: String,
        to: String,
    },
    /// Write `property` on `update` as the count of `link` edges on `via` targets.
    CountLinks {
        link: String,
        update: String,
        property: String,
        via: String,
    },
}

/// Declared relationship multiplicity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cardinality {
    OneToOne,
    OneToMany,
    ManyToOne,
    ManyToMany,
}

impl Cardinality {
    #[must_use]
    pub fn parse(raw: &str) -> Self {
        match raw {
            "1:1" => Self::OneToOne,
            "1:n" | "1:*" => Self::OneToMany,
            "n:1" | "*:1" => Self::ManyToOne,
            _ => Self::ManyToMany,
        }
    }

    /// Unique outgoing (from) and unique incoming (to).
    #[must_use]
    pub fn unique_ends(self) -> (bool, bool) {
        match self {
            Self::OneToOne => (true, true),
            Self::OneToMany => (false, true),
            Self::ManyToOne => (true, false),
            Self::ManyToMany => (false, false),
        }
    }
}

/// Parse effects at publication. Non-array is invalid (empty array is a plan).
pub fn parse_plan(effects: &Value, parameters: &[ParamSpec]) -> Result<Vec<EffectOp>> {
    let Some(arr) = effects.as_array() else {
        return Err(OntoError::Invalid(
            "action effects must be an array of typed operations".into(),
        ));
    };
    let names: BTreeSet<&str> = parameters.iter().map(|p| p.name.as_str()).collect();
    let mut plan = Vec::new();
    for effect in arr {
        plan.push(parse_one(effect, &names)?);
    }
    Ok(plan)
}

fn parse_one(effect: &Value, names: &BTreeSet<&str>) -> Result<EffectOp> {
    let obj = effect
        .as_object()
        .ok_or_else(|| OntoError::Invalid("effect must be object".into()))?;
    let verbs: Vec<&str> = [
        obj.contains_key("create").then_some("create"),
        (obj.contains_key("update") && !obj.contains_key("count_links")).then_some("update"),
        obj.contains_key("link").then_some("link"),
        obj.contains_key("close_link").then_some("close_link"),
        obj.contains_key("count_links").then_some("count_links"),
    ]
    .into_iter()
    .flatten()
    .collect();
    let Some(verb) = verbs.first().copied() else {
        return Err(OntoError::Invalid(
            "effect must have exactly one of create, update, link, close_link, count_links".into(),
        ));
    };
    if verbs.len() != 1 {
        return Err(OntoError::Invalid(
            "effect must have exactly one of create, update, link, close_link, count_links".into(),
        ));
    }
    for key in obj.keys() {
        if !allowed_field(verb, key) {
            return Err(OntoError::Invalid(format!("unknown effect field {key}")));
        }
    }
    match verb {
        "create" => parse_create(obj, names),
        "update" => parse_update(obj, names),
        "link" => parse_link(obj, names),
        "close_link" => parse_close_link(obj, names),
        "count_links" => parse_count_links(obj, names),
        other => Err(OntoError::Invalid(format!("unknown effect verb {other}"))),
    }
}

fn parse_properties(
    obj: &serde_json::Map<String, Value>,
    names: &BTreeSet<&str>,
) -> Result<BTreeMap<String, Value>> {
    let mut properties = BTreeMap::new();
    if let Some(fields) = obj.get("properties").and_then(Value::as_object) {
        for (k, v) in fields {
            check_ref(v, names)?;
            properties.insert(k.clone(), v.clone());
        }
    }
    Ok(properties)
}

fn parse_create(obj: &serde_json::Map<String, Value>, names: &BTreeSet<&str>) -> Result<EffectOp> {
    let type_name = obj
        .get("create")
        .and_then(Value::as_str)
        .ok_or_else(|| OntoError::Invalid("create needs a type name".into()))?;
    Ok(EffectOp::Create {
        type_name: type_name.into(),
        properties: parse_properties(obj, names)?,
    })
}

fn parse_update(obj: &serde_json::Map<String, Value>, names: &BTreeSet<&str>) -> Result<EffectOp> {
    let target = obj
        .get("update")
        .and_then(Value::as_str)
        .ok_or_else(|| OntoError::Invalid("update needs a target param".into()))?;
    require_param(target, names)?;
    Ok(EffectOp::Update {
        target: target.into(),
        properties: parse_properties(obj, names)?,
    })
}

fn parse_link(obj: &serde_json::Map<String, Value>, names: &BTreeSet<&str>) -> Result<EffectOp> {
    let link = obj.get("link").cloned().unwrap_or(Value::Null);
    check_ref(&link, names)?;
    let from = obj
        .get("from")
        .and_then(Value::as_str)
        .unwrap_or("from")
        .to_string();
    require_param(&from, names)?;
    let to = obj.get("to").and_then(Value::as_str).map(str::to_string);
    if let Some(t) = &to {
        require_param(t, names)?;
    }
    Ok(EffectOp::Link { link, from, to })
}

fn parse_close_link(
    obj: &serde_json::Map<String, Value>,
    names: &BTreeSet<&str>,
) -> Result<EffectOp> {
    let link = obj.get("close_link").cloned().unwrap_or(Value::Null);
    check_ref(&link, names)?;
    let from = obj
        .get("from")
        .and_then(Value::as_str)
        .unwrap_or("from")
        .to_string();
    require_param(&from, names)?;
    let to = obj
        .get("to")
        .and_then(Value::as_str)
        .ok_or_else(|| OntoError::Invalid("close_link needs both endpoints".into()))?;
    require_param(to, names)?;
    Ok(EffectOp::CloseLink {
        link,
        from,
        to: to.into(),
    })
}

fn parse_count_links(
    obj: &serde_json::Map<String, Value>,
    names: &BTreeSet<&str>,
) -> Result<EffectOp> {
    let link = obj
        .get("count_links")
        .and_then(Value::as_str)
        .ok_or_else(|| OntoError::Invalid("count_links needs a link type".into()))?;
    let update = obj
        .get("update")
        .and_then(Value::as_str)
        .ok_or_else(|| OntoError::Invalid("count_links needs update".into()))?;
    let property = obj
        .get("property")
        .and_then(Value::as_str)
        .ok_or_else(|| OntoError::Invalid("count_links needs property".into()))?;
    let via = obj
        .get("via")
        .and_then(Value::as_str)
        .ok_or_else(|| OntoError::Invalid("count_links needs via".into()))?;
    require_param(update, names)?;
    Ok(EffectOp::CountLinks {
        link: link.into(),
        update: update.into(),
        property: property.into(),
        via: via.into(),
    })
}

fn allowed_field(verb: &str, key: &str) -> bool {
    match verb {
        "create" => matches!(key, "create" | "properties"),
        "update" => matches!(key, "update" | "properties"),
        "link" => matches!(key, "link" | "from" | "to"),
        "close_link" => matches!(key, "close_link" | "from" | "to"),
        "count_links" => matches!(key, "count_links" | "update" | "property" | "via"),
        _ => false,
    }
}

fn check_ref(v: &Value, names: &BTreeSet<&str>) -> Result<()> {
    if let Some(s) = v.as_str() {
        if let Some(name) = s.strip_prefix('$') {
            return require_param(name, names);
        }
    }
    Ok(())
}

fn require_param(name: &str, names: &BTreeSet<&str>) -> Result<()> {
    if names.contains(name) {
        Ok(())
    } else {
        Err(OntoError::Invalid(format!(
            "effect references unknown parameter {name}"
        )))
    }
}

/// Whether adding `from`→`to` would break the declared multiplicity.
#[must_use]
pub fn breaks_cardinality(card: Cardinality, from_used: bool, to_used: bool) -> bool {
    let (unique_from, unique_to) = card.unique_ends();
    (unique_from && from_used) || (unique_to && to_used)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ParamSpec;
    use serde_json::json;

    fn params(names: &[&str]) -> Vec<ParamSpec> {
        names
            .iter()
            .map(|n| ParamSpec {
                name: (*n).into(),
                value_type: "Text".into(),
                object_type: None,
                required: true,
            })
            .collect()
    }

    #[test]
    fn does_reject_unknown_effect_verb_at_parse() {
        let err = parse_plan(&json!([{ "explode": true }]), &params(&["x"])).unwrap_err();
        assert!(matches!(err, OntoError::Invalid(_)));
    }

    #[test]
    fn does_reject_unknown_parameter_ref() {
        let err = parse_plan(
            &json!([{ "update": "tank", "properties": { "n": "$ghost" } }]),
            &params(&["tank"]),
        )
        .unwrap_err();
        assert!(matches!(err, OntoError::Invalid(_)));
    }

    #[test]
    fn does_parse_count_links_with_update_field() {
        let plan = parse_plan(
            &json!([{
                "count_links": "occupies",
                "update": "section",
                "property": "enrolled",
                "via": "section_has_seat"
            }]),
            &params(&["section"]),
        )
        .unwrap();
        assert_eq!(
            plan,
            vec![EffectOp::CountLinks {
                link: "occupies".into(),
                update: "section".into(),
                property: "enrolled".into(),
                via: "section_has_seat".into(),
            }]
        );
    }

    #[test]
    fn does_treat_one_to_one_as_unique_both_ends() {
        assert!(breaks_cardinality(Cardinality::OneToOne, true, false));
        assert!(breaks_cardinality(Cardinality::OneToOne, false, true));
        assert!(!breaks_cardinality(Cardinality::OneToOne, false, false));
        assert!(!breaks_cardinality(Cardinality::OneToMany, true, false));
    }
}
