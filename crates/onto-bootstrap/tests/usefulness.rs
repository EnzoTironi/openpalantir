//! Scoreboard pins real `does_*` tests. A ghost name is a failing spec.

use onto_bootstrap::usefulness::CLAIMS;
use std::fs;
use std::path::{Path, PathBuf};

fn workspace_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/onto-bootstrap lives two levels under the workspace")
        .to_path_buf()
}

fn rust_sources(dir: &Path) -> String {
    let mut out = String::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(cur) = stack.pop() {
        if cur.ends_with("target") {
            continue;
        }
        let entries =
            fs::read_dir(&cur).unwrap_or_else(|e| panic!("read_dir {}: {e}", cur.display()));
        for entry in entries {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                out.push_str(
                    &fs::read_to_string(&path)
                        .unwrap_or_else(|e| panic!("read {}: {e}", path.display())),
                );
                out.push('\n');
            }
        }
    }
    out
}

#[test]
fn does_map_buyer_claims_to_existing_does_tests() {
    let hay = rust_sources(&workspace_dir().join("crates"));
    assert!(
        CLAIMS.len() >= 20,
        "scoreboard must list buyer claims, got {}",
        CLAIMS.len()
    );
    for row in CLAIMS {
        assert!(
            row.test.starts_with("does_"),
            "claim must pin a does_* test, got {}",
            row.test
        );
        let needle = format!("fn {}(", row.test);
        assert!(
            hay.contains(&needle),
            "scoreboard test {} must exist as a fn, or the claim is theater",
            row.test
        );
    }
}
