use thiserror::Error;

#[derive(Debug, Error)]
#[allow(clippy::module_name_repetitions)] // crate error type
pub enum OntoError {
    #[error("{0}")]
    Denied(String),
    #[error("{0}")]
    Review(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Conflict(String),
    /// Action spec has no `compensation` name. Not a silent success.
    #[error("action `{0}` has no named compensation")]
    NoCompensation(String),
    /// Only an [`Verdict::Allow`] `DecisionRecord` can be compensated (Zhang 2026, Ch. 9).
    #[error("decision `{0}` cannot be compensated (verdict is not allow)")]
    NotCompensable(String),
    #[error("store: {0}")]
    Store(String),
    /// Read-set or schema version changed after evaluation. Retry the command.
    #[error("stale read-set")]
    StaleRead,
}

impl From<rusqlite::Error> for OntoError {
    fn from(value: rusqlite::Error) -> Self {
        Self::Store(value.to_string())
    }
}

impl From<serde_json::Error> for OntoError {
    fn from(value: serde_json::Error) -> Self {
        Self::Invalid(value.to_string())
    }
}

pub type Result<T> = std::result::Result<T, OntoError>;
