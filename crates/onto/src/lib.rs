//! Live operational-ontology engine: OMS, object store, OSS, Actions, Funnel.
//!
//! Schema lives in OMS and is mutated at runtime on a branch. Consumer syscalls
//! are projected from main after merge. There is no file-based constitution.

mod engine;
mod error;
mod surface;
mod types;

pub use engine::Engine;
pub use error::{OntoError, Result};
pub use surface::dispatch;
pub use types::*;
