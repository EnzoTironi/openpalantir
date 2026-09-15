//! One command: claim → named `does_*` test → PASS/FAIL.
//!
//! Runs the pinned tests. A deleted mechanism is FAIL, not a green stub.

use onto_bootstrap::usefulness::{outcome_for, Claim, Outcome, CLAIMS};
use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(err) => {
            eprintln!("{err}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<bool, String> {
    let workspace = workspace_dir()?;
    let output = run_cargo_tests(&workspace)?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    println!("Utilidade — ontologia operacional (agente primeiro, sem UI)");
    println!("Cite Zhang 2026. Testes são a spec, não um bench de latência.");
    println!();
    print_table(CLAIMS, &text);
    let pass = CLAIMS
        .iter()
        .filter(|row| outcome_for(row.test, &text) == Outcome::Pass)
        .count();
    let fail = CLAIMS.len() - pass;
    println!();
    println!(
        "{pass} PASS / {fail} FAIL  ({} reivindicações)",
        CLAIMS.len()
    );
    if fail > 0 && !output.status.success() {
        eprintln!("\ncargo test:\n{}", tail(&text, 24));
    }
    Ok(fail == 0)
}

fn print_table(rows: &[Claim], cargo_output: &str) {
    let claim_w = rows
        .iter()
        .map(|r| r.claim.chars().count())
        .max()
        .unwrap_or(0)
        .max("reivindicação".chars().count());
    let test_w = rows
        .iter()
        .map(|r| r.test.chars().count())
        .max()
        .unwrap_or(0)
        .max("teste".chars().count());
    println!(
        "{}  {}  resultado",
        pad("reivindicação", claim_w),
        pad("teste", test_w)
    );
    println!("{}  {}  ---------", "-".repeat(claim_w), "-".repeat(test_w));
    for row in rows {
        let cell = outcome_for(row.test, cargo_output).as_cell();
        println!(
            "{}  {}  {}",
            pad(row.claim, claim_w),
            pad(row.test, test_w),
            cell
        );
    }
}

fn pad(s: &str, width: usize) -> String {
    let n = s.chars().count();
    if n >= width {
        return s.to_string();
    }
    format!("{s}{}", " ".repeat(width - n))
}

fn tail(text: &str, lines: usize) -> String {
    let mut all: Vec<&str> = text.lines().collect();
    if all.len() > lines {
        all = all.split_off(all.len() - lines);
    }
    all.join("\n")
}

fn run_cargo_tests(workspace: &Path) -> Result<std::process::Output, String> {
    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let mut cmd = Command::new(cargo);
    cmd.current_dir(workspace)
        .arg("test")
        .arg("--workspace")
        .arg("--color")
        .arg("never")
        .arg("--")
        .arg("--test-threads=1")
        .arg("--color=never");
    for row in CLAIMS {
        cmd.arg(row.test);
    }
    cmd.output()
        .map_err(|e| format!("falha ao executar cargo test: {e}"))
}

fn workspace_dir() -> Result<PathBuf, String> {
    let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    here.parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| "crates/onto-bootstrap deve viver sob o workspace".into())
}
