//! Minimal SeaORM/SQLite diagnostic; never opens an AgentChat database.

use std::{
    error::Error,
    fs::{DirBuilder, OpenOptions},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::Path,
    time::{Duration, Instant},
};

use sea_orm::{
    ActiveModelTrait, ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DbBackend,
    EntityTrait, PaginatorTrait, Schema, Set, SqliteTransactionMode, TransactionOptions,
    TransactionTrait,
    sea_query::{Alias, Func, Query},
};
use serde_json::{Value, json};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

mod row {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "probe_rows")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: i64,
        #[sea_orm(column_type = "Text")]
        pub body: String,
    }
    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
    impl ActiveModelBehavior for ActiveModel {}
}

async fn connect(path: &Path, mode: &str) -> Result<DatabaseConnection> {
    let memory = mode == "memory";
    let sync = if mode == "normal" { "NORMAL" } else { "FULL" };
    let path = path.to_owned();
    let mut options = ConnectOptions::new("sqlite::memory:");
    options
        .min_connections(1)
        .max_connections(1)
        .sqlx_logging(false)
        .record_stmt_in_spans(false)
        .map_sqlx_sqlite_opts(move |opts| {
            let opts = opts
                .in_memory(memory)
                .shared_cache(false)
                .create_if_missing(false)
                .foreign_keys(true)
                .busy_timeout(Duration::from_millis(5_000))
                .pragma("synchronous", sync)
                .pragma("wal_autocheckpoint", "1000");
            if memory {
                opts.pragma("journal_mode", "MEMORY")
            } else {
                opts.filename(&path).pragma("journal_mode", "WAL")
            }
        });
    Ok(Database::connect(options).await?)
}

async fn setting(db: &DatabaseConnection, name: &str, column: &str) -> Result<Value> {
    let query = Query::select()
        .column(Alias::new(column))
        .from_function(Func::cust(Alias::new(name)), Alias::new("setting"))
        .to_owned();
    let row = db.query_one(&query).await?.ok_or("Missing setting")?;
    if let Ok(value) = row.try_get::<i64>("", column) {
        Ok(json!(value))
    } else {
        Ok(json!(row.try_get::<String>("", column)?))
    }
}

async fn settings(db: &DatabaseConnection) -> Result<Value> {
    let mut settings = serde_json::Map::new();
    for (key, column) in [
        ("journal_mode", "journal_mode"),
        ("synchronous", "synchronous"),
        ("foreign_keys", "foreign_keys"),
        ("busy_timeout", "timeout"),
        ("page_size", "page_size"),
        ("cache_size", "cache_size"),
        ("fullfsync", "fullfsync"),
        ("checkpoint_fullfsync", "checkpoint_fullfsync"),
    ] {
        settings.insert(
            key.into(),
            setting(db, &format!("pragma_{key}"), column).await?,
        );
    }
    for function in ["sqlite_version", "sqlite_source_id"] {
        let query = Query::select()
            .expr_as(Func::cust(Alias::new(function)), Alias::new("value"))
            .to_owned();
        let value: String = db
            .query_one(&query)
            .await?
            .ok_or("Missing version")?
            .try_get("", "value")?;
        settings.insert(function.into(), json!(value));
    }
    Ok(Value::Object(settings))
}

fn elapsed_ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1_000.0
}

