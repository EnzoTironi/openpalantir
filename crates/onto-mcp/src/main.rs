#![allow(clippy::missing_errors_doc)] // OntoError is the public contract
#![allow(clippy::missing_panics_doc)] // process entry: bind/open failures abort

use axum::{extract::State, routing::post, Json, Router};
use onto::{dispatch, Actor, AgentTier, Engine, Session};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HostRole {
    Builder,
    Consumer,
    Reviewer,
}

#[derive(Clone)]
struct App {
    engine: Arc<Engine>,
    role: HostRole,
}

#[derive(Deserialize)]
struct RpcRequest {
    jsonrpc: Option<String>,
    id: Option<Value>,
    method: Option<String>,
    params: Option<Value>,
}

#[derive(Serialize)]
struct RpcResponse {
    jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<Value>,
}

/// Host-resolved principal. Request metadata never chooses roles or tier.
fn session_from_role(role: HostRole) -> Session {
    match role {
        HostRole::Builder => Session::new(
            Actor::builder("mcp.builder", &["modeler", "reviewer"]),
            "mcp",
        ),
        HostRole::Consumer => Session::new(
            Actor::consumer("mcp.consumer", &["operator"], AgentTier::T2),
            "mcp",
        ),
        HostRole::Reviewer => Session::new(
            Actor::consumer("mcp.reviewer", &["supervisor", "operator"], AgentTier::T3),
            "mcp",
        ),
    }
}

fn is_notification(req: &RpcRequest) -> bool {
    req.method.as_deref().is_some_and(|m| {
        m.starts_with("notifications/") || (m == "initialized" && req.id.is_none())
    })
}

fn handle(engine: &Engine, role: HostRole, req: RpcRequest) -> Option<RpcResponse> {
    if is_notification(&req) {
        return None;
    }
    if req.jsonrpc.as_deref().is_some_and(|v| v != "2.0") {
        return Some(rpc_err(req.id, "jsonrpc must be 2.0"));
    }
    let id = req.id;
    let method = req.method.unwrap_or_default();
    let params = req.params.unwrap_or(json!({}));
    Some(match method.as_str() {
        "initialize" => RpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": {
                    "name": match role {
                        HostRole::Builder => "onto-builder",
                        HostRole::Consumer => "onto-consumer",
                        HostRole::Reviewer => "onto-reviewer",
                    },
                    "version": onto::ENGINE_VERSION
                }
            })),
            error: None,
        },
        "tools/list" => {
            let session = session_from_role(role);
            match engine.list_tools(&session) {
                Ok(tools) => {
                    let listed: Vec<Value> = tools
                        .into_iter()
                        .map(|t| {
                            json!({
                                "name": t.name,
                                "description": t.description,
                                "inputSchema": t.input_schema
                            })
                        })
                        .collect();
                    RpcResponse {
                        jsonrpc: "2.0",
                        id,
                        result: Some(json!({ "tools": listed })),
                        error: None,
                    }
                }
                Err(e) => rpc_err(id, &e.to_string()),
            }
        }
        "tools/call" => {
            let session = session_from_role(role);
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
            match dispatch(engine, &session, name, arguments) {
                Ok(v) => RpcResponse {
                    jsonrpc: "2.0",
                    id,
                    result: Some(json!({
                        "content": [{ "type": "text", "text": v.to_string() }],
                        "structuredContent": v
                    })),
                    error: None,
                },
                Err(e) => RpcResponse {
                    jsonrpc: "2.0",
                    id,
                    result: Some(json!({
                        "content": [{ "type": "text", "text": e.to_string() }],
                        "isError": true
                    })),
                    error: None,
                },
            }
        }
        other => rpc_err(id, &format!("unknown method {other}")),
    })
}

fn rpc_err(id: Option<Value>, msg: &str) -> RpcResponse {
    RpcResponse {
        jsonrpc: "2.0",
        id,
        result: None,
        error: Some(json!({ "code": -32000, "message": msg })),
    }
}

async fn http_rpc(
    State(app): State<App>,
    Json(req): Json<RpcRequest>,
) -> Json<Option<RpcResponse>> {
    Json(handle(&app.engine, app.role, req))
}

fn parse_role() -> HostRole {
    let mut role = HostRole::Consumer;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--key" {
            role = match args.get(i + 1).map(String::as_str) {
                Some("builder") => HostRole::Builder,
                Some("reviewer") => HostRole::Reviewer,
                _ => HostRole::Consumer,
            };
            i += 2;
        } else {
            i += 1;
        }
    }
    role
}

fn wants_http() -> bool {
    std::env::args().any(|a| a == "--http")
}

fn db_path() -> Option<String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    args.windows(2)
        .find(|w| w[0] == "--db")
        .map(|w| w[1].clone())
}

fn bootstrap() -> bool {
    std::env::args().any(|a| a == "--bootstrap")
}

fn role_label(role: HostRole) -> &'static str {
    match role {
        HostRole::Builder => "builder",
        HostRole::Consumer => "consumer",
        HostRole::Reviewer => "reviewer",
    }
}

#[tokio::main]
async fn main() {
    let role = parse_role();
    let engine = match db_path() {
        Some(path) => Engine::open(&path).expect("open db"),
        None => Engine::memory().expect("memory"),
    };
    if bootstrap() && role == HostRole::Consumer {
        onto_bootstrap::install(&engine).expect("bootstrap");
    }
    let engine = Arc::new(engine);
    if wants_http() {
        let app = Router::new()
            .route("/mcp", post(http_rpc))
            .with_state(App { engine, role });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:43177")
            .await
            .expect("bind 43177");
        eprintln!(
            "onto-mcp {} on http://127.0.0.1:43177/mcp",
            role_label(role)
        );
        axum::serve(listener, app).await.expect("serve");
    } else {
        let stdin = io::stdin();
        let mut stdout = io::stdout();
        for line in stdin.lock().lines() {
            let line = match line {
                Ok(l) if !l.trim().is_empty() => l,
                Ok(_) => continue,
                Err(_) => break,
            };
            let req: RpcRequest = match serde_json::from_str(&line) {
                Ok(r) => r,
                Err(e) => {
                    let resp = rpc_err(None, &e.to_string());
                    writeln!(stdout, "{}", serde_json::to_string(&resp).unwrap()).ok();
                    stdout.flush().ok();
                    continue;
                }
            };
            if let Some(resp) = handle(&engine, role, req) {
                writeln!(stdout, "{}", serde_json::to_string(&resp).unwrap()).ok();
                stdout.flush().ok();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use onto::KeyKind;

    #[test]
    fn does_omit_response_if_notification() {
        let engine = Engine::memory().unwrap();
        let req = RpcRequest {
            jsonrpc: Some("2.0".into()),
            id: None,
            method: Some("notifications/initialized".into()),
            params: None,
        };
        assert!(handle(&engine, HostRole::Consumer, req).is_none());
    }

    #[test]
    fn does_bind_reviewer_to_t3_host_principal() {
        let session = session_from_role(HostRole::Reviewer);
        assert_eq!(session.actor.key, KeyKind::Consumer);
        assert_eq!(session.actor.tier, AgentTier::T3);
        assert!(session.actor.has_role("supervisor"));
    }
}
