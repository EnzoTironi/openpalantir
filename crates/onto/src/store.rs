//! Persistence seam. `SQLite` is the teaching backend.
//!
//! [`SqliteStore`] owns the connection and the lock. [`Engine`](crate::Engine)
//! coordinates. A second backend implements [`Store`] without rewriting
//! `write_path`, OSS, or functions. No public method here takes a rusqlite type.

use crate::bitemporal::{self, AsOf, LoadedVersion, VersionSpan};
use crate::command::{DecisionApply, IdempotencyRow};
use crate::decision::EffectStatus;
use crate::error::{OntoError, Result};
use crate::oss::OBJECT_SETS_TABLE;
use crate::security::POLICIES_TABLE;
use crate::types::{
    DataSnapshot, DecisionRecordView, GuardResult, InboxItem, Verdict, WritePathStep, MAIN_BRANCH,
};
use crate::write_path::{pin_version, StagedOp};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde_json::Value;
use std::sync::{Mutex, MutexGuard};

const SCHEMA_TABLES: [&str; 8] = [
    "schema_value_types",
    "schema_object_types",
    "schema_link_types",
    "schema_interfaces",
    "schema_action_types",
    "schema_functions",
    OBJECT_SETS_TABLE,
    POLICIES_TABLE,
];

const OMS_SCHEMA: &str = r"
            PRAGMA foreign_keys = ON;
            CREATE TABLE IF NOT EXISTS branches (
                name TEXT PRIMARY KEY,
                status TEXT NOT NULL,
                created_by TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                base_revision TEXT NOT NULL DEFAULT ''
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
                created_at INTEGER NOT NULL,
                rule_pin TEXT NOT NULL DEFAULT '',
                apply_action TEXT NOT NULL DEFAULT ''
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
                created_at INTEGER NOT NULL,
                payload_digest TEXT NOT NULL DEFAULT ''
            );
            CREATE TABLE IF NOT EXISTS audit_log (
                id TEXT PRIMARY KEY,
                at INTEGER NOT NULL,
                actor TEXT NOT NULL,
                kind TEXT NOT NULL,
                payload TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS effect_intentions (
                decision_record_id TEXT PRIMARY KEY,
                declaration TEXT NOT NULL,
                status TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            ";

/// Persistence operations. One impl now: [`SqliteStore`].
pub trait Store: Send + Sync {
    fn init(&self) -> Result<()>;
    fn insert_audit(&self, id: &str, at: i64, actor: &str, kind: &str, payload: &str)
        -> Result<()>;
    fn branch_status(&self, name: &str) -> Result<Option<String>>;
    fn insert_open_branch(
        &self,
        name: &str,
        created_by: &str,
        at: i64,
        base_revision: &str,
    ) -> Result<()>;
    fn set_branch_status(&self, name: &str, status: &str) -> Result<()>;
    fn copy_schema(&self, from: &str, to: &str) -> Result<()>;
    fn replace_main_schema(&self, from: &str) -> Result<()>;
    fn replace_main_schema_if_base(&self, from: &str, expected_base: &str) -> Result<()>;
    fn schema_revision(&self, branch: &str) -> Result<String>;
    fn branch_base_revision(&self, name: &str) -> Result<Option<String>>;
    fn put_spec(&self, table: &str, branch: &str, name: &str, spec: &str) -> Result<()>;
    fn delete_spec(&self, table: &str, branch: &str, name: &str) -> Result<()>;
    fn load_specs(&self, branch: &str, table: &str) -> Result<Vec<String>>;
    fn load_spec(&self, table: &str, branch: &str, name: &str) -> Result<Option<String>>;
    fn insert_proposal(&self, id: &str, branch: &str, submitted_by: &str, at: i64) -> Result<()>;
    fn proposal_branch_status(&self, id: &str) -> Result<(String, String)>;
    fn set_proposal(&self, id: &str, status: &str, reviewed_by: Option<&str>) -> Result<()>;
    fn candidate_ids(&self, type_name: Option<&str>) -> Result<Vec<String>>;
    fn load_version(&self, id: &str, as_of: AsOf) -> Result<LoadedVersion>;
    fn list_spans(&self, id: &str) -> Result<Vec<VersionSpan>>;
    fn identity_exists(&self, id: &str) -> Result<bool>;
    fn current_properties(&self, id: &str) -> Result<String>;
    fn insert_object(
        &self,
        id: &str,
        type_name: &str,
        title: Option<&str>,
        properties: &str,
        at: i64,
    ) -> Result<()>;
    fn append_version(
        &self,
        id: &str,
        properties: &str,
        title: Option<&str>,
        at: i64,
    ) -> Result<String>;
    fn link_targets(&self, from_id: &str, link_type: &str) -> Result<Vec<String>>;
    fn list_inbox(&self) -> Result<Vec<InboxItem>>;
    fn load_inbox(&self, id: &str) -> Result<InboxRow>;
    fn set_inbox_status(&self, id: &str, status: &str) -> Result<()>;
    fn insert_decision(&self, rec: &DecisionWrite<'_>) -> Result<()>;
    fn load_decision(&self, id: &str) -> Result<DecisionRecordView>;
    fn put_idempotency(
        &self,
        key: &str,
        rec_id: &str,
        action: &str,
        outcome: &str,
        at: i64,
    ) -> Result<()>;
    fn get_idempotency(&self, key: &str) -> Result<Option<IdempotencyRow>>;
    fn commit_staged(&self, ops: &[StagedOp], at: i64) -> Result<Vec<String>>;
    fn commit_decision(&self, unit: &DecisionCommit<'_>) -> Result<DecisionApply>;
    fn append_version_recorded(
        &self,
        id: &str,
        properties: &str,
        title: Option<&str>,
        valid_at: i64,
        tx_at: i64,
    ) -> Result<String>;
    fn insert_object_recorded(
        &self,
        id: &str,
        type_name: &str,
        title: Option<&str>,
        properties: &str,
        valid_at: i64,
        tx_at: i64,
    ) -> Result<()>;
    fn object_type_of(&self, id: &str) -> Result<String>;
}

/// Inbox row including the pinned apply Action and rule digest.
#[derive(Debug, Clone)]
pub struct InboxRow {
    pub action_name: String,
    pub params: String,
    pub status: String,
    pub proposed_by: String,
    pub rule_pin: String,
    pub apply_action: String,
}

/// One transactional decision: write-set, dossier, idempotency, intention.
pub struct DecisionCommit<'a> {
    pub ops: &'a [StagedOp],
    pub decision: DecisionWrite<'a>,
    pub idempotency_key: Option<&'a str>,
    pub idempotency_outcome: Option<&'a str>,
    pub payload_digest: Option<&'a str>,
    pub read_set: &'a [(String, String)],
    pub schema_revision: Option<&'a str>,
    pub audit_id: Option<&'a str>,
    pub audit_actor: Option<&'a str>,
    pub audit_kind: Option<&'a str>,
    pub audit_payload: Option<&'a str>,
    pub effect_declaration: Option<&'a str>,
}

/// Columns for one `DecisionRecord` insert. Not a service layer.
pub struct DecisionWrite<'a> {
    pub id: &'a str,
    pub action: &'a str,
    pub actor: &'a str,
    pub confirmer: Option<&'a str>,
    pub verdict: Verdict,
    pub params: &'a Value,
    pub guards: &'a [GuardResult],
    pub effects: &'a Value,
    pub snapshot: &'a DataSnapshot,
    pub trace: &'a [WritePathStep],
    pub at: i64,
}

/// `SQLite` teaching backend. Owns the connection mutex.
#[allow(clippy::module_name_repetitions)] // the Store seam's sqlite impl
pub struct SqliteStore {
    db: Mutex<Connection>,
}

impl SqliteStore {
    pub fn memory() -> Result<Self> {
        let store = Self {
            db: Mutex::new(Connection::open_in_memory()?),
        };
        store.init()?;
        Ok(store)
    }

    pub fn open(path: &str) -> Result<Self> {
        let store = Self {
            db: Mutex::new(Connection::open(path)?),
        };
        store.init()?;
        Ok(store)
    }

    fn conn(&self) -> Result<MutexGuard<'_, Connection>> {
        self.db
            .lock()
            .map_err(|_| OntoError::Store("sqlite mutex poisoned".into()))
    }
}

fn schema_table(table: &str) -> Result<&str> {
    SCHEMA_TABLES
        .iter()
        .copied()
        .find(|t| *t == table)
        .ok_or_else(|| OntoError::Invalid(format!("unknown schema table {table}")))
}

fn schema_revision_on(db: &Connection, branch: &str) -> Result<String> {
    let mut body = String::new();
    for table in SCHEMA_TABLES {
        let mut stmt = db.prepare(&format!(
            "SELECT name, spec FROM {table} WHERE branch = ?1 ORDER BY name"
        ))?;
        let rows = stmt.query_map(params![branch], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (name, spec) = row?;
            body.push_str(table);
            body.push('\n');
            body.push_str(&name);
            body.push('\n');
            body.push_str(&spec);
            body.push('\n');
        }
    }
    Ok(pin_version(branch, &body))
}

fn copy_branch_over_main(tx: &Transaction<'_>, from: &str) -> Result<()> {
    for table in SCHEMA_TABLES {
        tx.execute(
            &format!("DELETE FROM {table} WHERE branch = ?1"),
            params![MAIN_BRANCH],
        )?;
        tx.execute(
            &format!(
                "INSERT INTO {table}(branch, name, spec)
                 SELECT ?1, name, spec FROM {table} WHERE branch = ?2"
            ),
            params![MAIN_BRANCH, from],
        )?;
    }
    Ok(())
}

fn apply_staged(tx: &Transaction<'_>, ops: &[StagedOp], at: i64) -> Result<Vec<String>> {
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
                    tx,
                    id,
                    type_name,
                    title.as_deref(),
                    properties,
                    *created_at,
                )?;
                created.push(id.clone());
            }
            StagedOp::UpdateObject { id, properties } => {
                bitemporal::append_version(tx, id, properties, None, at)?;
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
                status,
                rule_pin,
                apply_action,
            } => {
                tx.execute(
                    "INSERT INTO inbox(id, action_name, proposed_by, params, status, created_at, rule_pin, apply_action)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        id,
                        action_name,
                        proposed_by,
                        inbox_params,
                        status,
                        created_at,
                        rule_pin,
                        apply_action
                    ],
                )?;
            }
            StagedOp::ConfirmInbox { id } => {
                let n = tx.execute(
                    "UPDATE inbox SET status = 'confirmed' WHERE id = ?1 AND status = 'pending'",
                    params![id],
                )?;
                if n != 1 {
                    return Err(OntoError::Conflict(format!(
                        "inbox {id} is not exclusively pending"
                    )));
                }
            }
            StagedOp::CloseLink {
                type_name,
                from_id,
                to_id,
            } => {
                let n = tx.execute(
                    "DELETE FROM links WHERE type_name = ?1 AND from_id = ?2 AND to_id = ?3",
                    params![type_name, from_id, to_id],
                )?;
                if n == 0 {
                    return Err(OntoError::NotFound(format!(
                        "link {type_name} {from_id}->{to_id}"
                    )));
                }
            }
        }
    }
    Ok(created)
}

