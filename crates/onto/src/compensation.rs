//! Compensation as a forward inverse Action (Zhang 2026, Ch. 9).
//!
//! Compensate by submitting the Action named on `ActionTypeSpec.compensation`
//! through the seven-step write path. A new `DecisionRecord` is sealed.
//! The original record is not updated, deleted, or rolled back. Object
//! versions append; history at the old `as_of` still shows the first write.
//!
//! # Context
//! `ActionTypeSpec.compensation` is an optional Action name. Side effects
//! already carry idempotency keys; a retry of the inverse uses the same
//! key and must not double-apply.
//!
//! # Inputs
//! Original Allow `DecisionRecordView`, its Action spec, optional overlay
//! params (`target_do`, `idempotency_key`, …).
//!
//! # Outputs
//! [`Compensation::Inverse`] naming the Action to execute, plus params
//! for `execute_action`. Missing name is [`crate::OntoError::NoCompensation`],
//! never a silent success.
//!
//! # Side effects
//! None in this module. The engine submits the inverse; this file only
//! plans it.
//!
//! # Example
//! ```
//! use onto::{ActionTypeSpec, Compensation, ExecutionMode};
//! use serde_json::json;
//!
//! let spec = ActionTypeSpec {
//!     name: "approve_setpoint_change".into(),
//!     mode: ExecutionMode::Auto,
//!     parameters: vec![],
//!     guards: json!([]),
//!     required_roles: vec![],
//!     required_tier: 0,
//!     effects: json!([]),
//!     compensation: Some("revert_setpoint_change".into()),
//!     side_effects: json!({}),
//!     on_review: None,
//! };
//! let plan = Compensation::from_spec(&spec).unwrap();
//! let Compensation::Inverse { action } = plan;
//! assert_eq!(action, "revert_setpoint_change");
//! ```
//!
//! # Relations
//! Engine `compensate_action` loads the record, calls these helpers, then
//! `execute_action`. Bootstrap names `revert_setpoint_change` on approve.

use crate::error::{OntoError, Result};
use crate::types::{ActionTypeSpec, DecisionRecordView, Verdict};
use serde_json::{json, Value};

/// Named inverse Action. There is no rollback variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Compensation {
    Inverse { action: String },
}

impl Compensation {
    /// Resolve the compensation Action name. Empty or missing is an error.
    pub fn from_spec(spec: &ActionTypeSpec) -> Result<Self> {
        match spec.compensation.as_deref() {
            Some(name) if !name.is_empty() => Ok(Self::Inverse {
                action: name.to_string(),
            }),
            Some(_) | None => Err(OntoError::NoCompensation(spec.name.clone())),
        }
    }
}

/// Only an Allow may be compensated. Review and Deny stay on the record.
pub fn require_allow(original: &DecisionRecordView) -> Result<()> {
    match original.verdict {
        Verdict::Allow => Ok(()),
        Verdict::Review => Err(OntoError::NotCompensable(original.id.clone())),
        Verdict::Deny => Err(OntoError::NotCompensable(original.id.clone())),
    }
}

/// Params for the inverse Action. Never reuses the original idempotency key.
///
/// Overlay wins. If overlay omits `target_do`, a numeric snapshot value on
/// the tank is used. Default key is `compensate:{original.id}`.
pub fn inverse_params(original: &DecisionRecordView, overlay: &Value) -> Value {
    let mut params = match &original.params {
        Value::Object(map) => Value::Object(map.clone()),
        other => json!({ "original": other }),
    };
    if let Some(base) = params.as_object_mut() {
        base.remove("idempotency_key");
    }
    if let Some(over) = overlay.as_object() {
        if let Some(base) = params.as_object_mut() {
            for (k, v) in over {
                if !v.is_null() {
                    base.insert(k.clone(), v.clone());
                }
            }
        }
    }
    if overlay.get("target_do").is_none() {
        if let Some(prev) = previous_written(original, "tank", "target_do") {
            if !prev.is_null() {
                params["target_do"] = prev;
            }
        }
    }
    let has_key = params
        .get("idempotency_key")
        .and_then(|v| v.as_str())
        .map(|s| !s.is_empty())
        .unwrap_or(false);
    if !has_key {
        params["idempotency_key"] = json!(format!("compensate:{}", original.id));
    }
    params["compensates"] = json!(original.id);
    params
}

