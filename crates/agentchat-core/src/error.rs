//! Stable, transport-independent application errors.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Application result with a machine-actionable error.
pub type Result<T> = std::result::Result<T, Error>;

/// Stable public error identifiers from the interface contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// Invalid operation arguments or malformed patterns.
    InvalidArgument,
    /// A handle or channel violates its grammar.
    InvalidName,
    /// A mention does not identify a joined participant.
    UnknownMention,
    /// A thread reference is not a root in the selected channel.
    InvalidThread,
    /// A pattern or intersection exceeds its resource budget.
    PatternTooComplex,
    /// A requested object does not exist.
    NotFound,
    /// Project discovery found no reference.
    NotInitialized,
    /// Requested scope conflicts with the existing reference.
    ScopeConflict,
    /// Project metadata is bound to a different canonical location.
    ProjectBindingMismatch,
    /// Existing configuration cannot be safely merged.
    ConfigurationConflict,
    /// An exact host-conversation descriptor is required.
    SessionRequired,
    /// The bound identity is not participating.
    NotJoined,
    /// An immutable identity or binding conflicts.
    IdentityConflict,
    /// An exclusive reservation overlaps another owner's lease.
    ReservationConflict,
    /// The lease generation or state no longer permits the operation.
    StaleLease,
    /// An idempotency key was successfully used for different arguments.
    IdempotencyConflict,
    /// The selected host or platform cannot supply a usable adapter observation.
    UnsupportedHost,
    /// A required transport or host API surface is unavailable.
    UnsupportedProtocol,
    /// The database schema is newer than this implementation.
    UnsupportedSchema,
    /// The linked SQLite version lacks the required WAL fix.
    UnsupportedSqlite,
    /// Temporary lock or queue contention.
    Busy,
    /// The project runtime is temporarily inaccessible.
    RuntimeUnavailable,
    /// A filesystem operation failed.
    IoError,
    /// A database operation failed.
    StorageError,
    /// Database integrity or migration checksums are invalid.
    IntegrityError,
}

impl ErrorCode {
    /// CLI exit category; hooks apply their host's exit semantics instead.
    pub const fn exit_code(self) -> i32 {
        match self {
            Self::InvalidArgument
            | Self::InvalidName
            | Self::UnknownMention
            | Self::InvalidThread
            | Self::PatternTooComplex
            | Self::NotFound => 2,
            Self::NotInitialized
            | Self::ScopeConflict
            | Self::ProjectBindingMismatch
            | Self::ConfigurationConflict => 3,
            Self::SessionRequired | Self::NotJoined | Self::IdentityConflict => 4,
            Self::ReservationConflict | Self::StaleLease | Self::IdempotencyConflict => 5,
            Self::UnsupportedHost
            | Self::UnsupportedProtocol
            | Self::UnsupportedSchema
            | Self::UnsupportedSqlite => 6,
            Self::Busy | Self::RuntimeUnavailable => 7,
            Self::IoError | Self::StorageError | Self::IntegrityError => 8,
        }
    }
}

/// Error envelope shared by application services and both public transports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[error("{message}")]
pub struct Error {
    /// Stable code; clients need not parse the English message.
    pub code: ErrorCode,
    /// Human-readable explanation without credentials or message bodies.
    pub message: String,
    /// Whether an unchanged transport retry can reasonably succeed.
    pub retryable: bool,
    /// Structured operation-specific context.
    pub details: Value,
}

impl Error {
    /// Construct a non-retryable error with empty structured details.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: false,
            details: serde_json::json!({}),
        }
    }
}
