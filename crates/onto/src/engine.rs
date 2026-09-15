use crate::bitemporal::{self, AsOf};
use crate::compensation::{self, Compensation};
use crate::error::{OntoError, Result};
use crate::functions::{self, FunctionSpec};
use crate::oss::{
    apply_permission, evaluate_members, ObjectSet, ObjectSetFilter, ObjectSetSpec,
    OBJECT_SETS_TABLE,
};
use crate::security::{authorize, filter_view, AuthzDecision, AuthzOp, PolicySpec, POLICIES_TABLE};
use crate::tiers::{self, AutoBound, RiskBand};
use crate::types::*;
use crate::write_path::{pin_version, resolve_idempotency_key, StagedOp, WritePath};
use rusqlite::{params, Connection, OptionalExtension};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::sync::Mutex;

pub struct Engine {
    db: Mutex<Connection>,
    clock: Mutex<i64>,
}

impl Engine {
    pub fn memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let engine = Self {
            db: Mutex::new(conn),
            clock: Mutex::new(1_700_000_000),
        };
        engine.init()?;
        Ok(engine)
    }

    pub fn open(path: &str) -> Result<Self> {
        let conn = Connection::open(path)?;
        let engine = Self {
            db: Mutex::new(conn),
            clock: Mutex::new(1_700_000_000),
        };
        engine.init()?;
        Ok(engine)
    }

    pub fn set_clock(&self, secs: i64) {
        *self.clock.lock().expect("clock") = secs;
    }

    pub fn now(&self) -> i64 {
        *self.clock.lock().expect("clock")
    }

    fn init(&self) -> Result<()> {
        let db = self.db.lock().expect("db");
        db.execute_batch(
            r#"
            PRAGMA foreign_keys = ON;
            CREATE TABLE IF NOT EXISTS branches (
                name TEXT PRIMARY KEY,
                status TEXT NOT NULL,
                created_by TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS schema_value_types (
                branch TEXT NOT NULL,
                name TEXT NOT NULL,
                spec TEXT NOT NULL,
                PRIMARY KEY (branch, name)
            );
            CREATE TABLE IF NOT EXISTS schema_object_types (
                branch TEXT NOT NULL,
                name TEXT NOT NULL,
                spec TEXT NOT NULL,
                PRIMARY KEY (branch, name)
            );
            CREATE TABLE IF NOT EXISTS schema_link_types (
                branch TEXT NOT NULL,
                name TEXT NOT NULL,
                spec TEXT NOT NULL,
                PRIMARY KEY (branch, name)
            );
            CREATE TABLE IF NOT EXISTS schema_interfaces (
                branch TEXT NOT NULL,
                name TEXT NOT NULL,
                spec TEXT NOT NULL,
                PRIMARY KEY (branch, name)
            );
            CREATE TABLE IF NOT EXISTS schema_action_types (
                branch TEXT NOT NULL,
                name TEXT NOT NULL,
                spec TEXT NOT NULL,
                PRIMARY KEY (branch, name)
            );
            CREATE TABLE IF NOT EXISTS schema_functions (
                branch TEXT NOT NULL,
                name TEXT NOT NULL,
                spec TEXT NOT NULL,
                PRIMARY KEY (branch, name)
            );
            CREATE TABLE IF NOT EXISTS schema_object_sets (
                branch TEXT NOT NULL,
                name TEXT NOT NULL,
                spec TEXT NOT NULL,
                PRIMARY KEY (branch, name)
            );
            CREATE TABLE IF NOT EXISTS schema_policies (
                branch TEXT NOT NULL,
                name TEXT NOT NULL,
                spec TEXT NOT NULL,
                PRIMARY KEY (branch, name)
            );
            CREATE TABLE IF NOT EXISTS proposals (
                id TEXT PRIMARY KEY,
                branch TEXT NOT NULL,
                status TEXT NOT NULL,
                submitted_by TEXT,
                reviewed_by TEXT,
                created_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS objects (
                id TEXT PRIMARY KEY,
                type_name TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS links (
                id TEXT PRIMARY KEY,
                type_name TEXT NOT NULL,
                from_id TEXT NOT NULL,
                to_id TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS inbox (
                id TEXT PRIMARY KEY,
                action_name TEXT NOT NULL,
                proposed_by TEXT NOT NULL,
                params TEXT NOT NULL,
                status TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS decision_records (
                id TEXT PRIMARY KEY,
                action_name TEXT NOT NULL,
                actor TEXT NOT NULL,
                confirmer TEXT,
                verdict TEXT NOT NULL,
                params TEXT NOT NULL,
                guard_results TEXT NOT NULL,
                effects TEXT NOT NULL,
                rule_version TEXT NOT NULL,
                function_version TEXT NOT NULL,
                engine_version TEXT NOT NULL,
                data_snapshot TEXT NOT NULL,
                proof_trace TEXT NOT NULL DEFAULT '[]',
                created_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS side_effect_keys (
                idempotency_key TEXT PRIMARY KEY,
                decision_record_id TEXT NOT NULL,
                action_name TEXT NOT NULL,
                outcome TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS audit_log (
                id TEXT PRIMARY KEY,
                at INTEGER NOT NULL,
                actor TEXT NOT NULL,
                kind TEXT NOT NULL,
                payload TEXT NOT NULL
            );
            "#,
        )?;
        db.execute_batch(bitemporal::SCHEMA)?;
        let _ = db.execute(
            "ALTER TABLE decision_records ADD COLUMN proof_trace TEXT NOT NULL DEFAULT '[]'",
            [],
        );
        db.execute(
            "INSERT OR IGNORE INTO branches(name, status, created_by, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![MAIN_BRANCH, "merged", "kernel", 0],
        )?;
        Ok(())
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

    fn audit(&self, actor: &str, kind: &str, payload: Value) -> Result<()> {
        let db = self.db.lock().expect("db");
        db.execute(
            "INSERT INTO audit_log(id, at, actor, kind, payload) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![new_id(), self.now(), actor, kind, payload.to_string()],
        )?;
        Ok(())
    }

    pub fn kernel_types() -> &'static [&'static str] {
        KERNEL_TYPES
    }

    pub fn open_branch(&self, session: &Session, name: &str) -> Result<String> {
        Self::require_builder(session)?;
        if name == MAIN_BRANCH {
            return Err(OntoError::Invalid("cannot reopen main".into()));
        }
        {
            let db = self.db.lock().expect("db");
            let exists: Option<String> = db
                .query_row(
                    "SELECT status FROM branches WHERE name = ?1",
                    params![name],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(status) = exists {
                if status == "open" {
                    return Ok(name.to_string());
                }
                return Err(OntoError::Conflict(format!("branch {name} is {status}")));
            }
            db.execute(
                "INSERT INTO branches(name, status, created_by, created_at) VALUES (?1, ?2, ?3, ?4)",
                params![name, "open", session.actor.id, self.now()],
            )?;
        }
        self.copy_schema(MAIN_BRANCH, name)?;
        self.audit(&session.actor.id, "open_branch", json!({ "branch": name }))?;
        Ok(name.to_string())
    }

    fn copy_schema(&self, from: &str, to: &str) -> Result<()> {
        let tables = [
            "schema_value_types",
            "schema_object_types",
            "schema_link_types",
            "schema_interfaces",
            "schema_action_types",
            "schema_functions",
            OBJECT_SETS_TABLE,
            POLICIES_TABLE,
        ];
        let db = self.db.lock().expect("db");
        for table in tables {
            db.execute(
                &format!(
                    "INSERT OR REPLACE INTO {table}(branch, name, spec)
                     SELECT ?1, name, spec FROM {table} WHERE branch = ?2"
                ),
                params![to, from],
            )?;
        }
        Ok(())
    }

    fn require_open_branch(&self, branch: &str) -> Result<()> {
        if branch == MAIN_BRANCH {
            return Err(OntoError::Invalid(
                "mutate schema on a working branch, not main".into(),
            ));
        }
        let db = self.db.lock().expect("db");
        let status: String = db
            .query_row(
                "SELECT status FROM branches WHERE name = ?1",
                params![branch],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| OntoError::NotFound(format!("branch {branch}")))?;
        if status != "open" {
            return Err(OntoError::Conflict(format!("branch {branch} is {status}")));
        }
        Ok(())
    }

    fn put_spec(
        db: &Connection,
        table: &str,
        branch: &str,
        name: &str,
        spec: &Value,
    ) -> Result<()> {
        db.execute(
            &format!("INSERT OR REPLACE INTO {table}(branch, name, spec) VALUES (?1, ?2, ?3)"),
            params![branch, name, spec.to_string()],
        )?;
        Ok(())
    }

    pub fn create_value_type(
        &self,
        session: &Session,
        branch: &str,
        spec: ValueTypeSpec,
    ) -> Result<String> {
        Self::require_builder(session)?;
        self.require_open_branch(branch)?;
        let db = self.db.lock().expect("db");
        Self::put_spec(
            &db,
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
        let db = self.db.lock().expect("db");
        Self::put_spec(
            &db,
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
        let db = self.db.lock().expect("db");
        Self::put_spec(
            &db,
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
        let db = self.db.lock().expect("db");
        Self::put_spec(
            &db,
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
        let db = self.db.lock().expect("db");
        Self::put_spec(
            &db,
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
        let db = self.db.lock().expect("db");
        db.execute(
            "DELETE FROM schema_object_types WHERE branch = ?1 AND name = ?2",
            params![branch, type_name],
        )?;
        Ok(())
    }

    pub fn create_link_type(
        &self,
        session: &Session,
        branch: &str,
        spec: LinkTypeSpec,
    ) -> Result<String> {
        Self::require_builder(session)?;
        self.require_open_branch(branch)?;
        let db = self.db.lock().expect("db");
        Self::put_spec(
            &db,
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
        let db = self.db.lock().expect("db");
        Self::put_spec(
            &db,
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
        let mut spec = self.load_object_type(branch, type_name)?;
        if !spec.interfaces.iter().any(|i| i == interface) {
            spec.interfaces.push(interface.to_string());
        }
        let db = self.db.lock().expect("db");
        Self::put_spec(
            &db,
            "schema_object_types",
            branch,
            type_name,
            &serde_json::to_value(&spec)?,
        )?;
        Ok(())
    }

    pub fn create_action_type(
        &self,
        session: &Session,
        branch: &str,
        spec: ActionTypeSpec,
    ) -> Result<String> {
        Self::require_builder(session)?;
        self.require_open_branch(branch)?;
        let db = self.db.lock().expect("db");
        Self::put_spec(
            &db,
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
        let db = self.db.lock().expect("db");
        db.execute(
            "UPDATE branches SET status = 'proposed' WHERE name = ?1",
            params![branch],
        )?;
        db.execute(
            "INSERT INTO proposals(id, branch, status, submitted_by, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, branch, "under_review", session.actor.id, self.now()],
        )?;
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
        let db = self.db.lock().expect("db");
        let (branch, status): (String, String) = db.query_row(
            "SELECT branch, status FROM proposals WHERE id = ?1",
            params![proposal_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if status != "under_review" {
            return Err(OntoError::Conflict(format!("proposal is {status}")));
        }
        if approve {
            db.execute(
                "UPDATE proposals SET status = 'approved', reviewed_by = ?1 WHERE id = ?2",
                params![session.actor.id, proposal_id],
            )?;
        } else {
            db.execute(
                "UPDATE proposals SET status = 'rejected', reviewed_by = ?1 WHERE id = ?2",
                params![session.actor.id, proposal_id],
            )?;
            db.execute(
                "UPDATE branches SET status = 'rejected' WHERE name = ?1",
                params![branch],
            )?;
        }
        Ok(())
    }

    pub fn merge_to_main(&self, session: &Session, proposal_id: &str) -> Result<()> {
        Self::require_builder(session)?;
        if !session.actor.has_role("reviewer") {
            return Err(OntoError::Denied("reviewer role required".into()));
        }
        let branch = {
            let db = self.db.lock().expect("db");
            let (branch, status): (String, String) = db.query_row(
                "SELECT branch, status FROM proposals WHERE id = ?1",
                params![proposal_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            if status != "approved" {
                return Err(OntoError::Conflict(
                    "proposal must be approved before merge".into(),
                ));
            }
            branch
        };
        self.replace_main_schema(&branch)?;
        {
            let db = self.db.lock().expect("db");
            db.execute(
                "UPDATE proposals SET status = 'merged' WHERE id = ?1",
                params![proposal_id],
            )?;
            db.execute(
                "UPDATE branches SET status = 'merged' WHERE name = ?1",
                params![branch],
            )?;
        }
        self.audit(
            &session.actor.id,
            "merge_to_main",
            json!({ "proposal": proposal_id, "branch": branch }),
        )?;
        Ok(())
    }

    fn replace_main_schema(&self, from: &str) -> Result<()> {
        let tables = [
            "schema_value_types",
            "schema_object_types",
            "schema_link_types",
            "schema_interfaces",
            "schema_action_types",
            "schema_functions",
            OBJECT_SETS_TABLE,
            POLICIES_TABLE,
        ];
        let db = self.db.lock().expect("db");
        for table in tables {
            db.execute(
                &format!("DELETE FROM {table} WHERE branch = ?1"),
                params![MAIN_BRANCH],
            )?;
            db.execute(
                &format!(
                    "INSERT INTO {table}(branch, name, spec)
                     SELECT ?1, name, spec FROM {table} WHERE branch = ?2"
                ),
                params![MAIN_BRANCH, from],
            )?;
        }
        Ok(())
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
        let db = self.db.lock().expect("db");
        let mut stmt = db.prepare(&format!(
            "SELECT spec FROM {table} WHERE branch = ?1 ORDER BY name"
        ))?;
        let rows = stmt.query_map(params![branch], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(serde_json::from_str(&row?)?);
        }
        Ok(out)
    }

    fn load_object_type(&self, branch: &str, name: &str) -> Result<ObjectTypeSpec> {
        let db = self.db.lock().expect("db");
        let spec: String = db
            .query_row(
                "SELECT spec FROM schema_object_types WHERE branch = ?1 AND name = ?2",
                params![branch, name],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| OntoError::NotFound(format!("object type {name} on {branch}")))?;
        Ok(serde_json::from_str(&spec)?)
    }

    fn load_action_type(&self, branch: &str, name: &str) -> Result<ActionTypeSpec> {
        let db = self.db.lock().expect("db");
        let spec: String = db
            .query_row(
                "SELECT spec FROM schema_action_types WHERE branch = ?1 AND name = ?2",
                params![branch, name],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| OntoError::NotFound(format!("action type {name}")))?;
        Ok(serde_json::from_str(&spec)?)
    }

    fn load_function(&self, branch: &str, name: &str) -> Result<FunctionSpec> {
        let db = self.db.lock().expect("db");
        let spec: String = db
            .query_row(
                "SELECT spec FROM schema_functions WHERE branch = ?1 AND name = ?2",
                params![branch, name],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| OntoError::NotFound(format!("function {name} on {branch}")))?;
        Ok(serde_json::from_str(&spec)?)
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
        let db = self.db.lock().expect("db");
        let spec: Option<String> = db
            .query_row(
                "SELECT spec FROM schema_value_types WHERE branch = ?1 AND name = ?2",
                params![branch, name],
                |r| r.get(0),
            )
            .optional()?;
        Ok(spec.map(|s| serde_json::from_str(&s)).transpose()?)
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
        let db = self.db.lock().expect("db");
        Self::put_spec(
            &db,
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
        let db = self.db.lock().expect("db");
        Self::put_spec(
            &db,
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

    fn authorize_write(
        &self,
        session: &Session,
        type_name: &str,
        instance_id: &str,
    ) -> Result<AuthzDecision> {
        Ok(authorize(
            &self.load_policies()?,
            session,
            AuthzOp::Write,
            Some(type_name),
            Some(instance_id),
            None,
        ))
    }

    fn require_write(&self, session: &Session, type_name: &str, instance_id: &str) -> Result<()> {
        match self.authorize_write(session, type_name, instance_id)? {
            AuthzDecision::Allow => Ok(()),
            AuthzDecision::Deny => Err(OntoError::Denied(format!(
                "write denied for {type_name}/{instance_id}"
            ))),
        }
    }

    pub fn search_objects(&self, session: &Session, query: Query) -> Result<Vec<ObjectView>> {
        self.search_object_set(session, ObjectSet::from_query(&query))
    }

    pub fn search_object_set(&self, session: &Session, set: ObjectSet) -> Result<Vec<ObjectView>> {
        Self::require_consumer(session)?;
        let resolved = self.resolve_object_set(set)?;
        let ids = self.candidate_ids(resolved.type_name.as_deref())?;
        evaluate_members(&resolved, ids, |id| self.load_permitted_view(session, id))
    }

    pub fn aggregate_set(&self, session: &Session, set: ObjectSet) -> Result<Value> {
        Self::require_consumer(session)?;
        let resolved = self.resolve_object_set(set.unbounded())?;
        let ids = self.candidate_ids(resolved.type_name.as_deref())?;
        let members = evaluate_members(&resolved, ids, |id| self.load_permitted_view(session, id))?;
        Ok(json!({
            "type_name": resolved.type_name,
            "name": resolved.name,
            "count": members.len() as i64
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
        let db = self.db.lock().expect("db");
        let spec: String = db
            .query_row(
                "SELECT spec FROM schema_object_sets WHERE branch = ?1 AND name = ?2",
                params![branch, name],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| OntoError::NotFound(format!("object set {name} on {branch}")))?;
        Ok(serde_json::from_str(&spec)?)
    }

    fn candidate_ids(&self, type_name: Option<&str>) -> Result<Vec<String>> {
        let db = self.db.lock().expect("db");
        let ids = match type_name {
            Some(t) => {
                let mut stmt = db.prepare("SELECT id FROM objects WHERE type_name = ?1")?;
                let rows = stmt.query_map(params![t], |r| r.get(0))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            }
            None => {
                let mut stmt = db.prepare("SELECT id FROM objects")?;
                let rows = stmt.query_map([], |r| r.get(0))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            }
        };
        Ok(ids)
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
    /// not a silent current row. Property-level Deny hides the property.
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

    /// Spans for one identity, oldest valid_from first.
    pub fn object_spans(&self, id: &str) -> Result<Vec<bitemporal::VersionSpan>> {
        let db = self.db.lock().expect("db");
        bitemporal::list_spans(&db, id)
    }

    fn load_object_view(&self, id: &str, as_of: AsOf) -> Result<ObjectView> {
        let loaded = {
            let db = self.db.lock().expect("db");
            bitemporal::load(&db, id, as_of)?
        };
        let clock = match as_of {
            AsOf::Current => self.now(),
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
        if let Some(spec) = spec {
            for prop in spec.properties {
                if !properties.contains_key(&prop.name) && !prop.nullable {
                    missing.push(prop.name.clone());
                }
                if let Some(pv) = properties.get(&prop.name) {
                    if let (Some(budget), Some(stamp)) = (spec.freshness_budget_secs, &pv.as_of) {
                        if let Ok(ts) = stamp.parse::<i64>() {
                            if clock - ts > budget {
                                stale.push(prop.name);
                            }
                        }
                    }
                }
            }
        }
        Ok(ObjectView {
            id: id.to_string(),
            type_name: loaded.type_name,
            title: loaded.title,
            properties,
            missing,
            stale,
        })
    }

    pub fn traverse_links(
        &self,
        session: &Session,
        from_id: &str,
        link_type: &str,
    ) -> Result<Vec<ObjectView>> {
        Self::require_consumer(session)?;
        let allow_cycles = self
            .get_schema(session, None)?
            .link_types
            .iter()
            .find(|l| l.name == link_type)
            .map(|l| l.allow_cycles)
            .unwrap_or(false);
        let db = self.db.lock().expect("db");
        let mut stmt =
            db.prepare("SELECT to_id FROM links WHERE from_id = ?1 AND type_name = ?2")?;
        let targets: Vec<String> = stmt
            .query_map(params![from_id, link_type], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        drop(db);
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
            ids.push(self.ingest_one(rec)?);
        }
        self.audit(
            &session.actor.id,
            "funnel_ingest",
            json!({ "count": ids.len() }),
        )?;
        Ok(ids)
    }

    fn ingest_one(&self, rec: IngestRecord) -> Result<String> {
        let spec = self.load_object_type(MAIN_BRANCH, &rec.type_name)?;
        let as_of = rec.as_of.clone().unwrap_or_else(|| self.now().to_string());
        let id = rec.id.clone().unwrap_or_else(new_id);
        let existing = {
            let db = self.db.lock().expect("db");
            if bitemporal::identity_exists(&db, &id)? {
                Some(bitemporal::current_properties(&db, &id)?)
            } else {
                None
            }
        };
        let mut props: BTreeMap<String, PropertyView> = existing
            .as_ref()
            .map(|s| serde_json::from_str(s))
            .transpose()?
            .unwrap_or_default();
        for (name, value) in rec.properties {
            let source = spec
                .properties
                .iter()
                .find(|p| p.name == name)
                .map(|p| p.source)
                .unwrap_or(PropertySource::Mapped);
            if source == PropertySource::ActionWritten {
                continue;
            }
            if let Some(cur) = props.get(&name) {
                if cur.source == PropertySource::ActionWritten {
                    continue;
                }
            }
            if source == PropertySource::Derived {
                continue;
            }
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
                .and_then(|p| p.value.as_str().map(|s| s.to_string()))
        });
        let encoded = serde_json::to_string(&props)?;
        let at = rec
            .as_of
            .as_ref()
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or_else(|| self.now());
        let db = self.db.lock().expect("db");
        if existing.is_some() {
            bitemporal::append_version(&db, &id, &encoded, title.as_deref(), at)?;
        } else {
            bitemporal::insert_object(&db, &id, &rec.type_name, title.as_deref(), &encoded, at)?;
        }
        Ok(id)
    }

    pub fn list_inbox(&self, session: &Session) -> Result<Vec<InboxItem>> {
        Self::require_consumer(session)?;
        tiers::require_syscall(session.actor.tier, "list_inbox")?;
        let db = self.db.lock().expect("db");
        let mut stmt = db.prepare(
            "SELECT id, action_name, proposed_by, params, status, created_at FROM inbox ORDER BY created_at",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(InboxItem {
                id: r.get(0)?,
                action_name: r.get(1)?,
                proposed_by: r.get(2)?,
                params: serde_json::from_str(&r.get::<_, String>(3)?).unwrap_or(Value::Null),
                status: r.get(4)?,
                created_at: r.get::<_, i64>(5)?.to_string(),
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
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
                        tools.push(action_to_tool(&action));
                    }
                }
                Ok(tools)
            }
        }
    }

    pub fn submit_action(
        &self,
        session: &Session,
        action_name: &str,
        params: Value,
    ) -> Result<ActionOutcome> {
        Self::require_consumer(session)?;
        tiers::require_syscall(session.actor.tier, "submit_action")?;
        let spec = self.load_action_type(MAIN_BRANCH, action_name)?;
        self.execute_action(session, &spec, params, None)
    }

    /// Submit the named inverse Action for an Allow DecisionRecord.
    ///
    /// The original record is left in place. A new DecisionRecord is sealed
    /// through [`Self::execute_action`]. Missing compensation is
    /// [`OntoError::NoCompensation`], not a silent success.
    pub fn compensate_action(
        &self,
        session: &Session,
        decision_record_id: &str,
        overlay: Value,
    ) -> Result<ActionOutcome> {
        Self::require_consumer(session)?;
        let original = self.get_decision_record(session, decision_record_id)?;
        compensation::require_allow(&original)?;
        let original_spec = self.load_action_type(MAIN_BRANCH, &original.action_name)?;
        let Compensation::Inverse { action } = Compensation::from_spec(&original_spec)?;
        let spec = self.load_action_type(MAIN_BRANCH, &action)?;
        let params = compensation::inverse_params(&original, &overlay);
        self.execute_action(session, &spec, params, None)
    }

    pub fn confirm_action(&self, session: &Session, inbox_id: &str) -> Result<ActionOutcome> {
        Self::require_consumer(session)?;
        tiers::require_syscall(session.actor.tier, "confirm_action")?;
        let (action_name, params, status, proposed_by): (String, String, String, String) = {
            let db = self.db.lock().expect("db");
            db.query_row(
                "SELECT action_name, params, status, proposed_by FROM inbox WHERE id = ?1",
                params![inbox_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?
        };
        tiers::require_distinct_confirmer(&session.actor.id, &proposed_by)?;
        if status != "pending" {
            return Err(OntoError::Conflict(format!("inbox item is {status}")));
        }
        let params: Value = serde_json::from_str(&params)?;
        let apply_name = action_name.replacen("propose_", "approve_", 1);
        let spec = match self.load_action_type(MAIN_BRANCH, &apply_name) {
            Ok(spec) => spec,
            Err(_) => {
                let mut spec = self.load_action_type(MAIN_BRANCH, &action_name)?;
                spec.mode = ExecutionMode::Auto;
                spec
            }
        };
        let outcome = self.execute_action(session, &spec, params, Some(inbox_id))?;
        Ok(outcome)
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
        let params = {
            let db = self.db.lock().expect("db");
            let (action_name, raw, status, proposed_by): (String, String, String, String) = db
                .query_row(
                    "SELECT action_name, params, status, proposed_by FROM inbox WHERE id = ?1",
                    params![inbox_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )?;
            if status != "pending" {
                return Err(OntoError::Conflict(format!("inbox item is {status}")));
            }
            tiers::require_distinct_confirmer(&session.actor.id, &proposed_by)?;
            let mut p: Value = serde_json::from_str(&raw)?;
            if let Value::Object(map) = &mut p {
                map.insert("override_category".into(), json!(category));
                map.insert("override_reason".into(), json!(reason));
                map.insert("source_action".into(), json!(action_name));
                map.insert("proposed_by".into(), json!(proposed_by));
            }
            db.execute(
                "UPDATE inbox SET status = 'overridden' WHERE id = ?1",
                params![inbox_id],
            )?;
            p
        };
        let override_spec = self.load_action_type(MAIN_BRANCH, "override_setpoint").ok();
        if let Some(spec) = override_spec {
            return self.execute_action(session, &spec, params, Some(inbox_id));
        }
        let rec = self.persist_decision(
            session,
            "override_setpoint",
            Some(&session.actor.id),
            Verdict::Allow,
            &params,
            &[GuardResult {
                name: "override".into(),
                verdict: Verdict::Allow,
                reason: format!("{category}: {reason}"),
            }],
            json!({ "inbox": inbox_id }),
            &DataSnapshot {
                objects: vec![],
                rule_version: "override_setpoint".into(),
                function_version: functions::digest(&[]),
                engine_version: ENGINE_VERSION.into(),
            },
            &[WritePathStep::Submit, WritePathStep::SealDecisionRecord],
        )?;
        Ok(ActionOutcome {
            verdict: Verdict::Allow,
            reason: format!("overridden: {category}"),
            decision_record_id: Some(rec),
            inbox_id: Some(inbox_id.into()),
            created_ids: vec![],
            alternative: None,
            guard_results: vec![],
        })
    }

    /// T4 unsupervised auto. Bound starts empty, so every claim is denied.
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
        self.submit_action(session, action_name, params)
    }

    pub fn get_decision_record(&self, session: &Session, id: &str) -> Result<DecisionRecordView> {
        Self::require_consumer(session)?;
        let db = self.db.lock().expect("db");
        db.query_row(
            "SELECT id, action_name, actor, confirmer, verdict, params, guard_results, effects, rule_version, function_version, engine_version, data_snapshot, created_at, COALESCE(proof_trace, '[]')
             FROM decision_records WHERE id = ?1",
            params![id],
            |r| {
                let verdict_raw: String = r.get(4)?;
                let verdict = match verdict_raw.as_str() {
                    "allow" => Verdict::Allow,
                    "review" => Verdict::Review,
                    "deny" => Verdict::Deny,
                    _ => Verdict::Deny,
                };
                Ok(DecisionRecordView {
                    id: r.get(0)?,
                    action_name: r.get(1)?,
                    actor: r.get(2)?,
                    confirmer: r.get(3)?,
                    verdict,
                    params: serde_json::from_str(&r.get::<_, String>(5)?).unwrap_or(Value::Null),
                    guard_results: serde_json::from_str(&r.get::<_, String>(6)?).unwrap_or_default(),
                    effects: serde_json::from_str(&r.get::<_, String>(7)?).unwrap_or(Value::Null),
                    rule_version: r.get(8)?,
                    function_version: r.get(9)?,
                    engine_version: r.get(10)?,
                    data_snapshot: serde_json::from_str(&r.get::<_, String>(11)?).unwrap_or(Value::Null),
                    created_at: r.get::<_, i64>(12)?.to_string(),
                    proof_trace: serde_json::from_str(&r.get::<_, String>(13).unwrap_or_else(|_| "[]".into()))
                        .unwrap_or_default(),
                })
            },
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => OntoError::NotFound(format!("decision {id}")),
            other => OntoError::Store(other.to_string()),
        })
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
        params: Value,
        confirmer_inbox: Option<&str>,
    ) -> Result<ActionOutcome> {
        let key = resolve_idempotency_key(&spec.name, &session.actor.id, &params);
        if let Some(cached) = self.load_cached_outcome(&key)? {
            return Ok(cached);
        }

        let path = WritePath::begin(key.clone());
        match self.check_param_and_permission(session, spec, &params) {
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
                self.finish_abort(session, spec, &params, path, auth.reason, None)
            }
            Ok(Ok(param_reads)) => {
                let path = path.param_and_permission(param_reads);
                let (guards, guard_reads) = self.evaluate_guards(&spec.guards, &params)?;
                let worst = worst_verdict(&guards);
                let path = path.submission_criteria(guards, guard_reads, worst);
                match worst {
                    Verdict::Deny | Verdict::Review => {
                        let reason = path
                            .guards
                            .iter()
                            .find(|g| g.verdict == worst)
                            .map(|g| g.reason.clone())
                            .unwrap_or_else(|| match worst {
                                Verdict::Deny => "denied".into(),
                                Verdict::Review => "needs review".into(),
                                Verdict::Allow => unreachable!("matched deny/review"),
                            });
                        let alternative = spec.on_review.clone();
                        let path = path.discard_stage();
                        self.finish_abort(session, spec, &params, path, reason, alternative)
                    }
                    Verdict::Allow => {
                        self.finish_allow(session, spec, params, confirmer_inbox, path)
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
        let mut reads = Vec::new();
        for p in &spec.parameters {
            if p.object_type.is_some() {
                if let Some(id) = params.get(&p.name).and_then(|v| v.as_str()) {
                    reads.push(self.snapshot_object(id)?);
                }
            }
        }
        Ok(Ok(reads))
    }

    fn finish_abort(
        &self,
        session: &Session,
        spec: &ActionTypeSpec,
        params: &Value,
        path: WritePath<crate::write_path::SealDecisionRecord>,
        reason: String,
        alternative: Option<String>,
    ) -> Result<ActionOutcome> {
        let snapshot = path.data_snapshot(
            rule_version(spec),
            self.function_version_for_reads(&path.reads),
            ENGINE_VERSION.into(),
        );
        let confirmer = match path.verdict {
            Verdict::Deny => None,
            Verdict::Review => None,
            Verdict::Allow => None,
        };
        let effects = match path.verdict {
            Verdict::Review => json!({ "alternative": spec.on_review }),
            Verdict::Deny | Verdict::Allow => json!({}),
        };
        let rec = self.persist_decision(
            session,
            &spec.name,
            confirmer,
            path.verdict,
            params,
            &path.guards,
            effects,
            &snapshot,
            &path.trace,
        )?;
        let outcome = ActionOutcome {
            verdict: path.verdict,
            reason,
            decision_record_id: Some(rec.clone()),
            inbox_id: None,
            created_ids: vec![],
            alternative,
            guard_results: path.guards.clone(),
        };
        self.store_idempotency(&path.idempotency_key, &spec.name, &rec, &outcome)?;
        Ok(outcome)
    }

    fn finish_allow(
        &self,
        session: &Session,
        spec: &ActionTypeSpec,
        params: Value,
        confirmer_inbox: Option<&str>,
        path: WritePath<crate::write_path::SubmissionCriteria>,
    ) -> Result<ActionOutcome> {
        let (staged, inbox_id) = self.build_stage(session, spec, &params, confirmer_inbox)?;
        let path = path.stage(staged);
        let created = self.commit_staged(&path.staged)?;
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
        let rec = self.persist_decision(
            session,
            &spec.name,
            confirmer,
            Verdict::Allow,
            &params,
            &path.guards,
            effects,
            &snapshot,
            &path.trace,
        )?;
        let reason = match spec.mode {
            ExecutionMode::Propose | ExecutionMode::Shadow => "proposed",
            ExecutionMode::Auto | ExecutionMode::Approve => "committed",
        };
        let outcome = ActionOutcome {
            verdict: Verdict::Allow,
            reason: reason.into(),
            decision_record_id: Some(rec.clone()),
            inbox_id,
            created_ids: created,
            alternative: None,
            guard_results: path.guards.clone(),
        };
        self.store_idempotency(&path.idempotency_key, &spec.name, &rec, &outcome)?;
        self.audit(
            &session.actor.id,
            "side_effect",
            json!({
                "idempotency_key": path.idempotency_key,
                "action": spec.name,
                "declaration": spec.side_effects,
                "decision_record_id": rec,
            }),
        )?;
        let _finished: WritePath<crate::write_path::DeclareSideEffects> = path;
        Ok(outcome)
    }

    fn build_stage(
        &self,
        session: &Session,
        spec: &ActionTypeSpec,
        params: &Value,
        confirmer_inbox: Option<&str>,
    ) -> Result<(Vec<StagedOp>, Option<String>)> {
        match spec.mode {
            ExecutionMode::Propose | ExecutionMode::Shadow => {
                let inbox_id = new_id();
                Ok((
                    vec![StagedOp::InsertInbox {
                        id: inbox_id.clone(),
                        action_name: spec.name.clone(),
                        proposed_by: session.actor.id.clone(),
                        params: params.to_string(),
                        created_at: self.now(),
                    }],
                    Some(inbox_id),
                ))
            }
            ExecutionMode::Auto | ExecutionMode::Approve => {
                let mut ops = self.stage_effects(&spec.effects, params, &session.actor.id)?;
                let inbox_id = confirmer_inbox.map(|s| s.to_string());
                if let Some(id) = &inbox_id {
                    ops.push(StagedOp::ConfirmInbox { id: id.clone() });
                }
                Ok((ops, inbox_id))
            }
        }
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
            if let Some(vt) = self.load_value_type(MAIN_BRANCH, &p.value_type)? {
                if vt.base == "number" {
                    let n = val.as_f64().ok_or_else(|| {
                        OntoError::Invalid(format!("{} must be a number", p.name))
                    })?;
                    if let Some(min) = vt.min {
                        if n < min {
                            return Err(OntoError::Invalid(format!("{} below {}", p.name, min)));
                        }
                    }
                    if let Some(max) = vt.max {
                        if n > max {
                            return Err(OntoError::Invalid(format!("{} above {}", p.name, max)));
                        }
                    }
                }
            }
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

    fn evaluate_guards(
        &self,
        guards: &Value,
        params: &Value,
    ) -> Result<(Vec<GuardResult>, Vec<SnapshotObject>)> {
        let mut reads = Vec::new();
        if guards.is_null() || guards == &json!({}) {
            return Ok((
                vec![GuardResult {
                    name: "empty".into(),
                    verdict: Verdict::Allow,
                    reason: "no guards".into(),
                }],
                reads,
            ));
        }
        let Some(arr) = guards.as_array() else {
            return Ok((
                vec![self.eval_one_guard(guards, params, &mut reads)?],
                reads,
            ));
        };
        let mut out = Vec::new();
        for g in arr {
            out.push(self.eval_one_guard(g, params, &mut reads)?);
        }
        Ok((out, reads))
    }

    fn eval_one_guard(
        &self,
        guard: &Value,
        params: &Value,
        reads: &mut Vec<SnapshotObject>,
    ) -> Result<GuardResult> {
        let obj = guard
            .as_object()
            .ok_or_else(|| OntoError::Invalid("guard must be object".into()))?;
        if let Some(name) = obj.get("freshness").and_then(|v| v.as_str()) {
            let max = obj
                .get("max_age_secs")
                .and_then(|v| v.as_i64())
                .unwrap_or(300);
            let id = param_str(params, name)?;
            let view = self.read_for_guard(&id, reads)?;
            let as_of = view
                .properties
                .get("last_reading_at")
                .or_else(|| view.properties.get("observed_at"))
                .and_then(|p| {
                    p.as_of
                        .clone()
                        .or_else(|| p.value.as_str().map(|s| s.to_string()))
                        .or_else(|| p.value.as_i64().map(|n| n.to_string()))
                });
            let Some(as_of) = as_of else {
                return Ok(GuardResult {
                    name: "freshness".into(),
                    verdict: Verdict::Review,
                    reason: format!("no freshness stamp on {name} (Complete fail)"),
                });
            };
            let ts = as_of.parse::<i64>().unwrap_or(0);
            if self.now() - ts > max {
                return Ok(GuardResult {
                    name: "freshness".into(),
                    verdict: Verdict::Review,
                    reason: format!(
                        "stale {name}: age {}s > {max}s (Current fail)",
                        self.now() - ts
                    ),
                });
            }
            return Ok(GuardResult {
                name: "freshness".into(),
                verdict: Verdict::Allow,
                reason: "fresh".into(),
            });
        }
        if let Some(field) = obj.get("exists_field") {
            let object_param = obj
                .get("object")
                .and_then(|v| v.as_str())
                .ok_or_else(|| OntoError::Invalid("exists_field needs object".into()))?;
            let field = field.as_str().unwrap_or("");
            let id = param_str(params, object_param)?;
            let view = self.read_for_guard(&id, reads)?;
            if view.properties.get(field).is_none() {
                return Ok(GuardResult {
                    name: "complete".into(),
                    verdict: Verdict::Review,
                    reason: format!("missing {field} on {object_param} (Complete fail)"),
                });
            }
            return Ok(GuardResult {
                name: "complete".into(),
                verdict: Verdict::Allow,
                reason: "present".into(),
            });
        }
        if let Some(param) = obj.get("lte_field").and_then(|v| v.as_str()) {
            let object_param = obj
                .get("object")
                .and_then(|v| v.as_str())
                .unwrap_or("permit");
            let field = obj
                .get("field")
                .and_then(|v| v.as_str())
                .unwrap_or("do_max");
            let n = param_f64(params, param)?;
            let permit_id = param_str(params, object_param)?;
            let view = self.read_for_guard(&permit_id, reads)?;
            let bound = view
                .properties
                .get(field)
                .and_then(|p| p.value.as_f64())
                .unwrap_or(f64::MAX);
            if n > bound {
                return Ok(GuardResult {
                    name: "permit_limit".into(),
                    verdict: Verdict::Deny,
                    reason: format!("{param}={n} exceeds {field}={bound}"),
                });
            }
            return Ok(GuardResult {
                name: "permit_limit".into(),
                verdict: Verdict::Allow,
                reason: "within permit".into(),
            });
        }
        if let Some(max) = obj.get("lte").and_then(|v| v.as_f64()) {
            let param = obj
                .get("param")
                .and_then(|v| v.as_str())
                .unwrap_or("target_do");
            let n = param_f64(params, param)?;
            if n > max {
                return Ok(GuardResult {
                    name: "lte".into(),
                    verdict: Verdict::Deny,
                    reason: format!("{param}={n} > {max}"),
                });
            }
            return Ok(GuardResult {
                name: "lte".into(),
                verdict: Verdict::Allow,
                reason: "ok".into(),
            });
        }
        if let Some(days) = obj
            .get("max_days_since_calibration")
            .and_then(|v| v.as_i64())
        {
            let object_param = obj
                .get("object")
                .and_then(|v| v.as_str())
                .unwrap_or("sensor");
            let id = param_str(params, object_param)?;
            let view = self.read_for_guard(&id, reads)?;
            let cal = view
                .properties
                .get("calibration_date")
                .and_then(|p| p.value.as_i64().or_else(|| p.value.as_str()?.parse().ok()))
                .unwrap_or(0);
            let elapsed = (self.now() - cal) / 86_400;
            if elapsed > days {
                return Ok(GuardResult {
                    name: "calibration".into(),
                    verdict: Verdict::Review,
                    reason: format!("sensor calibration is {elapsed} days old"),
                });
            }
            return Ok(GuardResult {
                name: "calibration".into(),
                verdict: Verdict::Allow,
                reason: "calibrated".into(),
            });
        }
        Ok(GuardResult {
            name: "unknown".into(),
            verdict: Verdict::Allow,
            reason: "unrecognized guard treated as pass".into(),
        })
    }

    fn read_for_guard(&self, id: &str, reads: &mut Vec<SnapshotObject>) -> Result<ObjectView> {
        let view = self.load_object_view(id, AsOf::Current)?;
        reads.push(snapshot_from_view(&view));
        Ok(view)
    }

    fn stage_effects(&self, effects: &Value, params: &Value, actor: &str) -> Result<Vec<StagedOp>> {
        let mut staged = Vec::new();
        let mut created = Vec::new();
        let Some(arr) = effects.as_array() else {
            return Ok(staged);
        };
        for effect in arr {
            let obj = effect
                .as_object()
                .ok_or_else(|| OntoError::Invalid("effect must be object".into()))?;
            if let Some(type_name) = obj.get("create").and_then(|v| v.as_str()) {
                let mut props = BTreeMap::new();
                if let Some(fields) = obj.get("properties").and_then(|v| v.as_object()) {
                    for (k, v) in fields {
                        let value = resolve_value(v, params);
                        props.insert(
                            k.clone(),
                            PropertyView {
                                value,
                                source: PropertySource::ActionWritten,
                                as_of: Some(self.now().to_string()),
                                provenance: Some(format!("actor:{actor}")),
                            },
                        );
                    }
                }
                let id = new_id();
                let title = props
                    .get("title")
                    .or_else(|| props.get("name"))
                    .and_then(|p| p.value.as_str().map(|s| s.to_string()));
                staged.push(StagedOp::InsertObject {
                    id: id.clone(),
                    type_name: type_name.into(),
                    title,
                    properties: serde_json::to_string(&props)?,
                    created_at: self.now(),
                });
                created.push(id);
            }
            if let Some(target_param) = obj.get("update").and_then(|v| v.as_str()) {
                let id = param_str(params, target_param)?;
                let mut view_props = {
                    let db = self.db.lock().expect("db");
                    let raw = bitemporal::current_properties(&db, &id)?;
                    serde_json::from_str::<BTreeMap<String, PropertyView>>(&raw)?
                };
                if let Some(fields) = obj.get("properties").and_then(|v| v.as_object()) {
                    for (k, v) in fields {
                        let value = resolve_value(v, params);
                        view_props.insert(
                            k.clone(),
                            PropertyView {
                                value,
                                source: PropertySource::ActionWritten,
                                as_of: Some(self.now().to_string()),
                                provenance: Some(format!("actor:{actor}")),
                            },
                        );
                    }
                }
                staged.push(StagedOp::UpdateObject {
                    id,
                    properties: serde_json::to_string(&view_props)?,
                });
            }
            if obj.get("link").is_some() {
                let link = match resolve_value(obj.get("link").unwrap_or(&Value::Null), params) {
                    Value::String(s) if !s.is_empty() => s,
                    _ => {
                        return Err(OntoError::Invalid("link effect needs a link type".into()));
                    }
                };
                let from = param_str(
                    params,
                    obj.get("from").and_then(|v| v.as_str()).unwrap_or("from"),
                )?;
                let to = if let Some(to_param) = obj.get("to").and_then(|v| v.as_str()) {
                    param_str(params, to_param)?
                } else if let Some(last) = created.last() {
                    last.clone()
                } else {
                    return Err(OntoError::Invalid("link effect needs to".into()));
                };
                staged.push(StagedOp::InsertLink {
                    id: new_id(),
                    type_name: link,
                    from_id: from,
                    to_id: to,
                });
            }
        }
        Ok(staged)
    }

    fn commit_staged(&self, ops: &[StagedOp]) -> Result<Vec<String>> {
        let at = self.now();
        let mut db = self.db.lock().expect("db");
        let tx = db.transaction()?;
        let mut created = Vec::new();
        for op in ops {
            match op {
                StagedOp::InsertObject {
                    id,
                    type_name,
                    title,
                    properties,
                    created_at,
                } => {
                    bitemporal::insert_object(
                        &tx,
                        id,
                        type_name,
                        title.as_deref(),
                        properties,
                        *created_at,
                    )?;
                    created.push(id.clone());
                }
                StagedOp::UpdateObject { id, properties } => {
                    bitemporal::append_version(&tx, id, properties, None, at)?;
                }
                StagedOp::InsertLink {
                    id,
                    type_name,
                    from_id,
                    to_id,
                } => {
                    tx.execute(
                        "INSERT INTO links(id, type_name, from_id, to_id) VALUES (?1, ?2, ?3, ?4)",
                        params![id, type_name, from_id, to_id],
                    )?;
                }
                StagedOp::InsertInbox {
                    id,
                    action_name,
                    proposed_by,
                    params: inbox_params,
                    created_at,
                } => {
                    tx.execute(
                        "INSERT INTO inbox(id, action_name, proposed_by, params, status, created_at)
                         VALUES (?1, ?2, ?3, ?4, 'pending', ?5)",
                        params![id, action_name, proposed_by, inbox_params, created_at],
                    )?;
                }
                StagedOp::ConfirmInbox { id } => {
                    tx.execute(
                        "UPDATE inbox SET status = 'confirmed' WHERE id = ?1",
                        params![id],
                    )?;
                }
            }
        }
        tx.commit()?;
        Ok(created)
    }

    fn persist_decision(
        &self,
        session: &Session,
        action: &str,
        confirmer: Option<&str>,
        verdict: Verdict,
        params: &Value,
        guards: &[GuardResult],
        effects: Value,
        snapshot: &DataSnapshot,
        trace: &[WritePathStep],
    ) -> Result<String> {
        let id = new_id();
        let verdict_s = match verdict {
            Verdict::Allow => "allow",
            Verdict::Review => "review",
            Verdict::Deny => "deny",
        };
        let db = self.db.lock().expect("db");
        db.execute(
            "INSERT INTO decision_records(
                id, action_name, actor, confirmer, verdict, params, guard_results, effects,
                rule_version, function_version, engine_version, data_snapshot, proof_trace, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                id,
                action,
                session.actor.id,
                confirmer,
                verdict_s,
                params.to_string(),
                serde_json::to_string(guards)?,
                effects.to_string(),
                snapshot.rule_version,
                snapshot.function_version,
                snapshot.engine_version,
                serde_json::to_string(snapshot)?,
                serde_json::to_string(trace)?,
                self.now()
            ],
        )?;
        Ok(id)
    }

    fn store_idempotency(
        &self,
        key: &str,
        action: &str,
        rec_id: &str,
        outcome: &ActionOutcome,
    ) -> Result<()> {
        let db = self.db.lock().expect("db");
        db.execute(
            "INSERT OR IGNORE INTO side_effect_keys(idempotency_key, decision_record_id, action_name, outcome, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                key,
                rec_id,
                action,
                serde_json::to_string(outcome)?,
                self.now()
            ],
        )?;
        Ok(())
    }

    fn load_cached_outcome(&self, key: &str) -> Result<Option<ActionOutcome>> {
        let db = self.db.lock().expect("db");
        let raw: Option<String> = db
            .query_row(
                "SELECT outcome FROM side_effect_keys WHERE idempotency_key = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()?;
        Ok(raw.map(|s| serde_json::from_str(&s)).transpose()?)
    }
}

struct AuthFail {
    name: String,
    reason: String,
}

fn snapshot_from_view(view: &ObjectView) -> SnapshotObject {
    SnapshotObject {
        id: view.id.clone(),
        type_name: view.type_name.clone(),
        properties: view
            .properties
            .iter()
            .map(|(k, p)| (k.clone(), p.value.clone()))
            .collect(),
    }
}

fn rule_version(spec: &ActionTypeSpec) -> String {
    pin_version(&spec.name, &serde_json::to_string(spec).unwrap_or_default())
}

fn param_str(params: &Value, name: &str) -> Result<String> {
    params
        .get(name)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| OntoError::Invalid(format!("param {name} must be a string id")))
}

fn param_f64(params: &Value, name: &str) -> Result<f64> {
    params
        .get(name)
        .and_then(|v| v.as_f64())
        .ok_or_else(|| OntoError::Invalid(format!("param {name} must be a number")))
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
    ]
    .into_iter()
    .map(|(name, description)| ToolSpec {
        name: name.into(),
        description: description.into(),
        input_schema: json!({ "type": "object" }),
    })
    .collect()
}

fn action_to_tool(action: &ActionTypeSpec) -> ToolSpec {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    for p in &action.parameters {
        properties.insert(p.name.clone(), json!({ "type": p.value_type }));
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
