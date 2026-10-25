use uuid::Uuid;

use crate::model::ProposalState;

#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    #[error("{0} not found")]
    NotFound(String),
    #[error("{0}")]
    Invalid(String),
    /// The proposal was built against a version that is no longer current.
    #[error(
        "assertion {assertion} is at version {actual}, but the proposal expects version {expected}"
    )]
    Stale {
        proposal: Option<Uuid>,
        assertion: Uuid,
        expected: i32,
        actual: i32,
    },
    #[error("proposal {0} is already decided: {1}")]
    AlreadyDecided(Uuid, ProposalState),
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    /// The files store did not take or give a file.
    #[error("files store: {0}")]
    Files(String),
}

pub type Result<T> = std::result::Result<T, LedgerError>;

pub(crate) fn invalid(message: impl Into<String>) -> LedgerError {
    LedgerError::Invalid(message.into())
}

/// Parses a TEXT column into one of the ledger's enums.
pub(crate) fn parse_text<T: std::str::FromStr<Err = String>>(value: &str) -> Result<T> {
    value
        .parse()
        .map_err(|e: String| LedgerError::Database(sqlx::Error::Decode(e.into())))
}

pub(crate) fn is_unique_violation(err: &sqlx::Error) -> bool {
    matches!(err, sqlx::Error::Database(db) if db.code().as_deref() == Some("23505"))
}
