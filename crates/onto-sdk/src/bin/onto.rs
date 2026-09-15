use onto_sdk::{parse_session, Client, Engine};
use serde_json::{Map, Value};
use std::env;
use std::io::{self, IsTerminal, Read};

fn main() {
    let raw: Vec<String> = env::args().skip(1).collect();
    if raw.is_empty() {
        eprintln!(
            "usage: onto [--db PATH] --key consumer|builder --id ID --roles a,b --tier N TOOL [JSON]"
        );
        std::process::exit(2);
    }
    let mut db = None::<String>;
    let mut key = "consumer".to_string();
    let mut id = "cli".to_string();
    let mut roles: Vec<String> = Vec::new();
    let mut tier = 2u8;
    let mut rest: Vec<String> = Vec::new();
    let mut i = 0;
    while i < raw.len() {
        match raw[i].as_str() {
            "--db" => {
                db = Some(raw[i + 1].clone());
                i += 2;
            }
            "--key" => {
                key.clone_from(&raw[i + 1]);
                i += 2;
            }
            "--id" => {
                id.clone_from(&raw[i + 1]);
                i += 2;
            }
            "--roles" => {
                roles = raw[i + 1]
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect();
                i += 2;
            }
            "--tier" => {
                tier = raw[i + 1].parse().unwrap_or(2);
                i += 2;
            }
            _ => {
                rest.extend(raw[i..].iter().cloned());
                break;
            }
        }
    }
    let tool = rest.first().cloned().unwrap_or_else(|| "list_tools".into());
    let payload = if rest.len() > 1 {
        serde_json::from_str(&rest[1]).unwrap_or(Value::Null)
    } else if io::stdin().is_terminal() {
        Value::Object(Map::default())
    } else {
        let mut buf = String::new();
        io::stdin().read_to_string(&mut buf).ok();
        serde_json::from_str(&buf).unwrap_or(Value::Object(Map::default()))
    };
    let engine = match db {
        Some(path) => Engine::open(&path).expect("open db"),
        None => Engine::memory().expect("memory db"),
    };
    let role_refs: Vec<&str> = roles.iter().map(String::as_str).collect();
    let client = Client::new(&engine, parse_session(&key, &id, &role_refs, tier));
    match client.call(&tool, payload) {
        Ok(v) => println!("{}", serde_json::to_string_pretty(&v).unwrap()),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
