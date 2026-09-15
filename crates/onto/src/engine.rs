use crate::error::{OntoError, Result};
use crate::types::*;
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
                title TEXT,
                properties TEXT NOT NULL,
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
        self.audit(
            &session.actor.id,
            "open_branch",
            json!({ "branch": name }),
        )?;
        Ok(name.to_string())
    }

    fn copy_schema(&self, from: &str, to: &str) -> Result<()> {
        let tables = [
            "schema_value_types",
            "schema_object_types",
            "schema_link_types",
            "schema_interfaces",
            "schema_action_types",
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

    fn put_spec(db: &Connection, table: &str, branch: &str, name: &str, spec: &Value) -> Result<()> {
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

    pub fn review_proposal(&self, session: &Session, proposal_id: &str, approve: bool) -> Result<()> {
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
        })
    }

    fn load_all<T: for<'de> DeserializeOwned>(&self, branch: &str, table: &str) -> Result<Vec<T>> {
        let db = self.db.lock().expect("db");
        let mut stmt =
            db.prepare(&format!("SELECT spec FROM {table} WHERE branch = ?1 ORDER BY name"))?;
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

    pub fn search_objects(&self, session: &Session, query: Query) -> Result<Vec<ObjectView>> {
        Self::require_consumer(session)?;
        let db = self.db.lock().expect("db");
        let mut sql = String::from("SELECT id FROM objects");
        let mut args: Vec<String> = Vec::new();
        if let Some(t) = &query.type_name {
            sql.push_str(" WHERE type_name = ?1");
            args.push(t.clone());
        }
        sql.push_str(" LIMIT ?");
        let mut stmt = db.prepare(&sql)?;
        let ids: Vec<String> = if args.is_empty() {
            stmt.query_map(params![query.limit as i64], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        } else {
            stmt.query_map(params![args[0], query.limit as i64], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        drop(stmt);
        drop(db);
        let mut out = Vec::new();
        for id in ids {
            let view = self.load_object_view(&id)?;
            if self.matches_equals(&view, &query.equals) {
                out.push(self.filter_view(session, view));
            }
        }
        Ok(out)
    }

    fn matches_equals(&self, view: &ObjectView, equals: &BTreeMap<String, Value>) -> bool {
        equals.iter().all(|(k, v)| {
            view.properties
                .get(k)
                .is_some_and(|p| p.value == *v)
        })
    }

    fn filter_view(&self, session: &Session, mut view: ObjectView) -> ObjectView {
        if session.actor.has_role("restricted") {
            view.properties.retain(|name, _| name != "rationale");
        }
        view
    }

    pub fn get_object(&self, session: &Session, id: &str) -> Result<ObjectView> {
        Self::require_consumer(session)?;
        Ok(self.filter_view(session, self.load_object_view(id)?))
    }

    fn load_object_view(&self, id: &str) -> Result<ObjectView> {
        let db = self.db.lock().expect("db");
        let (type_name, title, props): (String, Option<String>, String) = db
            .query_row(
                "SELECT type_name, title, properties FROM objects WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?
            .ok_or_else(|| OntoError::NotFound(format!("object {id}")))?;
        drop(db);
        let raw: BTreeMap<String, PropertyView> = serde_json::from_str(&props)?;
        let spec = self.load_object_type(MAIN_BRANCH, &type_name).ok();
        let mut properties = raw;
        if let Some(spec) = &spec {
            for prop in &spec.properties {
                if prop.source == PropertySource::Derived && prop.name == "days_since_calibration" {
                    if let Some(cal) = properties.get("calibration_date") {
                        if let Some(as_of) = cal.value.as_i64().or_else(|| {
                            cal.value.as_str().and_then(|s| s.parse::<i64>().ok())
                        }) {
                            let days = ((self.now() - as_of).max(0)) / 86_400;
                            properties.insert(
                                prop.name.clone(),
                                PropertyView {
                                    value: json!(days),
                                    source: PropertySource::Derived,
                                    as_of: Some(self.now().to_string()),
                                    provenance: Some("function:days_since_calibration".into()),
                                },
                            );
                        }
                    }
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
                    if let (Some(budget), Some(as_of)) = (spec.freshness_budget_secs, &pv.as_of) {
                        if let Ok(ts) = as_of.parse::<i64>() {
                            if self.now() - ts > budget {
                                stale.push(prop.name);
                            }
                        }
                    }
                }
            }
        }
        Ok(ObjectView {
            id: id.to_string(),
            type_name,
            title,
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
            out.push(self.get_object(session, &tid)?);
        }
        Ok(out)
    }

    pub fn aggregate(&self, session: &Session, type_name: &str) -> Result<Value> {
        Self::require_consumer(session)?;
        let db = self.db.lock().expect("db");
        let count: i64 = db.query_row(
            "SELECT COUNT(*) FROM objects WHERE type_name = ?1",
            params![type_name],
            |r| r.get(0),
        )?;
        Ok(json!({ "type_name": type_name, "count": count }))
    }

    pub fn list_missing_evidence(&self, session: &Session, id: &str) -> Result<Value> {
        let view = self.get_object(session, id)?;
        Ok(json!({
            "id": view.id,
            "missing": view.missing,
            "stale": view.stale
        }))
    }

    pub fn create_link(
        &self,
        session: &Session,
        type_name: &str,
        from_id: &str,
        to_id: &str,
    ) -> Result<String> {
        Self::require_consumer(session)?;
        let id = new_id();
        let db = self.db.lock().expect("db");
        db.execute(
            "INSERT INTO links(id, type_name, from_id, to_id) VALUES (?1, ?2, ?3, ?4)",
            params![id, type_name, from_id, to_id],
        )?;
        Ok(id)
    }

    pub fn funnel_ingest(&self, session: &Session, records: Vec<IngestRecord>) -> Result<Vec<String>> {
        Self::require_consumer(session)?;
        let mut ids = Vec::new();
        for rec in records {
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
        let as_of = rec
            .as_of
            .clone()
            .unwrap_or_else(|| self.now().to_string());
        let id = rec.id.clone().unwrap_or_else(new_id);
        let existing = {
            let db = self.db.lock().expect("db");
            db.query_row(
                "SELECT properties FROM objects WHERE id = ?1",
                params![id],
                |r| r.get::<_, String>(0),
            )
            .optional()?
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
        let title = spec
            .title_prop
            .as_ref()
            .and_then(|t| props.get(t).and_then(|p| p.value.as_str().map(|s| s.to_string())));
        let db = self.db.lock().expect("db");
        if existing.is_some() {
            db.execute(
                "UPDATE objects SET properties = ?1, title = ?2 WHERE id = ?3",
                params![serde_json::to_string(&props)?, title, id],
            )?;
        } else {
            db.execute(
                "INSERT INTO objects(id, type_name, title, properties, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id, rec.type_name, title, serde_json::to_string(&props)?, self.now()],
            )?;
        }
        Ok(id)
    }

    pub fn list_inbox(&self, session: &Session) -> Result<Vec<InboxItem>> {
        Self::require_consumer(session)?;
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
            KeyKind::Consumer => self.load_action_type(MAIN_BRANCH, name),
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
                let schema = self.get_schema(session, None)?;
                for action in schema.action_types {
                    if session.actor.tier >= action.required_tier {
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
        let spec = self.load_action_type(MAIN_BRANCH, action_name)?;
        self.execute_action(session, &spec, params, None)
    }

    pub fn confirm_action(&self, session: &Session, inbox_id: &str) -> Result<ActionOutcome> {
        Self::require_consumer(session)?;
        if !session.actor.has_role("supervisor") && session.actor.tier < 3 {
            return Err(OntoError::Denied("confirmer must be supervisor or tier 3+".into()));
        }
        let (action_name, params, status): (String, String, String) = {
            let db = self.db.lock().expect("db");
            db.query_row(
                "SELECT action_name, params, status FROM inbox WHERE id = ?1",
                params![inbox_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?
        };
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
        if outcome.verdict == Verdict::Allow {
            let db = self.db.lock().expect("db");
            db.execute(
                "UPDATE inbox SET status = 'confirmed' WHERE id = ?1",
                params![inbox_id],
            )?;
        }
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
        if !session.actor.has_role("supervisor") {
            return Err(OntoError::Denied("override requires supervisor".into()));
        }
        let params = {
            let db = self.db.lock().expect("db");
            let (action_name, raw, status): (String, String, String) = db.query_row(
                "SELECT action_name, params, status FROM inbox WHERE id = ?1",
                params![inbox_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            if status != "pending" {
                return Err(OntoError::Conflict(format!("inbox item is {status}")));
            }
            let mut p: Value = serde_json::from_str(&raw)?;
            if let Value::Object(map) = &mut p {
                map.insert("override_category".into(), json!(category));
                map.insert("override_reason".into(), json!(reason));
                map.insert("source_action".into(), json!(action_name));
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
        let rec = self.seal_decision(
            session,
            "override_setpoint",
            Some(&session.actor.id),
            Verdict::Allow,
            params.clone(),
            vec![GuardResult {
                name: "override".into(),
                verdict: Verdict::Allow,
                reason: format!("{category}: {reason}"),
            }],
            json!({ "inbox": inbox_id }),
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

    pub fn get_decision_record(&self, session: &Session, id: &str) -> Result<DecisionRecordView> {
        Self::require_consumer(session)?;
        let db = self.db.lock().expect("db");
        db.query_row(
            "SELECT id, action_name, actor, confirmer, verdict, params, guard_results, effects, rule_version, function_version, engine_version, data_snapshot, created_at
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
        if session.actor.tier < spec.required_tier {
            return self.deny(
                session,
                spec,
                params,
                "authorization",
                format!("tier {} required, actor is {}", spec.required_tier, session.actor.tier),
            );
        }
        for role in &spec.required_roles {
            if !session.actor.has_role(role) {
                return self.deny(
                    session,
                    spec,
                    params,
                    "authorization",
                    format!("role {role} required"),
                );
            }
        }
        self.validate_params(spec, &params)?;
        let guard_results = self.evaluate_guards(&spec.guards, &params)?;
        let worst = worst_verdict(&guard_results);
        match worst {
            Verdict::Deny => {
                let reason = guard_results
                    .iter()
                    .find(|g| g.verdict == Verdict::Deny)
                    .map(|g| g.reason.clone())
                    .unwrap_or_else(|| "denied".into());
                let rec = self.seal_decision(
                    session,
                    &spec.name,
                    confirmer_inbox.map(|_| session.actor.id.as_str()),
                    Verdict::Deny,
                    params,
                    guard_results.clone(),
                    json!({}),
                )?;
                Ok(ActionOutcome {
                    verdict: Verdict::Deny,
                    reason,
                    decision_record_id: Some(rec),
                    inbox_id: None,
                    created_ids: vec![],
                    alternative: spec.on_review.clone(),
                    guard_results,
                })
            }
            Verdict::Review => {
                let reason = guard_results
                    .iter()
                    .find(|g| g.verdict == Verdict::Review)
                    .map(|g| g.reason.clone())
                    .unwrap_or_else(|| "needs review".into());
                let rec = self.seal_decision(
                    session,
                    &spec.name,
                    None,
                    Verdict::Review,
                    params.clone(),
                    guard_results.clone(),
                    json!({ "alternative": spec.on_review }),
                )?;
                Ok(ActionOutcome {
                    verdict: Verdict::Review,
                    reason,
                    decision_record_id: Some(rec),
                    inbox_id: None,
                    created_ids: vec![],
                    alternative: spec.on_review.clone(),
                    guard_results,
                })
            }
            Verdict::Allow => match spec.mode {
                ExecutionMode::Propose | ExecutionMode::Shadow => {
                    let inbox_id = new_id();
                    let db = self.db.lock().expect("db");
                    db.execute(
                        "INSERT INTO inbox(id, action_name, proposed_by, params, status, created_at)
                         VALUES (?1, ?2, ?3, ?4, 'pending', ?5)",
                        params![
                            inbox_id,
                            spec.name,
                            session.actor.id,
                            params.to_string(),
                            self.now()
                        ],
                    )?;
                    drop(db);
                    let rec = self.seal_decision(
                        session,
                        &spec.name,
                        None,
                        Verdict::Allow,
                        params,
                        guard_results.clone(),
                        json!({ "inbox": inbox_id, "mode": spec.mode }),
                    )?;
                    Ok(ActionOutcome {
                        verdict: Verdict::Allow,
                        reason: "proposed".into(),
                        decision_record_id: Some(rec),
                        inbox_id: Some(inbox_id),
                        created_ids: vec![],
                        alternative: None,
                        guard_results,
                    })
                }
                ExecutionMode::Auto | ExecutionMode::Approve => {
                    let created = self.apply_effects(&spec.effects, &params, &session.actor.id)?;
                    let rec = self.seal_decision(
                        session,
                        &spec.name,
                        Some(&session.actor.id),
                        Verdict::Allow,
                        params,
                        guard_results.clone(),
                        json!({ "created": created, "side_effects": spec.side_effects }),
                    )?;
                    Ok(ActionOutcome {
                        verdict: Verdict::Allow,
                        reason: "committed".into(),
                        decision_record_id: Some(rec),
                        inbox_id: confirmer_inbox.map(|s| s.to_string()),
                        created_ids: created,
                        alternative: None,
                        guard_results,
                    })
                }
            },
        }
    }

    fn deny(
        &self,
        session: &Session,
        spec: &ActionTypeSpec,
        params: Value,
        name: &str,
        reason: String,
    ) -> Result<ActionOutcome> {
        let guards = vec![GuardResult {
            name: name.into(),
            verdict: Verdict::Deny,
            reason: reason.clone(),
        }];
        let rec = self.seal_decision(
            session,
            &spec.name,
            None,
            Verdict::Deny,
            params,
            guards.clone(),
            json!({}),
        )?;
        Ok(ActionOutcome {
            verdict: Verdict::Deny,
            reason,
            decision_record_id: Some(rec),
            inbox_id: None,
            created_ids: vec![],
            alternative: None,
            guard_results: guards,
        })
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
                    let n = val
                        .as_f64()
                        .ok_or_else(|| OntoError::Invalid(format!("{} must be a number", p.name)))?;
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
                let id = val
                    .as_str()
                    .ok_or_else(|| OntoError::Invalid(format!("{} must be an object id", p.name)))?;
                let view = self.load_object_view(id)?;
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

    fn evaluate_guards(&self, guards: &Value, params: &Value) -> Result<Vec<GuardResult>> {
        if guards.is_null() || guards == &json!({}) {
            return Ok(vec![GuardResult {
                name: "empty".into(),
                verdict: Verdict::Allow,
                reason: "no guards".into(),
            }]);
        }
        let Some(arr) = guards.as_array() else {
            return Ok(vec![self.eval_one_guard(guards, params)?]);
        };
        let mut out = Vec::new();
        for g in arr {
            out.push(self.eval_one_guard(g, params)?);
        }
        Ok(out)
    }

    fn eval_one_guard(&self, guard: &Value, params: &Value) -> Result<GuardResult> {
        let obj = guard
            .as_object()
            .ok_or_else(|| OntoError::Invalid("guard must be object".into()))?;
        if let Some(name) = obj.get("freshness").and_then(|v| v.as_str()) {
            let max = obj.get("max_age_secs").and_then(|v| v.as_i64()).unwrap_or(300);
            let id = param_str(params, name)?;
            let view = self.load_object_view(&id)?;
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
                    reason: format!("stale {name}: age {}s > {max}s (Current fail)", self.now() - ts),
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
            let view = self.load_object_view(&id)?;
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
            let object_param = obj.get("object").and_then(|v| v.as_str()).unwrap_or("permit");
            let field = obj.get("field").and_then(|v| v.as_str()).unwrap_or("do_max");
            let n = param_f64(params, param)?;
            let permit_id = param_str(params, object_param)?;
            let view = self.load_object_view(&permit_id)?;
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
            let param = obj.get("param").and_then(|v| v.as_str()).unwrap_or("target_do");
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
        if let Some(days) = obj.get("max_days_since_calibration").and_then(|v| v.as_i64()) {
            let object_param = obj.get("object").and_then(|v| v.as_str()).unwrap_or("sensor");
            let id = param_str(params, object_param)?;
            let view = self.load_object_view(&id)?;
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

    fn apply_effects(
        &self,
        effects: &Value,
        params: &Value,
        actor: &str,
    ) -> Result<Vec<String>> {
        let mut created = Vec::new();
        let Some(arr) = effects.as_array() else {
            return Ok(created);
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
                let db = self.db.lock().expect("db");
                db.execute(
                    "INSERT INTO objects(id, type_name, title, properties, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![id, type_name, title, serde_json::to_string(&props)?, self.now()],
                )?;
                created.push(id);
            }
            if let Some(target_param) = obj.get("update").and_then(|v| v.as_str()) {
                let id = param_str(params, target_param)?;
                let mut view_props = {
                    let db = self.db.lock().expect("db");
                    let raw: String = db.query_row(
                        "SELECT properties FROM objects WHERE id = ?1",
                        params![id],
                        |r| r.get(0),
                    )?;
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
                let db = self.db.lock().expect("db");
                db.execute(
                    "UPDATE objects SET properties = ?1 WHERE id = ?2",
                    params![serde_json::to_string(&view_props)?, id],
                )?;
            }
            if let Some(link) = obj.get("link").and_then(|v| v.as_str()) {
                let from = param_str(params, obj.get("from").and_then(|v| v.as_str()).unwrap_or("from"))?;
                let to = if let Some(to_param) = obj.get("to").and_then(|v| v.as_str()) {
                    param_str(params, to_param)?
                } else if let Some(last) = created.last() {
                    last.clone()
                } else {
                    return Err(OntoError::Invalid("link effect needs to".into()));
                };
                let lid = new_id();
                let db = self.db.lock().expect("db");
                db.execute(
                    "INSERT INTO links(id, type_name, from_id, to_id) VALUES (?1, ?2, ?3, ?4)",
                    params![lid, link, from, to],
                )?;
            }
        }
        Ok(created)
    }

    fn seal_decision(
        &self,
        session: &Session,
        action: &str,
        confirmer: Option<&str>,
        verdict: Verdict,
        params: Value,
        guards: Vec<GuardResult>,
        effects: Value,
    ) -> Result<String> {
        let id = new_id();
        let verdict_s = match verdict {
            Verdict::Allow => "allow",
            Verdict::Review => "review",
            Verdict::Deny => "deny",
        };
        let snapshot = json!({
            "actor": session.actor.id,
            "purpose": session.purpose,
            "clock": self.now()
        });
        let rule_version = self
            .load_action_type(MAIN_BRANCH, action)
            .ok()
            .map(|a| a.name)
            .unwrap_or_else(|| action.to_string());
        let db = self.db.lock().expect("db");
        db.execute(
            "INSERT INTO decision_records(
                id, action_name, actor, confirmer, verdict, params, guard_results, effects,
                rule_version, function_version, engine_version, data_snapshot, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                id,
                action,
                session.actor.id,
                confirmer,
                verdict_s,
                params.to_string(),
                serde_json::to_string(&guards)?,
                effects.to_string(),
                rule_version,
                "fn-0.1",
                ENGINE_VERSION,
                snapshot.to_string(),
                self.now()
            ],
        )?;
        Ok(id)
    }
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
        ("open_branch", "Open a working schema branch copied from main"),
        ("create_value_type", "Create a value type on a branch"),
        ("create_object_type", "Create an object type on a branch"),
        ("alter_object_type", "Replace an object type on a branch"),
        ("add_property", "Add a property to an object type on a branch"),
        ("alter_property", "Alter a property on a branch"),
        ("archive_object_type", "Archive an object type on a branch"),
        ("create_link_type", "Create a link type on a branch"),
        ("alter_link_type", "Alter a link type on a branch"),
        ("create_interface", "Create an interface on a branch"),
        ("attach_interface", "Attach an interface to an object type"),
        ("create_action_type", "Create an action type on a branch"),
        ("alter_action_type", "Alter an action type on a branch"),
        ("submit_proposal", "Submit a branch for review"),
        ("review_proposal", "Approve or reject a proposal"),
        ("merge_to_main", "Merge an approved proposal into main"),
        ("get_schema", "Read schema of a branch (never production instances)"),
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
        ("search_objects", "Permission-first object search"),
        ("get_object", "Load one object with freshness, provenance, missing fields"),
        ("traverse_links", "Cycle-aware link traversal"),
        ("aggregate", "Count objects of a type"),
        ("list_missing_evidence", "Missing and stale fields for an object"),
        ("describe_action", "Projected Action card"),
        ("submit_action", "Submit a predefined Action"),
        ("list_inbox", "Pending proposals as objects"),
        ("confirm_action", "Confirm a pending proposal (TOCTOU revalidation)"),
        ("override_action", "Categorized override of a pending proposal"),
        ("get_decision_record", "Replayable decision dossier"),
        ("get_rejection", "Structured rejection reasons"),
        ("funnel_ingest", "Event-path ingest; never overwrites ActionWritten"),
        ("create_link", "Create a named link between instances"),
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
