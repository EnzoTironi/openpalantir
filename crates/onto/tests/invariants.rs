//! Standing constraints that stay true if the suite is the spec.
//!
//! These fail when the unit mechanism is deleted: write-path public enum,
//! `create_link` denial, Engine SQL, Store as the only persistence trait,
//! uuid pin, and instructional healthcare (no dose path).

use onto::{dispatch, Actor, AgentTier, Engine, OntoError, Session};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};

fn onto_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn workspace_dir() -> PathBuf {
    onto_dir()
        .parent()
        .and_then(Path::parent)
        .expect("crates/onto lives two levels under the workspace")
        .to_path_buf()
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn rust_sources(dir: &Path) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(cur) = stack.pop() {
        for entry in
            fs::read_dir(&cur).unwrap_or_else(|e| panic!("read_dir {}: {e}", cur.display()))
        {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                out.push((path.clone(), read(&path)));
            }
        }
    }
    out
}

#[test]
fn does_pin_uuid_at_1_11_1() {
    let manifest = read(&workspace_dir().join("Cargo.toml"));
    assert!(
        manifest.contains("uuid = { version = \"=1.11.1\""),
        "workspace uuid must stay pinned at =1.11.1, got:\n{manifest}"
    );
}

#[test]
fn does_keep_engine_free_of_sql() {
    let src = read(&onto_dir().join("src/engine.rs"));
    for needle in [
        "rusqlite",
        "SELECT ",
        "INSERT INTO",
        "DELETE FROM",
        "CREATE TABLE",
        "execute_batch",
        ".prepare(",
    ] {
        assert!(
            !src.contains(needle),
            "Engine coordinates; Store owns SQL. Found {needle:?} in engine.rs"
        );
    }
    assert!(
        src.contains("store: Box<dyn Store>"),
        "Engine must coordinate through the Store seam, not a raw connection"
    );
    assert!(
        src.contains("pub fn from_store"),
        "Store must be injectable; Engine must not hard-wire one backend"
    );
}

#[test]
fn does_keep_store_as_only_persistence_trait() {
    let mut traits = Vec::new();
    for (path, src) in rust_sources(&onto_dir().join("src")) {
        for (idx, line) in src.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("pub trait ") {
                traits.push(format!("{}:{}:{trimmed}", path.display(), idx + 1));
            }
        }
    }
    assert_eq!(
        traits.len(),
        1,
        "Store must remain the only public persistence trait, got {traits:?}"
    );
    assert!(
        traits[0].contains("pub trait Store: Send + Sync"),
        "the only public trait must be Store, got {traits:?}"
    );
}

#[test]
fn does_omit_create_link_from_engine_public_api() {
    let src = read(&onto_dir().join("src/engine.rs"));
    assert!(
        !src.contains("fn create_link("),
        "create_link is not an Engine method; links are Action or Funnel writes"
    );
    assert!(
        src.contains("fn create_link_type("),
        "builder schema still names link types"
    );
}

#[test]
fn does_refuse_create_link_on_public_dispatch() {
    let engine = Engine::memory().unwrap();
    let consumer = Session::new(
        Actor::consumer("ops.maya", &["operator"], AgentTier::T2),
        "invariants",
    );
    match dispatch(
        &engine,
        &consumer,
        "create_link",
        json!({
            "type_name": "contains",
            "from_id": "a",
            "to_id": "b"
        }),
    ) {
        Err(OntoError::Denied(msg)) => {
            assert!(
                msg.contains("create_link") || msg.contains("Action or Funnel"),
                "Denied must name the forbidden write, got {msg}"
            );
        }
        other => panic!("create_link must be Denied, not {other:?}"),
    }
    let tools = engine.list_tools(&consumer).unwrap();
    assert!(
        !tools.iter().any(|t| t.name == "create_link"),
        "create_link must not appear on the consumer tool list"
    );
}

#[test]
fn does_omit_prescribed_quantity_from_engine() {
    let needles = [
        ["fn", "dose"].join(" "),
        ["mg", "kg"].join("/"),
        ["ti", "trate"].concat(),
    ];
    for (path, src) in rust_sources(&onto_dir().join("src")) {
        for needle in &needles {
            assert!(
                !src.contains(needle),
                "{} must not contain {needle:?}",
                path.display()
            );
        }
    }
}

#[test]
fn does_expose_seven_write_path_steps() {
    assert_eq!(onto::WritePathStep::ALL.len(), 7);
    assert_eq!(
        onto::WritePathStep::ALL,
        [
            onto::WritePathStep::Submit,
            onto::WritePathStep::ParamAndPermission,
            onto::WritePathStep::SubmissionCriteria,
            onto::WritePathStep::StagedEdits,
            onto::WritePathStep::Commit,
            onto::WritePathStep::SealDecisionRecord,
            onto::WritePathStep::DeclareSideEffects,
        ]
    );
}
