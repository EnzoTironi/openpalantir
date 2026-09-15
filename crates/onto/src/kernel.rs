//! Kernel language objects as live OMS records (Zhang 2026, Ch. 4–5).
//!
//! `KERNEL_TYPES` is not a closed string list. After [`Engine::memory`] each
//! name is an `ObjectType` instance on main, queryable with `search_objects`.
//! Interfaces are contracts: [`KernelInterface::Reviewable`] on an `ActionType`
//! in Propose mode must mint an inbox object;
//! [`KernelInterface::Evidenced`] on an `ObjectType` makes missing or stale
//! required properties a Complete/Current fail at submit.
//!
//! # Context
//! Typology is `Entity | Event | State | DecisionRecord`. Reviewable and
//! Evidenced used to be names created in wastewater bootstrap with no
//! enforcement.
//!
//! # Inputs
//! [`install`] takes the OMS connection and clock. Contract helpers take the
//! live `ActionType` / `ObjectType` / Interface specs plus the object view.
//!
//! # Outputs
//! Kernel `ObjectType` records, interface specs, and read grants for those
//! records. [`require_inbox`] and [`evidenced_guard`] are the checks the
//! write path must call — mode alone is not the Reviewable contract.
//!
//! # Side effects
//! [`install`] writes schema, instances, and policies on main. It is the
//! kernel boot path; builders still cannot mutate main directly.
//!
//! # Relations
//! [`crate::store::SqliteStore::init`] calls [`install`]. [`crate::Engine::attach_interface`]
//! records the attach on a branch. Unmerged attach is invisible to these
//! helpers because submit loads specs from main.

use crate::bitemporal;
use crate::error::Result;
use crate::security::{AuthzDecision, AuthzLevel, AuthzOp, PolicySpec, POLICIES_TABLE};
use crate::types::{
    ActionTypeSpec, ExecutionMode, GuardResult, InterfaceSpec, ObjectTypeSpec, ObjectView,
    PropertySource, PropertySpec, PropertyView, Typology, Verdict, KERNEL_TYPES, MAIN_BRANCH,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// Named kernel contracts. A new variant is a compile break until the
/// write path handles it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::module_name_repetitions)] // public OMS name; kernel::Interface collides with InterfaceSpec
pub enum KernelInterface {
    Reviewable,
    Evidenced,
}

impl KernelInterface {
    pub const ALL: [KernelInterface; 2] = [Self::Reviewable, Self::Evidenced];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reviewable => "Reviewable",
            Self::Evidenced => "Evidenced",
        }
    }

    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "Reviewable" => Some(Self::Reviewable),
            "Evidenced" => Some(Self::Evidenced),
            _ => None,
        }
    }

    /// Required properties on the interface record (Zhang 2026, Ch. 4).
    #[must_use]
    pub fn required_properties(self) -> &'static [&'static str] {
        match self {
            Self::Reviewable => &["status"],
            Self::Evidenced => &["rationale"],
        }
    }

    #[must_use]
    pub fn spec(self) -> InterfaceSpec {
        InterfaceSpec {
            name: self.as_str().into(),
            required_properties: self
                .required_properties()
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
        }
    }
}

/// Whether `names` lists this kernel interface.
#[must_use]
pub fn attached(names: &[String], iface: KernelInterface) -> bool {
    names.iter().any(|n| n == iface.as_str())
}

/// Propose + Reviewable must create an inbox object. Propose without the
/// interface must not. Mode alone is not the contract.
#[must_use]
pub fn require_inbox(spec: &ActionTypeSpec) -> bool {
    match spec.mode {
        ExecutionMode::Propose => attached(&spec.interfaces, KernelInterface::Reviewable),
        ExecutionMode::Auto | ExecutionMode::Approve | ExecutionMode::Shadow => false,
    }
}

/// Gap against an Evidenced type's required properties.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvidenceGap {
    Missing(String),
    Stale {
        property: String,
        age_secs: i64,
        budget_secs: i64,
    },
}

/// Missing or stale required evidence on a view. Empty when Evidenced is
/// not attached to the type (including unmerged attach).
#[must_use]
pub fn evidence_gaps(
    object_type: &ObjectTypeSpec,
    interface: &InterfaceSpec,
    view: &ObjectView,
    now: i64,
) -> Vec<EvidenceGap> {
    if !attached(&object_type.interfaces, KernelInterface::Evidenced) {
        return Vec::new();
    }
    if interface.name != KernelInterface::Evidenced.as_str() {
        return Vec::new();
    }
    let mut gaps = Vec::new();
    for prop in &interface.required_properties {
        match view.properties.get(prop) {
            None => gaps.push(EvidenceGap::Missing(prop.clone())),
            Some(pv) if pv.value.is_null() => gaps.push(EvidenceGap::Missing(prop.clone())),
            Some(pv) => {
                let Some(budget) = object_type.freshness_budget_secs else {
                    continue;
                };
                match pv.as_of.as_deref().map(str::parse::<i64>) {
                    None | Some(Err(_)) => gaps.push(EvidenceGap::Missing(prop.clone())),
                    Some(Ok(ts)) => {
                        let age = now - ts;
                        if age > budget {
                            gaps.push(EvidenceGap::Stale {
                                property: prop.clone(),
                                age_secs: age,
                                budget_secs: budget,
                            });
                        }
                    }
                }
            }
        }
    }
    gaps
}