fn check_read_set(tx: &Transaction<'_>, read_set: &[(String, String)]) -> Result<()> {
    for (id, expected) in read_set {
        if expected.is_empty() {
            continue;
        }
        let current = match bitemporal::load(tx, id, AsOf::Current) {
            Ok(v) => v.version_id,
            Err(OntoError::NotFound(_)) => {
                return Err(OntoError::StaleRead);
            }
            Err(e) => return Err(e),
        };
        if current != *expected {
            return Err(OntoError::StaleRead);
        }
    }
    Ok(())
}

fn unique_violation(err: &rusqlite::Error) -> bool {
    matches!(
        err,
        rusqlite::Error::SqliteFailure(f, _)
            if f.code == rusqlite::ErrorCode::ConstraintViolation
    )
}

fn load_idempotency_row(tx: &Transaction<'_>, key: &str) -> Result<Option<IdempotencyRow>> {
    Ok(tx
        .query_row(
            "SELECT outcome, COALESCE(payload_digest, '') FROM side_effect_keys WHERE idempotency_key = ?1",
            params![key],
            |r| {
                Ok(IdempotencyRow {
                    outcome: r.get(0)?,
                    payload_digest: r.get(1)?,
                })
            },
        )
        .optional()?)
}

fn reserve_idempotency(
    tx: &Transaction<'_>,
    key: &str,
    rec_id: &str,
    action: &str,
    outcome: &str,
    digest: &str,
    at: i64,
) -> Result<Option<String>> {
    match tx.execute(
        "INSERT INTO side_effect_keys(idempotency_key, decision_record_id, action_name, outcome, created_at, payload_digest)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![key, rec_id, action, outcome, at, digest],
    ) {
        Ok(_) => Ok(None),
        Err(e) if unique_violation(&e) => {
            let row = load_idempotency_row(tx, key)?
                .ok_or_else(|| OntoError::Conflict(format!("idempotency {key} collided")))?;
            if row.payload_digest != digest {
                return Err(OntoError::Conflict(
                    "idempotency key is bound to a different payload".into(),
                ));
            }
            Ok(Some(row.outcome))
        }
        Err(e) => Err(e.into()),
    }
}

