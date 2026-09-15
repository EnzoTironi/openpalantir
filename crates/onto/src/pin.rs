//! Apply-pin over the Action spec and the current OMS schema revision.

use crate::types::ActionTypeSpec;
use crate::write_path::pin_version;

/// Hash the apply Action plus the schema/policy/function/type closure.
///
/// This is evaluator identity, not cryptographic tamper-evidence.
#[must_use]
#[allow(clippy::module_name_repetitions)] // public pin entry
pub fn apply_pin(apply_name: &str, spec: &ActionTypeSpec, schema_revision: &str) -> String {
    let body = format!(
        "{}|{schema_revision}",
        serde_json::to_string(spec).unwrap_or_default()
    );
    pin_version(apply_name, &body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiers::AgentTier;
    use crate::types::ExecutionMode;
    use serde_json::json;

    fn spec(name: &str) -> ActionTypeSpec {
        ActionTypeSpec {
            name: name.into(),
            mode: ExecutionMode::Approve,
            parameters: vec![],
            guards: json!([]),
            required_roles: vec![],
            required_tier: AgentTier::T3,
            effects: json!([]),
            compensation: None,
            side_effects: json!({}),
            on_review: None,
            interfaces: vec![],
        }
    }

    #[test]
    fn does_change_pin_if_schema_revision_changes() {
        let action = spec("approve_setpoint_change");
        let a = apply_pin("approve_setpoint_change", &action, "rev-a");
        let b = apply_pin("approve_setpoint_change", &action, "rev-b");
        assert_ne!(a, b);
        assert!(a.starts_with("approve_setpoint_change:"));
    }
}
