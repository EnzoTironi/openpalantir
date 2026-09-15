use crate::bitemporal::AsOf;
use crate::engine::Engine;
use crate::error::{OntoError, Result};
use crate::functions::FunctionSpec;
use crate::oss::ObjectSetSpec;
use crate::security::PolicySpec;
use crate::tiers::{self, RiskBand};
#[allow(clippy::wildcard_imports)] // surface is the syscall match over types
use crate::types::*;
use serde_json::{json, Value};

pub fn dispatch(engine: &Engine, session: &Session, tool: &str, args: Value) -> Result<Value> {
    if session.actor.key == KeyKind::Consumer {
        tiers::require_syscall(session.actor.tier, tool)?;
    }
    if tool == "list_tools" {
        return Ok(serde_json::to_value(engine.list_tools(session)?)?);
    }
    if let Some(action) = tool.strip_prefix("action.") {
        let outcome = engine.submit_action(session, action, args)?;
        return Ok(serde_json::to_value(outcome)?);
    }
    match session.actor.key {
        KeyKind::Builder => dispatch_builder(engine, session, tool, &args),
        KeyKind::Consumer => dispatch_consumer(engine, session, tool, &args),
    }
}

#[allow(clippy::too_many_lines)] // builder syscall match is the surface
fn dispatch_builder(engine: &Engine, session: &Session, tool: &str, args: &Value) -> Result<Value> {
    match tool {
        "open_branch" => {
            let name = str_arg(args, "name")?;
            Ok(json!({ "branch": engine.open_branch(session, name)? }))
        }
        "create_value_type" => {
            let branch = str_arg(args, "branch")?;
            let spec: ValueTypeSpec =
                serde_json::from_value(args.get("spec").cloned().unwrap_or(args.clone()))?;
            Ok(json!({ "name": engine.create_value_type(session, branch, spec)? }))
        }
        "create_object_type" | "alter_object_type" => {
            let branch = str_arg(args, "branch")?;
            let spec: ObjectTypeSpec = serde_json::from_value(require(args, "spec")?)?;
            let name = if tool == "create_object_type" {
                engine.create_object_type(session, branch, spec)?
            } else {
                engine.alter_object_type(session, branch, spec)?
            };
            Ok(json!({ "name": name }))
        }
        "add_property" => {
            let branch = str_arg(args, "branch")?;
            let type_name = str_arg(args, "type_name")?;
            let prop: PropertySpec = serde_json::from_value(require(args, "property")?)?;
            engine.add_property(session, branch, type_name, prop)?;
            Ok(json!({ "ok": true }))
        }
        "alter_property" => {
            let branch = str_arg(args, "branch")?;
            let type_name = str_arg(args, "type_name")?;
            let prop: PropertySpec = serde_json::from_value(require(args, "property")?)?;
            engine.alter_property(session, branch, type_name, prop)?;
            Ok(json!({ "ok": true }))
        }
        "archive_object_type" => {
            engine.archive_object_type(
                session,
                str_arg(args, "branch")?,
                str_arg(args, "type_name")?,
            )?;
            Ok(json!({ "ok": true }))
        }
        "create_link_type" | "alter_link_type" => {
            let branch = str_arg(args, "branch")?;
            let spec: LinkTypeSpec = serde_json::from_value(require(args, "spec")?)?;
            let name = engine.create_link_type(session, branch, spec)?;
            Ok(json!({ "name": name }))
        }
        "create_interface" => {
            let spec: InterfaceSpec = serde_json::from_value(require(args, "spec")?)?;
            Ok(json!({
                "name": engine.create_interface(session, str_arg(args, "branch")?, spec)?
            }))
        }
        "attach_interface" => {
            engine.attach_interface(
                session,
                str_arg(args, "branch")?,
                str_arg(args, "type_name")?,
                str_arg(args, "interface")?,
            )?;
            Ok(json!({ "ok": true }))
        }
        "create_action_type" | "alter_action_type" => {
            let spec: ActionTypeSpec = serde_json::from_value(require(args, "spec")?)?;
            Ok(json!({
                "name": engine.create_action_type(session, str_arg(args, "branch")?, spec)?
            }))
        }
        "create_function" => {
            let spec: FunctionSpec = serde_json::from_value(require(args, "spec")?)?;
            Ok(json!({
                "name": engine.create_function(session, str_arg(args, "branch")?, spec)?
            }))
        }
        "create_object_set" => {
            let spec: ObjectSetSpec = serde_json::from_value(require(args, "spec")?)?;
            Ok(json!({
                "name": engine.create_object_set(session, str_arg(args, "branch")?, spec)?
            }))
        }
        "create_policy" => {
            let spec: PolicySpec = serde_json::from_value(require(args, "spec")?)?;
            Ok(json!({
                "name": engine.create_policy(session, str_arg(args, "branch")?, spec)?
            }))
        }
        "submit_proposal" => Ok(json!({
            "proposal_id": engine.submit_proposal(session, str_arg(args, "branch")?)?
        })),
        "review_proposal" => {
            engine.review_proposal(
                session,
                str_arg(args, "proposal_id")?,
                args.get("approve")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            )?;
            Ok(json!({ "ok": true }))
        }
        "merge_to_main" => {
            engine.merge_to_main(session, str_arg(args, "proposal_id")?)?;
            Ok(json!({ "ok": true }))
        }
        "get_schema" => {
            let branch = args.get("branch").and_then(|v| v.as_str());
            Ok(serde_json::to_value(engine.get_schema(session, branch)?)?)
        }
        "get_object" | "search_objects" | "submit_action" | "funnel_ingest" | "list_inbox" => Err(
            OntoError::Denied("builder key cannot read or write production instances".into()),
        ),
        other => Err(OntoError::NotFound(format!("unknown builder tool {other}"))),
    }
}