fn insert_decision_on(tx: &Transaction<'_>, rec: &DecisionWrite<'_>) -> Result<()> {
    tx.execute(
        "INSERT INTO decision_records(
            id, action_name, actor, confirmer, verdict, params, guard_results, effects,
            rule_version, function_version, engine_version, data_snapshot, proof_trace, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            rec.id,
            rec.action,
            rec.actor,
            rec.confirmer,
            rec.verdict.as_stored(),
            rec.params.to_string(),
            serde_json::to_string(rec.guards)?,
            rec.effects.to_string(),
            rec.snapshot.rule_version,
            rec.snapshot.function_version,
            rec.snapshot.engine_version,
            serde_json::to_string(rec.snapshot)?,
            serde_json::to_string(rec.trace)?,
            rec.at
        ],
    )?;
    Ok(())
}

impl Store for SqliteStore {
    fn init(&self) -> Result<()> {
        let db = self.conn()?;
        db.execute_batch(OMS_SCHEMA)?;
        db.execute_batch(bitemporal::SCHEMA)?;
        let _ = db.execute(
            "ALTER TABLE decision_records ADD COLUMN proof_trace TEXT NOT NULL DEFAULT '[]'",
            [],
        );
        let _ = db.execute(
            "ALTER TABLE branches ADD COLUMN base_revision TEXT NOT NULL DEFAULT ''",
            [],
        );
        let _ = db.execute(
            "ALTER TABLE inbox ADD COLUMN rule_pin TEXT NOT NULL DEFAULT ''",
            [],
        );
        let _ = db.execute(
            "ALTER TABLE inbox ADD COLUMN apply_action TEXT NOT NULL DEFAULT ''",
            [],
        );
        let _ = db.execute(
            "ALTER TABLE side_effect_keys ADD COLUMN payload_digest TEXT NOT NULL DEFAULT ''",
            [],
        );
        db.execute(
            "INSERT OR IGNORE INTO branches(name, status, created_by, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![MAIN_BRANCH, "merged", "kernel", 0],
        )?;
        crate::kernel::install(&db, 1_700_000_000)?;
        Ok(())
    }

