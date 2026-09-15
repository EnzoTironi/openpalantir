//! Bitemporal object versions (Zhang 2026, Ch. 5).
//!
//! Identity is stable. A write appends a version with valid time and
//! transaction time; it does not rewrite history. Open end is `None`.
//! The only way to add a version for an existing identity is
//! [`VersionSpan::close_and_succeed`] / [`append_version`], which closes
//! the previous open span first — two overlapping open versions for the
//! same object id cannot be constructed through this API.

use crate::error::{OntoError, Result};
use crate::types::new_id;
use rusqlite::{params, Connection, OptionalExtension};

pub const SCHEMA: &str = r"
CREATE TABLE IF NOT EXISTS object_versions (
    version_id TEXT PRIMARY KEY,
    object_id TEXT NOT NULL,
    title TEXT,
    properties TEXT NOT NULL,
    valid_from INTEGER NOT NULL,
    valid_to INTEGER,
    tx_from INTEGER NOT NULL,
    tx_to INTEGER,
    FOREIGN KEY (object_id) REFERENCES objects(id)
);
CREATE UNIQUE INDEX IF NOT EXISTS object_versions_one_open
ON object_versions(object_id) WHERE valid_to IS NULL AND tx_to IS NULL;
CREATE INDEX IF NOT EXISTS object_versions_by_object
ON object_versions(object_id, valid_from);
";

/// Valid-time clock for a read. `Current` is the open version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AsOf {
    Current,
    Valid(i64),
}

/// Valid time and transaction time of one version. `None` on `*_to` is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VersionSpan {
    pub valid_from: i64,
    pub valid_to: Option<i64>,
    pub tx_from: i64,
    pub tx_to: Option<i64>,
}

impl VersionSpan {
    /// First (or successor) current version: open `valid_to` and open `tx_to`.
    #[must_use]
    pub fn current(at: i64) -> Self {
        Self {
            valid_from: at,
            valid_to: None,
            tx_from: at,
            tx_to: None,
        }
    }

    #[must_use]
    pub fn is_open(&self) -> bool {
        self.valid_to.is_none() && self.tx_to.is_none()
    }

    /// Half-open valid coverage: `valid_from <= t < valid_to`, or open end.
    #[must_use]
    pub fn covers_valid(&self, t: i64) -> bool {
        self.valid_from <= t && self.valid_to.is_none_or(|end| t < end)
    }

    /// Close this open version at `at` and return `(closed, successor open)`.
    ///
    /// Overlapping opens cannot come out of this function: the predecessor
    /// is always closed before the successor is minted. A close at the same
    /// instant as `valid_from` yields a zero-width closed span that covers
    /// no valid time; the successor owns that instant.
    pub fn close_and_succeed(self, at: i64) -> Result<(Self, Self)> {
        if !self.is_open() {
            return Err(OntoError::Conflict(
                "cannot succeed a closed version".into(),
            ));
        }
        let cut = at.max(self.valid_from);
        let closed = VersionSpan {
            valid_from: self.valid_from,
            valid_to: Some(cut),
            tx_from: self.tx_from,
            tx_to: Some(cut),
        };
        debug_assert!(
            !closed.is_open(),
            "close_and_succeed must not leave the predecessor open"
        );
        debug_assert!(
            !closed.covers_valid(cut) || cut > closed.valid_from && closed.covers_valid(cut - 1),
            "closed and successor must not both cover `cut`"
        );
        Ok((closed, VersionSpan::current(cut)))
    }
}

/// Row loaded for OSS `get_object`.
#[derive(Debug, Clone)]
pub struct LoadedVersion {
    pub version_id: String,
    pub object_id: String,
    pub type_name: String,
    pub title: Option<String>,
    pub properties: String,
    pub span: VersionSpan,
}

/// Insert identity, then the first open version. Used by Action create and Funnel.
pub fn insert_object(
    conn: &Connection,
    id: &str,
    type_name: &str,
    title: Option<&str>,
    properties: &str,
    at: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO objects(id, type_name, created_at) VALUES (?1, ?2, ?3)",
        params![id, type_name, at],
    )?;
    insert_open_version(conn, id, title, properties, VersionSpan::current(at))?;
    Ok(())
}

