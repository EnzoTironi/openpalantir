//! Thin typed client. Humans, agents, tests, and the MCP server share `dispatch`.

use onto::{dispatch, ActionOutcome, Result, SchemaSnapshot, ToolSpec};
use serde_json::{json, Value};

pub use onto::{Actor, AgentTier, Engine, KeyKind, OntoError, Session};

pub struct Client<'a> {
    pub engine: &'a Engine,
    pub session: Session,
}

impl<'a> Client<'a> {
    pub fn new(engine: &'a Engine, session: Session) -> Self {
        Self { engine, session }
    }

    pub fn call(&self, tool: &str, args: Value) -> Result<Value> {
        dispatch(self.engine, &self.session, tool, args)
    }

    pub fn list_tools(&self) -> Result<Vec<ToolSpec>> {
        self.engine.list_tools(&self.session)
    }

    pub fn get_schema(&self, branch: Option<&str>) -> Result<SchemaSnapshot> {
        let mut args = json!({});
        if let Some(b) = branch {
            args["branch"] = json!(b);
        }
        Ok(serde_json::from_value(self.call("get_schema", args)?)?)
    }

    pub fn submit_action(&self, action: &str, params: Value) -> Result<ActionOutcome> {
        Ok(serde_json::from_value(self.call(
            "submit_action",
            json!({ "action": action, "params": params }),
        )?)?)
    }
}

pub fn parse_session(key: &str, id: &str, roles: &[&str], tier: u8) -> Session {
    let tier = AgentTier::try_from(tier).unwrap_or(AgentTier::T2);
    match key {
        "builder" => Session::new(onto::Actor::builder(id, roles), "model"),
        _ => Session::new(onto::Actor::consumer(id, roles, tier), "operate"),
    }
}
