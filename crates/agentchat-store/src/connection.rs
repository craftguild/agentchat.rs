//! Private connection setup and read-only compatibility preflight.

use std::{path::Path, time::Duration};

use agentchat_core::{Error, ErrorCode, Result, project::ProjectIdentity};
use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseConnection,
    sea_query::{Alias, Expr, ExprTrait, Func, LikeExpr, Query},
};

use crate::{
    error::{database, integrity},
    schema,
};

pub(crate) async fn connect(path: &Path, read_only: bool) -> Result<DatabaseConnection> {
    connect_with_foreign_keys(path, read_only, true).await
}

/// Private schema-only connection for SQLite's transactional table rebuild.
/// The migration lock/backup/transaction and explicit integrity+FK validation
/// precede commit. It is closed before opening any ordinary enforced writer.
pub(crate) async fn connect_for_schema(path: &Path) -> Result<DatabaseConnection> {
    connect_with_foreign_keys(path, false, false).await
}

async fn connect_with_foreign_keys(
    path: &Path,
    read_only: bool,
    foreign_keys: bool,
) -> Result<DatabaseConnection> {
    let mut options = ConnectOptions::new("sqlite::memory:");
    let path = path.to_owned();
    options
        .max_connections(1)
        .min_connections(1)
        .sqlx_logging(false)
        .record_stmt_in_spans(false)
        .map_sqlx_sqlite_opts(move |opts| {
            let opts = opts
                .filename(&path)
                .in_memory(false)
                .shared_cache(false)
                .read_only(read_only)
                .create_if_missing(false)
                .foreign_keys(foreign_keys)
                .busy_timeout(Duration::from_millis(5_000));
            if read_only {
                opts
            } else {
                opts.pragma("journal_mode", "WAL")
                    .pragma("synchronous", "FULL")
            }
        });
    Database::connect(options).await.map_err(database)
}

pub(crate) async fn sqlite_version<C: ConnectionTrait>(db: &C) -> Result<String> {
    let query = Query::select()
        .expr_as(
            Func::cust(Alias::new("sqlite_version")),
            Alias::new("version"),
        )
        .to_owned();
    let result = db
        .query_one(&query)
        .await
        .map_err(database)?
        .ok_or_else(|| integrity("Missing SQLite version."))?;
    let version: String = result.try_get("", "version").map_err(database)?;
    let parts: Vec<u32> = version
        .split('.')
        .map(str::parse)
        .collect::<std::result::Result<_, _>>()
        .map_err(|_| integrity("Invalid linked SQLite version."))?;
    if parts.as_slice() < [3, 51, 3].as_slice() {
        return Err(Error::new(
            ErrorCode::UnsupportedSqlite,
            "SQLite 3.51.3 or newer is required.",
        ));
    }
    Ok(version)
}

pub(crate) async fn has_tables<C: ConnectionTrait>(db: &C) -> Result<bool> {
    let query = Query::select()
        .column(Alias::new("name"))
        .from(Alias::new("sqlite_schema"))
        .and_where(Expr::col(Alias::new("type")).eq("table"))
        .and_where(Expr::col(Alias::new("name")).not_like(LikeExpr::new(r"sqlite\_%").escape('\\')))
        .limit(1)
        .to_owned();
    Ok(db.query_one(&query).await.map_err(database)?.is_some())
}

pub(crate) async fn preflight<C: ConnectionTrait>(
    db: &C,
    expected: &ProjectIdentity,
) -> Result<Option<i32>> {
    sqlite_version(db).await?;
    // A schema with missing metadata is corruption/foreign data, not permission
    // to initialize over it. No persistent pragma is changed on this connection.
    if !has_tables(db).await? {
        return Ok(None);
    }
    let metadata_table = Query::select()
        .column(Alias::new("name"))
        .from(Alias::new("sqlite_schema"))
        .and_where(Expr::col(Alias::new("name")).eq("project_meta"))
        .to_owned();
    if db
        .query_one(&metadata_table)
        .await
        .map_err(database)?
        .is_none()
    {
        return Err(integrity(
            "Existing database contains foreign tables without project metadata.",
        ));
    }
    // The old metadata shape must be readable before selecting any new column.
    let meta = db
        .query_one(
            &Query::select()
                .columns(
                    ["project_id", "scope", "schema_version", "format_version"].map(Alias::new),
                )
                .from(Alias::new("project_meta"))
                .and_where(Expr::col(Alias::new("singleton")).eq(1))
                .to_owned(),
        )
        .await
        .map_err(database)?
        .ok_or_else(|| integrity("Project metadata is missing."))?;
    let version: i32 = meta.try_get("", "schema_version").map_err(database)?;
    let format: i32 = meta.try_get("", "format_version").map_err(database)?;
    if version != schema::VERSION || format != schema::FORMAT {
        return Err(Error::new(
            ErrorCode::UnsupportedSchema,
            "Unsupported project format; run agentchat install --new-project explicitly. Existing history is not converted or reset.",
        ));
    }
    let project_id: String = meta.try_get("", "project_id").map_err(database)?;
    let scope: String = meta.try_get("", "scope").map_err(database)?;
    if project_id != expected.id.to_string() || scope != expected.scope.as_str() {
        return Err(Error::new(
            ErrorCode::ProjectBindingMismatch,
            "Database project identity or scope does not match.",
        ));
    }
    schema::validate(db).await?;
    Ok(Some(version))
}
