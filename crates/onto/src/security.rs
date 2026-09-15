//! Four-level deny-by-default authorization (Zhang 2026, Ch. 10).
//!
//! Instance reads and leftover public writes check **platform → type →
//! instance → property**. A missing grant is [`AuthzDecision::Deny`], never an
//! implicit Allow. Property-level Deny hides that property on read.
//!
//! # Context
//! Kernel type name `Policy` existed without a check. This module is the check.
//! Grants are OMS records ([`PolicySpec`]) stored on a working branch and
//! merged to main like other schema.
//!
//! # Inputs
//! [`authorize`] takes the live grant list, a [`Session`], an [`AuthzOp`], and
//! optional type / instance / property. [`filter_view`] runs the same decision
//! per property.
//!
//! # Outputs
//! Exhaustive [`AuthzDecision`]. Callers must `match`; there is no `unwrap_or(Allow)`.
//!
//! # Side effects
//! None. Pure over `(grants, session, op, coordinates)`.
//!
//! # Relations
//! [`crate::Engine::create_policy`] stores the spec. Engine read paths call
//! [`filter_view`]. Funnel calls [`authorize`] for writes. Action execution is
//! the other legal instance write (Zhang 2026, Ch. 9).

use crate::types::{Actor, KeyKind, ObjectView, Session};
use serde::{Deserialize, Serialize};

/// OMS table for policy grants. Copied and merged like other schema.
pub const POLICIES_TABLE: &str = "schema_policies";

/// Ordered check levels (Zhang 2026, Ch. 10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthzLevel {
    Platform,
    Type,
    Instance,
    Property,
}

impl AuthzLevel {
    pub const ORDER: [AuthzLevel; 4] = [Self::Platform, Self::Type, Self::Instance, Self::Property];
}

/// Read hides; write is Funnel (and any leftover public store write).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthzOp {
    Read,
    Write,
}

/// Exhaustive verdict. Missing grant → [`AuthzDecision::Deny`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthzDecision {
    Allow,
    Deny,
}

impl AuthzDecision {
    #[must_use]
    pub fn is_allow(self) -> bool {
        match self {
            Self::Allow => true,
            Self::Deny => false,
        }
    }
}

/// OMS policy grant. Wildcards are explicit `"*"`, not a missing field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicySpec {
    pub name: String,
    pub level: AuthzLevel,
    pub op: AuthzOp,
    pub key: Option<KeyKind>,
    pub type_name: Option<String>,
    pub instance_id: Option<String>,
    pub property: Option<String>,
    #[serde(default)]
    pub roles: Vec<String>,
    #[serde(default)]
    pub min_tier: u8,
    pub decision: AuthzDecision,
}

impl PolicySpec {
    pub fn validate(&self) -> crate::error::Result<()> {
        if self.name.trim().is_empty() {
            return Err(crate::error::OntoError::Invalid(
                "policy name is required".into(),
            ));
        }
        Ok(())
    }
}

/// A missing grant is Deny, never an implicit Allow (Zhang 2026, Ch. 10).
pub fn missing_grant() -> AuthzDecision {
    AuthzDecision::Deny
}

/// Run platform, then type, then instance, then property. First Deny wins.
///
/// Property level is skipped when `property` is `None` (object-level write).
pub fn authorize(
    grants: &[PolicySpec],
    session: &Session,
    op: AuthzOp,
    type_name: Option<&str>,
    instance_id: Option<&str>,
    property: Option<&str>,
) -> AuthzDecision {
    for level in AuthzLevel::ORDER {
        if matches!(level, AuthzLevel::Property) && property.is_none() {
            continue;
        }
        match decide_level(grants, session, level, op, type_name, instance_id, property) {
            AuthzDecision::Allow => continue,
            AuthzDecision::Deny => return AuthzDecision::Deny,
        }
    }
    AuthzDecision::Allow
}

fn decide_level(
    grants: &[PolicySpec],
    session: &Session,
    level: AuthzLevel,
    op: AuthzOp,
    type_name: Option<&str>,
    instance_id: Option<&str>,
    property: Option<&str>,
) -> AuthzDecision {
    let matching: Vec<AuthzDecision> = grants
        .iter()
        .filter(|g| grant_matches(g, session, level, op, type_name, instance_id, property))
        .map(|g| g.decision)
        .collect();
    if matching.is_empty() {
        return missing_grant();
    }
    if matching.iter().any(|d| matches!(d, AuthzDecision::Deny)) {
        return AuthzDecision::Deny;
    }
    AuthzDecision::Allow
}

