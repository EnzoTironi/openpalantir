//! Live operational-ontology engine: OMS, object store, OSS, Actions, Funnel.
//!
//! Schema lives in OMS and is mutated at runtime on a branch. Consumer syscalls
//! are projected from main after merge. There is no file-based constitution.
//!
//! [`SqliteStore`] owns the `SQLite` lock. [`Engine`] coordinates. The [`Store`]
//! trait is the only persistence seam (one sqlite impl now).

#![allow(clippy::missing_errors_doc)] // OntoError is the public contract

mod bitemporal;
mod clock;
mod command;
mod compensation;
mod decision;
mod disclosure;
mod engine;
mod error;
mod functions;
mod guards;
mod kernel;
mod oss;
mod security;
mod store;
mod surface;
mod tiers;
mod types;
mod write_path;

pub use bitemporal::{AsOf, LoadedVersion, VersionSpan};
pub use command::{idempotency_key, payload_digest, DecisionApply, IdempotencyRow};
pub use compensation::{inverse_params, previous_written, require_allow, Compensation};
pub use decision::{chosen_integration, EffectIntention, EffectStatus, IntegrationModel};
pub use engine::Engine;
pub use error::{OntoError, Result};
pub use functions::{FunctionKind, FunctionSpec};
pub use guards::{json_schema_from_value_type, json_schema_type, parse_guards, Guard};
pub use kernel::KernelInterface;
pub use oss::{ObjectSet, ObjectSetFilter, ObjectSetSpec};
pub use security::{AuthzDecision, AuthzLevel, AuthzOp, PolicySpec};
pub use store::{DecisionCommit, InboxRow, SqliteStore, Store};
pub use surface::dispatch;
pub use tiers::{AgentTier, AutoBound, AutoClaim, IllegalTier, RiskBand};
pub use types::*;
