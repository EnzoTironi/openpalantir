//! Logical command identity: scoped idempotency and payload digest.
//!
//! The Store transaction reserves this key before applying effects.
//! Same key + same digest replays. Same key + different digest conflicts.

use crate::write_path::pin_version;
use serde_json::Value;

/// Outcome of one decision transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecisionApply {
    /// Fresh write-set committed.
    Written(Vec<String>),
    /// Existing outcome for the same key and digest.
    Replayed(String),
}

/// Cached retry row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdempotencyRow {
    pub outcome: String,
    pub payload_digest: String,
}

/// Digest of the request body, ignoring the caller-supplied key field.
#[must_use]
pub fn payload_digest(params: &Value) -> String {
    let mut body = params.clone();
    if let Some(obj) = body.as_object_mut() {
        obj.remove("idempotency_key");
    }
    pin_version("payload", &body.to_string())
}

/// Retry identity: action, actor, inbox-or-submit, caller key or body pin.
///
/// Confirmation includes the inbox id so two proposers reusing `1` stay distinct.
#[must_use]
pub fn idempotency_key(action: &str, actor: &str, params: &Value, inbox: Option<&str>) -> String {
    let scope = inbox.unwrap_or("submit");
    if let Some(k) = params.get("idempotency_key").and_then(Value::as_str) {
        if !k.is_empty() {
            return format!("{action}:{actor}:{scope}:{k}");
        }
    }
    pin_version(&format!("{action}:{actor}:{scope}"), &params.to_string())
}

/// Object id + open version id, stable order, one entry per object.
#[must_use]
pub fn read_set(reads: &[(String, String)]) -> Vec<(String, String)> {
    let mut out = reads.to_vec();
    out.sort_unstable();
    out.dedup_by(|a, b| a.0 == b.0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn does_bind_digest_to_payload_not_the_caller_key() {
        let a = json!({ "target_do": 2.0, "idempotency_key": "local-1" });
        let b = json!({ "target_do": 2.5, "idempotency_key": "local-1" });
        let same = json!({ "target_do": 2.0, "idempotency_key": "other" });
        assert_ne!(payload_digest(&a), payload_digest(&b));
        assert_eq!(payload_digest(&a), payload_digest(&same));
    }

    #[test]
    fn does_scope_confirm_key_to_inbox() {
        let params = json!({ "idempotency_key": "local-request-1" });
        let a = idempotency_key("approve_setpoint_change", "boss", &params, Some("inbox-a"));
        let b = idempotency_key("approve_setpoint_change", "boss", &params, Some("inbox-b"));
        let submit = idempotency_key("propose_setpoint_change", "alice", &params, None);
        assert_ne!(a, b);
        assert!(a.contains("inbox-a"));
        assert!(submit.contains(":submit:"));
    }
}
