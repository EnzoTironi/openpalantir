//! Permission-first object sets (Zhang 2026, Ch. 6).
//!
//! A set is a typed spec (type + filter + optional stored name), not a bag of
//! instance ids. Search, filter, and aggregate evaluate the spec after the
//! permission strip. Named specs are OMS records: branch-local until merge.

use crate::error::{OntoError, Result};
use crate::types::{ObjectView, Query, Session};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// OMS table for named object-set specs. Copied and merged like other schema.
pub const OBJECT_SETS_TABLE: &str = "schema_object_sets";

/// Equals-filter over permission-visible properties.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ObjectSetFilter {
    #[serde(default)]
    pub equals: BTreeMap<String, Value>,
}

/// Typed object set: type + filter + optional stored name.
///
/// Not a `Vec<String>` of ids. Members are whatever currently matches the
/// spec; a newly created matching object appears, a non-match does not.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObjectSet {
    pub type_name: Option<String>,
    pub filter: ObjectSetFilter,
    pub name: Option<String>,
    pub limit: usize,
}

/// Named set stored on an OMS branch. Invisible on main until merge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObjectSetSpec {
    pub name: String,
    pub type_name: Option<String>,
    #[serde(default)]
    pub equals: BTreeMap<String, Value>,
}

impl ObjectSet {
    #[must_use]
    pub fn inline(type_name: Option<String>, filter: ObjectSetFilter, limit: usize) -> Self {
        Self {
            type_name,
            filter,
            name: None,
            limit,
        }
    }

    /// Inline `Query` is one way to build a set. A non-empty `set_name` is a
    /// named-set reference; the engine resolves the OMS spec before evaluate.
    #[must_use]
    pub fn from_query(query: &Query) -> Self {
        if let Some(name) = query
            .set_name
            .as_ref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
        {
            return Self {
                type_name: None,
                filter: ObjectSetFilter::default(),
                name: Some(name.to_string()),
                limit: query.limit,
            };
        }
        Self::inline(
            query.type_name.clone(),
            ObjectSetFilter {
                equals: query.equals.clone(),
            },
            query.limit,
        )
    }

    #[must_use]
    pub fn from_spec(spec: &ObjectSetSpec, limit: usize) -> Self {
        Self {
            type_name: spec.type_name.clone(),
            filter: ObjectSetFilter {
                equals: spec.equals.clone(),
            },
            name: Some(spec.name.clone()),
            limit,
        }
    }

    #[must_use]
    pub fn unbounded(&self) -> Self {
        let mut next = self.clone();
        next.limit = usize::MAX;
        next
    }

    /// Match against a permission-filtered view (Zhang 2026, Ch. 6).
    #[must_use]
    pub fn matches(&self, view: &ObjectView) -> bool {
        if let Some(want) = &self.type_name {
            if view.type_name != *want {
                return false;
            }
        }
        self.filter
            .equals
            .iter()
            .all(|(k, v)| view.properties.get(k).is_some_and(|p| p.value == *v))
    }
}

impl ObjectSetSpec {
    pub fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            return Err(OntoError::Invalid("object set name is required".into()));
        }
        Ok(())
    }
}

/// Strip properties the actor is not allowed to see. Runs before the caller
/// receives the view, and before equals-match on that view.
pub fn apply_permission(session: &Session, mut view: ObjectView) -> ObjectView {
    if session.actor.has_role("restricted") {
        view.properties.retain(|name, _| name != "rationale");
    }
    view
}

/// Evaluate a resolved set against candidate ids. `load_permitted` must already
/// apply [`apply_permission`]. Missing ids are skipped. No members → `Ok([])`.
pub fn evaluate_members<F>(
    set: &ObjectSet,
    ids: impl IntoIterator<Item = String>,
    mut load_permitted: F,
) -> Result<Vec<ObjectView>>
where
    F: FnMut(&str) -> Result<ObjectView>,
{
    let mut out = Vec::new();
    for id in ids {
        let view = match load_permitted(&id) {
            Ok(view) => view,
            Err(OntoError::NotFound(_)) => continue,
            Err(e) => return Err(e),
        };
        if !set.matches(&view) {
            continue;
        }
        out.push(view);
        if out.len() >= set.limit {
            break;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiers::AgentTier;
    use crate::types::{Actor, PropertySource, PropertyView};
    use serde_json::json;

    fn view(id: &str, type_name: &str, pairs: &[(&str, Value)]) -> ObjectView {
        let mut properties = BTreeMap::new();
        for (k, v) in pairs {
            properties.insert(
                (*k).to_string(),
                PropertyView {
                    value: v.clone(),
                    source: PropertySource::Mapped,
                    as_of: None,
                    provenance: None,
                },
            );
        }
        ObjectView {
            id: id.into(),
            type_name: type_name.into(),
            title: None,
            properties,
            missing: vec![],
            stale: vec![],
        }
    }

    #[test]
    fn set_is_spec_not_id_list() {
        let set = ObjectSet::inline(
            Some("AerationTank".into()),
            ObjectSetFilter {
                equals: [("name".into(), json!("Basin 1"))].into_iter().collect(),
            },
            50,
        );
        assert!(set.name.is_none());
        assert_eq!(set.type_name.as_deref(), Some("AerationTank"));
        assert_eq!(set.filter.equals.get("name"), Some(&json!("Basin 1")));
    }

    #[test]
    fn matches_type_and_equals() {
        let set = ObjectSet::inline(
            Some("AerationTank".into()),
            ObjectSetFilter {
                equals: [("name".into(), json!("Basin 1"))].into_iter().collect(),
            },
            50,
        );
        let hit = view("tank-1", "AerationTank", &[("name", json!("Basin 1"))]);
        let miss_name = view("tank-2", "AerationTank", &[("name", json!("Basin 2"))]);
        let miss_type = view("blower-1", "Blower", &[("name", json!("Basin 1"))]);
        assert!(set.matches(&hit));
        assert!(!set.matches(&miss_name));
        assert!(!set.matches(&miss_type));
    }

    #[test]
    fn evaluate_empty_is_ok_empty() {
        let set = ObjectSet::inline(Some("Ghost".into()), ObjectSetFilter::default(), 50);
        let out = evaluate_members(&set, Vec::<String>::new(), |_| {
            Err(OntoError::NotFound("x".into()))
        })
        .unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn query_set_name_builds_named_ref() {
        let q = Query {
            set_name: Some("aeration_tanks".into()),
            ..Query::default()
        };
        let set = ObjectSet::from_query(&q);
        assert_eq!(set.name.as_deref(), Some("aeration_tanks"));
        assert!(set.type_name.is_none());
    }

    #[test]
    fn restricted_permission_strips_rationale() {
        let session = Session::new(
            Actor::consumer("ops.restricted", &["restricted"], AgentTier::T2),
            "test",
        );
        let raw = view(
            "rec-1",
            "LabMeasurement",
            &[("value", json!(1.0)), ("rationale", json!("secret"))],
        );
        let stripped = apply_permission(&session, raw);
        assert!(!stripped.properties.contains_key("rationale"));
        assert!(stripped.properties.contains_key("value"));
    }
}
