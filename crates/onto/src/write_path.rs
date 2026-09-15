//! Seven-step Action write path (Zhang 2026, Ch. 3.4 / Ch. 9).
//!
//! The executor is a typestate machine: `WritePath<Submit>` can only become
//! `WritePath<ParamAndPermission>`. There is no method that jumps a successor.
//! Guard failure at submission criteria discards the stage by never building it.

use crate::types::{DataSnapshot, GuardResult, SnapshotObject, Verdict, WritePathStep};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use std::marker::PhantomData;

pub struct Submit;
pub struct ParamAndPermission;
pub struct SubmissionCriteria;
pub struct StagedEdits;
pub struct Commit;
pub struct SealDecisionRecord;
pub struct DeclareSideEffects;

/// Ontology-internal edits held in memory until commit. Discarded if never committed.
#[derive(Debug, Clone)]
pub enum StagedOp {
    InsertObject {
        id: String,
        type_name: String,
        title: Option<String>,
        properties: String,
        created_at: i64,
    },
    UpdateObject {
        id: String,
        properties: String,
    },
    InsertLink {
        id: String,
        type_name: String,
        from_id: String,
        to_id: String,
    },
    InsertInbox {
        id: String,
        action_name: String,
        proposed_by: String,
        params: String,
        created_at: i64,
        status: String,
        rule_pin: String,
        apply_action: String,
    },
    ConfirmInbox {
        id: String,
    },
    CloseLink {
        type_name: String,
        from_id: String,
        to_id: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeclaredSideEffect {
    pub idempotency_key: String,
    pub declaration: Value,
}

/// Typestate cursor over [`WritePathStep`]. Marker `S` is the current step.
pub struct WritePath<S> {
    pub trace: Vec<WritePathStep>,
    pub reads: Vec<SnapshotObject>,
    pub staged: Vec<StagedOp>,
    pub guards: Vec<GuardResult>,
    pub verdict: Verdict,
    pub idempotency_key: String,
    pub created_ids: Vec<String>,
    pub side_effect: Option<DeclaredSideEffect>,
    _s: PhantomData<S>,
}

impl<S> WritePath<S> {
    fn into_successor<T>(mut self, step: WritePathStep) -> WritePath<T> {
        let current = match self.trace.last() {
            Some(step) => *step,
            None => WritePathStep::Submit,
        };
        debug_assert_eq!(current.successor(), Some(step));
        self.trace.push(step);
        WritePath {
            trace: self.trace,
            reads: self.reads,
            staged: self.staged,
            guards: self.guards,
            verdict: self.verdict,
            idempotency_key: self.idempotency_key,
            created_ids: self.created_ids,
            side_effect: self.side_effect,
            _s: PhantomData,
        }
    }

    /// Assemble the pinned snapshot from objects actually read on this path.
    pub fn data_snapshot(
        &self,
        rule_version: String,
        function_version: String,
        engine_version: String,
    ) -> DataSnapshot {
        DataSnapshot {
            objects: dedupe_reads(&self.reads),
            rule_version,
            function_version,
            engine_version,
        }
    }
}

impl WritePath<Submit> {
    pub fn begin(idempotency_key: String) -> Self {
        Self {
            trace: vec![WritePathStep::Submit],
            reads: Vec::new(),
            staged: Vec::new(),
            guards: Vec::new(),
            verdict: Verdict::Allow,
            idempotency_key,
            created_ids: Vec::new(),
            side_effect: None,
            _s: PhantomData,
        }
    }

    pub fn param_and_permission(
        mut self,
        reads: Vec<SnapshotObject>,
    ) -> WritePath<ParamAndPermission> {
        self.reads = reads;
        self.into_successor(WritePathStep::ParamAndPermission)
    }
}

impl WritePath<ParamAndPermission> {
    pub fn submission_criteria(
        mut self,
        guards: Vec<GuardResult>,
        more_reads: Vec<SnapshotObject>,
        verdict: Verdict,
    ) -> WritePath<SubmissionCriteria> {
        self.reads.extend(more_reads);
        self.guards = guards;
        self.verdict = verdict;
        self.into_successor(WritePathStep::SubmissionCriteria)
    }

    /// Auth/param denial never enters criteria or staging. Stage stays empty.
    pub fn abort_seal(
        self,
        guards: Vec<GuardResult>,
        verdict: Verdict,
    ) -> WritePath<SealDecisionRecord> {
        let mut next = WritePath::<SealDecisionRecord> {
            trace: self.trace,
            reads: self.reads,
            staged: Vec::new(),
            guards,
            verdict,
            idempotency_key: self.idempotency_key,
            created_ids: Vec::new(),
            side_effect: None,
            _s: PhantomData,
        };
        next.trace.push(WritePathStep::SealDecisionRecord);
        next
    }
}

impl WritePath<SubmissionCriteria> {
    pub fn stage(self, ops: Vec<StagedOp>) -> WritePath<StagedEdits> {
        let mut next = self.into_successor(WritePathStep::StagedEdits);
        next.staged = ops;
        next
    }

    /// Guard fail: do not build staged ops. Commit never runs.
    pub fn discard_stage(self) -> WritePath<SealDecisionRecord> {
        let mut next = WritePath::<SealDecisionRecord> {
            trace: self.trace,
            reads: self.reads,
            staged: Vec::new(),
            guards: self.guards,
            verdict: self.verdict,
            idempotency_key: self.idempotency_key,
            created_ids: Vec::new(),
            side_effect: None,
            _s: PhantomData,
        };
        next.trace.push(WritePathStep::SealDecisionRecord);
        next
    }
}

impl WritePath<StagedEdits> {
    pub fn commit(self, created_ids: Vec<String>) -> WritePath<Commit> {
        let mut next = self.into_successor(WritePathStep::Commit);
        next.created_ids = created_ids;
        next
    }
}

impl WritePath<Commit> {
    pub fn seal(self) -> WritePath<SealDecisionRecord> {
        self.into_successor(WritePathStep::SealDecisionRecord)
    }
}

impl WritePath<SealDecisionRecord> {
    pub fn declare(self, declaration: Value) -> WritePath<DeclareSideEffects> {
        let key = self.idempotency_key.clone();
        let mut next = self.into_successor(WritePathStep::DeclareSideEffects);
        next.side_effect = Some(DeclaredSideEffect {
            idempotency_key: key,
            declaration,
        });
        next
    }
}

pub fn pin_version(label: &str, body: &str) -> String {
    let mut h: u64 = 5381;
    for b in body.as_bytes() {
        h = h.wrapping_mul(33) ^ u64::from(*b);
    }
    format!("{label}:{h:016x}")
}

pub fn resolve_idempotency_key(
    action: &str,
    actor: &str,
    params: &Value,
    inbox: Option<&str>,
) -> String {
    crate::command::idempotency_key(action, actor, params, inbox)
}

fn dedupe_reads(reads: &[SnapshotObject]) -> Vec<SnapshotObject> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for obj in reads {
        if seen.insert(obj.id.clone()) {
            out.push(obj.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    #[test]
    fn does_chain_exactly_seven_successors() {
        assert_eq!(WritePathStep::ALL.len(), 7);
        let mut step = WritePathStep::Submit;
        let mut seen = vec![step];
        while let Some(next) = step.successor() {
            seen.push(next);
            step = next;
        }
        assert_eq!(seen, WritePathStep::ALL.to_vec());
        assert_eq!(
            seen.last().copied(),
            Some(WritePathStep::DeclareSideEffects)
        );
        assert_eq!(WritePathStep::DeclareSideEffects.successor(), None);
    }

    #[test]
    fn does_record_all_seven_steps_on_happy_path() {
        let done = WritePath::begin("k".into())
            .param_and_permission(vec![])
            .submission_criteria(vec![], vec![], Verdict::Allow)
            .stage(vec![])
            .commit(vec![])
            .seal()
            .declare(Value::Null);
        assert_eq!(done.trace, WritePathStep::ALL.to_vec());
    }

    #[test]
    fn does_skip_stage_and_commit_if_guard_fails() {
        let sealed = WritePath::begin("k".into())
            .param_and_permission(vec![])
            .submission_criteria(
                vec![GuardResult {
                    name: "permit_limit".into(),
                    verdict: Verdict::Deny,
                    reason: "over".into(),
                }],
                vec![],
                Verdict::Deny,
            )
            .discard_stage();
        assert_eq!(
            sealed.trace,
            vec![
                WritePathStep::Submit,
                WritePathStep::ParamAndPermission,
                WritePathStep::SubmissionCriteria,
                WritePathStep::SealDecisionRecord,
            ]
        );
        assert!(sealed.staged.is_empty());
        assert_eq!(sealed.verdict, Verdict::Deny);
        assert_eq!(sealed.created_ids, Vec::<String>::new());
        assert!(sealed.side_effect.is_none());
        assert!(!sealed.trace.contains(&WritePathStep::StagedEdits));
        assert!(!sealed.trace.contains(&WritePathStep::Commit));
        assert!(!sealed.trace.contains(&WritePathStep::DeclareSideEffects));
    }

    #[test]
    fn does_leave_stage_empty_if_abort_seals() {
        let sealed = WritePath::begin("k".into())
            .param_and_permission(vec![])
            .abort_seal(
                vec![GuardResult {
                    name: "authorization".into(),
                    verdict: Verdict::Deny,
                    reason: "no role".into(),
                }],
                Verdict::Deny,
            );
        assert_eq!(
            sealed.trace,
            vec![
                WritePathStep::Submit,
                WritePathStep::ParamAndPermission,
                WritePathStep::SealDecisionRecord,
            ]
        );
        assert!(sealed.staged.is_empty());
        assert_eq!(sealed.verdict, Verdict::Deny);
        assert!(!sealed.trace.contains(&WritePathStep::SubmissionCriteria));
        assert!(!sealed.trace.contains(&WritePathStep::StagedEdits));
        assert!(!sealed.trace.contains(&WritePathStep::Commit));
    }

    #[test]
    fn does_dedupe_snapshot_reads_by_object_id() {
        let tank = SnapshotObject {
            id: "tank-1".into(),
            type_name: "AerationTank".into(),
            version_id: "v1".into(),
            properties: BTreeMap::new(),
            as_of: BTreeMap::new(),
            provenance: BTreeMap::new(),
        };
        let later = SnapshotObject {
            id: "tank-1".into(),
            type_name: "AerationTank".into(),
            version_id: "v1".into(),
            properties: BTreeMap::new(),
            as_of: BTreeMap::new(),
            provenance: BTreeMap::new(),
        };
        let sensor = SnapshotObject {
            id: "sensor-1".into(),
            type_name: "DO_Sensor".into(),
            version_id: "v2".into(),
            properties: BTreeMap::new(),
            as_of: BTreeMap::new(),
            provenance: BTreeMap::new(),
        };
        let path = WritePath::begin("k".into()).param_and_permission(vec![tank, later, sensor]);
        let snap = path.data_snapshot("rule:1".into(), "fn:1".into(), "0.1.0".into());
        let ids: Vec<&str> = snap.objects.iter().map(|o| o.id.as_str()).collect();
        assert_eq!(ids, vec!["tank-1", "sensor-1"]);
        assert_eq!(snap.rule_version, "rule:1");
        assert_eq!(snap.function_version, "fn:1");
        assert_eq!(snap.engine_version, "0.1.0");
    }

    #[test]
    fn does_use_explicit_idempotency_key_if_present() {
        let key = resolve_idempotency_key(
            "approve_setpoint_change",
            "ops.chen",
            &json!({ "idempotency_key": "setpoint:tank-1:once", "target_do": 2.5 }),
            None,
        );
        assert_eq!(
            key,
            "approve_setpoint_change:ops.chen:submit:setpoint:tank-1:once"
        );
    }

    #[test]
    fn does_derive_idempotency_key_if_absent() {
        let a = resolve_idempotency_key(
            "approve_setpoint_change",
            "ops.chen",
            &json!({ "n": 1 }),
            None,
        );
        let b = resolve_idempotency_key(
            "approve_setpoint_change",
            "ops.chen",
            &json!({ "n": 2 }),
            None,
        );
        let empty = resolve_idempotency_key(
            "approve_setpoint_change",
            "ops.chen",
            &json!({ "idempotency_key": "" }),
            None,
        );
        assert_ne!(a, b);
        assert!(!a.is_empty());
        assert_ne!(empty, "");
        assert!(a.starts_with("approve_setpoint_change:ops.chen:"));
    }
}
