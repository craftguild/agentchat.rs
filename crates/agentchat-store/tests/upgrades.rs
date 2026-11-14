//! Released namespaces are refused, never automatically converted or reset.
use agentchat_core::{
    ErrorCode,
    project::{ProjectIdentity, Scope},
};
use agentchat_store::{Store, migrations::Migrator};
use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseConnection,
    sea_query::{Alias, ColumnDef, Expr, ExprTrait, Query, Table},
};
use sea_orm_migration::MigratorTrait;
use tempfile::TempDir;
use uuid::Uuid;

async fn retained(
    path: &std::path::Path,
    identity: &ProjectIdentity,
    version: i32,
) -> DatabaseConnection {
    let file = path.to_owned();
    let mut options = ConnectOptions::new("sqlite::memory:");
    options
        .max_connections(1)
        .sqlx_logging(false)
        .map_sqlx_sqlite_opts(move |o| {
            o.filename(&file)
                .in_memory(false)
                .create_if_missing(true)
                .foreign_keys(false)
        });
    let db = Database::connect(options).await.unwrap();
    Migrator::up(&db, Some(version as u32)).await.unwrap();
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
                identity.id.to_string().into(),
                "directory".into(),
                "2026-10-30T00:00:00.000Z".into(),
                version.into(),
                1.into(),
            ])
            .to_owned(),
    )
    .await
    .unwrap();
    db
}

fn identity() -> ProjectIdentity {
    ProjectIdentity {
        id: Uuid::new_v4(),
        scope: Scope::Directory,
    }
}

fn backups(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    std::fs::read_dir(root)
        .unwrap()
        .filter_map(|entry| {
            let path = entry.unwrap().path();
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .contains("backup")
                .then_some(path)
        })
        .collect()
}

#[tokio::test]
async fn every_released_schema_is_refused_without_writing_history_or_a_backup() {
    for previous in 1..agentchat_store::SCHEMA_VERSION {
        let root = TempDir::new().unwrap();
        let path = root.path().join("agentchat.sqlite3");
        let expected = identity();
        retained(&path, &expected, previous)
            .await
            .close()
            .await
            .unwrap();
        let before = std::fs::read(&path).unwrap();
        for _ in 0..2 {
            let error = Store::open(&path, expected.clone()).await.err().unwrap();
            assert_eq!(
                error.code,
                ErrorCode::UnsupportedSchema,
                "schema {previous}"
            );
            assert!(error.message.contains("agentchat install --new-project"));
            assert_eq!(std::fs::read(&path).unwrap(), before, "schema {previous}");
            assert!(backups(root.path()).is_empty());
        }
        // Explicit setup can choose a fresh UUID/path while old history remains.
        let fresh = root.path().join("new-project.sqlite3");
        Store::open(&fresh, identity())
            .await
            .unwrap()
            .close()
            .await
            .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}

#[tokio::test]
async fn noncanonical_old_namespace_is_not_repaired_or_reset() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("agentchat.sqlite3");
    let expected = identity();
    let db = retained(&path, &expected, 12).await;
    db.execute(
        &Table::create()
            .table(Alias::new("foreign_history"))
            .col(ColumnDef::new(Alias::new("body")).text())
            .to_owned(),
    )
    .await
    .unwrap();
    db.execute(
        &Query::insert()
            .into_table(Alias::new("foreign_history"))
            .columns([Alias::new("body")])
            .values_panic(["preserve these bytes".into()])
            .to_owned(),
    )
    .await
    .unwrap();
    db.close().await.unwrap();
    let before = std::fs::read(&path).unwrap();
    assert_eq!(
        Store::open(&path, expected).await.err().unwrap().code,
        ErrorCode::UnsupportedSchema
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(backups(root.path()).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_old_namespace_opens_all_refuse_without_racing_an_upgrade() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("agentchat.sqlite3");
    let expected = identity();
    retained(&path, &expected, 12).await.close().await.unwrap();
    let before = std::fs::read(&path).unwrap();
    let mut jobs = Vec::new();
    for _ in 0..8 {
        let path = path.clone();
        let expected = expected.clone();
        jobs.push(tokio::spawn(async move {
            Store::open(path, expected).await.err().unwrap().code
        }));
    }
    for job in jobs {
        assert_eq!(job.await.unwrap(), ErrorCode::UnsupportedSchema);
    }
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(backups(root.path()).is_empty());
}

#[tokio::test]
async fn old_format_marker_is_refused_even_when_schema_number_matches() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("agentchat.sqlite3");
    let expected = identity();
    Store::open(&path, expected.clone())
        .await
        .unwrap()
        .close()
        .await
        .unwrap();
    let file = path.clone();
    let mut options = ConnectOptions::new("sqlite::memory:");
    options
        .max_connections(1)
        .sqlx_logging(false)
        .map_sqlx_sqlite_opts(move |o| o.filename(&file).in_memory(false).create_if_missing(false));
    let db = Database::connect(options).await.unwrap();
    db.execute(
        &Query::update()
            .table(Alias::new("project_meta"))
            .value(Alias::new("format_version"), 1)
            .and_where(Expr::col(Alias::new("singleton")).eq(1))
            .to_owned(),
    )
    .await
    .unwrap();
    db.close().await.unwrap();
    let before = std::fs::read(&path).unwrap();
    assert_eq!(
        Store::open(&path, expected).await.err().unwrap().code,
        ErrorCode::UnsupportedSchema
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
}
