//! Explicit upgrade of pre-pin inbox rows and unpinned branches.
//!
//! [`crate::Engine::migrate_legacy`] cancels inbox rows with empty
//! `apply_action` and rejects open/proposed branches with empty
//! `base_revision`. It does not re-pin a stale branch to current main
//! (that would let the branch clobber main). Unmigrated rows stay
//! fail-closed: `confirm` rejects empty `apply_action`; merge rejects
//! empty `base_revision`.

use serde::{Deserialize, Serialize};

/// What [`crate::Engine::migrate_legacy`] changed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrateReport {
    pub cancelled_inbox: Vec<String>,
    pub rejected_branches: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn does_start_migrate_report_empty() {
        let r = MigrateReport::default();
        assert!(r.cancelled_inbox.is_empty());
        assert!(r.rejected_branches.is_empty());
    }
}
