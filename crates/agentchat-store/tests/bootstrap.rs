//! T18 foundation: real SQLite through SeaORM, no direct driver/raw SQL paths.

use std::os::unix::fs::PermissionsExt;

use agentchat_core::{
    ErrorCode,
    project::{ProjectIdentity, Scope},
};
use agentchat_store::Store;
use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseConnection,
    sea_query::{Alias, Expr, ExprTrait, Query},
};
use tempfile::TempDir;
use uuid::Uuid;

fn identity() -> ProjectIdentity {
    ProjectIdentity {
        id: Uuid::new_v4(),
        scope: Scope::Directory,
    }
}

async fn fixture(path: &std::path::Path) -> DatabaseConnection {
    let path = path.to_owned();
    let mut opts = ConnectOptions::new("sqlite::memory:");
    opts.max_connections(1)
        .sqlx_logging(false)
        .map_sqlx_sqlite_opts(move |o| o.filename(&path).in_memory(false).create_if_missing(false));
    Database::connect(opts).await.unwrap()
}

#[tokio::test]
async fn initializes_reopens_and_checks_actual_sqlite() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("private/agentchat.sqlite3");
    let id = identity();
    let store = Store::open(&path, id.clone()).await.unwrap();
    assert_eq!(store.identity(), &id);
    let version = store.sqlite_version().await.unwrap();
    println!("linked SQLite {version}");
    let settings = store.connection_settings().await.unwrap();
    assert_eq!(settings.journal_mode, "wal");
    assert_eq!(settings.synchronous, 2);
    assert!(settings.foreign_keys);
    assert_eq!(settings.busy_timeout_ms, 5_000);
    store.integrity_check().await.unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    store.close().await.unwrap();
    let store = Store::open(&path, id.clone()).await.unwrap();
    store.integrity_check().await.unwrap();
    store.close().await.unwrap();
    assert_eq!(
        Store::open(&path, identity()).await.err().unwrap().code,
        ErrorCode::ProjectBindingMismatch
    );
}

#[tokio::test]
async fn newer_schema_is_refused_without_changing_database() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("agentchat.sqlite3");
    let id = identity();
    Store::open(&path, id.clone())
        .await
        .unwrap()
        .close()
        .await
        .unwrap();
    let conn = fixture(&path).await;
    let update = Query::update()
        .table(Alias::new("project_meta"))
        .value(
            Alias::new("schema_version"),
            agentchat_store::SCHEMA_VERSION + 1,
        )
        .to_owned();
    conn.execute(&update).await.unwrap();
    conn.close().await.unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(
        Store::open(&path, id).await.err().unwrap().code,
        ErrorCode::UnsupportedSchema
    );
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

#[tokio::test]
async fn checksum_drift_is_detected_read_only() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("agentchat.sqlite3");
    let id = identity();
    Store::open(&path, id.clone())
        .await
        .unwrap()
        .close()
        .await
        .unwrap();
    let conn = fixture(&path).await;
    let update = Query::update()
        .table(Alias::new("schema_migrations"))
        .value(Alias::new("checksum"), "tampered")
        .and_where(Expr::col(Alias::new("version")).eq(1))
        .to_owned();
    conn.execute(&update).await.unwrap();
    conn.close().await.unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(
        Store::open(&path, id).await.err().unwrap().code,
        ErrorCode::IntegrityError
    );
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_bootstrap_preserves_one_identity() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("agentchat.sqlite3");
    let id = identity();
    let mut jobs = Vec::new();
    for _ in 0..8 {
        let path = path.clone();
        let id = id.clone();
        jobs.push(tokio::spawn(async move {
            let store = Store::open(path, id.clone()).await.unwrap();
            assert_eq!(store.identity(), &id);
            store.close().await.unwrap();
        }));
    }
    for job in jobs {
        job.await.unwrap();
    }
    let conn = fixture(&path).await;
    let query = Query::select()
        .column(Alias::new("project_id"))
        .from(Alias::new("project_meta"))
        .to_owned();
    let rows = conn.query_all(&query).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].try_get::<String>("", "project_id").unwrap(),
        id.id.to_string()
    );
    conn.close().await.unwrap();
}