#[allow(clippy::too_many_lines)] // consumer syscall match is the surface
fn dispatch_consumer(
    engine: &Engine,
    session: &Session,
    tool: &str,
    args: &Value,
) -> Result<Value> {
    match tool {
        "search_objects" => {
            let type_name = args
                .get("type_name")
                .and_then(Value::as_str)
                .map(str::to_string);
            let equals = args
                .get("equals")
                .and_then(Value::as_object)
                .map(|eq| eq.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                .unwrap_or_default();
            let limit = args
                .get("limit")
                .and_then(Value::as_u64)
                .and_then(|n| usize::try_from(n).ok())
                .unwrap_or(50);
            let q = Query {
                type_name,
                equals,
                limit,
                set_name: args
                    .get("set_name")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            };
            Ok(serde_json::to_value(engine.search_objects(session, q)?)?)
        }
        "get_object" => {
            let id = str_arg(args, "id")?;
            let as_of = match (args.get("recorded"), args.get("as_of")) {
                (Some(v), _) => {
                    let t = v
                        .as_i64()
                        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
                        .ok_or_else(|| {
                            OntoError::Invalid("recorded must be an integer clock".into())
                        })?;
                    AsOf::Recorded(t)
                }
                (None, None) => AsOf::Current,
                (None, Some(v)) => {
                    let t = v
                        .as_i64()
                        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
                        .ok_or_else(|| {
                            OntoError::Invalid("as_of must be an integer clock".into())
                        })?;
                    AsOf::Valid(t)
                }
            };
            Ok(serde_json::to_value(
                engine.get_object(session, id, as_of)?,
            )?)
        }
        "traverse_links" => Ok(serde_json::to_value(engine.traverse_links(
            session,
            str_arg(args, "from_id")?,
            str_arg(args, "link_type")?,
        )?)?),
        "aggregate" => engine.aggregate(session, str_arg(args, "type_name")?),
        "list_missing_evidence" => engine.list_missing_evidence(session, str_arg(args, "id")?),
        "describe_action" => Ok(serde_json::to_value(
            engine.describe_action(session, str_arg(args, "name")?)?,
        )?),
        "submit_action" => {
            let name = str_arg(args, "action")?;
            let params = args.get("params").cloned().unwrap_or(json!({}));
            Ok(serde_json::to_value(
                engine.submit_action(session, name, params)?,
            )?)
        }
        "list_inbox" => Ok(serde_json::to_value(engine.list_inbox(session)?)?),
        "confirm_action" => Ok(serde_json::to_value(
            engine.confirm_action(session, str_arg(args, "inbox_id")?)?,
        )?),
        "override_action" => Ok(serde_json::to_value(engine.override_action(
            session,
            str_arg(args, "inbox_id")?,
            str_arg(args, "category")?,
            str_arg(args, "reason")?,
        )?)?),
        "auto_action" => {
            let name = str_arg(args, "action")?;
            let object_set = str_arg(args, "object_set")?;
            let risk_band = RiskBand::parse(str_arg(args, "risk_band")?)?;
            let params = args.get("params").cloned().unwrap_or(json!({}));
            Ok(serde_json::to_value(engine.auto_action(
                session, name, object_set, risk_band, params,
            )?)?)
        }
        "get_decision_record" => Ok(serde_json::to_value(
            engine.get_decision_record(session, str_arg(args, "id")?)?,
        )?),
        "get_rejection" => engine.get_rejection(session, str_arg(args, "id")?),
        "compensate_action" => {
            let overlay = args.get("overlay").cloned().unwrap_or(json!({}));
            Ok(serde_json::to_value(engine.compensate_action(
                session,
                str_arg(args, "decision_record_id")?,
                overlay,
            )?)?)
        }
        "funnel_ingest" => {
            let records: Vec<IngestRecord> = serde_json::from_value(require(args, "records")?)?;
            Ok(json!({ "ids": engine.funnel_ingest(session, records)? }))
        }
        "create_link" => Err(OntoError::Denied(
            "create_link is not a consumer store write; use Action or Funnel".into(),
        )),
        "create_object_type" | "open_branch" | "merge_to_main" | "create_action_type"
        | "add_property" | "submit_proposal" | "review_proposal" | "create_function" => Err(
            OntoError::Denied("consumer key cannot mutate schema".into()),
        ),
        "list_tools" => Ok(serde_json::to_value(engine.list_tools(session)?)?),
        other => Err(OntoError::NotFound(format!(
            "unknown consumer tool {other}"
        ))),
    }
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| OntoError::Invalid(format!("missing string arg {key}")))
}

fn require(args: &Value, key: &str) -> Result<Value> {
    args.get(key)
        .cloned()
        .ok_or_else(|| OntoError::Invalid(format!("missing arg {key}")))
}
