use crate::bitemporal::{self, AsOf};
use crate::clock::Clock;
use crate::command::{self, payload_digest, DecisionApply};
use crate::compensation::{self, Compensation};
use crate::decision::{EffectIntention, EffectStatus};
use crate::disclosure;
use crate::effects::{self, Cardinality, EffectOp};
use crate::error::{OntoError, Result};
use crate::functions::{self, FunctionSpec};
use crate::guards;
use crate::kernel::{self, KernelInterface};
use crate::migrate::MigrateReport;
use crate::oss::{
    apply_permission, evaluate_members, ObjectSet, ObjectSetFilter, ObjectSetSpec,
    OBJECT_SETS_TABLE,
};
use crate::security::{authorize, filter_view, AuthzDecision, AuthzOp, PolicySpec, POLICIES_TABLE};
use crate::store::{ClosedLink, DecisionCommit, DecisionWrite, InboxRow, SqliteStore, Store};
use crate::tiers::{self, AgentTier, AutoBound, RiskBand};
#[allow(clippy::wildcard_imports)] // engine is the OMS wiring hub over types
use crate::types::*;
use crate::write_path::{pin_version, resolve_idempotency_key, StagedOp, WritePath};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};

/// Coordinates OMS syscalls. Persistence is [`Store`], not this type's fields.
pub struct Engine {
    store: Box<dyn Store>,
    clock: Clock,
}

impl Engine {
    /// Inject a persistence backend. Uses the production wall clock.
    #[must_use]
    pub fn from_store(store: impl Store + 'static) -> Self {
        Self {
            store: Box::new(store),
            clock: Clock::live(),
        }
    }

    pub fn memory() -> Result<Self> {
        Ok(Self {
            store: Box::new(SqliteStore::memory()?),
            clock: Clock::frozen(1_700_000_000),
        })
    }

    pub fn open(path: &str) -> Result<Self> {
        Ok(Self::from_store(SqliteStore::open(path)?))
    }

    pub fn set_clock(&self, secs: i64) {
        self.clock.set(secs);
    }

    #[must_use]
    pub fn now(&self) -> i64 {
        self.clock.now()
    }

    #[must_use]
    pub fn clock_is_frozen(&self) -> bool {
        self.clock.is_frozen()
    }

    fn require_builder(session: &Session) -> Result<()> {
        match session.actor.key {
            KeyKind::Builder => Ok(()),
            KeyKind::Consumer => Err(OntoError::Denied(
                "consumer key cannot mutate schema".into(),
            )),
        }
    }

    fn require_consumer(session: &Session) -> Result<()> {
        match session.actor.key {
            KeyKind::Consumer => Ok(()),
            KeyKind::Builder => Err(OntoError::Denied(
                "builder key cannot read or write production instances".into(),
            )),
        }
    }

    fn audit(&self, actor: &str, kind: &str, payload: &Value) -> Result<()> {
        self.store
            .insert_audit(&new_id(), self.now(), actor, kind, &payload.to_string())
    }