/// Close the open version (if any) and insert a successor. First write on an
/// identity with no versions inserts the open span directly.
pub fn append_version(
    conn: &Connection,
    object_id: &str,
    properties: &str,
    title: Option<&str>,
    at: i64,
) -> Result<String> {
    match load_open_row(conn, object_id)? {
        None => {
            let span = VersionSpan::current(at);
            insert_open_version(conn, object_id, title, properties, span)
        }
        Some(open) => {
            let (closed, next) = open.span.close_and_succeed(at)?;
            conn.execute(
                "UPDATE object_versions SET valid_to = ?1, tx_to = ?2 WHERE version_id = ?3",
                params![closed.valid_to, closed.tx_to, open.version_id],
            )?;
            let title = title.or(open.title.as_deref());
            insert_open_version(conn, object_id, title, properties, next)
        }
    }
}

pub fn load(conn: &Connection, id: &str, as_of: AsOf) -> Result<LoadedVersion> {
    match as_of {
        AsOf::Current => load_current(conn, id),
        AsOf::Valid(t) => load_at_valid(conn, id, t),
    }
}

pub fn current_properties(conn: &Connection, id: &str) -> Result<String> {
    Ok(load_current(conn, id)?.properties)
}

pub fn identity_exists(conn: &Connection, id: &str) -> Result<bool> {
    let found: Option<String> = conn
        .query_row("SELECT id FROM objects WHERE id = ?1", params![id], |r| {
            r.get(0)
        })
        .optional()?;
    Ok(found.is_some())
}