/// Property value pinned on the object named by `object_param` in the snapshot.
pub fn previous_written(
    original: &DecisionRecordView,
    object_param: &str,
    property: &str,
) -> Option<Value> {
    let id = original.params.get(object_param)?.as_str()?;
    let objects = original.data_snapshot.get("objects")?.as_array()?;
    let obj = objects.iter().find(|o| o.get("id").and_then(|v| v.as_str()) == Some(id))?;
    obj.get("properties")?.get(property).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ENGINE_VERSION, ExecutionMode};

    fn spec_with(compensation: Option<&str>) -> ActionTypeSpec {
        ActionTypeSpec {
            name: "approve_setpoint_change".into(),
            mode: ExecutionMode::Auto,
            parameters: vec![],
            guards: json!([]),
            required_roles: vec![],
            required_tier: 0,
            effects: json!([]),
            compensation: compensation.map(|s| s.into()),
            side_effects: json!({}),
            on_review: None,
        }
    }

    fn allow_record(id: &str, tank: &str, key: &str) -> DecisionRecordView {
        DecisionRecordView {
            id: id.into(),
            action_name: "approve_setpoint_change".into(),
            actor: "ops.chen".into(),
            confirmer: Some("ops.chen".into()),
            verdict: Verdict::Allow,
            params: json!({
                "tank": tank,
                "target_do": 2.5,
                "idempotency_key": key
            }),
            guard_results: vec![],
            effects: json!({}),
            rule_version: "approve_setpoint_change:x".into(),
            function_version: "none".into(),
            engine_version: ENGINE_VERSION.into(),
            data_snapshot: json!({
                "objects": [{
                    "id": tank,
                    "type_name": "AerationTank",
                    "properties": { "name": "Basin 1", "target_do": 1.8 }
                }]
            }),
            proof_trace: vec![],
            created_at: "1700000000".into(),
        }
    }

    #[test]
    fn missing_compensation_name_is_typed_error() {
        match Compensation::from_spec(&spec_with(None)) {
            Err(OntoError::NoCompensation(name)) => {
                assert_eq!(name, "approve_setpoint_change");
            }
            other => panic!("expected NoCompensation, got {other:?}"),
        }
        match Compensation::from_spec(&spec_with(Some(""))) {
            Err(OntoError::NoCompensation(_)) => {}
            other => panic!("empty name must not succeed, got {other:?}"),
        }
    }

    #[test]
    fn named_compensation_is_inverse_action() {
        let plan = Compensation::from_spec(&spec_with(Some("revert_setpoint_change"))).unwrap();
        match plan {
            Compensation::Inverse { action } => {
                assert_eq!(action, "revert_setpoint_change");
            }
        }
    }

    #[test]
    fn deny_and_review_are_not_compensable() {
        let mut rec = allow_record("d1", "tank-1", "k");
        rec.verdict = Verdict::Deny;
        assert!(matches!(
            require_allow(&rec),
            Err(OntoError::NotCompensable(id)) if id == "d1"
        ));
        rec.verdict = Verdict::Review;
        assert!(matches!(require_allow(&rec), Err(OntoError::NotCompensable(_))));
        rec.verdict = Verdict::Allow;
        assert!(require_allow(&rec).is_ok());
    }

    #[test]
    fn inverse_params_drop_original_key_and_default_compensate_key() {
        let rec = allow_record("rec-1", "tank-1", "setpoint:tank-1:2.5");
        let params = inverse_params(&rec, &json!({}));
        assert_eq!(params["idempotency_key"], "compensate:rec-1");
        assert_eq!(params["compensates"], "rec-1");
        assert_eq!(params["tank"], "tank-1");
        assert_eq!(
            params["target_do"],
            json!(1.8),
            "restore snapshot target_do when overlay omits it"
        );
    }

    #[test]
    fn overlay_target_do_and_key_win() {
        let rec = allow_record("rec-1", "tank-1", "old-key");
        let params = inverse_params(
            &rec,
            &json!({
                "target_do": 1.2,
                "idempotency_key": "compensate:once"
            }),
        );
        assert_eq!(params["target_do"], json!(1.2));
        assert_eq!(params["idempotency_key"], "compensate:once");
        assert_ne!(params["idempotency_key"], "setpoint:tank-1:2.5");
    }
}