    fn insert_audit(
        &self,
        id: &str,
        at: i64,
        actor: &str,
        kind: &str,
        payload: &str,
    ) -> Result<()> {
        self.conn()?.execute(
            "INSERT INTO audit_log(id, at, actor, kind, payload) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, at, actor, kind, payload],
        )?;
        Ok(())
    }

    fn branch_status(&self, name: &str) -> Result<Option<String>> {
        Ok(self
            .conn()?
            .query_row(
                "SELECT status FROM branches WHERE name = ?1",
                params![name],
                |r| r.get(0),
            )
            .optional()?)
    }

    fn insert_open_branch(
        &self,
        name: &str,
        created_by: &str,
        at: i64,
        base_revision: &str,
    ) -> Result<()> {
        self.conn()?.execute(
            "INSERT INTO branches(name, status, created_by, created_at, base_revision)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![name, "open", created_by, at, base_revision],
        )?;
        Ok(())
    }

    fn set_branch_status(&self, name: &str, status: &str) -> Result<()> {
        self.conn()?.execute(
            "UPDATE branches SET status = ?1 WHERE name = ?2",
            params![status, name],
        )?;
        Ok(())
    }

    fn copy_schema(&self, from: &str, to: &str) -> Result<()> {
        let db = self.conn()?;
        for table in SCHEMA_TABLES {
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

    fn replace_main_schema(&self, from: &str) -> Result<()> {
        let mut db = self.conn()?;
        let tx = db.transaction()?;
        copy_branch_over_main(&tx, from)?;
        tx.commit()?;
        Ok(())
    }

    fn replace_main_schema_if_base(&self, from: &str, expected_base: &str) -> Result<()> {
        let mut db = self.conn()?;
        let tx = db.transaction()?;
        let current = schema_revision_on(&tx, MAIN_BRANCH)?;
        if current != expected_base {
            return Err(OntoError::Conflict(
                "stale branch: main changed since this branch was opened".into(),
            ));
        }
        copy_branch_over_main(&tx, from)?;
        tx.commit()?;
        Ok(())
    }

    fn schema_revision(&self, branch: &str) -> Result<String> {
        let db = self.conn()?;
        schema_revision_on(&db, branch)
    }

    fn branch_base_revision(&self, name: &str) -> Result<Option<String>> {
        Ok(self
            .conn()?
            .query_row(
                "SELECT base_revision FROM branches WHERE name = ?1",
                params![name],
                |r| r.get(0),
            )
            .optional()?)
    }

    fn put_spec(&self, table: &str, branch: &str, name: &str, spec: &str) -> Result<()> {
        let table = schema_table(table)?;
        self.conn()?.execute(
            &format!("INSERT OR REPLACE INTO {table}(branch, name, spec) VALUES (?1, ?2, ?3)"),
            params![branch, name, spec],
        )?;
        Ok(())
    }

    fn delete_spec(&self, table: &str, branch: &str, name: &str) -> Result<()> {
        let table = schema_table(table)?;
        self.conn()?.execute(
            &format!("DELETE FROM {table} WHERE branch = ?1 AND name = ?2"),
            params![branch, name],
        )?;
        Ok(())
    }

    fn load_specs(&self, branch: &str, table: &str) -> Result<Vec<String>> {
        let table = schema_table(table)?;
        let db = self.conn()?;
        let mut stmt = db.prepare(&format!(
            "SELECT spec FROM {table} WHERE branch = ?1 ORDER BY name"
        ))?;
        let rows = stmt.query_map(params![branch], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    fn load_spec(&self, table: &str, branch: &str, name: &str) -> Result<Option<String>> {
        let table = schema_table(table)?;
        Ok(self
            .conn()?
            .query_row(
                &format!("SELECT spec FROM {table} WHERE branch = ?1 AND name = ?2"),
                params![branch, name],
                |r| r.get(0),
            )
            .optional()?)
    }

    fn insert_proposal(&self, id: &str, branch: &str, submitted_by: &str, at: i64) -> Result<()> {
        self.conn()?.execute(
            "INSERT INTO proposals(id, branch, status, submitted_by, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, branch, "under_review", submitted_by, at],
        )?;
        Ok(())
    }

    fn proposal_branch_status(&self, id: &str) -> Result<(String, String)> {
        Ok(self.conn()?.query_row(
            "SELECT branch, status FROM proposals WHERE id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    }

    fn set_proposal(&self, id: &str, status: &str, reviewed_by: Option<&str>) -> Result<()> {
        self.conn()?.execute(
            "UPDATE proposals SET status = ?1, reviewed_by = COALESCE(?2, reviewed_by) WHERE id = ?3",
            params![status, reviewed_by, id],
        )?;
        Ok(())
    }

    fn candidate_ids(&self, type_name: Option<&str>) -> Result<Vec<String>> {
        let db = self.conn()?;
        let ids = if let Some(t) = type_name {
            let mut stmt = db.prepare("SELECT id FROM objects WHERE type_name = ?1")?;
            let rows = stmt.query_map(params![t], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        } else {
            let mut stmt = db.prepare("SELECT id FROM objects")?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        Ok(ids)
    }

    fn load_version(&self, id: &str, as_of: AsOf) -> Result<LoadedVersion> {
        let db = self.conn()?;
        bitemporal::load(&db, id, as_of)
    }

    fn list_spans(&self, id: &str) -> Result<Vec<VersionSpan>> {
        let db = self.conn()?;
        bitemporal::list_spans(&db, id)
    }

    fn identity_exists(&self, id: &str) -> Result<bool> {
        let db = self.conn()?;
        bitemporal::identity_exists(&db, id)
    }

    fn current_properties(&self, id: &str) -> Result<String> {
        let db = self.conn()?;
        bitemporal::current_properties(&db, id)
    }

    fn insert_object(
        &self,
        id: &str,
        type_name: &str,
        title: Option<&str>,
        properties: &str,
        at: i64,
    ) -> Result<()> {
        let db = self.conn()?;
        bitemporal::insert_object(&db, id, type_name, title, properties, at)
    }

    fn append_version(
        &self,
        id: &str,
        properties: &str,
        title: Option<&str>,
        at: i64,
    ) -> Result<String> {
        let db = self.conn()?;
        bitemporal::append_version(&db, id, properties, title, at)
    }

    fn link_targets(&self, from_id: &str, link_type: &str) -> Result<Vec<String>> {
        let db = self.conn()?;
        let mut stmt =
            db.prepare("SELECT to_id FROM links WHERE from_id = ?1 AND type_name = ?2")?;
        let targets: Vec<String> = stmt
            .query_map(params![from_id, link_type], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(targets)
    }

    fn list_inbox(&self) -> Result<Vec<InboxItem>> {
        let db = self.conn()?;
        let mut stmt = db.prepare(
            "SELECT id, action_name, proposed_by, params, status, created_at FROM inbox ORDER BY created_at",
        )?;
        let rows = stmt.query_map([], |r| {
            let raw: String = r.get(3)?;
            Ok(InboxItem {
                id: r.get(0)?,
                action_name: r.get(1)?,
                proposed_by: r.get(2)?,
                params: serde_json::from_str(&raw).unwrap_or(Value::Null),
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

    fn load_inbox(&self, id: &str) -> Result<InboxRow> {
        self.conn()?
            .query_row(
                "SELECT action_name, params, status, proposed_by,
                        COALESCE(rule_pin, ''), COALESCE(apply_action, '')
                 FROM inbox WHERE id = ?1",
                params![id],
                |r| {
                    Ok(InboxRow {
                        action_name: r.get(0)?,
                        params: r.get(1)?,
                        status: r.get(2)?,
                        proposed_by: r.get(3)?,
                        rule_pin: r.get(4)?,
                        apply_action: r.get(5)?,
                    })
                },
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => OntoError::NotFound(format!("inbox {id}")),
                other => OntoError::Store(other.to_string()),
            })
    }

    fn set_inbox_status(&self, id: &str, status: &str) -> Result<()> {
        self.conn()?.execute(
            "UPDATE inbox SET status = ?1 WHERE id = ?2",
            params![status, id],
        )?;
        Ok(())
    }

    fn insert_decision(&self, rec: &DecisionWrite<'_>) -> Result<()> {
        self.conn()?.execute(
            "INSERT INTO decision_records(
                id, action_name, actor, confirmer, verdict, params, guard_results, effects,
                rule_version, function_version, engine_version, data_snapshot, proof_trace, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                rec.id,
                rec.action,
                rec.actor,
                rec.confirmer,
                rec.verdict.as_stored(),
                rec.params.to_string(),
                serde_json::to_string(rec.guards)?,
                rec.effects.to_string(),
                rec.snapshot.rule_version,
                rec.snapshot.function_version,
                rec.snapshot.engine_version,
                serde_json::to_string(rec.snapshot)?,
                serde_json::to_string(rec.trace)?,
                rec.at
            ],
        )?;
        Ok(())
    }

    fn load_decision(&self, id: &str) -> Result<DecisionRecordView> {
        self.conn()?
            .query_row(
                "SELECT id, action_name, actor, confirmer, verdict, params, guard_results, effects, rule_version, function_version, engine_version, data_snapshot, created_at, COALESCE(proof_trace, '[]')
                 FROM decision_records WHERE id = ?1",
                params![id],
                |r| {
                    let verdict_raw: String = r.get(4)?;
                    let params_raw: String = r.get(5)?;
                    let guards_raw: String = r.get(6)?;
                    let effects_raw: String = r.get(7)?;
                    let snap_raw: String = r.get(11)?;
                    let proof_raw: String = r.get::<_, String>(13).unwrap_or_else(|_| "[]".into());
                    Ok(DecisionRecordView {
                        id: r.get(0)?,
                        action_name: r.get(1)?,
                        actor: r.get(2)?,
                        confirmer: r.get(3)?,
                        verdict: Verdict::from_stored(&verdict_raw),
                        params: serde_json::from_str(&params_raw).unwrap_or(Value::Null),
                        guard_results: serde_json::from_str(&guards_raw).unwrap_or_default(),
                        effects: serde_json::from_str(&effects_raw).unwrap_or(Value::Null),
                        rule_version: r.get(8)?,
                        function_version: r.get(9)?,
                        engine_version: r.get(10)?,
                        data_snapshot: serde_json::from_str(&snap_raw).unwrap_or(Value::Null),
                        created_at: r.get::<_, i64>(12)?.to_string(),
                        proof_trace: serde_json::from_str(&proof_raw).unwrap_or_default(),
                    })
                },
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => OntoError::NotFound(format!("decision {id}")),
                other => OntoError::Store(other.to_string()),
            })
    }

    fn put_idempotency(
        &self,
        key: &str,
        rec_id: &str,
        action: &str,
        outcome: &str,
        at: i64,
    ) -> Result<()> {
        self.conn()?.execute(
            "INSERT OR IGNORE INTO side_effect_keys(idempotency_key, decision_record_id, action_name, outcome, created_at, payload_digest)
             VALUES (?1, ?2, ?3, ?4, ?5, '')",
            params![key, rec_id, action, outcome, at],
        )?;
        Ok(())
    }

    fn get_idempotency(&self, key: &str) -> Result<Option<IdempotencyRow>> {
        Ok(self
            .conn()?
            .query_row(
                "SELECT outcome, COALESCE(payload_digest, '') FROM side_effect_keys WHERE idempotency_key = ?1",
                params![key],
                |r| {
                    Ok(IdempotencyRow {
                        outcome: r.get(0)?,
                        payload_digest: r.get(1)?,
                    })
                },
            )
            .optional()?)
    }

    fn commit_staged(&self, ops: &[StagedOp], at: i64) -> Result<Vec<String>> {
        let mut db = self.conn()?;
        let tx = db.transaction()?;
        let created = apply_staged(&tx, ops, at)?;
        tx.commit()?;
        Ok(created)
    }

    fn commit_decision(&self, unit: &DecisionCommit<'_>) -> Result<DecisionApply> {
        let mut db = self.conn()?;
        let tx = db.transaction()?;
        if let Some(expected) = unit.schema_revision {
            let current = schema_revision_on(&tx, MAIN_BRANCH)?;
            if current != expected {
                return Err(OntoError::StaleRead);
            }
        }
        check_read_set(&tx, unit.read_set)?;
        if let (Some(key), Some(outcome)) = (unit.idempotency_key, unit.idempotency_outcome) {
            if let Some(replay) = reserve_idempotency(
                &tx,
                key,
                unit.decision.id,
                unit.decision.action,
                outcome,
                unit.payload_digest.unwrap_or(""),
                unit.decision.at,
            )? {
                return Ok(DecisionApply::Replayed(replay));
            }
        }
        let created = apply_staged(&tx, unit.ops, unit.decision.at)?;
        insert_decision_on(&tx, &unit.decision)?;
        if let Some(declaration) = unit.effect_declaration {
            tx.execute(
                "INSERT INTO effect_intentions(decision_record_id, declaration, status, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    unit.decision.id,
                    declaration,
                    EffectStatus::Declared.as_stored(),
                    unit.decision.at
                ],
            )?;
        }
        if let (Some(id), Some(actor), Some(kind), Some(payload)) = (
            unit.audit_id,
            unit.audit_actor,
            unit.audit_kind,
            unit.audit_payload,
        ) {
            tx.execute(
                "INSERT INTO audit_log(id, at, actor, kind, payload) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id, unit.decision.at, actor, kind, payload],
            )?;
        }
        tx.commit()?;
        Ok(DecisionApply::Written(created))
    }

    fn append_version_recorded(
        &self,
        id: &str,
        properties: &str,
        title: Option<&str>,
        valid_at: i64,
        tx_at: i64,
    ) -> Result<String> {
        let mut db = self.conn()?;
        let tx = db.transaction()?;
        let version =
            bitemporal::append_version_recorded(&tx, id, properties, title, valid_at, tx_at)?;
        tx.commit()?;
        Ok(version)
    }

    fn insert_object_recorded(
        &self,
        id: &str,
        type_name: &str,
        title: Option<&str>,
        properties: &str,
        valid_at: i64,
        tx_at: i64,
    ) -> Result<()> {
        let mut db = self.conn()?;
        let tx = db.transaction()?;
        bitemporal::insert_object_recorded(&tx, id, type_name, title, properties, valid_at, tx_at)?;
        tx.commit()?;
        Ok(())
    }

    fn object_type_of(&self, id: &str) -> Result<String> {
        let db = self.conn()?;
        db.query_row(
            "SELECT type_name FROM objects WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => OntoError::NotFound(format!("object {id}")),
            other => OntoError::Store(other.to_string()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn does_implement_store_seam() {
        fn is_store<T: Store>(_: &T) {}
        let store = SqliteStore::memory().unwrap();
        is_store(&store);
        store
            .put_spec(
                "schema_value_types",
                MAIN_BRANCH,
                "Text",
                r#"{"name":"Text","base":"string","min":null,"max":null,"unit":null}"#,
            )
            .unwrap();
        let specs = store.load_specs(MAIN_BRANCH, "schema_value_types").unwrap();
        assert_eq!(specs.len(), 1);
        assert!(specs[0].contains("Text"));
        assert!(store
            .load_spec("schema_value_types", MAIN_BRANCH, "missing")
            .unwrap()
            .is_none());
    }

    #[test]
    fn does_reject_unknown_schema_table_as_invalid_not_sql() {
        let store = SqliteStore::memory().unwrap();
        let err = store
            .put_spec("objects; drop table objects", MAIN_BRANCH, "x", "{}")
            .unwrap_err();
        assert!(matches!(err, OntoError::Invalid(_)));
    }
}