pub fn list_spans(conn: &Connection, id: &str) -> Result<Vec<VersionSpan>> {
    let mut stmt = conn.prepare(
        "SELECT valid_from, valid_to, tx_from, tx_to
         FROM object_versions WHERE object_id = ?1
         ORDER BY valid_from, tx_from",
    )?;
    let rows = stmt.query_map(params![id], |r| {
        Ok(VersionSpan {
            valid_from: r.get(0)?,
            valid_to: r.get(1)?,
            tx_from: r.get(2)?,
            tx_to: r.get(3)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

struct OpenRow {
    version_id: String,
    title: Option<String>,
    span: VersionSpan,
}

fn load_open_row(conn: &Connection, object_id: &str) -> Result<Option<OpenRow>> {
    Ok(conn
        .query_row(
            "SELECT version_id, title, valid_from, tx_from FROM object_versions
             WHERE object_id = ?1 AND valid_to IS NULL AND tx_to IS NULL",
            params![object_id],
            |r| {
                Ok(OpenRow {
                    version_id: r.get(0)?,
                    title: r.get(1)?,
                    span: VersionSpan {
                        valid_from: r.get(2)?,
                        valid_to: None,
                        tx_from: r.get(3)?,
                        tx_to: None,
                    },
                })
            },
        )
        .optional()?)
}

fn insert_open_version(
    conn: &Connection,
    object_id: &str,
    title: Option<&str>,
    properties: &str,
    span: VersionSpan,
) -> Result<String> {
    if !span.is_open() {
        return Err(OntoError::Invalid(
            "insert_open_version requires an open span".into(),
        ));
    }
    let version_id = new_id();
    conn.execute(
        "INSERT INTO object_versions(
            version_id, object_id, title, properties, valid_from, valid_to, tx_from, tx_to
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            version_id,
            object_id,
            title,
            properties,
            span.valid_from,
            span.valid_to,
            span.tx_from,
            span.tx_to,
        ],
    )?;
    Ok(version_id)
}

fn load_current(conn: &Connection, id: &str) -> Result<LoadedVersion> {
    conn.query_row(
        "SELECT v.version_id, o.id, o.type_name, v.title, v.properties,
                v.valid_from, v.valid_to, v.tx_from, v.tx_to
         FROM objects o
         JOIN object_versions v ON v.object_id = o.id
         WHERE o.id = ?1 AND v.valid_to IS NULL AND v.tx_to IS NULL",
        params![id],
        row_to_loaded,
    )
    .optional()?
    .ok_or_else(|| OntoError::NotFound(format!("object {id}")))
}

fn load_at_valid(conn: &Connection, id: &str, t: i64) -> Result<LoadedVersion> {
    conn.query_row(
        "SELECT v.version_id, o.id, o.type_name, v.title, v.properties,
                v.valid_from, v.valid_to, v.tx_from, v.tx_to
         FROM objects o
         JOIN object_versions v ON v.object_id = o.id
         WHERE o.id = ?1
           AND v.valid_from <= ?2
           AND (v.valid_to IS NULL OR v.valid_to > ?2)
         ORDER BY v.tx_from DESC
         LIMIT 1",
        params![id, t],
        row_to_loaded,
    )
    .optional()?
    .ok_or_else(|| OntoError::NotFound(format!("object {id} has no version covering as_of {t}")))
}

fn row_to_loaded(r: &rusqlite::Row<'_>) -> rusqlite::Result<LoadedVersion> {
    Ok(LoadedVersion {
        version_id: r.get(0)?,
        object_id: r.get(1)?,
        type_name: r.get(2)?,
        title: r.get(3)?,
        properties: r.get(4)?,
        span: VersionSpan {
            valid_from: r.get(5)?,
            valid_to: r.get(6)?,
            tx_from: r.get(7)?,
            tx_to: r.get(8)?,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            r"
            PRAGMA foreign_keys = ON;
            CREATE TABLE objects (
                id TEXT PRIMARY KEY,
                type_name TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            ",
        )
        .unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        conn
    }

    #[test]
    fn current_span_is_open() {
        let s = VersionSpan::current(10);
        assert!(s.is_open());
        assert!(s.covers_valid(10));
        assert!(s.covers_valid(99));
        assert!(!s.covers_valid(9));
    }

    #[test]
    fn close_and_succeed_splits_valid_time_without_overlap() {
        let open = VersionSpan::current(10);
        let (closed, next) = open.close_and_succeed(20).unwrap();
        assert!(!closed.is_open());
        assert!(next.is_open());
        assert!(closed.covers_valid(10));
        assert!(closed.covers_valid(19));
        assert!(!closed.covers_valid(20));
        assert!(next.covers_valid(20));
        assert!(!next.covers_valid(19));
        assert!(closed.close_and_succeed(21).is_err());
    }

    #[test]
    fn append_closes_previous_open_so_a_second_open_cannot_exist() {
        let conn = mem();
        insert_object(&conn, "tank-1", "AerationTank", Some("Basin 1"), "{}", 10).unwrap();
        append_version(&conn, "tank-1", "{\"n\":1}", None, 20).unwrap();
        let spans = list_spans(&conn, "tank-1").unwrap();
        assert_eq!(spans.len(), 2);
        assert_eq!(spans.iter().filter(|s| s.is_open()).count(), 1);
        assert!(spans[0].covers_valid(10));
        assert!(!spans[0].covers_valid(20));
        assert!(spans[1].covers_valid(20));
        let current = load(&conn, "tank-1", AsOf::Current).unwrap();
        assert_eq!(current.properties, "{\"n\":1}");
        let old = load(&conn, "tank-1", AsOf::Valid(10)).unwrap();
        assert_eq!(old.properties, "{}");
        let miss = load(&conn, "tank-1", AsOf::Valid(9));
        assert!(matches!(miss, Err(OntoError::NotFound(_))));
    }

    #[test]
    fn unique_index_rejects_a_second_open_row() {
        let conn = mem();
        insert_object(&conn, "tank-1", "AerationTank", None, "{}", 1).unwrap();
        let err = conn.execute(
            "INSERT INTO object_versions(
                version_id, object_id, title, properties, valid_from, valid_to, tx_from, tx_to
             ) VALUES ('v2', 'tank-1', NULL, '{}', 2, NULL, 2, NULL)",
            [],
        );
        assert!(err.is_err(), "two open versions must be unrepresentable");
    }
}
