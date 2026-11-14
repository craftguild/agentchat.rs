//! Do not leak SQL statements or bound message text into routine errors.

use agentchat_core::{Error, ErrorCode};
use sea_orm::{DbErr, RuntimeErr};

pub(crate) fn database(error: DbErr) -> Error {
    // SQLite extended codes retain the base error in the low byte. Match the
    // structured code so errors never depend on parsing localized SQL messages.
    let busy = match &error {
        DbErr::Exec(RuntimeErr::SqlxError(e))
        | DbErr::Query(RuntimeErr::SqlxError(e))
        | DbErr::Conn(RuntimeErr::SqlxError(e)) => e
            .as_database_error()
            .and_then(|e| e.code())
            .and_then(|s| s.parse::<i32>().ok())
            .is_some_and(|code| matches!(code & 255, 5 | 6)),
        _ => false,
    };
    let mut result = Error::new(
        if busy {
            ErrorCode::Busy
        } else {
            ErrorCode::StorageError
        },
        if busy {
            "SQLite writer is busy."
        } else {
            "A database operation failed."
        },
    );
    result.retryable = busy;
    result
}

pub(crate) fn io(_: std::io::Error) -> Error {
    Error::new(
        ErrorCode::IoError,
        "A project storage filesystem operation failed.",
    )
}

pub(crate) fn integrity(message: &str) -> Error {
    Error::new(ErrorCode::IntegrityError, message)
}
