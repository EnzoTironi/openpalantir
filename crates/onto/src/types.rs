use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

use crate::functions::FunctionSpec;
use crate::tiers::AgentTier;

pub const ENGINE_VERSION: &str = "0.1.0";
pub const MAIN_BRANCH: &str = "main";

pub const KERNEL_TYPES: &[&str] = &[
    "ObjectType",
    "PropertyType",
    "ValueType",
    "LinkType",
    "InterfaceType",
    "ActionType",
    "FunctionType",
    "Policy",
    "OntologyBranch",
    "OntologyProposal",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyKind {
    Consumer,
    Builder,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Typology {
    Entity,
    Event,
    State,
    DecisionRecord,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PropertySource {
    Mapped,
    Derived,
    ActionWritten,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Allow,
    Review,
    Deny,
}

impl Verdict {
    /// Stored column. Unknown rows are Deny (deny-by-default).
    #[must_use]
    pub fn from_stored(raw: &str) -> Self {
        match raw {
            "allow" => Self::Allow,
            "review" => Self::Review,
            _ => Self::Deny,
        }
    }

    #[must_use]
    pub fn as_stored(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Review => "review",
            Self::Deny => "deny",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    Auto,
    Propose,
    Approve,
    Shadow,
}

/// Ordered Action write-path steps (Zhang 2026, Ch. 3.4 / Ch. 9).
///
/// The executor is a typestate machine over this enum. A legal run visits
/// each variant in order. There is no public constructor that starts mid-path
/// and no method that jumps a successor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WritePathStep {
    Submit,
    ParamAndPermission,
    SubmissionCriteria,
    StagedEdits,
    Commit,
    SealDecisionRecord,
    DeclareSideEffects,
}

impl WritePathStep {
    pub const ALL: [WritePathStep; 7] = [
        Self::Submit,
        Self::ParamAndPermission,
        Self::SubmissionCriteria,
        Self::StagedEdits,
        Self::Commit,
        Self::SealDecisionRecord,
        Self::DeclareSideEffects,
    ];

    #[must_use]
    pub fn successor(self) -> Option<Self> {
        match self {
            Self::Submit => Some(Self::ParamAndPermission),
            Self::ParamAndPermission => Some(Self::SubmissionCriteria),
            Self::SubmissionCriteria => Some(Self::StagedEdits),
            Self::StagedEdits => Some(Self::Commit),
            Self::Commit => Some(Self::SealDecisionRecord),
            Self::SealDecisionRecord => Some(Self::DeclareSideEffects),
            Self::DeclareSideEffects => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BranchStatus {
    Open,
    Proposed,
    Merged,
    Rejected,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Actor {
    pub id: String,
    pub key: KeyKind,
    pub roles: Vec<String>,
    pub tier: AgentTier,
}

impl Actor {
    pub fn builder(id: impl Into<String>, roles: &[&str]) -> Self {
        Self {
            id: id.into(),
            key: KeyKind::Builder,
            roles: roles.iter().copied().map(str::to_string).collect(),
            tier: AgentTier::T1,
        }
    }

    pub fn consumer(id: impl Into<String>, roles: &[&str], tier: AgentTier) -> Self {
        Self {
            id: id.into(),
            key: KeyKind::Consumer,
            roles: roles.iter().copied().map(str::to_string).collect(),
            tier,
        }
    }

    #[must_use]
    pub fn has_role(&self, role: &str) -> bool {
        self.roles.iter().any(|r| r == role)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub actor: Actor,
    pub purpose: String,
}

impl Session {
    pub fn new(actor: Actor, purpose: impl Into<String>) -> Self {
        Self {
            actor,
            purpose: purpose.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValueTypeSpec {
    pub name: String,
    pub base: String,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub unit: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PropertySpec {
    pub name: String,
    pub value_type: String,
    pub source: PropertySource,
    pub nullable: bool,
    /// Named [`FunctionSpec`] for [`PropertySource::Derived`]. Looked up, never matched on `name`.
    #[serde(default)]
    pub function: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectTypeSpec {
    pub name: String,
    pub typology: Typology,
    pub title_prop: Option<String>,
    pub interfaces: Vec<String>,
    pub freshness_budget_secs: Option<i64>,
    pub properties: Vec<PropertySpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkTypeSpec {
    pub name: String,
    pub from_type: String,
    pub to_type: String,
    pub cardinality: String,
    pub allow_cycles: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterfaceSpec {
    pub name: String,
    pub required_properties: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParamSpec {
    pub name: String,
    pub value_type: String,
    pub object_type: Option<String>,
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionTypeSpec {
    pub name: String,
    pub mode: ExecutionMode,
    pub parameters: Vec<ParamSpec>,
    pub guards: Value,
    pub required_roles: Vec<String>,
    pub required_tier: AgentTier,
    pub effects: Value,
    pub compensation: Option<String>,
    pub side_effects: Value,
    pub on_review: Option<String>,
    /// Kernel interfaces attached on a branch (Zhang 2026, Ch. 4). Empty until merge.
    #[serde(default)]
    pub interfaces: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaSnapshot {
    pub branch: String,
    pub value_types: Vec<ValueTypeSpec>,
    pub object_types: Vec<ObjectTypeSpec>,
    pub link_types: Vec<LinkTypeSpec>,
    pub interfaces: Vec<InterfaceSpec>,
    pub action_types: Vec<ActionTypeSpec>,
    #[serde(default)]
    pub functions: Vec<FunctionSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PropertyView {
    pub value: Value,
    pub source: PropertySource,
    pub as_of: Option<String>,
    pub provenance: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectView {
    pub id: String,
    pub type_name: String,
    pub title: Option<String>,
    pub properties: BTreeMap<String, PropertyView>,
    pub missing: Vec<String>,
    pub stale: Vec<String>,
    /// Open version id at the moment of the read. Empty on synthetic views.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub version_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkView {
    pub id: String,
    pub type_name: String,
    pub from_id: String,
    pub to_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuardResult {
    pub name: String,
    pub verdict: Verdict,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionOutcome {
    pub verdict: Verdict,
    pub reason: String,
    pub decision_record_id: Option<String>,
    pub inbox_id: Option<String>,
    pub created_ids: Vec<String>,
    pub alternative: Option<String>,
    pub guard_results: Vec<GuardResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionRecordView {
    pub id: String,
    pub action_name: String,
    pub actor: String,
    pub confirmer: Option<String>,
    pub verdict: Verdict,
    pub params: Value,
    pub guard_results: Vec<GuardResult>,
    pub effects: Value,
    pub rule_version: String,
    pub function_version: String,
    pub engine_version: String,
    pub data_snapshot: Value,
    #[serde(default)]
    pub proof_trace: Vec<WritePathStep>,
    pub created_at: String,
}

/// Object identity and property values pinned when an Action read the store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotObject {
    pub id: String,
    pub type_name: String,
    pub properties: BTreeMap<String, Value>,
    /// Open version id used for compare-and-swap at commit.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub version_id: String,
    /// Valid-time stamps per property (R13). Empty when the read had none.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub as_of: BTreeMap<String, String>,
    /// Provenance per property (R13). Empty when the read had none.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub provenance: BTreeMap<String, String>,
    /// True when this identity was loaded for a guard (delegated read).
    #[serde(default, skip_serializing_if = "is_false")]
    pub delegated: bool,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde skip_serializing_if needs &T
fn is_false(flag: &bool) -> bool {
    !*flag
}

/// Pin an [`ObjectView`] including per-property `as_of` and provenance.
#[must_use]
pub fn snapshot_from_view(view: &ObjectView) -> SnapshotObject {
    let mut as_of = BTreeMap::new();
    let mut provenance = BTreeMap::new();
    for (k, p) in &view.properties {
        if let Some(stamp) = &p.as_of {
            as_of.insert(k.clone(), stamp.clone());
        }
        if let Some(prov) = &p.provenance {
            provenance.insert(k.clone(), prov.clone());
        }
    }
    SnapshotObject {
        id: view.id.clone(),
        type_name: view.type_name.clone(),
        version_id: view.version_id.clone(),
        properties: view
            .properties
            .iter()
            .map(|(k, p)| (k.clone(), p.value.clone()))
            .collect(),
        as_of,
        provenance,
        delegated: false,
    }
}

/// Pins the objects actually read plus the three versions that produced the verdict.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataSnapshot {
    pub objects: Vec<SnapshotObject>,
    pub rule_version: String,
    pub function_version: String,
    pub engine_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxItem {
    pub id: String,
    pub action_name: String,
    pub proposed_by: String,
    pub params: Value,
    pub status: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestRecord {
    pub type_name: String,
    pub id: Option<String>,
    pub properties: BTreeMap<String, Value>,
    pub as_of: Option<String>,
    pub provenance: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Query {
    pub type_name: Option<String>,
    pub equals: BTreeMap<String, Value>,
    pub limit: usize,
    /// When set, search resolves this named OMS object set (Zhang 2026, Ch. 6).
    pub set_name: Option<String>,
}

impl Default for Query {
    fn default() -> Self {
        Self {
            type_name: None,
            equals: BTreeMap::new(),
            limit: 50,
            set_name: None,
        }
    }
}

#[must_use]
pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[must_use]
pub fn now_rfc3339() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{secs}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn does_deny_unknown_stored_verdict() {
        assert_eq!(Verdict::from_stored("allow"), Verdict::Allow);
        assert_eq!(Verdict::from_stored("review"), Verdict::Review);
        assert_eq!(Verdict::from_stored("deny"), Verdict::Deny);
        assert_eq!(Verdict::from_stored(""), Verdict::Deny);
        assert_eq!(Verdict::from_stored("Allow"), Verdict::Deny);
        assert_eq!(Verdict::from_stored("unknown"), Verdict::Deny);
    }

    #[test]
    fn does_roundtrip_stored_verdicts() {
        for verdict in [Verdict::Allow, Verdict::Review, Verdict::Deny] {
            assert_eq!(Verdict::from_stored(verdict.as_stored()), verdict);
        }
    }

    #[test]
    fn does_walk_write_path_successors_in_order() {
        assert_eq!(WritePathStep::ALL.len(), 7);
        let mut step = WritePathStep::Submit;
        let mut seen = vec![step];
        while let Some(next) = step.successor() {
            seen.push(next);
            step = next;
        }
        assert_eq!(seen, WritePathStep::ALL.to_vec());
        assert_eq!(WritePathStep::DeclareSideEffects.successor(), None);
    }
}
