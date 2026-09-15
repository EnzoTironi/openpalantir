//! In-process host drain. Allow is not external delivery.

use crate::decision::EffectStatus;
use crate::engine::Engine;
use crate::error::Result;
use crate::types::Session;

/// Reconcile, claim each declared intention, no-op dispatch, then ack.
///
/// No mail, ERP, or connector. The host process is this function.
pub fn drain_declared(engine: &Engine, session: &Session) -> Result<Vec<String>> {
    engine.reconcile_effects(session)?;
    let declared = engine.list_effect_intentions(session, Some(EffectStatus::Declared))?;
    let mut acked = Vec::new();
    for item in declared {
        engine.claim_effect(session, &item.decision_record_id)?;
        engine.ack_effect(session, &item.decision_record_id)?;
        acked.push(item.decision_record_id);
    }
    Ok(acked)
}
