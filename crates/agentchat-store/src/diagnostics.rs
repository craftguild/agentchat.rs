//! Integrity and foreign-key diagnostics through SeaORM/SeaQuery only.

use agentchat_core::Result;
use sea_orm::{
    ConnectionTrait, DatabaseConnection,
    sea_query::{Alias, Asterisk, Expr, Func, Query},
};

use crate::error::{database, integrity};

/// Actual settings of the storage connection, rather than intended configuration.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ConnectionSettings {
    /// Persistent journaling mode; supported project connections require WAL.
    pub journal_mode: String,
    /// SQLite synchronous level; FULL is integer 2.
    pub synchronous: i64,
    /// Whether this connection enforces relational foreign keys.
    pub foreign_keys: bool,
    /// Busy timeout in milliseconds.
    pub busy_timeout_ms: i64,
}

pub(crate) async fn settings(db: &DatabaseConnection) -> Result<ConnectionSettings> {
    async fn read<T: sea_orm::TryGetable>(
        db: &DatabaseConnection,
        function: &str,
        column: &str,
    ) -> Result<T> {
        let query = Query::select()
            .column(Alias::new(column))
            .from_function(Func::cust(Alias::new(function)), Alias::new("settings"))
            .to_owned();
        db.query_one(&query)
            .await
            .map_err(database)?
            .ok_or_else(|| integrity("Missing SQLite connection setting."))?
            .try_get("", column)
            .map_err(database)
    }
    Ok(ConnectionSettings {
        journal_mode: read(db, "pragma_journal_mode", "journal_mode").await?,
        synchronous: read(db, "pragma_synchronous", "synchronous").await?,
        foreign_keys: read::<i64>(db, "pragma_foreign_keys", "foreign_keys").await? == 1,
        busy_timeout_ms: read(db, "pragma_busy_timeout", "timeout").await?,
    })
}

pub(crate) async fn check<C: ConnectionTrait>(db: &C) -> Result<()> {
    let checks = Query::select()
        .column(Alias::new("integrity_check"))
        .from_function(
            Func::cust(Alias::new("pragma_integrity_check")),
            Alias::new("checks"),
        )
        .limit(100)
        .to_owned();
    let rows = db.query_all(&checks).await.map_err(database)?;
    if rows.len() != 1
        || rows[0]
            .try_get::<String>("", "integrity_check")
            .map_err(database)?
            != "ok"
    {
        return Err(integrity("SQLite integrity check failed."));
    }
    let foreign_keys = Query::select()
        .expr(Expr::col(Asterisk))
        .from_function(
            Func::cust(Alias::new("pragma_foreign_key_check")),
            Alias::new("checks"),
        )
        .limit(1)
        .to_owned();
    if db
        .query_one(&foreign_keys)
        .await
        .map_err(database)?
        .is_some()
    {
        return Err(integrity("SQLite foreign-key check failed."));
    }
    Ok(())
}