fn grant_matches(
    grant: &PolicySpec,
    session: &Session,
    level: AuthzLevel,
    op: AuthzOp,
    type_name: Option<&str>,
    instance_id: Option<&str>,
    property: Option<&str>,
) -> bool {
    if grant.level != level || grant.op != op {
        return false;
    }
    if !key_matches(grant.key, session.actor.key) {
        return false;
    }
    if session.actor.tier.rank() < grant.min_tier {
        return false;
    }
    if !roles_match(&grant.roles, &session.actor) {
        return false;
    }
    match level {
        AuthzLevel::Platform => true,
        AuthzLevel::Type => wildcard_match(grant.type_name.as_deref(), type_name),
        AuthzLevel::Instance => {
            wildcard_match(grant.type_name.as_deref(), type_name)
                && wildcard_match(grant.instance_id.as_deref(), instance_id)
        }
        AuthzLevel::Property => {
            wildcard_match(grant.type_name.as_deref(), type_name)
                && wildcard_match(grant.instance_id.as_deref(), instance_id)
                && wildcard_match(grant.property.as_deref(), property)
        }
    }
}

fn key_matches(grant_key: Option<KeyKind>, actor_key: KeyKind) -> bool {
    match grant_key {
        None => true,
        Some(want) => want == actor_key,
    }
}

fn roles_match(roles: &[String], actor: &Actor) -> bool {
    if roles.is_empty() {
        return true;
    }
    roles.iter().any(|r| actor.has_role(r))
}

fn wildcard_match(grant_val: Option<&str>, actual: Option<&str>) -> bool {
    match grant_val {
        None | Some("*") => true,
        Some(want) => actual == Some(want),
    }
}

