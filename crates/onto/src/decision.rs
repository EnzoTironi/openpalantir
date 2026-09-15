//! Decision unit and durable effect intention (Zhang 2026, §3.4.2 / §9.3).
//!
//! The Store owns the transaction that commits the write-set, dossier,
//! idempotency row, and effect intention together. The host owns dispatch.
//! [`EffectStatus::Declared`] is not external delivery.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Host-visible status of a declared side effect. The kernel never marks
/// delivery; Allow/committed is ontology-internal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectStatus {
    Declared,
}

impl EffectStatus {
    #[must_use]
    pub fn as_stored(self) -> &'static str {
        match self {
            Self::Declared => "declared",
        }
    }
}

/// Intention recorded in the same transaction as the `DecisionRecord`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectIntention {
    pub decision_record_id: String,
    pub declaration: Value,
    pub status: EffectStatus,
}

/// Integration model chosen for Zoen: the Rust Engine is the canonical writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrationModel {
    /// Engine writes domain state, decision, and effect intention. Zoen host
    /// dispatches declared effects. Not a second editable ontology.
    EngineCanonicalWriter,
}

#[must_use]
pub fn chosen_integration() -> IntegrationModel {
    IntegrationModel::EngineCanonicalWriter
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn does_declare_effect_without_claiming_delivery() {
        assert_eq!(EffectStatus::Declared.as_stored(), "declared");
        assert_eq!(
            chosen_integration(),
            IntegrationModel::EngineCanonicalWriter
        );
    }
}