    #[must_use]
    pub fn kernel_types() -> &'static [&'static str] {
        KERNEL_TYPES
    }

    pub fn open_branch(&self, session: &Session, name: &str) -> Result<String> {
        Self::require_builder(session)?;
        if name == MAIN_BRANCH {
            return Err(OntoError::Invalid("cannot reopen main".into()));
        }
        if let Some(status) = self.store.branch_status(name)? {
            if status == "open" {
                return Ok(name.to_string());
            }
            return Err(OntoError::Conflict(format!("branch {name} is {status}")));
        }
        let base = self.store.schema_revision(MAIN_BRANCH)?;
        self.store
            .insert_open_branch(name, &session.actor.id, self.now(), &base)?;
        self.store.copy_schema(MAIN_BRANCH, name)?;
        self.audit(&session.actor.id, "open_branch", &json!({ "branch": name }))?;
        Ok(name.to_string())
    }

    fn require_open_branch(&self, branch: &str) -> Result<()> {
        if branch == MAIN_BRANCH {
            return Err(OntoError::Invalid(
                "mutate schema on a working branch, not main".into(),
            ));
        }
        let status = self
            .store
            .branch_status(branch)?
            .ok_or_else(|| OntoError::NotFound(format!("branch {branch}")))?;
        if status != "open" {
            return Err(OntoError::Conflict(format!("branch {branch} is {status}")));
        }
        Ok(())
    }

    fn put_spec(&self, table: &str, branch: &str, name: &str, spec: &Value) -> Result<()> {
        self.store.put_spec(table, branch, name, &spec.to_string())
    }

    pub fn create_value_type(
        &self,
        session: &Session,
        branch: &str,
        spec: ValueTypeSpec,
    ) -> Result<String> {
        Self::require_builder(session)?;
        self.require_open_branch(branch)?;
        self.put_spec(
            "schema_value_types",
            branch,
            &spec.name,
            &serde_json::to_value(&spec)?,
        )?;
        Ok(spec.name)
    }

    pub fn create_function(
        &self,
        session: &Session,
        branch: &str,
        spec: FunctionSpec,
    ) -> Result<String> {
        Self::require_builder(session)?;
        self.require_open_branch(branch)?;
        self.put_spec(
            "schema_functions",
            branch,
            &spec.name,
            &serde_json::to_value(&spec)?,
        )?;
        Ok(spec.name)
    }

    pub fn create_object_type(
        &self,
        session: &Session,
        branch: &str,
        spec: ObjectTypeSpec,
    ) -> Result<String> {
        Self::require_builder(session)?;
        self.require_open_branch(branch)?;
        if KERNEL_TYPES.contains(&spec.name.as_str()) {
            return Err(OntoError::Invalid("kernel type name is reserved".into()));
        }
        self.put_spec(
            "schema_object_types",
            branch,
            &spec.name,
            &serde_json::to_value(&spec)?,
        )?;
        Ok(spec.name)
    }

    pub fn alter_object_type(
        &self,
        session: &Session,
        branch: &str,
        spec: ObjectTypeSpec,
    ) -> Result<String> {
        self.create_object_type(session, branch, spec)
    }

    pub fn add_property(
        &self,
        session: &Session,
        branch: &str,
        type_name: &str,
        prop: PropertySpec,
    ) -> Result<()> {
        Self::require_builder(session)?;
        self.require_open_branch(branch)?;
        let mut spec = self.load_object_type(branch, type_name)?;
        if spec.properties.iter().any(|p| p.name == prop.name) {
            return Err(OntoError::Conflict(format!(
                "property {} already exists",
                prop.name
            )));
        }
        spec.properties.push(prop);
        self.put_spec(
            "schema_object_types",
            branch,
            type_name,
            &serde_json::to_value(&spec)?,
        )?;
        Ok(())
    }

    pub fn alter_property(
        &self,
        session: &Session,
        branch: &str,
        type_name: &str,
        prop: PropertySpec,
    ) -> Result<()> {
        Self::require_builder(session)?;
        self.require_open_branch(branch)?;
        let mut spec = self.load_object_type(branch, type_name)?;
        match spec.properties.iter_mut().find(|p| p.name == prop.name) {
            Some(existing) => *existing = prop,
            None => spec.properties.push(prop),
        }
        self.put_spec(
            "schema_object_types",
            branch,
            type_name,
            &serde_json::to_value(&spec)?,
        )?;
        Ok(())
    }

    pub fn archive_object_type(
        &self,
        session: &Session,
        branch: &str,
        type_name: &str,
    ) -> Result<()> {
        Self::require_builder(session)?;
        self.require_open_branch(branch)?;
        self.store
            .delete_spec("schema_object_types", branch, type_name)
    }

    pub fn create_link_type(
        &self,
        session: &Session,
        branch: &str,
        spec: LinkTypeSpec,
    ) -> Result<String> {
        Self::require_builder(session)?;
        self.require_open_branch(branch)?;
        self.put_spec(
            "schema_link_types",
            branch,
            &spec.name,
            &serde_json::to_value(&spec)?,
        )?;
        Ok(spec.name)
    }

    pub fn alter_link_type(
        &self,
        session: &Session,
        branch: &str,
        spec: LinkTypeSpec,
    ) -> Result<String> {
        self.create_link_type(session, branch, spec)
    }

    pub fn create_interface(
        &self,
        session: &Session,
        branch: &str,
        spec: InterfaceSpec,
    ) -> Result<String> {
        Self::require_builder(session)?;
        self.require_open_branch(branch)?;
        self.put_spec(
            "schema_interfaces",
            branch,
            &spec.name,
            &serde_json::to_value(&spec)?,
        )?;
        Ok(spec.name)
    }

    pub fn attach_interface(
        &self,
        session: &Session,
        branch: &str,
        type_name: &str,
        interface: &str,
    ) -> Result<()> {
        Self::require_builder(session)?;
        self.require_open_branch(branch)?;
        match self.load_object_type(branch, type_name) {
            Ok(mut spec) => {
                if !spec.interfaces.iter().any(|i| i == interface) {
                    spec.interfaces.push(interface.to_string());
                }
                self.put_spec(
                    "schema_object_types",
                    branch,
                    type_name,
                    &serde_json::to_value(&spec)?,
                )
            }
            Err(OntoError::NotFound(_)) => {
                let mut spec = self.load_action_type(branch, type_name)?;
                if !spec.interfaces.iter().any(|i| i == interface) {
                    spec.interfaces.push(interface.to_string());
                }
                self.put_spec(
                    "schema_action_types",
                    branch,
                    type_name,
                    &serde_json::to_value(&spec)?,
                )
            }
            Err(e) => Err(e),
        }
    }

    pub fn create_action_type(
        &self,
        session: &Session,
        branch: &str,
        spec: ActionTypeSpec,
    ) -> Result<String> {
        Self::require_builder(session)?;
        self.require_open_branch(branch)?;
        guards::parse_guards(&spec.guards)?;
        let plan = effects::parse_plan(&spec.effects, &spec.parameters)?;
        for op in &plan {
            if let EffectOp::CountLinks { link, via, .. } = op {
                for name in [link.as_str(), via.as_str()] {
                    if self
                        .store
                        .load_spec("schema_link_types", branch, name)?
                        .is_none()
                    {
                        return Err(OntoError::Invalid(format!(
                            "count_links unknown link type {name}"
                        )));
                    }
                }
            }
        }
        self.put_spec(
            "schema_action_types",
            branch,
            &spec.name,
            &serde_json::to_value(&spec)?,
        )?;
        Ok(spec.name)
    }

    pub fn alter_action_type(
        &self,
        session: &Session,
        branch: &str,
        spec: ActionTypeSpec,
    ) -> Result<String> {
        self.create_action_type(session, branch, spec)
    }

    pub fn submit_proposal(&self, session: &Session, branch: &str) -> Result<String> {
        Self::require_builder(session)?;
        self.require_open_branch(branch)?;
        let id = new_id();
        self.store.set_branch_status(branch, "proposed")?;
        self.store
            .insert_proposal(&id, branch, &session.actor.id, self.now())?;
        Ok(id)
    }

    pub fn review_proposal(
        &self,
        session: &Session,
        proposal_id: &str,
        approve: bool,
    ) -> Result<()> {
        Self::require_builder(session)?;
        if !session.actor.has_role("reviewer") {
            return Err(OntoError::Denied("reviewer role required".into()));
        }
        let (branch, status) = self.store.proposal_branch_status(proposal_id)?;
        if status != "under_review" {
            return Err(OntoError::Conflict(format!("proposal is {status}")));
        }
        if approve {
            self.store
                .set_proposal(proposal_id, "approved", Some(&session.actor.id))?;
        } else {
            self.store
                .set_proposal(proposal_id, "rejected", Some(&session.actor.id))?;
            self.store.set_branch_status(&branch, "rejected")?;
        }
        Ok(())
    }

    pub fn merge_to_main(&self, session: &Session, proposal_id: &str) -> Result<()> {
        Self::require_builder(session)?;
        if !session.actor.has_role("reviewer") {
            return Err(OntoError::Denied("reviewer role required".into()));
        }
        let (branch, status) = self.store.proposal_branch_status(proposal_id)?;
        if status != "approved" {
            return Err(OntoError::Conflict(
                "proposal must be approved before merge".into(),
            ));
        }
        let expected = self
            .store
            .branch_base_revision(&branch)?
            .ok_or_else(|| OntoError::NotFound(format!("branch {branch}")))?;
        if expected.is_empty() {
            return Err(OntoError::Invalid(
                "branch has no pinned base revision".into(),
            ));
        }
        self.store.publish_main(
            &branch,
            &expected,
            proposal_id,
            &session.actor.id,
            self.now(),
            &new_id(),
            &json!({ "proposal": proposal_id, "branch": branch }).to_string(),
        )?;
        Ok(())
    }

    /// Cancel unpinned inbox rows and reject unpinned schema branches.
    ///
    /// Unmigrated rows stay fail-closed. Does not re-pin a stale branch onto
    /// current main.
    pub fn migrate_legacy(&self, session: &Session) -> Result<MigrateReport> {
        Self::require_builder(session)?;
        let mut report = MigrateReport::default();
        for id in self.store.list_empty_apply_inbox()? {
            self.store.set_inbox_status(&id, "cancelled")?;
            report.cancelled_inbox.push(id);
        }
        for name in self.store.list_unpinned_branches()? {
            self.store.set_branch_status(&name, "rejected")?;
            self.store.reject_proposals_on_branch(&name)?;
            report.rejected_branches.push(name);
        }
        self.audit(&session.actor.id, "migrate_legacy", &json!({}))?;
        Ok(report)
    }

    pub fn get_schema(&self, session: &Session, branch: Option<&str>) -> Result<SchemaSnapshot> {
        let branch = match session.actor.key {
            KeyKind::Builder => branch.unwrap_or(MAIN_BRANCH),
            KeyKind::Consumer => {
                if branch.is_some() && branch != Some(MAIN_BRANCH) {
                    return Err(OntoError::Denied(
                        "consumer key cannot read a working branch".into(),
                    ));
                }
                MAIN_BRANCH
            }
        };
        Ok(SchemaSnapshot {
            branch: branch.to_string(),
            value_types: self.load_all(branch, "schema_value_types")?,
            object_types: self.load_all(branch, "schema_object_types")?,
            link_types: self.load_all(branch, "schema_link_types")?,
            interfaces: self.load_all(branch, "schema_interfaces")?,
            action_types: self.load_all(branch, "schema_action_types")?,
            functions: self.load_all(branch, "schema_functions")?,
        })
    }

    fn load_all<T: for<'de> DeserializeOwned>(&self, branch: &str, table: &str) -> Result<Vec<T>> {
        self.store
            .load_specs(branch, table)?
            .iter()
            .map(|s| serde_json::from_str(s).map_err(Into::into))
            .collect()
    }

    fn load_named<T: for<'de> DeserializeOwned>(
        &self,
        table: &str,
        branch: &str,
        name: &str,
        missing: impl FnOnce() -> String,
    ) -> Result<T> {
        let spec = self
            .store
            .load_spec(table, branch, name)?
            .ok_or_else(|| OntoError::NotFound(missing()))?;
        Ok(serde_json::from_str(&spec)?)
    }

    fn load_object_type(&self, branch: &str, name: &str) -> Result<ObjectTypeSpec> {
        self.load_named("schema_object_types", branch, name, || {
            format!("object type {name} on {branch}")
        })
    }

    fn load_action_type(&self, branch: &str, name: &str) -> Result<ActionTypeSpec> {
        self.load_named("schema_action_types", branch, name, || {
            format!("action type {name}")
        })
    }

    fn load_function(&self, branch: &str, name: &str) -> Result<FunctionSpec> {
        self.load_named("schema_functions", branch, name, || {
            format!("function {name} on {branch}")
        })
    }

    fn load_interface(&self, branch: &str, name: &str) -> Result<InterfaceSpec> {
        self.load_named("schema_interfaces", branch, name, || {
            format!("interface {name} on {branch}")
        })
    }

    /// Pins functions whose derived output is present on objects actually read.
    fn function_version_for_reads(&self, reads: &[SnapshotObject]) -> String {
        let mut pins = Vec::new();
        for obj in reads {
            let Ok(tspec) = self.load_object_type(MAIN_BRANCH, &obj.type_name) else {
                continue;
            };
            for prop in &tspec.properties {
                if prop.source != PropertySource::Derived {
                    continue;
                }
                let Some(fn_name) = &prop.function else {
                    continue;
                };
                if !obj.properties.contains_key(&prop.name) {
                    continue;
                }
                if let Ok(fspec) = self.load_function(MAIN_BRANCH, fn_name) {
                    pins.push(fspec.pin());
                }
            }
        }
        functions::digest(&pins)
    }

    fn load_value_type(&self, branch: &str, name: &str) -> Result<Option<ValueTypeSpec>> {
        Ok(self
            .store
            .load_spec("schema_value_types", branch, name)?
            .map(|s| serde_json::from_str(&s))
            .transpose()?)
    }

    pub fn create_object_set(
        &self,
        session: &Session,
        branch: &str,
        spec: ObjectSetSpec,
    ) -> Result<String> {
        Self::require_builder(session)?;
        self.require_open_branch(branch)?;
        spec.validate()?;
        self.put_spec(
            OBJECT_SETS_TABLE,
            branch,
            &spec.name,
            &serde_json::to_value(&spec)?,
        )?;
        Ok(spec.name)
    }

    pub fn create_policy(
        &self,
        session: &Session,
        branch: &str,
        spec: PolicySpec,
    ) -> Result<String> {
        Self::require_builder(session)?;
        self.require_open_branch(branch)?;
        spec.validate()?;
        self.put_spec(
            POLICIES_TABLE,
            branch,
            &spec.name,
            &serde_json::to_value(&spec)?,
        )?;
        Ok(spec.name)
    }

    fn load_policies(&self) -> Result<Vec<PolicySpec>> {
        self.load_all(MAIN_BRANCH, POLICIES_TABLE)
    }

    fn authorize_read(
        &self,
        session: &Session,
        type_name: &str,
        instance_id: &str,
    ) -> Result<AuthzDecision> {
        Ok(authorize(
            &self.load_policies()?,
            session,
            AuthzOp::Read,
            Some(type_name),
            Some(instance_id),
            None,
        ))
    }

    fn require_write(&self, session: &Session, type_name: &str, instance_id: &str) -> Result<()> {
        self.require_write_property(session, type_name, instance_id, None)
    }

    fn require_write_property(
        &self,
        session: &Session,
        type_name: &str,
        instance_id: &str,
        property: Option<&str>,
    ) -> Result<()> {
        match authorize(
            &self.load_policies()?,
            session,
            AuthzOp::Write,
            Some(type_name),
            Some(instance_id),
            property,
        ) {
            AuthzDecision::Allow => Ok(()),
            AuthzDecision::Deny => Err(OntoError::Denied(format!(
                "write denied for {type_name}/{instance_id}{}",
                property.map_or_else(String::new, |p| format!(".{p}"))
            ))),
        }
    }

    #[allow(clippy::needless_pass_by_value)] // owned Query is the public search card
    pub fn search_objects(&self, session: &Session, query: Query) -> Result<Vec<ObjectView>> {
        self.search_object_set(session, ObjectSet::from_query(&query))
    }

    pub fn search_object_set(&self, session: &Session, set: ObjectSet) -> Result<Vec<ObjectView>> {
        Self::require_consumer(session)?;
        let resolved = self.resolve_object_set(set)?;
        let ids = self.candidate_ids(resolved.type_name.as_deref())?;
        evaluate_members(&resolved, ids, |id| self.load_permitted_view(session, id))
    }

    #[allow(clippy::needless_pass_by_value)] // owned ObjectSet is the public aggregate card
    pub fn aggregate_set(&self, session: &Session, set: ObjectSet) -> Result<Value> {
        Self::require_consumer(session)?;
        let resolved = self.resolve_object_set(set.unbounded())?;
        let ids = self.candidate_ids(resolved.type_name.as_deref())?;
        let members = evaluate_members(&resolved, ids, |id| self.load_permitted_view(session, id))?;
        Ok(json!({
            "type_name": resolved.type_name,
            "name": resolved.name,
            "count": i64::try_from(members.len()).unwrap_or(i64::MAX)
        }))
    }

    fn resolve_object_set(&self, set: ObjectSet) -> Result<ObjectSet> {
        let Some(name) = set.name.as_deref() else {
            return Ok(set);
        };
        let spec = self.load_object_set_spec(MAIN_BRANCH, name)?;
        Ok(ObjectSet::from_spec(&spec, set.limit))
    }

    fn load_object_set_spec(&self, branch: &str, name: &str) -> Result<ObjectSetSpec> {
        self.load_named(OBJECT_SETS_TABLE, branch, name, || {
            format!("object set {name} on {branch}")
        })
    }

    fn candidate_ids(&self, type_name: Option<&str>) -> Result<Vec<String>> {
        self.store.candidate_ids(type_name)
    }

    fn load_permitted_view(&self, session: &Session, id: &str) -> Result<ObjectView> {
        let view = self.load_object_view(id, AsOf::Current)?;
        match self.authorize_read(session, &view.type_name, &view.id)? {
            AuthzDecision::Allow => {
                let view = filter_view(&self.load_policies()?, session, view);
                Ok(apply_permission(session, view))
            }
            AuthzDecision::Deny => Err(OntoError::NotFound(format!("object {id}"))),
        }
    }

    /// Current version when `as_of` is [`AsOf::Current`]; otherwise the version
    /// whose valid span covers that time. Missing coverage is [`OntoError::NotFound`],
    /// not a silent current row. Property-level `Deny` hides the property.
    pub fn get_object(&self, session: &Session, id: &str, as_of: AsOf) -> Result<ObjectView> {
        Self::require_consumer(session)?;
        let view = self.load_object_view(id, as_of)?;
        match self.authorize_read(session, &view.type_name, &view.id)? {
            AuthzDecision::Allow => {
                let view = filter_view(&self.load_policies()?, session, view);
                Ok(apply_permission(session, view))
            }
            AuthzDecision::Deny => Err(OntoError::Denied(format!(
                "read denied for {}/{id}",
                view.type_name
            ))),
        }
    }

    /// Spans for one identity, oldest `valid_from` first.
    pub fn object_spans(&self, id: &str) -> Result<Vec<bitemporal::VersionSpan>> {
        self.store.list_spans(id)
    }

    fn load_object_view(&self, id: &str, as_of: AsOf) -> Result<ObjectView> {
        let loaded = self.store.load_version(id, as_of)?;
        let clock = match as_of {
            AsOf::Current | AsOf::Recorded(_) => self.now(),
            AsOf::Valid(t) => t,
        };
        let raw: BTreeMap<String, PropertyView> = serde_json::from_str(&loaded.properties)?;
        let spec = self.load_object_type(MAIN_BRANCH, &loaded.type_name).ok();
        let mut properties = raw;
        if let Some(spec) = &spec {
            for prop in &spec.properties {
                if prop.source != PropertySource::Derived {
                    continue;
                }
                let Some(fn_name) = prop.function.as_deref() else {
                    continue;
                };
                let Ok(fspec) = self.load_function(MAIN_BRANCH, fn_name) else {
                    continue;
                };
                if let Some(view) = functions::apply(&fspec, &properties, clock) {
                    properties.insert(prop.name.clone(), view);
                }
            }
        }
        let mut missing = Vec::new();
        let mut stale = Vec::new();
        if let Some(spec) = &spec {
            for prop in &spec.properties {
                if !properties.contains_key(&prop.name) && !prop.nullable {
                    missing.push(prop.name.clone());
                }
                if let Some(pv) = properties.get(&prop.name) {
                    if let (Some(budget), Some(stamp)) = (spec.freshness_budget_secs, &pv.as_of) {
                        if let Ok(ts) = stamp.parse::<i64>() {
                            if clock - ts > budget {
                                stale.push(prop.name.clone());
                            }
                        }
                    }
                }
            }
        }
        let mut view = ObjectView {
            id: id.to_string(),
            type_name: loaded.type_name,
            title: loaded.title,
            properties,
            missing,
            stale,
            version_id: loaded.version_id,
        };
        if let Some(spec) = spec {
            if kernel::attached(&spec.interfaces, KernelInterface::Evidenced) {
                if let Ok(iface) =
                    self.load_interface(MAIN_BRANCH, KernelInterface::Evidenced.as_str())
                {
                    for gap in kernel::evidence_gaps(&spec, &iface, &view, clock) {
                        match gap {
                            kernel::EvidenceGap::Missing(p) => {
                                if !view.missing.contains(&p) {
                                    view.missing.push(p);
                                }
                            }
                            kernel::EvidenceGap::Stale { property, .. } => {
                                if !view.stale.contains(&property) {
                                    view.stale.push(property);
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(view)
    }

    pub fn traverse_links(
        &self,
        session: &Session,
        from_id: &str,
        link_type: &str,
    ) -> Result<Vec<ObjectView>> {
        Self::require_consumer(session)?;
        let origin = self.load_object_view(from_id, AsOf::Current)?;
        match self.authorize_read(session, &origin.type_name, &origin.id)? {
            AuthzDecision::Allow => {}
            AuthzDecision::Deny => {
                return Err(OntoError::Denied(format!(
                    "read denied for {}/{from_id}",
                    origin.type_name
                )));
            }
        }
        let allow_cycles = self
            .get_schema(session, None)?
            .link_types
            .iter()
            .find(|l| l.name == link_type)
            .is_some_and(|l| l.allow_cycles);
        let targets = self.store.link_targets(from_id, link_type)?;
        let mut visited = HashSet::from([from_id.to_string()]);
        let mut out = Vec::new();
        for tid in targets {
            if !allow_cycles && !visited.insert(tid.clone()) {
                continue;
            }
            out.push(self.get_object(session, &tid, AsOf::Current)?);
        }
        Ok(out)
    }

    pub fn aggregate(&self, session: &Session, type_name: &str) -> Result<Value> {
        self.aggregate_set(
            session,
            ObjectSet::inline(
                Some(type_name.into()),
                ObjectSetFilter::default(),
                usize::MAX,
            ),
        )
    }

    pub fn list_missing_evidence(&self, session: &Session, id: &str) -> Result<Value> {
        let view = self.get_object(session, id, AsOf::Current)?;
        Ok(json!({
            "id": view.id,
            "missing": view.missing,
            "stale": view.stale
        }))
    }

    pub fn funnel_ingest(
        &self,
        session: &Session,
        records: Vec<IngestRecord>,
    ) -> Result<Vec<String>> {
        Self::require_consumer(session)?;
        let mut ids = Vec::new();
        for rec in records {
            let id = rec.id.clone().unwrap_or_else(new_id);
            self.require_write(session, &rec.type_name, &id)?;
            let mut rec = rec;
            rec.id = Some(id);
            ids.push(self.ingest_one(session, rec)?);
        }
        self.audit(
            &session.actor.id,
            "funnel_ingest",
            &json!({ "count": ids.len() }),
        )?;
        Ok(ids)
    }

    fn ingest_one(&self, session: &Session, rec: IngestRecord) -> Result<String> {
        let spec = self.load_object_type(MAIN_BRANCH, &rec.type_name)?;
        let as_of = rec.as_of.clone().unwrap_or_else(|| self.now().to_string());
        let id = rec.id.clone().unwrap_or_else(new_id);
        let existing = if self.store.identity_exists(&id)? {
            let actual = self.store.object_type_of(&id)?;
            if actual != rec.type_name {
                return Err(OntoError::Invalid(format!(
                    "funnel type {} does not match identity {id} ({actual})",
                    rec.type_name
                )));
            }
            Some(self.store.current_properties(&id)?)
        } else {
            None
        };
        let mut props: BTreeMap<String, PropertyView> = match existing.as_ref() {
            Some(s) => serde_json::from_str(s)?,
            None => BTreeMap::new(),
        };
        for (name, value) in rec.properties {
            let declared = crate::funnel::declared_property(&spec, &name)?;
            self.require_write_property(session, &rec.type_name, &id, Some(&name))?;
            if declared.source == PropertySource::ActionWritten {
                continue;
            }
            if let Some(cur) = props.get(&name) {
                if cur.source == PropertySource::ActionWritten {
                    continue;
                }
            }
            if declared.source == PropertySource::Derived {
                continue;
            }
            let Some(vt) = self.load_value_type(MAIN_BRANCH, &declared.value_type)? else {
                return Err(OntoError::Invalid(format!(
                    "unknown value type {}",
                    declared.value_type
                )));
            };
            Self::validate_loaded_value(&vt, &name, &value)?;
            props.insert(
                name,
                PropertyView {
                    value,
                    source: PropertySource::Mapped,
                    as_of: Some(as_of.clone()),
                    provenance: rec.provenance.clone(),
                },
            );
        }
        let title = spec.title_prop.as_ref().and_then(|t| {
            props
                .get(t)
                .and_then(|p| p.value.as_str().map(str::to_string))
        });
        let encoded = serde_json::to_string(&props)?;
        let valid_at = rec
            .as_of
            .as_ref()
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or_else(|| self.now());
        let tx_at = self.now();
        if existing.is_some() {
            self.store
                .append_version_recorded(&id, &encoded, title.as_deref(), valid_at, tx_at)?;
        } else {
            self.store.insert_object_recorded(
                &id,
                &rec.type_name,
                title.as_deref(),
                &encoded,
                valid_at,
                tx_at,
            )?;
        }
        Ok(id)
    }

    pub fn list_inbox(&self, session: &Session) -> Result<Vec<InboxItem>> {
        Self::require_consumer(session)?;
        tiers::require_syscall(session.actor.tier, "list_inbox")?;
        Ok(disclosure::visible_inbox(session, self.store.list_inbox()?))
    }

    pub fn describe_action(&self, session: &Session, name: &str) -> Result<ActionTypeSpec> {
        match session.actor.key {
            KeyKind::Consumer => {
                tiers::require_syscall(session.actor.tier, "describe_action")?;
                self.load_action_type(MAIN_BRANCH, name)
            }
            KeyKind::Builder => Err(OntoError::Denied(
                "builder key cannot read production actions as syscalls".into(),
            )),
        }
    }

    pub fn list_tools(&self, session: &Session) -> Result<Vec<ToolSpec>> {
        match session.actor.key {
            KeyKind::Builder => Ok(builder_tools()),
            KeyKind::Consumer => {
                let mut tools = consumer_base_tools();
                tools.retain(|t| session.actor.tier.allows_syscall(&t.name));
                let schema = self.get_schema(session, None)?;
                for action in schema.action_types {
                    if session.actor.tier >= action.required_tier
                        && session.actor.tier.allows_syscall("submit_action")
                    {
                        tools.push(Self::action_to_tool(&action, &schema.value_types));
                    }
                }
                Ok(tools)
            }
        }
    }

    #[allow(clippy::needless_pass_by_value)] // params is the public JSON card
    pub fn submit_action(
        &self,
        session: &Session,
        action_name: &str,
        params: Value,
    ) -> Result<ActionOutcome> {
        Self::require_consumer(session)?;
        tiers::require_syscall(session.actor.tier, "submit_action")?;
        if session.actor.tier == AgentTier::T4 {
            return Err(OntoError::Denied(
                "T4 cannot submit_action; use auto_action".into(),
            ));
        }
        let spec = self.load_action_type(MAIN_BRANCH, action_name)?;
        self.execute_action(session, &spec, &params, None)
    }

    /// Submit the named inverse Action for an `Allow` `DecisionRecord`.
    ///
    /// The original record is left in place. A new `DecisionRecord` is sealed
    /// through [`Self::execute_action`]. Missing compensation is
    /// [`OntoError::NoCompensation`], not a silent success.
    #[allow(clippy::needless_pass_by_value)] // overlay is the public param object
    pub fn compensate_action(
        &self,
        session: &Session,
        decision_record_id: &str,
        overlay: Value,
    ) -> Result<ActionOutcome> {
        Self::require_consumer(session)?;
        tiers::require_syscall(session.actor.tier, "compensate_action")?;
        let original = self.get_decision_record(session, decision_record_id)?;
        compensation::require_allow(&original)?;
        let original_spec = self.load_action_type(MAIN_BRANCH, &original.action_name)?;
        let Compensation::Inverse { action } = Compensation::from_spec(&original_spec)?;
        let spec = self.load_action_type(MAIN_BRANCH, &action)?;
        let params = compensation::inverse_params(&original, &overlay);
        self.execute_action(session, &spec, &params, None)
    }

    pub fn list_effect_intentions(
        &self,
        session: &Session,
        status: Option<EffectStatus>,
    ) -> Result<Vec<EffectIntention>> {
        Self::require_consumer(session)?;
        tiers::require_syscall(session.actor.tier, "list_effect_intentions")?;
        self.store.list_effect_intentions(status)
    }

    pub fn claim_effect(&self, session: &Session, decision_record_id: &str) -> Result<()> {
        Self::require_consumer(session)?;
        tiers::require_syscall(session.actor.tier, "claim_effect")?;
        self.store.claim_effect(decision_record_id)
    }

    pub fn ack_effect(&self, session: &Session, decision_record_id: &str) -> Result<()> {
        Self::require_consumer(session)?;
        tiers::require_syscall(session.actor.tier, "ack_effect")?;
        self.store.ack_effect(decision_record_id)
    }

    pub fn reconcile_effects(&self, session: &Session) -> Result<Vec<EffectIntention>> {
        Self::require_consumer(session)?;
        tiers::require_syscall(session.actor.tier, "reconcile_effects")?;
        self.store.reconcile_effects()
    }

    pub fn list_closed_links(&self, session: &Session) -> Result<Vec<ClosedLink>> {
        Self::require_consumer(session)?;
        self.store.list_closed_links()
    }

    pub fn confirm_action(&self, session: &Session, inbox_id: &str) -> Result<ActionOutcome> {
        Self::require_consumer(session)?;
        tiers::require_syscall(session.actor.tier, "confirm_action")?;
        let row = self.store.load_inbox(inbox_id)?;
        tiers::require_distinct_confirmer(&session.actor.id, &row.proposed_by)?;
        match row.status.as_str() {
            "shadow" => {
                return Err(OntoError::Denied(
                    "shadow output cannot enter real confirmation".into(),
                ))
            }
            "confirmed" => {
                let params: Value = serde_json::from_str(&row.params)?;
                let apply = apply_action_name_from_row(&row);
                let spec = self.load_action_type(MAIN_BRANCH, &apply)?;
                if let Err(auth) = self.check_param_and_permission(session, &spec, &params)? {
                    return Err(OntoError::Denied(auth.reason));
                }
                let key =
                    resolve_idempotency_key(&apply, &session.actor.id, &params, Some(inbox_id));
                if let Some(cached) = self.load_cached_outcome(&key, &payload_digest(&params))? {
                    return Ok(cached);
                }
                return Err(OntoError::Conflict("inbox item is confirmed".into()));
            }
            "pending" => {}
            other => return Err(OntoError::Conflict(format!("inbox item is {other}"))),
        }
        let params: Value = serde_json::from_str(&row.params)?;
        if row.apply_action.is_empty() {
            return Err(OntoError::Invalid(
                "inbox has no pinned apply action".into(),
            ));
        }
        let spec = self.load_action_type(MAIN_BRANCH, &row.apply_action)?;
        match spec.mode {
            ExecutionMode::Shadow => {
                return Err(OntoError::Denied(
                    "shadow action cannot enter real confirmation".into(),
                ))
            }
            ExecutionMode::Propose | ExecutionMode::Auto | ExecutionMode::Approve => {}
        }
        if !row.rule_pin.is_empty() {
            let schema = self.store.schema_revision(MAIN_BRANCH)?;
            if crate::pin::apply_pin(&row.apply_action, &spec, &schema) != row.rule_pin {
                return Err(OntoError::Conflict(
                    "prepared proposal effects no longer match the pinned plan".into(),
                ));
            }
        }
        self.execute_action(session, &spec, &params, Some(inbox_id))
    }

    pub fn override_action(
        &self,
        session: &Session,
        inbox_id: &str,
        category: &str,
        reason: &str,
    ) -> Result<ActionOutcome> {
        Self::require_consumer(session)?;
        tiers::require_syscall(session.actor.tier, "override_action")?;
        if !session.actor.has_role("supervisor") {
            return Err(OntoError::Denied("override requires supervisor".into()));
        }
        let (params, override_name) = {
            let row = self.store.load_inbox(inbox_id)?;
            if row.status != "pending" {
                return Err(OntoError::Conflict(format!("inbox item is {}", row.status)));
            }
            tiers::require_distinct_confirmer(&session.actor.id, &row.proposed_by)?;
            let source = self.load_action_type(MAIN_BRANCH, &row.action_name)?;
            let override_name = source
                .side_effects
                .get("override")
                .and_then(Value::as_str)
                .ok_or_else(|| OntoError::Invalid("source action has no override".into()))?
                .to_string();
            let mut p: Value = serde_json::from_str(&row.params)?;
            if let Value::Object(map) = &mut p {
                map.insert("override_category".into(), json!(category));
                map.insert("override_reason".into(), json!(reason));
                map.insert("source_action".into(), json!(row.action_name));
                map.insert("proposed_by".into(), json!(row.proposed_by));
            }
            (p, override_name)
        };
        let spec = self.load_action_type(MAIN_BRANCH, &override_name)?;
        self.execute_action(session, &spec, &params, Some(inbox_id))
    }

    /// T4 unsupervised auto. Bound starts empty, so every claim is denied.
    #[allow(clippy::needless_pass_by_value)] // params is the public JSON card
    pub fn auto_action(
        &self,
        session: &Session,
        action_name: &str,
        object_set: &str,
        risk_band: RiskBand,
        params: Value,
    ) -> Result<ActionOutcome> {
        Self::require_consumer(session)?;
        tiers::require_syscall(session.actor.tier, "auto_action")?;
        tiers::require_auto(
            session.actor.tier,
            &AutoBound::empty(),
            action_name,
            object_set,
            risk_band,
        )?;
        let spec = self.load_action_type(MAIN_BRANCH, action_name)?;
        self.execute_action(session, &spec, &params, None)
    }

    pub fn get_decision_record(&self, session: &Session, id: &str) -> Result<DecisionRecordView> {
        Self::require_consumer(session)?;
        let mut rec = self.store.load_decision(id)?;
        let grants = self.load_policies()?;
        disclosure::redact_record(&grants, session, &mut rec);
        Ok(rec)
    }

    pub fn get_rejection(&self, session: &Session, decision_id: &str) -> Result<Value> {
        let rec = self.get_decision_record(session, decision_id)?;
        Ok(json!({
            "id": rec.id,
            "verdict": rec.verdict,
            "action": rec.action_name,
            "guards": rec.guard_results
        }))
    }

    fn execute_action(
        &self,
        session: &Session,
        spec: &ActionTypeSpec,
        params: &Value,
        confirmer_inbox: Option<&str>,
    ) -> Result<ActionOutcome> {
        let mut last = None;
        for _ in 0..8 {
            match self.execute_action_once(session, spec, params, confirmer_inbox) {
                Err(OntoError::StaleRead) => {
                    last = Some(OntoError::StaleRead);
                    continue;
                }
                other => return other,
            }
        }
        Err(last.unwrap_or(OntoError::Conflict("command retry exhausted".into())))
    }

    fn execute_action_once(
        &self,
        session: &Session,
        spec: &ActionTypeSpec,
        params: &Value,
        confirmer_inbox: Option<&str>,
    ) -> Result<ActionOutcome> {
        match spec.mode {
            ExecutionMode::Approve if confirmer_inbox.is_none() => {
                return Err(OntoError::Denied(format!(
                    "action {} requires a pending proposal",
                    spec.name
                )));
            }
            ExecutionMode::Propose
            | ExecutionMode::Auto
            | ExecutionMode::Shadow
            | ExecutionMode::Approve => {}
        }
        let key = resolve_idempotency_key(&spec.name, &session.actor.id, params, confirmer_inbox);
        let digest = payload_digest(params);
        let path = WritePath::begin(key.clone());
        match self.check_param_and_permission(session, spec, params) {
            Err(e) => Err(e),
            Ok(Err(auth)) => {
                let path = path.param_and_permission(Vec::new()).abort_seal(
                    vec![GuardResult {
                        name: auth.name,
                        verdict: Verdict::Deny,
                        reason: auth.reason.clone(),
                    }],
                    Verdict::Deny,
                );
                self.finish_abort(session, spec, params, &path, auth.reason, None, &digest)
            }
            Ok(Ok(param_reads)) => {
                if let Some(cached) = self.load_cached_outcome(&key, &digest)? {
                    return Ok(cached);
                }
                let path = path.param_and_permission(param_reads);
                let (mut guards, guard_reads) = self.evaluate_guards(&spec.guards, params)?;
                guards.extend(self.evidenced_guards(spec, params)?);
                let worst = worst_verdict(&guards);
                let path = path.submission_criteria(guards, guard_reads, worst);
                match worst {
                    Verdict::Deny | Verdict::Review => {
                        let reason = path.guards.iter().find(|g| g.verdict == worst).map_or_else(
                            || match worst {
                                Verdict::Deny => "denied".into(),
                                Verdict::Review => "needs review".into(),
                                Verdict::Allow => unreachable!("matched deny/review"),
                            },
                            |g| g.reason.clone(),
                        );
                        let alternative = spec.on_review.clone();
                        let path = path.discard_stage();
                        self.finish_abort(
                            session,
                            spec,
                            params,
                            &path,
                            reason,
                            alternative,
                            &digest,
                        )
                    }
                    Verdict::Allow => {
                        self.finish_allow(session, spec, params, confirmer_inbox, path, &digest)
                    }
                }
            }
        }
    }

    fn check_param_and_permission(
        &self,
        session: &Session,
        spec: &ActionTypeSpec,
        params: &Value,
    ) -> Result<std::result::Result<Vec<SnapshotObject>, AuthFail>> {
        if session.actor.tier < spec.required_tier {
            return Ok(Err(AuthFail {
                name: "authorization".into(),
                reason: format!(
                    "tier {} required, actor is {}",
                    spec.required_tier, session.actor.tier
                ),
            }));
        }
        for role in &spec.required_roles {
            if !session.actor.has_role(role) {
                return Ok(Err(AuthFail {
                    name: "authorization".into(),
                    reason: format!("role {role} required"),
                }));
            }
        }
        self.validate_params(spec, params)?;
        match self.authorize_effect_writes(session, spec, params) {
            Ok(()) => {}
            Err(OntoError::Denied(reason)) => {
                return Ok(Err(AuthFail {
                    name: "authorization".into(),
                    reason,
                }));
            }
            Err(e) => return Err(e),
        }
        let mut reads = Vec::new();
        for p in &spec.parameters {
            if p.object_type.is_some() {
                if let Some(id) = params.get(&p.name).and_then(Value::as_str) {
                    reads.push(self.snapshot_object(id)?);
                }
            }
        }
        Ok(Ok(reads))
    }

    #[allow(clippy::too_many_arguments)] // digest travels with the abort seal
    fn finish_abort(
        &self,
        session: &Session,
        spec: &ActionTypeSpec,
        params: &Value,
        path: &WritePath<crate::write_path::SealDecisionRecord>,
        reason: String,
        alternative: Option<String>,
        digest: &str,
    ) -> Result<ActionOutcome> {
        let snapshot = path.data_snapshot(
            rule_version(spec),
            self.function_version_for_reads(&path.reads),
            ENGINE_VERSION.into(),
        );
        let confirmer = None;
        let effects = match path.verdict {
            Verdict::Review => json!({ "alternative": spec.on_review }),
            Verdict::Deny | Verdict::Allow => json!({}),
        };
        let rec = new_id();
        let outcome = ActionOutcome {
            verdict: path.verdict,
            reason,
            decision_record_id: Some(rec.clone()),
            inbox_id: None,
            created_ids: vec![],
            alternative,
            guard_results: path.guards.clone(),
        };
        let outcome_json = serde_json::to_string(&outcome)?;
        let read_set = read_set_of(&path.reads);
        let schema = self.store.schema_revision(MAIN_BRANCH)?;
        match self.store.commit_decision(&DecisionCommit {
            ops: &[],
            decision: DecisionWrite {
                id: &rec,
                action: &spec.name,
                actor: &session.actor.id,
                confirmer,
                verdict: path.verdict,
                params,
                guards: &path.guards,
                effects: &effects,
                snapshot: &snapshot,
                trace: &path.trace,
                at: self.now(),
            },
            idempotency_key: Some(&path.idempotency_key),
            idempotency_outcome: Some(&outcome_json),
            payload_digest: Some(digest),
            read_set: &read_set,
            schema_revision: Some(&schema),
            audit_id: None,
            audit_actor: None,
            audit_kind: None,
            audit_payload: None,
            effect_declaration: None,
        })? {
            DecisionApply::Written(_) => Ok(outcome),
            DecisionApply::Replayed(raw) => Ok(serde_json::from_str(&raw)?),
        }
    }

    #[allow(clippy::too_many_lines)] // cardinality Deny and decision commit stay one path
    fn finish_allow(
        &self,
        session: &Session,
        spec: &ActionTypeSpec,
        params: &Value,
        confirmer_inbox: Option<&str>,
        path: WritePath<crate::write_path::SubmissionCriteria>,
        digest: &str,
    ) -> Result<ActionOutcome> {
        let (staged, inbox_id) = match self.build_stage(session, spec, params, confirmer_inbox) {
            Ok(v) => v,
            Err(OntoError::Denied(reason)) => {
                let path = path.deny_stage(GuardResult {
                    name: "cardinality".into(),
                    verdict: Verdict::Deny,
                    reason: reason.clone(),
                });
                return self.finish_abort(session, spec, params, &path, reason, None, digest);
            }
            Err(e) => return Err(e),
        };
        let path = path.stage(staged);
        let created: Vec<String> = path
            .staged
            .iter()
            .filter_map(|op| match op {
                StagedOp::InsertObject { id, .. } => Some(id.clone()),
                StagedOp::UpdateObject { .. }
                | StagedOp::InsertLink { .. }
                | StagedOp::InsertInbox { .. }
                | StagedOp::ConfirmInbox { .. }
                | StagedOp::CloseLink { .. } => None,
            })
            .collect();
        let path = path.commit(created.clone());
        let path = path.seal();

        let declaration = json!({
            "idempotency_key": path.idempotency_key,
            "declared": spec.side_effects,
        });
        let path = path.declare(declaration.clone());

        let snapshot = path.data_snapshot(
            rule_version(spec),
            self.function_version_for_reads(&path.reads),
            ENGINE_VERSION.into(),
        );
        let confirmer = match spec.mode {
            ExecutionMode::Auto | ExecutionMode::Approve => Some(session.actor.id.as_str()),
            ExecutionMode::Propose | ExecutionMode::Shadow => None,
        };
        let mut effects = json!({
            "created": created,
            "side_effects": spec.side_effects,
            "idempotency_key": path.idempotency_key,
        });
        if let Some(iid) = &inbox_id {
            effects["inbox"] = json!(iid);
            effects["mode"] = json!(spec.mode);
        }
        let rec = new_id();
        let reason = match spec.mode {
            ExecutionMode::Propose | ExecutionMode::Shadow => "proposed",
            ExecutionMode::Auto | ExecutionMode::Approve => "committed",
        };
        let outcome = ActionOutcome {
            verdict: Verdict::Allow,
            reason: reason.into(),
            decision_record_id: Some(rec.clone()),
            inbox_id,
            created_ids: created.clone(),
            alternative: None,
            guard_results: path.guards.clone(),
        };
        let audit_id = new_id();
        let audit_payload = json!({
            "idempotency_key": path.idempotency_key,
            "action": spec.name,
            "declaration": spec.side_effects,
            "decision_record_id": rec,
            "effect_status": "declared",
        })
        .to_string();
        let outcome_json = serde_json::to_string(&outcome)?;
        let declaration = declaration.to_string();
        let read_set = read_set_of(&path.reads);
        let schema = self.store.schema_revision(MAIN_BRANCH)?;
        let applied = self.store.commit_decision(&DecisionCommit {
            ops: &path.staged,
            decision: DecisionWrite {
                id: &rec,
                action: &spec.name,
                actor: &session.actor.id,
                confirmer,
                verdict: Verdict::Allow,
                params,
                guards: &path.guards,
                effects: &effects,
                snapshot: &snapshot,
                trace: &path.trace,
                at: self.now(),
            },
            idempotency_key: Some(&path.idempotency_key),
            idempotency_outcome: Some(&outcome_json),
            payload_digest: Some(digest),
            read_set: &read_set,
            schema_revision: Some(&schema),
            audit_id: Some(&audit_id),
            audit_actor: Some(session.actor.id.as_str()),
            audit_kind: Some("side_effect"),
            audit_payload: Some(&audit_payload),
            effect_declaration: Some(&declaration),
        })?;
        let _: WritePath<crate::write_path::DeclareSideEffects> = path;
        match applied {
            DecisionApply::Written(_) => Ok(outcome),
            DecisionApply::Replayed(raw) => Ok(serde_json::from_str(&raw)?),
        }
    }

    fn build_stage(
        &self,
        session: &Session,
        spec: &ActionTypeSpec,
        params: &Value,
        confirmer_inbox: Option<&str>,
    ) -> Result<(Vec<StagedOp>, Option<String>)> {
        match spec.mode {
            ExecutionMode::Propose => {
                if !kernel::require_inbox(spec) {
                    return Ok((Vec::new(), None));
                }
                let inbox_id = new_id();
                let apply_action = apply_action_name(&spec.name);
                let rule_pin = self.pin_apply_action(&apply_action, spec)?;
                Ok((
                    vec![StagedOp::InsertInbox {
                        id: inbox_id.clone(),
                        action_name: spec.name.clone(),
                        proposed_by: session.actor.id.clone(),
                        params: params.to_string(),
                        created_at: self.now(),
                        status: "pending".into(),
                        rule_pin,
                        apply_action,
                    }],
                    Some(inbox_id),
                ))
            }
            ExecutionMode::Shadow => {
                let inbox_id = new_id();
                Ok((
                    vec![StagedOp::InsertInbox {
                        id: inbox_id.clone(),
                        action_name: spec.name.clone(),
                        proposed_by: session.actor.id.clone(),
                        params: params.to_string(),
                        created_at: self.now(),
                        status: "shadow".into(),
                        rule_pin: rule_version(spec),
                        apply_action: spec.name.clone(),
                    }],
                    Some(inbox_id),
                ))
            }
            ExecutionMode::Auto | ExecutionMode::Approve => {
                let mut ops = self.stage_effects(spec, params, &session.actor.id)?;
                let inbox_id = confirmer_inbox.map(str::to_string);
                if let Some(id) = &inbox_id {
                    ops.push(StagedOp::ConfirmInbox { id: id.clone() });
                }
                Ok((ops, inbox_id))
            }
        }
    }

    fn evidenced_guards(&self, spec: &ActionTypeSpec, params: &Value) -> Result<Vec<GuardResult>> {
        let mut out = Vec::new();
        for p in &spec.parameters {
            let Some(ot) = &p.object_type else {
                continue;
            };
            let type_spec = match self.load_object_type(MAIN_BRANCH, ot) {
                Ok(s) => s,
                Err(OntoError::NotFound(_)) => continue,
                Err(e) => return Err(e),
            };
            if !kernel::attached(&type_spec.interfaces, KernelInterface::Evidenced) {
                continue;
            }
            let iface = self.load_interface(MAIN_BRANCH, KernelInterface::Evidenced.as_str())?;
            let Some(id) = params.get(&p.name).and_then(Value::as_str) else {
                continue;
            };
            let view = self.load_object_view(id, AsOf::Current)?;
            if let Some(g) = kernel::evidenced_guard(&type_spec, &iface, &view, self.now()) {
                out.push(g);
            }
        }
        Ok(out)
    }

    fn snapshot_object(&self, id: &str) -> Result<SnapshotObject> {
        let view = self.load_object_view(id, AsOf::Current)?;
        Ok(snapshot_from_view(&view))
    }

    fn validate_params(&self, spec: &ActionTypeSpec, params: &Value) -> Result<()> {
        let obj = params
            .as_object()
            .ok_or_else(|| OntoError::Invalid("params must be an object".into()))?;
        for p in &spec.parameters {
            let Some(val) = obj.get(&p.name) else {
                if p.required {
                    return Err(OntoError::Invalid(format!("missing param {}", p.name)));
                }
                continue;
            };
            let Some(vt) = self.load_value_type(MAIN_BRANCH, &p.value_type)? else {
                return Err(OntoError::Invalid(format!(
                    "unknown value type {}",
                    p.value_type
                )));
            };
            Self::validate_loaded_value(&vt, &p.name, val)?;
            if let Some(ot) = &p.object_type {
                let id = val.as_str().ok_or_else(|| {
                    OntoError::Invalid(format!("{} must be an object id", p.name))
                })?;
                let view = self.load_object_view(id, AsOf::Current)?;
                if view.type_name != *ot {
                    return Err(OntoError::Invalid(format!(
                        "{} must be {ot}, got {}",
                        p.name, view.type_name
                    )));
                }
            }
        }
        Ok(())
    }

    fn validate_loaded_value(vt: &ValueTypeSpec, field: &str, val: &Value) -> Result<()> {
        match vt.base.as_str() {
            "number" => {
                let n = val
                    .as_f64()
                    .ok_or_else(|| OntoError::Invalid(format!("{field} must be a number")))?;
                if let Some(min) = vt.min {
                    if n < min {
                        return Err(OntoError::Invalid(format!("{field} below {min}")));
                    }
                }
                if let Some(max) = vt.max {
                    if n > max {
                        return Err(OntoError::Invalid(format!("{field} above {max}")));
                    }
                }
            }
            "string" => {
                if !val.is_string() {
                    return Err(OntoError::Invalid(format!("{field} must be a string")));
                }
            }
            "boolean" => {
                if !val.is_boolean() {
                    return Err(OntoError::Invalid(format!("{field} must be a boolean")));
                }
            }
            other => {
                return Err(OntoError::Invalid(format!(
                    "unsupported value type base {other}"
                )));
            }
        }
        Ok(())
    }

    fn evaluate_guards(
        &self,
        guards: &Value,
        params: &Value,
    ) -> Result<(Vec<GuardResult>, Vec<SnapshotObject>)> {
        let parsed = guards::parse_guards(guards)?;
        guards::evaluate(
            &parsed,
            params,
            self.now(),
            |id| self.load_object_view(id, AsOf::Current),
            |link, from, to| {
                Ok(self
                    .store
                    .link_targets(from, link)?
                    .iter()
                    .any(|id| id == to))
            },
        )
    }

    #[allow(clippy::too_many_lines)] // typed plan apply stays one write-set builder
    fn stage_effects(
        &self,
        spec: &ActionTypeSpec,
        params: &Value,
        actor: &str,
    ) -> Result<Vec<StagedOp>> {
        let plan = effects::parse_plan(&spec.effects, &spec.parameters)?;
        let mut staged = Vec::new();
        let mut created_ids = Vec::new();
        let mut created_types: BTreeMap<String, String> = BTreeMap::new();
        let mut pending: BTreeMap<String, BTreeMap<String, PropertyView>> = BTreeMap::new();
        let mut counts = Vec::new();
        for op in plan {
            match op {
                EffectOp::Create {
                    type_name,
                    properties,
                } => {
                    if self.load_object_type(MAIN_BRANCH, &type_name).is_err() {
                        return Err(OntoError::Invalid(format!(
                            "cannot create unknown type {type_name}"
                        )));
                    }
                    let mut props = BTreeMap::new();
                    for (k, v) in properties {
                        props.insert(
                            k,
                            PropertyView {
                                value: resolve_value(&v, params),
                                source: PropertySource::ActionWritten,
                                as_of: Some(self.now().to_string()),
                                provenance: Some(format!("actor:{actor}")),
                            },
                        );
                    }
                    let id = new_id();
                    let title = props
                        .get("title")
                        .or_else(|| props.get("name"))
                        .and_then(|p| p.value.as_str().map(str::to_string));
                    pending.insert(id.clone(), props.clone());
                    staged.push(StagedOp::InsertObject {
                        id: id.clone(),
                        type_name: type_name.clone(),
                        title,
                        properties: serde_json::to_string(&props)?,
                        created_at: self.now(),
                    });
                    created_ids.push(id.clone());
                    created_types.insert(id, type_name);
                }
                EffectOp::Update { target, properties } => {
                    let id = param_str(params, &target)?;
                    let fields = properties
                        .into_iter()
                        .map(|(k, v)| (k, resolve_value(&v, params)))
                        .collect();
                    self.stage_property_map(&mut staged, &mut pending, &id, fields, actor)?;
                }
                EffectOp::Link { link, from, to } => {
                    let link = match resolve_value(&link, params) {
                        Value::String(s) if !s.is_empty() => s,
                        _ => {
                            return Err(OntoError::Invalid("link effect needs a link type".into()));
                        }
                    };
                    let from = param_str(params, &from)?;
                    let to = if let Some(to_param) = to {
                        param_str(params, &to_param)?
                    } else if let Some(last) = created_ids.last() {
                        last.clone()
                    } else {
                        return Err(OntoError::Invalid("link effect needs to".into()));
                    };
                    self.validate_link(&link, &from, &to, &created_types)?;
                    self.require_cardinality(&staged, &link, &from, &to)?;
                    staged.push(StagedOp::InsertLink {
                        id: new_id(),
                        type_name: link,
                        from_id: from,
                        to_id: to,
                    });
                }
                EffectOp::CloseLink { link, from, to } => {
                    let type_name = match resolve_value(&link, params) {
                        Value::String(s) if !s.is_empty() => s,
                        _ => {
                            return Err(OntoError::Invalid(
                                "close_link effect needs a link type".into(),
                            ));
                        }
                    };
                    staged.push(StagedOp::CloseLink {
                        type_name,
                        from_id: param_str(params, &from)?,
                        to_id: param_str(params, &to)?,
                    });
                }
                EffectOp::CountLinks {
                    link,
                    update,
                    property,
                    via,
                } => counts.push((link, update, property, via)),
            }
        }
        for (link, update, property, via) in counts {
            let id = param_str(params, &update)?;
            let seats = self.seats_via(&staged, &via, &id)?;
            let mut n: i64 = 0;
            for seat in seats {
                let add = i64::try_from(self.occupant_count(&staged, &link, &seat)?)
                    .map_err(|_| OntoError::Invalid("link count overflow".into()))?;
                n = n.saturating_add(add);
            }
            let mut fields = BTreeMap::new();
            fields.insert(property, json!(n));
            self.stage_property_map(&mut staged, &mut pending, &id, fields, actor)?;
        }
        Ok(staged)
    }

    fn stage_property_map(
        &self,
        staged: &mut Vec<StagedOp>,
        pending: &mut BTreeMap<String, BTreeMap<String, PropertyView>>,
        id: &str,
        fields: BTreeMap<String, Value>,
        actor: &str,
    ) -> Result<()> {
        let mut view_props = if let Some(existing) = pending.get(id) {
            existing.clone()
        } else {
            let raw = self.store.current_properties(id)?;
            serde_json::from_str::<BTreeMap<String, PropertyView>>(&raw)?
        };
        for (k, value) in fields {
            view_props.insert(
                k,
                PropertyView {
                    value,
                    source: PropertySource::ActionWritten,
                    as_of: Some(self.now().to_string()),
                    provenance: Some(format!("actor:{actor}")),
                },
            );
        }
        pending.insert(id.to_string(), view_props.clone());
        staged.retain(|op| match op {
            StagedOp::UpdateObject { id: sid, .. } => sid != id,
            StagedOp::InsertObject { .. }
            | StagedOp::InsertLink { .. }
            | StagedOp::InsertInbox { .. }
            | StagedOp::ConfirmInbox { .. }
            | StagedOp::CloseLink { .. } => true,
        });
        staged.push(StagedOp::UpdateObject {
            id: id.to_string(),
            properties: serde_json::to_string(&view_props)?,
        });
        Ok(())
    }

    fn require_cardinality(
        &self,
        staged: &[StagedOp],
        link: &str,
        from: &str,
        to: &str,
    ) -> Result<()> {
        let spec: LinkTypeSpec = self.load_named("schema_link_types", MAIN_BRANCH, link, || {
            format!("link type {link}")
        })?;
        let card = Cardinality::parse(&spec.cardinality);
        if effects::breaks_cardinality(
            card,
            self.outgoing_taken(staged, link, from)?,
            self.incoming_taken(staged, link, to)?,
        ) {
            return Err(OntoError::Denied(format!("link {link} breaks cardinality")));
        }
        Ok(())
    }

    fn outgoing_taken(&self, staged: &[StagedOp], link: &str, from: &str) -> Result<bool> {
        let live = !self.store.link_targets(from, link)?.is_empty();
        let closed = staged.iter().any(|op| {
            matches!(
                op,
                StagedOp::CloseLink {
                    type_name,
                    from_id,
                    ..
                } if type_name == link && from_id == from
            )
        });
        let added = staged.iter().any(|op| {
            matches!(
                op,
                StagedOp::InsertLink {
                    type_name,
                    from_id,
                    ..
                } if type_name == link && from_id == from
            )
        });
        Ok((live && !closed) || added)
    }

    fn incoming_taken(&self, staged: &[StagedOp], link: &str, to: &str) -> Result<bool> {
        let live = !self.store.link_sources(to, link)?.is_empty();
        let closed = staged.iter().any(|op| {
            matches!(
                op,
                StagedOp::CloseLink {
                    type_name,
                    to_id,
                    ..
                } if type_name == link && to_id == to
            )
        });
        let added = staged.iter().any(|op| {
            matches!(
                op,
                StagedOp::InsertLink {
                    type_name,
                    to_id,
                    ..
                } if type_name == link && to_id == to
            )
        });
        Ok((live && !closed) || added)
    }

    fn seats_via(&self, staged: &[StagedOp], via: &str, from: &str) -> Result<Vec<String>> {
        let mut seats = self.store.link_targets(from, via)?;
        for op in staged {
            match op {
                StagedOp::InsertLink {
                    type_name,
                    from_id,
                    to_id,
                    ..
                } if type_name == via && from_id == from => {
                    if !seats.contains(to_id) {
                        seats.push(to_id.clone());
                    }
                }
                StagedOp::CloseLink {
                    type_name,
                    from_id,
                    to_id,
                } if type_name == via && from_id == from => {
                    seats.retain(|s| s != to_id);
                }
                StagedOp::InsertObject { .. }
                | StagedOp::UpdateObject { .. }
                | StagedOp::InsertLink { .. }
                | StagedOp::InsertInbox { .. }
                | StagedOp::ConfirmInbox { .. }
                | StagedOp::CloseLink { .. } => {}
            }
        }
        Ok(seats)
    }

    fn occupant_count(&self, staged: &[StagedOp], link: &str, seat: &str) -> Result<usize> {
        let mut occupants = self.store.link_sources(seat, link)?;
        for op in staged {
            match op {
                StagedOp::InsertLink {
                    type_name,
                    from_id,
                    to_id,
                    ..
                } if type_name == link && to_id == seat => {
                    if !occupants.contains(from_id) {
                        occupants.push(from_id.clone());
                    }
                }
                StagedOp::CloseLink {
                    type_name,
                    from_id,
                    to_id,
                } if type_name == link && to_id == seat => {
                    occupants.retain(|s| s != from_id);
                }
                StagedOp::InsertObject { .. }
                | StagedOp::UpdateObject { .. }
                | StagedOp::InsertLink { .. }
                | StagedOp::InsertInbox { .. }
                | StagedOp::ConfirmInbox { .. }
                | StagedOp::CloseLink { .. } => {}
            }
        }
        Ok(occupants.len())
    }

    fn load_cached_outcome(&self, key: &str, digest: &str) -> Result<Option<ActionOutcome>> {
        let Some(row) = self.store.get_idempotency(key)? else {
            return Ok(None);
        };
        if !row.payload_digest.is_empty() && row.payload_digest != digest {
            return Err(OntoError::Conflict(
                "idempotency key is bound to a different payload".into(),
            ));
        }
        Ok(Some(serde_json::from_str(&row.outcome)?))
    }

    fn pin_apply_action(&self, apply_name: &str, propose: &ActionTypeSpec) -> Result<String> {
        let schema = self.store.schema_revision(MAIN_BRANCH)?;
        Ok(self.load_action_type(MAIN_BRANCH, apply_name).map_or_else(
            |_| crate::pin::apply_pin(apply_name, propose, &schema),
            |spec| crate::pin::apply_pin(apply_name, &spec, &schema),
        ))
    }

    fn authorize_effect_writes(
        &self,
        session: &Session,
        spec: &ActionTypeSpec,
        params: &Value,
    ) -> Result<()> {
        let plan = effects::parse_plan(&spec.effects, &spec.parameters)?;
        for op in plan {
            match op {
                EffectOp::Update { target, properties } => {
                    let id = param_str(params, &target)?;
                    let type_name = self.store.object_type_of(&id)?;
                    self.require_write(session, &type_name, &id)?;
                    for name in properties.keys() {
                        self.require_write_property(session, &type_name, &id, Some(name))?;
                    }
                }
                EffectOp::Create { type_name, .. } => {
                    self.require_write(session, &type_name, "*")?;
                }
                EffectOp::Link { from, to, .. } => {
                    self.authorize_link_end(session, params, &from)?;
                    if let Some(to) = to {
                        self.authorize_link_end(session, params, &to)?;
                    }
                }
                EffectOp::CloseLink { from, to, .. } => {
                    self.authorize_link_end(session, params, &from)?;
                    self.authorize_link_end(session, params, &to)?;
                }
                EffectOp::CountLinks {
                    update, property, ..
                } => {
                    let id = param_str(params, &update)?;
                    let type_name = self.store.object_type_of(&id)?;
                    self.require_write(session, &type_name, &id)?;
                    self.require_write_property(session, &type_name, &id, Some(&property))?;
                }
            }
        }
        Ok(())
    }

    fn authorize_link_end(&self, session: &Session, params: &Value, name: &str) -> Result<()> {
        if let Ok(id) = param_str(params, name) {
            if self.store.identity_exists(&id)? {
                let type_name = self.store.object_type_of(&id)?;
                self.require_write(session, &type_name, &id)?;
            }
        }
        Ok(())
    }

    fn validate_link(
        &self,
        link: &str,
        from: &str,
        to: &str,
        created: &BTreeMap<String, String>,
    ) -> Result<()> {
        let spec = self.load_named("schema_link_types", MAIN_BRANCH, link, || {
            format!("link type {link}")
        })?;
        let spec: LinkTypeSpec = spec;
        let from_ok = created.contains_key(from) || self.store.identity_exists(from)?;
        let to_ok = created.contains_key(to) || self.store.identity_exists(to)?;
        if !from_ok || !to_ok {
            return Err(OntoError::Invalid(format!(
                "link {link} endpoints must exist"
            )));
        }
        let from_type = created
            .get(from)
            .cloned()
            .map_or_else(|| self.store.object_type_of(from), Ok)?;
        if from_type != spec.from_type {
            return Err(OntoError::Invalid(format!(
                "link {link} from must be {}",
                spec.from_type
            )));
        }
        let to_type = created
            .get(to)
            .cloned()
            .map_or_else(|| self.store.object_type_of(to), Ok)?;
        if to_type != spec.to_type {
            return Err(OntoError::Invalid(format!(
                "link {link} to must be {}",
                spec.to_type
            )));
        }
        Ok(())
    }

    fn action_to_tool(action: &ActionTypeSpec, value_types: &[ValueTypeSpec]) -> ToolSpec {
        let mut properties = serde_json::Map::new();
        let mut required = Vec::new();
        for p in &action.parameters {
            let schema = value_types
                .iter()
                .find(|vt| vt.name == p.value_type)
                .map_or_else(
                    || json!({ "type": guards::json_schema_type(&p.value_type) }),
                    guards::json_schema_from_value_type,
                );
            properties.insert(p.name.clone(), schema);
            if p.required {
                required.push(p.name.clone());
            }
        }
        ToolSpec {
            name: format!("action.{}", action.name),
            description: format!("Predefined action {}", action.name),
            input_schema: json!({
                "type": "object",
                "properties": properties,
                "required": required
            }),
        }
    }
}

struct AuthFail {
    name: String,
    reason: String,
}

fn rule_version(spec: &ActionTypeSpec) -> String {
    pin_version(&spec.name, &serde_json::to_string(spec).unwrap_or_default())
}

fn apply_action_name(propose_name: &str) -> String {
    if let Some(rest) = propose_name.strip_prefix("propose_") {
        format!("approve_{rest}")
    } else {
        propose_name.to_string()
    }
}

fn apply_action_name_from_row(row: &InboxRow) -> String {
    if row.apply_action.is_empty() {
        apply_action_name(&row.action_name)
    } else {
        row.apply_action.clone()
    }
}

fn param_str(params: &Value, name: &str) -> Result<String> {
    params
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| OntoError::Invalid(format!("param {name} must be a string id")))
}

fn resolve_value(v: &Value, params: &Value) -> Value {
    if let Some(s) = v.as_str() {
        if let Some(name) = s.strip_prefix('$') {
            return params.get(name).cloned().unwrap_or(Value::Null);
        }
    }
    v.clone()
}

fn worst_verdict(results: &[GuardResult]) -> Verdict {
    if results.iter().any(|g| g.verdict == Verdict::Deny) {
        return Verdict::Deny;
    }
    if results.iter().any(|g| g.verdict == Verdict::Review) {
        return Verdict::Review;
    }
    Verdict::Allow
}

fn builder_tools() -> Vec<ToolSpec> {
    [
        (
            "open_branch",
            "Open a working schema branch copied from main",
        ),
        ("create_value_type", "Create a value type on a branch"),
        ("create_object_type", "Create an object type on a branch"),
        ("alter_object_type", "Replace an object type on a branch"),
        (
            "add_property",
            "Add a property to an object type on a branch",
        ),
        ("alter_property", "Alter a property on a branch"),
        ("archive_object_type", "Archive an object type on a branch"),
        ("create_link_type", "Create a link type on a branch"),
        ("alter_link_type", "Alter a link type on a branch"),
        ("create_interface", "Create an interface on a branch"),
        ("attach_interface", "Attach an interface to an object type"),
        ("create_action_type", "Create an action type on a branch"),
        ("alter_action_type", "Alter an action type on a branch"),
        ("create_function", "Create a function record on a branch"),
        ("create_object_set", "Create a named object set on a branch"),
        (
            "create_policy",
            "Create a four-level policy grant on a branch",
        ),
        ("submit_proposal", "Submit a branch for review"),
        ("review_proposal", "Approve or reject a proposal"),
        ("merge_to_main", "Merge an approved proposal into main"),
        (
            "migrate_legacy",
            "Cancel inbox without apply_action; reject branches without base_revision",
        ),
        (
            "get_schema",
            "Read schema of a branch (never production instances)",
        ),
    ]
    .into_iter()
    .map(|(name, description)| ToolSpec {
        name: name.into(),
        description: description.into(),
        input_schema: json!({ "type": "object" }),
    })
    .collect()
}

fn consumer_base_tools() -> Vec<ToolSpec> {
    [
        ("search_objects", "Permission-first object-set search"),
        (
            "get_object",
            "Load one object with freshness, provenance, missing fields; as_of is valid time",
        ),
        ("traverse_links", "Cycle-aware link traversal"),
        ("aggregate", "Count members of an object set"),
        (
            "list_missing_evidence",
            "Missing and stale fields for an object",
        ),
        ("describe_action", "Projected Action card"),
        ("submit_action", "Submit a predefined Action"),
        ("list_inbox", "Pending proposals as objects"),
        (
            "confirm_action",
            "Confirm a pending proposal (TOCTOU revalidation)",
        ),
        (
            "override_action",
            "Categorized override of a pending proposal",
        ),
        ("get_decision_record", "Replayable decision dossier"),
        ("get_rejection", "Structured rejection reasons"),
        (
            "funnel_ingest",
            "Event-path ingest; never overwrites ActionWritten",
        ),
        (
            "auto_action",
            "T4 auto inside a declared bound {action_type × object_set × risk_band}",
        ),
        (
            "compensate_action",
            "Submit the named inverse Action of an Allow DecisionRecord",
        ),
        (
            "list_effect_intentions",
            "List durable effect intentions; Allow is not delivery",
        ),
        (
            "claim_effect",
            "Host claims a declared effect intention (not delivery)",
        ),
        (
            "ack_effect",
            "Host acks a claimed effect intention (not delivery)",
        ),
        (
            "reconcile_effects",
            "List declared and claimed effect intentions still in custody",
        ),
    ]
    .into_iter()
    .map(|(name, description)| ToolSpec {
        name: name.into(),
        description: description.into(),
        input_schema: json!({ "type": "object" }),
    })
    .collect()
}

fn read_set_of(reads: &[SnapshotObject]) -> Vec<(String, String)> {
    command::read_set(
        &reads
            .iter()
            .filter(|o| !o.version_id.is_empty())
            .map(|o| (o.id.clone(), o.version_id.clone()))
            .collect::<Vec<_>>(),
    )
}