fn distribution(values: &mut [f64]) -> Value {
    values.sort_by(f64::total_cmp);
    let n = values.len();
    json!({"samples": n, "sum_ms": values.iter().sum::<f64>(),
        "p50_ms": values[(n - 1) / 2], "p95_ms": values[(n * 95).div_ceil(100) - 1],
        "max_ms": values[n - 1]})
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 3 || !["memory", "full", "normal", "full-batch8"].contains(&args[0].as_str()) {
        return Err("Usage: sqlite_probe MODE ROWS NEW_OUTPUT_DIRECTORY".into());
    }
    let mode = &args[0];
    let rows: i64 = args[1].parse()?;
    if !(8..=1_000_000).contains(&rows) || rows % 8 != 0 {
        return Err("ROWS must be a multiple of eight between 8 and 1000000".into());
    }
    let root = Path::new(&args[2]);
    DirBuilder::new().mode(0o700).create(root)?;
    let path = root.join("probe.sqlite3");
    if mode != "memory" {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?
            .sync_all()?;
    }
    let db = connect(&path, mode).await?;
    let configuration = settings(&db).await?;
    assert_eq!(
        configuration["journal_mode"],
        if mode == "memory" { "memory" } else { "wal" }
    );
    assert_eq!(
        configuration["synchronous"],
        if mode == "normal" { 1 } else { 2 }
    );
    assert_eq!(configuration["foreign_keys"], 1);
    assert_eq!(configuration["busy_timeout"], 5_000);
    // This disposable one-table schema is generated from its model. It neither
    // invokes nor alters the application's migration ledger or production store.
    db.execute(&Schema::new(DbBackend::Sqlite).create_table_from_entity(row::Entity))
        .await?;
    let batch = if mode == "full-batch8" { 8 } else { 1 };
    let body = "x".repeat(512);
    let mut begin = Vec::new();
    let mut insert = Vec::new();
    let mut commit = Vec::new();
    let mut transaction = Vec::new();
    let write_started = Instant::now();
    for first in (0..rows).step_by(batch as usize) {
        let started = Instant::now();
        let stage = Instant::now();
        let tx = db
            .begin_with_options(TransactionOptions {
                sqlite_transaction_mode: Some(SqliteTransactionMode::Immediate),
                ..Default::default()
            })
            .await?;
        begin.push(elapsed_ms(stage));
        let stage = Instant::now();
        // Each row uses the same ORM insert. Batch8 changes only how many of
        // those identical inserts share one transaction and its final commit.
        for id in first..first + batch {
            row::ActiveModel {
                id: Set(id),
                body: Set(body.clone()),
            }
            .insert(&tx)
            .await?;
        }
        insert.push(elapsed_ms(stage));
        let stage = Instant::now();
        tx.commit().await?;
        commit.push(elapsed_ms(stage));
        transaction.push(elapsed_ms(started));
    }
    let write_ms = elapsed_ms(write_started);
    let mut reads = Vec::new();
    for id in 0..rows {
        let started = Instant::now();
        let found = row::Entity::find_by_id(id)
            .one(&db)
            .await?
            .ok_or("Missing row")?;
        reads.push(elapsed_ms(started));
        assert_eq!(found.body, body);
    }
    assert_eq!(row::Entity::find().count(&db).await?, rows as u64);
    let close_started = Instant::now();
    db.close().await?;
    let close_ms = elapsed_ms(close_started);
    if mode != "memory" {
        let reopened = connect(&path, mode).await?;
        assert_eq!(row::Entity::find().count(&reopened).await?, rows as u64);
        assert_eq!(
            row::Entity::find_by_id(rows - 1)
                .one(&reopened)
                .await?
                .ok_or("Missing reopened row")?
                .body,
            body
        );
        reopened.close().await?;
    }
    let report = json!({"version": 1, "sea_orm": "2.0.4", "mode": mode,
        "rows": rows, "payload_bytes": body.len(), "rows_per_transaction": batch,
        "settings": configuration, "configured_wal_autocheckpoint_pages": 1000, "write_ms": write_ms, "rows_per_second": rows as f64 * 1000.0 / write_ms,
        "begin": distribution(&mut begin), "insert": distribution(&mut insert),
        "commit": distribution(&mut commit), "transaction": distribution(&mut transaction),
        "point_read": distribution(&mut reads), "close_ms": close_ms,
        "verified_rows": rows, "disk_reopen_verified": mode != "memory"});
    std::fs::write(
        root.join("result.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!("{}", serde_json::to_string(&report)?);
    Ok(())
}
