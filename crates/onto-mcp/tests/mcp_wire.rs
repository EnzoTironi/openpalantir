//! Wire-level stdio: spawn the onto-mcp binary. In-process handle is not enough.

use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn onto_mcp_bin() -> PathBuf {
    for key in ["CARGO_BIN_EXE_onto_mcp", "CARGO_BIN_EXE_onto-mcp"] {
        if let Ok(path) = std::env::var(key) {
            return PathBuf::from(path);
        }
    }
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    if let Ok(dir) = std::env::var("CARGO_TARGET_DIR") {
        return PathBuf::from(dir).join(profile).join("onto-mcp");
    }
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.pop();
    path.pop();
    path.push("target");
    path.push(profile);
    path.push("onto-mcp");
    path
}

fn spawn_stdio() -> std::process::Child {
    let bin = onto_mcp_bin();
    assert!(
        bin.exists(),
        "onto-mcp binary missing at {} (build the bin first)",
        bin.display()
    );
    Command::new(bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn onto-mcp")
}

fn write_line(child: &mut std::process::Child, line: &str) {
    let stdin = child.stdin.as_mut().expect("stdin");
    writeln!(stdin, "{line}").expect("write");
    stdin.flush().expect("flush");
}

#[test]
fn does_omit_stdio_line_if_notification() {
    let mut child = spawn_stdio();
    write_line(
        &mut child,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
    );
    write_line(
        &mut child,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#,
    );
    drop(child.stdin.take());
    let stdout = child.stdout.take().expect("stdout");
    let lines: Vec<String> = BufReader::new(stdout)
        .lines()
        .map(|l| l.expect("line"))
        .collect();
    let _ = child.wait();
    assert_eq!(lines.len(), 1, "{lines:?}");
    let v: Value = serde_json::from_str(&lines[0]).unwrap();
    assert_eq!(v["result"]["protocolVersion"], "2024-11-05");
}

#[test]
fn does_reject_stdio_if_jsonrpc_is_missing() {
    let mut child = spawn_stdio();
    write_line(&mut child, r#"{"id":1,"method":"initialize"}"#);
    drop(child.stdin.take());
    let stdout = child.stdout.take().expect("stdout");
    let lines: Vec<String> = BufReader::new(stdout)
        .lines()
        .map(|l| l.expect("line"))
        .collect();
    let _ = child.wait();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("jsonrpc must be 2.0"), "{}", lines[0]);
}

#[test]
fn does_negotiate_advertised_protocol_on_stdio() {
    let mut child = spawn_stdio();
    write_line(
        &mut child,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"1999-01-01"}}"#,
    );
    write_line(
        &mut child,
        r#"{"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}"#,
    );
    drop(child.stdin.take());
    let stdout = child.stdout.take().expect("stdout");
    let lines: Vec<String> = BufReader::new(stdout)
        .lines()
        .map(|l| l.expect("line"))
        .collect();
    let _ = child.wait();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(
        lines[0].contains("unsupported protocolVersion"),
        "{}",
        lines[0]
    );
    let ok: Value = serde_json::from_str(&lines[1]).unwrap();
    assert_eq!(ok["result"]["protocolVersion"], "2024-11-05");
}