/// First evidence gap as a Complete/Current Review guard.
#[must_use]
pub fn evidenced_guard(
    object_type: &ObjectTypeSpec,
    interface: &InterfaceSpec,
    view: &ObjectView,
    now: i64,
) -> Option<GuardResult> {
    let gap = evidence_gaps(object_type, interface, view, now)
        .into_iter()
        .next()?;
    match gap {
        EvidenceGap::Missing(property) => Some(GuardResult {
            name: "complete".into(),
            verdict: Verdict::Review,
            reason: format!("missing {property} (Complete fail)"),
        }),
        EvidenceGap::Stale {
            property,
            age_secs,
            budget_secs,
        } => Some(GuardResult {
            name: "current".into(),
            verdict: Verdict::Review,
            reason: format!("stale {property}: age {age_secs}s > {budget_secs}s (Current fail)"),
        }),
    }
}

/// Meta `ObjectType` used to store kernel language records.
#[must_use]
pub fn object_type_spec() -> ObjectTypeSpec {
    ObjectTypeSpec {
        name: "ObjectType".into(),
        typology: Typology::Entity,
        title_prop: Some("name".into()),
        interfaces: vec![],
        freshness_budget_secs: None,
        properties: vec![
            PropertySpec {
                name: "name".into(),
                value_type: "Text".into(),
                source: PropertySource::Mapped,
                nullable: false,
                function: None,
            },
            PropertySpec {
                name: "typology".into(),
                value_type: "Text".into(),
                source: PropertySource::Mapped,
                nullable: false,
                function: None,
            },
        ],
    }
}

fn kernel_typology(name: &str) -> Typology {
    match name {
        "OntologyProposal" => Typology::DecisionRecord,
        "OntologyBranch" => Typology::Event,
        _ => Typology::Entity,
    }
}

fn typology_key(t: Typology) -> &'static str {
    match t {
        Typology::Entity => "entity",
        Typology::Event => "event",
        Typology::State => "state",
        Typology::DecisionRecord => "decision_record",
    }
}

fn kernel_properties(name: &str, typology: Typology, now: i64) -> BTreeMap<String, PropertyView> {
    let stamp = now.to_string();
    let mut props = BTreeMap::new();
    props.insert(
        "name".into(),
        PropertyView {
            value: json!(name),
            source: PropertySource::Mapped,
            as_of: Some(stamp.clone()),
            provenance: Some("kernel".into()),
        },
    );
    props.insert(
        "typology".into(),
        PropertyView {
            value: json!(typology_key(typology)),
            source: PropertySource::Mapped,
            as_of: Some(stamp),
            provenance: Some("kernel".into()),
        },
    );
    props
}

fn kernel_read_policies() -> Vec<PolicySpec> {
    let object_type = Some("ObjectType".to_string());
    [
        (
            "kernel_object_type_platform_read",
            AuthzLevel::Platform,
            None,
            None,
        ),
        (
            "kernel_object_type_type_read",
            AuthzLevel::Type,
            object_type.clone(),
            None,
        ),
        (
            "kernel_object_type_instance_read",
            AuthzLevel::Instance,
            object_type.clone(),
            Some("*".into()),
        ),
        (
            "kernel_object_type_property_read",
            AuthzLevel::Property,
            object_type,
            Some("*".into()),
        ),
    ]
    .into_iter()
    .map(|(name, level, type_name, instance_id)| PolicySpec {
        name: name.into(),
        level,
        op: AuthzOp::Read,
        key: Some(crate::types::KeyKind::Consumer),
        type_name,
        instance_id,
        property: match level {
            AuthzLevel::Property => Some("*".into()),
            AuthzLevel::Platform | AuthzLevel::Type | AuthzLevel::Instance => None,
        },
        roles: vec![],
        min_tier: 1,
        decision: AuthzDecision::Allow,
    })
    .collect()
}

fn put_spec(db: &Connection, table: &str, branch: &str, name: &str, spec: &Value) -> Result<()> {
    db.execute(
        &format!("INSERT OR IGNORE INTO {table}(branch, name, spec) VALUES (?1, ?2, ?3)"),
        params![branch, name, spec.to_string()],
    )?;
    Ok(())
}

