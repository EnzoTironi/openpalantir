//! Live operational-ontology engine: OMS, object store, OSS, Actions, Funnel.
//!
//! Schema lives in OMS and is mutated at runtime on a branch. Consumer syscalls
//! are projected from main after merge. There is no file-based constitution.
//!
//! [`SqliteStore`] owns the `SQLite` lock. [`Engine`] coordinates. The [`Store`]
//! trait is the only persistence seam (one sqlite impl now).

#![allow(clippy::missing_errors_doc)] // OntoError is the public contract

mod bitemporal;
mod compensation;
mod engine;
mod error;
mod functions;
mod kernel;
mod oss;
mod security;
mod store;
mod surface;
mod tiers;
mod types;
mod write_path;

pub use bitemporal::{AsOf, LoadedVersion, VersionSpan};
pub use compensation::{inverse_params, previous_written, require_allow, Compensation};
pub use engine::Engine;
pub use error::{OntoError, Result};
pub use functions::{FunctionKind, FunctionSpec};
pub use kernel::KernelInterface;
pub use oss::{ObjectSet, ObjectSetFilter, ObjectSetSpec};
pub use security::{AuthzDecision, AuthzLevel, AuthzOp, PolicySpec};
pub use store::{SqliteStore, Store};
pub use surface::dispatch;
pub use tiers::{AgentTier, AutoBound, AutoClaim, IllegalTier, RiskBand};
pub use types::*;
