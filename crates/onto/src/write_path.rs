//! Seven-step Action write path (Zhang 2026, Ch. 3.4 / Ch. 9).
//!
//! The executor is a typestate machine: `WritePath<Submit>` can only become
//! `WritePath<ParamAndPermission>`. There is no method that jumps a successor.
//! Guard failure at submission criteria discards the stage by never building it.

use crate::types::{
    DataSnapshot, GuardResult, SnapshotObject, Verdict, WritePathStep,
};
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
    },
    ConfirmInbox {
        id: String,
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
        let current = *self
            .trace
            .last()
            .expect("write path always starts with Submit");
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

pub fn resolve_idempotency_key(action: &str, actor: &str, params: &Value) -> String {
    if let Some(k) = params.get("idempotency_key").and_then(|v| v.as_str()) {
        if !k.is_empty() {
            return k.to_string();
        }
    }
    pin_version(&format!("{action}:{actor}"), &params.to_string())
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

    #[test]
    fn successor_chain_is_exactly_seven() {
        assert_eq!(WritePathStep::ALL.len(), 7);
        let mut step = WritePathStep::Submit;
        let mut seen = vec![step];
        while let Some(next) = step.successor() {
            seen.push(next);
            step = next;
        }
        assert_eq!(seen, WritePathStep::ALL.to_vec());
        assert_eq!(seen.last().copied(), Some(WritePathStep::DeclareSideEffects));
        assert_eq!(WritePathStep::DeclareSideEffects.successor(), None);
    }

    #[test]
    fn typestate_happy_path_records_all_steps() {
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
    fn guard_fail_trace_skips_stage_and_commit() {
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
    }
}
