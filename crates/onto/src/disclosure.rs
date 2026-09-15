//! Record-level disclosure. Denied properties take their metadata with them.
//!
//! Guard reasons that name a hidden operand are replaced. Inbox rows are
//! visible to the proposer or a supervisor, not the whole store.

use crate::security::{authorize, AuthzDecision, AuthzOp, PolicySpec};
use crate::types::{DecisionRecordView, InboxItem, Session};
use serde_json::Value;

/// Drop snapshot values, timestamps, and provenance the actor cannot read.
pub fn redact_record(grants: &[PolicySpec], session: &Session, rec: &mut DecisionRecordView) {
    redact_params(grants, session, rec);
    redact_guards(grants, session, rec);
    redact_snapshot(grants, session, &mut rec.data_snapshot);
}

/// Inbox rows the actor proposed, or every row if they hold `supervisor`.
#[must_use]
pub fn visible_inbox(session: &Session, rows: Vec<InboxItem>) -> Vec<InboxItem> {
    if session.actor.has_role("supervisor") {
        return rows;
    }
    rows.into_iter()
        .filter(|r| r.proposed_by == session.actor.id)
        .collect()
}

fn redact_snapshot(grants: &[PolicySpec], session: &Session, snapshot: &mut Value) {
    let Some(objects) = snapshot.get_mut("objects").and_then(Value::as_array_mut) else {
        return;
    };
    objects.retain(|obj| {
        let id = obj.get("id").and_then(Value::as_str).unwrap_or_default();
        let type_name = obj
            .get("type_name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        authorize(
            grants,
            session,
            AuthzOp::Read,
            Some(type_name),
            Some(id),
            None,
        )
        .is_allow()
    });
    for obj in objects.iter_mut() {
        let id = obj
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let type_name = obj
            .get("type_name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        strip_property_maps(grants, session, &type_name, &id, obj, "properties");
        strip_property_maps(grants, session, &type_name, &id, obj, "as_of");
        strip_property_maps(grants, session, &type_name, &id, obj, "provenance");
    }
}

fn strip_property_maps(
    grants: &[PolicySpec],
    session: &Session,
    type_name: &str,
    id: &str,
    obj: &mut Value,
    field: &str,
) {
    let Some(map) = obj.get_mut(field).and_then(Value::as_object_mut) else {
        return;
    };
    map.retain(|name, _| {
        authorize(
            grants,
            session,
            AuthzOp::Read,
            Some(type_name),
            Some(id),
            Some(name),
        )
        .is_allow()
    });
}

fn redact_params(grants: &[PolicySpec], session: &Session, rec: &mut DecisionRecordView) {
    let hidden = hidden_property_names(grants, session, &rec.data_snapshot);
    if hidden.is_empty() {
        return;
    }
    if let Some(obj) = rec.params.as_object_mut() {
        obj.retain(|k, _| !hidden.iter().any(|h| h == k));
    }
}

fn redact_guards(grants: &[PolicySpec], session: &Session, rec: &mut DecisionRecordView) {
    let hidden = hidden_property_names(grants, session, &rec.data_snapshot);
    if hidden.is_empty() {
        return;
    }
    for g in &mut rec.guard_results {
        if hidden.iter().any(|h| g.reason.contains(h)) {
            g.reason = "redacted".into();
        }
    }
}

fn hidden_property_names(
    grants: &[PolicySpec],
    session: &Session,
    snapshot: &Value,
) -> Vec<String> {
    let mut hidden = Vec::new();
    let Some(objects) = snapshot.get("objects").and_then(Value::as_array) else {
        return hidden;
    };
    for obj in objects {
        let id = obj.get("id").and_then(Value::as_str).unwrap_or_default();
        let type_name = obj
            .get("type_name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let Some(props) = obj.get("properties").and_then(Value::as_object) else {
            continue;
        };
        for name in props.keys() {
            if matches!(
                authorize(
                    grants,
                    session,
                    AuthzOp::Read,
                    Some(type_name),
                    Some(id),
                    Some(name),
                ),
                AuthzDecision::Deny
            ) {
                hidden.push(name.clone());
            }
        }
        if let Some(as_of) = obj.get("as_of").and_then(Value::as_object) {
            for name in as_of.keys() {
                if matches!(
                    authorize(
                        grants,
                        session,
                        AuthzOp::Read,
                        Some(type_name),
                        Some(id),
                        Some(name),
                    ),
                    AuthzDecision::Deny
                ) {
                    hidden.push(name.clone());
                }
            }
        }
    }
    hidden.sort();
    hidden.dedup();
    hidden
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::{AuthzDecision, AuthzLevel, PolicySpec};
    use crate::tiers::AgentTier;
    use crate::types::{Actor, GuardResult, KeyKind, Verdict, WritePathStep};
    use serde_json::json;

    fn restricted() -> Session {
        Session::new(
            Actor::consumer("restricted", &["restricted"], AgentTier::T2),
            "test",
        )
    }

    fn grant_deny_do_max() -> Vec<PolicySpec> {
        vec![
            PolicySpec {
                name: "platform_read".into(),
                level: AuthzLevel::Platform,
                op: AuthzOp::Read,
                key: Some(KeyKind::Consumer),
                type_name: None,
                instance_id: None,
                property: None,
                roles: vec![],
                min_tier: 0,
                decision: AuthzDecision::Allow,
            },
            PolicySpec {
                name: "type_read".into(),
                level: AuthzLevel::Type,
                op: AuthzOp::Read,
                key: Some(KeyKind::Consumer),
                type_name: Some("*".into()),
                instance_id: None,
                property: None,
                roles: vec!["restricted".into()],
                min_tier: 0,
                decision: AuthzDecision::Allow,
            },
            PolicySpec {
                name: "instance_read".into(),
                level: AuthzLevel::Instance,
                op: AuthzOp::Read,
                key: Some(KeyKind::Consumer),
                type_name: Some("*".into()),
                instance_id: Some("*".into()),
                property: None,
                roles: vec!["restricted".into()],
                min_tier: 0,
                decision: AuthzDecision::Allow,
            },
            PolicySpec {
                name: "property_read".into(),
                level: AuthzLevel::Property,
                op: AuthzOp::Read,
                key: Some(KeyKind::Consumer),
                type_name: Some("*".into()),
                instance_id: Some("*".into()),
                property: Some("*".into()),
                roles: vec!["restricted".into()],
                min_tier: 0,
                decision: AuthzDecision::Allow,
            },
            PolicySpec {
                name: "do_max_deny".into(),
                level: AuthzLevel::Property,
                op: AuthzOp::Read,
                key: Some(KeyKind::Consumer),
                type_name: Some("*".into()),
                instance_id: Some("*".into()),
                property: Some("do_max".into()),
                roles: vec!["restricted".into()],
                min_tier: 0,
                decision: AuthzDecision::Deny,
            },
        ]
    }

    #[test]
    fn does_drop_denied_property_metadata_with_the_value() {
        let mut rec = DecisionRecordView {
            id: "d".into(),
            action_name: "propose".into(),
            actor: "ops".into(),
            confirmer: None,
            verdict: Verdict::Allow,
            params: json!({ "target_do": 2.0, "do_max": 4.0 }),
            guard_results: vec![GuardResult {
                name: "permit_limit".into(),
                verdict: Verdict::Deny,
                reason: "target_do=8 exceeds do_max=4".into(),
            }],
            effects: json!({}),
            rule_version: "r".into(),
            function_version: "f".into(),
            engine_version: "0.1.0".into(),
            data_snapshot: json!({
                "objects": [{
                    "id": "permit",
                    "type_name": "PermitVersion",
                    "properties": { "do_max": 4.0, "name": "2026" },
                    "as_of": { "do_max": "100", "name": "100" },
                    "provenance": { "do_max": "lab", "name": "lab" }
                }]
            }),
            proof_trace: Vec::<WritePathStep>::new(),
            created_at: "1".into(),
        };
        redact_record(&grant_deny_do_max(), &restricted(), &mut rec);
        let obj = &rec.data_snapshot["objects"][0];
        assert!(obj["properties"].get("do_max").is_none());
        assert!(obj["as_of"].get("do_max").is_none());
        assert!(obj["provenance"].get("do_max").is_none());
        assert_eq!(obj["properties"]["name"], json!("2026"));
        assert_eq!(rec.guard_results[0].reason, "redacted");
        assert!(rec.params.get("do_max").is_none());
    }
}
