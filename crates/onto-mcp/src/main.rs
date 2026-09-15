#![allow(clippy::missing_errors_doc)] // OntoError is the public contract
#![allow(clippy::missing_panics_doc)] // process entry: bind/open failures abort

use axum::{extract::State, routing::post, Json, Router};
use onto::{dispatch, Actor, AgentTier, Engine, KeyKind, Session};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
use std::sync::Arc;

#[derive(Clone)]
struct App {
    engine: Arc<Engine>,
    key: KeyKind,
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

fn session_from_key(key: KeyKind, params: &Value) -> Session {
    let meta = params.get("_meta").cloned().unwrap_or(json!({}));
    let id = meta
        .get("actor")
        .and_then(Value::as_str)
        .unwrap_or(match key {
            KeyKind::Builder => "builder",
            KeyKind::Consumer => "consumer",
        });
    let roles: Vec<String> = meta.get("roles").and_then(Value::as_array).map_or_else(
        || match key {
            KeyKind::Builder => vec!["modeler".into(), "reviewer".into()],
            KeyKind::Consumer => vec!["operator".into(), "supervisor".into()],
        },
        |a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        },
    );
    let tier = meta
        .get("tier")
        .and_then(Value::as_u64)
        .and_then(|n| u8::try_from(n).ok())
        .and_then(|n| AgentTier::try_from(n).ok())
        .unwrap_or(AgentTier::T3);
    let refs: Vec<&str> = roles.iter().map(String::as_str).collect();
    let actor = match key {
        KeyKind::Builder => Actor::builder(id, &refs),
        KeyKind::Consumer => Actor::consumer(id, &refs, tier),
    };
    Session::new(actor, "mcp")
}

fn handle(engine: &Engine, key: KeyKind, req: RpcRequest) -> RpcResponse {
    let id = req.id;
    let method = req.method.unwrap_or_default();
    let params = req.params.unwrap_or(json!({}));
    match method.as_str() {
        "initialize" => RpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": {
                    "name": match key {
                        KeyKind::Builder => "onto-builder",
                        KeyKind::Consumer => "onto-consumer",
                    },
                    "version": onto::ENGINE_VERSION
                }
            })),
            error: None,
        },
        "notifications/initialized" | "initialized" => RpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(json!({})),
            error: None,
        },
        "tools/list" => {
            let session = session_from_key(key, &params);
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
            let session = session_from_key(key, &params);
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
    }
}

fn rpc_err(id: Option<Value>, msg: &str) -> RpcResponse {
    RpcResponse {
        jsonrpc: "2.0",
        id,
        result: None,
        error: Some(json!({ "code": -32000, "message": msg })),
    }
}

async fn http_rpc(State(app): State<App>, Json(req): Json<RpcRequest>) -> Json<RpcResponse> {
    Json(handle(&app.engine, app.key, req))
}

fn parse_key() -> KeyKind {
    let mut key = KeyKind::Consumer;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--key" {
            if args.get(i + 1).map(String::as_str) == Some("builder") {
                key = KeyKind::Builder;
            }
            i += 2;
        } else {
            i += 1;
        }
    }
    key
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

#[tokio::main]
async fn main() {
    let key = parse_key();
    let engine = match db_path() {
        Some(path) => Engine::open(&path).expect("open db"),
        None => Engine::memory().expect("memory"),
    };
    if bootstrap() && key == KeyKind::Consumer {
        onto_bootstrap::install(&engine).expect("bootstrap");
    }
    let engine = Arc::new(engine);
    if wants_http() {
        let app = Router::new()
            .route("/mcp", post(http_rpc))
            .with_state(App { engine, key });
        let listener = tokio::net::TcpListener::bind("0.0.0.0:43177")
            .await
            .expect("bind 43177");
        eprintln!(
            "onto-mcp {} on http://127.0.0.1:43177/mcp",
            match key {
                KeyKind::Builder => "builder",
                KeyKind::Consumer => "consumer",
            }
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
            if req.jsonrpc.as_deref() != Some("2.0") && req.method.is_none() {
                continue;
            }
            let resp = handle(&engine, key, req);
            writeln!(stdout, "{}", serde_json::to_string(&resp).unwrap()).ok();
            stdout.flush().ok();
        }
    }
}