/// Install kernel `ObjectType` records, Reviewable/Evidenced contracts, and
/// consumer read grants onto main. Idempotent.
pub fn install(db: &Connection, now: i64) -> Result<()> {
    let meta = object_type_spec();
    put_spec(
        db,
        "schema_object_types",
        MAIN_BRANCH,
        &meta.name,
        &serde_json::to_value(&meta)?,
    )?;
    for iface in KernelInterface::ALL {
        let spec = iface.spec();
        put_spec(
            db,
            "schema_interfaces",
            MAIN_BRANCH,
            &spec.name,
            &serde_json::to_value(&spec)?,
        )?;
    }
    for name in KERNEL_TYPES {
        let typology = kernel_typology(name);
        let props = kernel_properties(name, typology, now);
        let encoded = serde_json::to_string(&props)?;
        let exists: Option<String> = db
            .query_row(
                "SELECT id FROM objects WHERE id = ?1",
                params![*name],
                |r| r.get(0),
            )
            .optional()?;
        if exists.is_none() {
            bitemporal::insert_object(db, name, "ObjectType", Some(name), &encoded, now)?;
        }
    }
    for spec in kernel_read_policies() {
        put_spec(
            db,
            POLICIES_TABLE,
            MAIN_BRANCH,
            &spec.name,
            &serde_json::to_value(&spec)?,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;
    use crate::tiers::AgentTier;
    use crate::types::{Actor, Query, Session};
    use serde_json::json;

    fn propose(interfaces: &[&str]) -> ActionTypeSpec {
        ActionTypeSpec {
            name: "propose_sample".into(),
            mode: ExecutionMode::Propose,
            parameters: vec![],
            guards: json!([]),
            required_roles: vec![],
            required_tier: AgentTier::T2,
            effects: json!([]),
            compensation: None,
            side_effects: json!({}),
            on_review: None,
            interfaces: interfaces.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    fn evidenced_type(attached_ifaces: &[&str], budget: Option<i64>) -> ObjectTypeSpec {
        ObjectTypeSpec {
            name: "LabNote".into(),
            typology: Typology::Entity,
            title_prop: Some("name".into()),
            interfaces: attached_ifaces.iter().map(|s| (*s).to_string()).collect(),
            freshness_budget_secs: budget,
            properties: vec![PropertySpec {
                name: "rationale".into(),
                value_type: "Text".into(),
                source: PropertySource::Mapped,
                nullable: true,
                function: None,
            }],
        }
    }

    fn view_with(pairs: &[(&str, Value, Option<&str>)]) -> ObjectView {
        let mut properties = BTreeMap::new();
        for (k, v, as_of) in pairs {
            properties.insert(
                (*k).to_string(),
                PropertyView {
                    value: v.clone(),
                    source: PropertySource::Mapped,
                    as_of: as_of.map(str::to_string),
                    provenance: None,
                },
            );
        }
        ObjectView {
            id: "note-1".into(),
            type_name: "LabNote".into(),
            title: None,
            properties,
            missing: vec![],
            stale: vec![],
        }
    }

    #[test]
    fn does_not_require_inbox_if_propose_lacks_reviewable() {
        assert!(!require_inbox(&propose(&[])));
    }

    #[test]
    fn does_require_inbox_if_propose_has_reviewable() {
        assert!(require_inbox(&propose(&["Reviewable"])));
    }

    #[test]
    fn does_not_require_inbox_if_mode_is_auto() {
        let mut spec = propose(&["Reviewable"]);
        spec.mode = ExecutionMode::Auto;
        assert!(!require_inbox(&spec));
    }

    #[test]
    fn does_fail_complete_if_evidenced_property_is_missing() {
        let spec = evidenced_type(&["Evidenced"], None);
        let iface = KernelInterface::Evidenced.spec();
        let view = view_with(&[]);
        let guard = evidenced_guard(&spec, &iface, &view, 1_700_000_000).unwrap();
        assert_eq!(guard.verdict, Verdict::Review);
        assert!(guard.reason.contains("Complete fail"));
    }

    #[test]
    fn does_fail_current_if_evidenced_property_is_stale() {
        let spec = evidenced_type(&["Evidenced"], Some(60));
        let iface = KernelInterface::Evidenced.spec();
        let view = view_with(&[("rationale", json!("ok"), Some("100"))]);
        let guard = evidenced_guard(&spec, &iface, &view, 1_000).unwrap();
        assert_eq!(guard.verdict, Verdict::Review);
        assert!(guard.reason.contains("Current fail"));
    }

    #[test]
    fn does_report_no_gaps_if_evidenced_is_unattached() {
        let spec = evidenced_type(&[], None);
        let iface = KernelInterface::Evidenced.spec();
        let view = view_with(&[]);
        assert!(evidence_gaps(&spec, &iface, &view, 0).is_empty());
    }

    #[test]
    fn does_expose_kernel_types_as_objecttype_records() {
        let engine = Engine::memory().unwrap();
        let intern = Session::new(Actor::consumer("ops.intern", &[], AgentTier::T1), "test");
        let found = engine
            .search_objects(
                &intern,
                Query {
                    type_name: Some("ObjectType".into()),
                    ..Query::default()
                },
            )
            .unwrap();
        let names: Vec<String> = found
            .iter()
            .map(|v| {
                v.properties
                    .get("name")
                    .and_then(|p| p.value.as_str())
                    .unwrap_or(&v.id)
                    .to_string()
            })
            .collect();
        for k in KERNEL_TYPES {
            assert!(
                names.iter().any(|n| n == k),
                "kernel type {k} must be a queryable ObjectType record, got {names:?}"
            );
        }
    }
}
