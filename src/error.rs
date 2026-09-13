use std::io;

use thiserror::Error;

pub type Result<T> = std::result::Result<T, RewindError>;

#[derive(Debug, Error)]
pub enum RewindError {
    #[error("workspace is not initialized from this path")]
    WorkspaceNotInitialized,
    #[error("workspace identity conflict: {0}")]
    WorkspaceIdentity(String),
    #[error("workspace condition blocks this operation: {0}")]
    ConditionBlocked(String),
    #[error("workspace recovery is required: {0}")]
    RecoveryRequired(String),
    #[error("conflict at {path}: expected {expected}, found {found}")]
    Conflict {
        path: String,
        expected: String,
        found: String,
    },
    #[error("unsupported filesystem object or metadata: {0}")]
    Unsupported(String),
    #[error("path is outside the workspace: {0}")]
    PathEscape(String),
    #[error("storage error: {0}")]
    Storage(String),
    #[error("CAS error: {0}")]
    Cas(String),
    #[error("scan incomplete: {0}")]
    ScanIncomplete(String),
    #[error("journal error: {0}")]
    Journal(String),
    #[error("lock unavailable: {0}")]
    LockUnavailable(String),
    #[error("invalid command: {0}")]
    InvalidCommand(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("serialization error: {0}")]
    Serialization(String),
    #[error("database error: {0}")]
    Database(String),
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
}

impl From<rusqlite::Error> for RewindError {
    fn from(value: rusqlite::Error) -> Self {
        Self::Database(value.to_string())
    }
}

impl From<serde_json::Error> for RewindError {
    fn from(value: serde_json::Error) -> Self {
        Self::Serialization(value.to_string())
    }
}