/// Drop properties (and missing/stale names) the actor is not allowed to see.
pub fn filter_view(grants: &[PolicySpec], session: &Session, mut view: ObjectView) -> ObjectView {
    let type_name = view.type_name.clone();
    let id = view.id.clone();
    view.properties.retain(|name, _| {
        authorize(
            grants,
            session,
            AuthzOp::Read,
            Some(&type_name),
            Some(&id),
            Some(name),
        )
        .is_allow()
    });
    view.missing.retain(|name| {
        authorize(
            grants,
            session,
            AuthzOp::Read,
            Some(&type_name),
            Some(&id),
            Some(name),
        )
        .is_allow()
    });
    view.stale.retain(|name| {
        authorize(
            grants,
            session,
            AuthzOp::Read,
            Some(&type_name),
            Some(&id),
            Some(name),
        )
        .is_allow()
    });
    view
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiers::AgentTier;
    use crate::types::{Actor, PropertySource, PropertyView};
    use serde_json::json;
    use std::collections::BTreeMap;

    fn intern() -> Session {
        Session::new(Actor::consumer("ops.intern", &[], AgentTier::T1), "test")
    }

    fn restricted() -> Session {
        Session::new(
            Actor::consumer("ops.restricted", &["restricted"], AgentTier::T2),
            "test",
        )
    }

    fn operator() -> Session {
        Session::new(
            Actor::consumer("ops.maya", &["operator"], AgentTier::T2),
            "test",
        )
    }

    #[allow(clippy::too_many_arguments)] // policy grant test fixture
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

    #[allow(clippy::too_many_lines)] // four-level grant fixture
    fn seeded() -> Vec<PolicySpec> {
        vec![
            grant(
                "platform_read",
                AuthzLevel::Platform,
                AuthzOp::Read,
                Some(KeyKind::Consumer),
                None,
                None,
                None,
                &[],
                0,
                AuthzDecision::Allow,
            ),
            grant(
                "platform_write",
                AuthzLevel::Platform,
                AuthzOp::Write,
                Some(KeyKind::Consumer),
                None,
                None,
                None,
                &[],
                0,
                AuthzDecision::Allow,
            ),
            grant(
                "type_read",
                AuthzLevel::Type,
                AuthzOp::Read,
                Some(KeyKind::Consumer),
                Some("*"),
                None,
                None,
                &["operator", "supervisor", "restricted"],
                1,
                AuthzDecision::Allow,
            ),
            grant(
                "type_write",
                AuthzLevel::Type,
                AuthzOp::Write,
                Some(KeyKind::Consumer),
                Some("*"),
                None,
                None,
                &["operator", "supervisor"],
                2,
                AuthzDecision::Allow,
            ),
            grant(
                "instance_read",
                AuthzLevel::Instance,
                AuthzOp::Read,
                Some(KeyKind::Consumer),
                Some("*"),
                Some("*"),
                None,
                &["operator", "supervisor", "restricted"],
                1,
                AuthzDecision::Allow,
            ),
            grant(
                "instance_write",
                AuthzLevel::Instance,
                AuthzOp::Write,
                Some(KeyKind::Consumer),
                Some("*"),
                Some("*"),
                None,
                &["operator", "supervisor"],
                2,
                AuthzDecision::Allow,
            ),
            grant(
                "property_read",
                AuthzLevel::Property,
                AuthzOp::Read,
                Some(KeyKind::Consumer),
                Some("*"),
                Some("*"),
                Some("*"),
                &["operator", "supervisor", "restricted"],
                1,
                AuthzDecision::Allow,
            ),
            grant(
                "rationale_deny",
                AuthzLevel::Property,
                AuthzOp::Read,
                Some(KeyKind::Consumer),
                Some("*"),
                Some("*"),
                Some("rationale"),
                &["restricted"],
                1,
                AuthzDecision::Deny,
            ),
            grant(
                "do_max_deny",
                AuthzLevel::Property,
                AuthzOp::Read,
                Some(KeyKind::Consumer),
                Some("*"),
                Some("*"),
                Some("do_max"),
                &["restricted"],
                1,
                AuthzDecision::Deny,
            ),
        ]
    }

    #[test]
    fn does_deny_if_grant_is_missing() {
        let d = authorize(
            &[],
            &intern(),
            AuthzOp::Write,
            Some("AerationTank"),
            Some("tank-1"),
            None,
        );
        assert_eq!(d, AuthzDecision::Deny);
        assert_eq!(missing_grant(), AuthzDecision::Deny);
    }

    #[test]
    fn does_deny_write_if_intern_lacks_type_grant() {
        let d = authorize(
            &seeded(),
            &intern(),
            AuthzOp::Write,
            Some("AerationTank"),
            Some("tank-9"),
            None,
        );
        assert_eq!(d, AuthzDecision::Deny);
    }

    #[test]
    fn does_allow_write_if_operator_has_type_grant() {
        let d = authorize(
            &seeded(),
            &operator(),
            AuthzOp::Write,
            Some("AerationTank"),
            Some("tank-1"),
            None,
        );
        assert_eq!(d, AuthzDecision::Allow);
    }

    #[test]
    fn does_hide_denied_properties_if_restricted() {
        let grants = seeded();
        let mut properties = BTreeMap::new();
        for (k, v) in [
            ("name", json!("2026-A")),
            ("do_max", json!(4.0)),
            ("rationale", json!("secret")),
        ] {
            properties.insert(
                k.into(),
                PropertyView {
                    value: v,
                    source: PropertySource::Mapped,
                    as_of: None,
                    provenance: None,
                },
            );
        }
        let raw = ObjectView {
            id: "permit-2026".into(),
            type_name: "PermitVersion".into(),
            title: None,
            properties,
            missing: vec![],
            stale: vec![],
        };
        let hidden = filter_view(&grants, &restricted(), raw.clone());
        assert!(!hidden.properties.contains_key("rationale"));
        assert!(!hidden.properties.contains_key("do_max"));
        assert!(hidden.properties.contains_key("name"));
        let open = filter_view(&grants, &operator(), raw);
        assert!(open.properties.contains_key("rationale"));
        assert!(open.properties.contains_key("do_max"));
    }

    #[test]
    fn does_order_levels_platform_type_instance_property() {
        assert_eq!(
            AuthzLevel::ORDER,
            [
                AuthzLevel::Platform,
                AuthzLevel::Type,
                AuthzLevel::Instance,
                AuthzLevel::Property,
            ]
        );
    }
}
