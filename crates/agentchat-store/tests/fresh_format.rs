//! Fresh-format setup refuses released history without modifying its database.
use agentchat_core::{
    ErrorCode,
    ports::Storage,
    project::{ProjectIdentity, Scope},
};
use agentchat_store::{Store, migrations::Migrator};
use sea_orm::{
    ConnectOptions, ConnectionTrait, Database,
    sea_query::{Alias, Query},
};
use sea_orm_migration::MigratorTrait;
use tempfile::TempDir;
use uuid::Uuid;

fn identity() -> ProjectIdentity {
    ProjectIdentity {
        id: Uuid::new_v4(),
        scope: Scope::Directory,
    }
}

#[tokio::test]
async fn released_v13_is_refused_read_only_with_explicit_new_project_guidance() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("agentchat.sqlite3");
    let target = path.clone();
    let expected = identity();
    let mut options = ConnectOptions::new("sqlite::memory:");
    options
        .max_connections(1)
        .sqlx_logging(false)
        .map_sqlx_sqlite_opts(move |options| {
            // The released actor-table reconstruction requires the same private,
            // schema-only connection used by guarded bootstrap. This disposable
            // fixture closes it before exercising the ordinary enforced open path.
            options
                .filename(&target)
                .in_memory(false)
                .create_if_missing(true)
                .foreign_keys(false)
        });
    let db = Database::connect(options).await.unwrap();
    Migrator::up(&db, Some(13)).await.unwrap();
    db.execute(
        &Query::insert()
            .into_table(Alias::new("project_meta"))
            .columns(
                [
                    "singleton",
                    "project_id",
                    "scope",
                    "created_at",
                    "schema_version",
                    "format_version",
                ]
                .map(Alias::new),
            )
            .values_panic([
                1.into(),
                expected.id.to_string().into(),
                "directory".into(),
                "2026-10-30T00:00:00.000Z".into(),
                13.into(),
                2.into(),
            ])
            .to_owned(),
    )
    .await
    .unwrap();
    db.close().await.unwrap();
    let before = std::fs::read(&path).unwrap();
    let error = Store::open(&path, expected).await.err().unwrap();
    assert_eq!(error.code, ErrorCode::UnsupportedSchema);
    assert!(error.message.contains("agentchat install --new-project"));
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[tokio::test]
async fn fresh_format_has_hour_default_and_reopens_without_conversion() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("agentchat.sqlite3");
    let expected = identity();
    let store = Store::open(&path, expected.clone()).await.unwrap();
    let mut tx = store.begin().await.unwrap();
    let settings = tx.receipt_settings().await.unwrap();
    assert_eq!(settings.delivery_confirmation_timeout_seconds, 3_600);
    assert!(settings.auto_read);
    tx.rollback().await.unwrap();
    store.integrity_check().await.unwrap();
    store.close().await.unwrap();
    let store = Store::open(&path, expected).await.unwrap();
    assert_eq!(agentchat_store::SCHEMA_VERSION, 14);
    store.integrity_check().await.unwrap();
    store.close().await.unwrap();
}
