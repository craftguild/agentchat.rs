//! Real SeaORM 2.x registry, read-only status, rollback and history preservation.

use agentchat_core::{
    ErrorCode,
    project::{ProjectIdentity, Scope},
};
use agentchat_store::{Store, migrations::Migrator};
use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseConnection, SqliteTransactionMode,
    TransactionOptions, TransactionTrait,
    sea_query::{Alias, ColumnDef, Expr, ExprTrait, Query, Table},
};
use sea_orm_migration::{
    DbErr, MigrationName, MigrationStatus, MigrationTrait, MigratorTrait, SchemaManager,
};
use tempfile::TempDir;
use uuid::Uuid;

async fn connect(path: &std::path::Path) -> DatabaseConnection {
    let filename = path.to_owned();
    let mut options = ConnectOptions::new("sqlite::memory:");
    options.sqlx_logging(false).map_sqlx_sqlite_opts(move |o| {
        o.filename(&filename)
            .in_memory(false)
            .shared_cache(false)
            .create_if_missing(true)
            .foreign_keys(false)
    });
    Database::connect(options).await.unwrap()
}

fn identity() -> ProjectIdentity {
    ProjectIdentity {
        id: Uuid::new_v4(),
        scope: Scope::Directory,
    }
}

async fn table_exists(conn: &DatabaseConnection, name: &str) -> bool {
    conn.query_one(
        &Query::select()
            .column(Alias::new("name"))
            .from(Alias::new("sqlite_schema"))
            .and_where(Expr::col(Alias::new("name")).eq(name))
            .to_owned(),
    )
    .await
    .unwrap()
    .is_some()
}

#[tokio::test]
async fn normal_open_uses_standard_seaorm_registry_and_repeats_without_duplicate_versions() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("agentchat.sqlite3");
    let id = identity();
    for _ in 0..3 {
        Store::open(&path, id.clone())
            .await
            .unwrap()
            .close()
            .await
            .unwrap();
    }
    let conn = connect(&path).await;
    let statuses = Migrator::get_migration_with_status_read_only(&conn)
        .await
        .unwrap();
    assert_eq!(statuses.len(), agentchat_store::SCHEMA_VERSION as usize);
    assert_eq!(statuses[0].name(), "m20261015_000001_initial");
    assert_eq!(statuses[0].status(), MigrationStatus::Applied);
    let rows = conn
        .query_all(
            &Query::select()
                .column(Alias::new("version"))
                .from(Alias::new("seaql_migrations"))
                .to_owned(),
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), agentchat_store::SCHEMA_VERSION as usize);
    for table in [
        "actors",
        "sessions",
        "channels",
        "dispatches",
        "dispatch_messages",
        "dispatch_notices",
        "delivery_state_transitions",
        "events",
        "deliveries",
        "wake_batches",
        "reservations",
        "reservation_patterns",
        "idempotency",
    ] {
        assert!(table_exists(&conn, table).await, "{table}");
    }
    let rows = conn
        .query_all(
            &Query::select()
                .column(Alias::new("name"))
                .from(Alias::new("sqlite_schema"))
                .and_where(Expr::col(Alias::new("type")).eq("table"))
                .to_owned(),
        )
        .await
        .unwrap();
    let mut actual = rows
        .iter()
        .map(|r| r.try_get::<String>("", "name").unwrap())
        .filter(|n| !n.starts_with("sqlite_"))
        .collect::<Vec<_>>();
    actual.sort();
    let mut expected = vec![
        "actors",
        "sessions",
        "channels",
        "channel_memberships",
        "dispatches",
        "dispatch_messages",
        "dispatch_notices",
        "events",
        "deliveries",
        "delivery_state_transitions",
        "message_streams",
        "read_receipts",
        "mentions",
        "wake_batches",
        "reservations",
        "reservation_patterns",
        "idempotency",
        "launches",
        "project_meta",
        "project_default_channels",
        "project_receipt_settings",
        "delivery_timeout_notices",
        "schema_migrations",
        "seaql_migrations",
    ];
    expected.sort();
    assert_eq!(
        actual, expected,
        "fresh14 has only the explicit Dispatch catalog"
    );
    conn.close().await.unwrap();
}

#[tokio::test]
async fn read_only_registry_check_never_installs_a_missing_tracking_table() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("agentchat.sqlite3");
    let id = identity();
    Store::open(&path, id.clone())
        .await
        .unwrap()
        .close()
        .await
        .unwrap();
    let conn = connect(&path).await;
    conn.execute(
        &Table::drop()
            .table(Alias::new("seaql_migrations"))
            .to_owned(),
    )
    .await
    .unwrap();
    conn.close().await.unwrap();
    let before = std::fs::read(&path).unwrap();
    assert_eq!(
        Store::open(&path, id).await.err().unwrap().code,
        ErrorCode::IntegrityError
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let conn = connect(&path).await;
    assert!(!table_exists(&conn, "seaql_migrations").await);
    conn.close().await.unwrap();
}

#[tokio::test]
async fn unknown_newer_seaorm_migration_is_refused_before_writes() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("agentchat.sqlite3");
    let id = identity();
    Store::open(&path, id.clone())
        .await
        .unwrap()
        .close()
        .await
        .unwrap();
    let conn = connect(&path).await;
    conn.execute(
        &Query::update()
            .table(Alias::new("seaql_migrations"))
            .value(Alias::new("version"), "m20261016_000003_future")
            .and_where(Expr::col(Alias::new("version")).eq("m20261015_000002_read_confirmations"))
            .to_owned(),
    )
    .await
    .unwrap();
    conn.close().await.unwrap();
    let before = std::fs::read(&path).unwrap();
    assert_eq!(
        Store::open(&path, id).await.err().unwrap().code,
        ErrorCode::UnsupportedSchema
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

struct FailingMigration;
impl MigrationName for FailingMigration {
    fn name(&self) -> &str {
        "m20261015_000004_test_failure"
    }
}
#[async_trait::async_trait]
impl MigrationTrait for FailingMigration {
    async fn up(&self, manager: &SchemaManager) -> std::result::Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("failed_upgrade"))
                    .col(ColumnDef::new(Alias::new("id")).integer().primary_key())
                    .to_owned(),
            )
            .await?;
        Err(DbErr::Custom(
            "Injected migration failure after DDL.".into(),
        ))
    }
    async fn down(&self, _: &SchemaManager) -> std::result::Result<(), DbErr> {
        Ok(())
    }
    fn use_transaction(&self) -> Option<bool> {
        Some(true)
    }
}

struct FailingMigrator;
#[async_trait::async_trait]
impl MigratorTrait for FailingMigrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        let mut migrations = Migrator::migrations();
        migrations.push(Box::new(FailingMigration));
        migrations
    }
}

#[tokio::test]
async fn failure_rolls_back_schema_and_standard_framework_tracking_together() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("agentchat.sqlite3");
    let conn = connect(&path).await;
    let tx = conn
        .begin_with_options(TransactionOptions {
            sqlite_transaction_mode: Some(SqliteTransactionMode::Immediate),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(FailingMigrator::up(&tx, None).await.is_err());
    tx.rollback().await.unwrap();
    for table in [
        "project_meta",
        "schema_migrations",
        "seaql_migrations",
        "agents",
        "failed_upgrade",
    ] {
        assert!(
            !table_exists(&conn, table).await,
            "partial schema survived: {table}"
        );
    }
    conn.close().await.unwrap();
    Store::open(&path, identity())
        .await
        .unwrap()
        .close()
        .await
        .unwrap();
}

#[tokio::test]
async fn failed_upgrade_and_downgrade_preserve_an_existing_project() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("agentchat.sqlite3");
    let id = identity();
    Store::open(&path, id.clone())
        .await
        .unwrap()
        .close()
        .await
        .unwrap();
    let conn = connect(&path).await;
    let tx = conn.begin().await.unwrap();
    assert!(FailingMigrator::up(&tx, None).await.is_err());
    tx.rollback().await.unwrap();
    assert!(!table_exists(&conn, "failed_upgrade").await);
    let tx = conn.begin().await.unwrap();
    assert!(Migrator::down(&tx, Some(1)).await.is_err());
    tx.rollback().await.unwrap();
    let states = Migrator::get_migration_with_status_read_only(&conn)
        .await
        .unwrap();
    assert_eq!(states[0].status(), MigrationStatus::Applied);
    conn.close().await.unwrap();
    let store = Store::open(&path, id.clone()).await.unwrap();
    assert_eq!(store.identity(), &id);
    store.integrity_check().await.unwrap();
    store.close().await.unwrap();
}

struct AddLabel;
impl MigrationName for AddLabel {
    fn name(&self) -> &str {
        "m20261015_000004_test_add_label"
    }
}
#[async_trait::async_trait]
impl MigrationTrait for AddLabel {
    async fn up(&self, manager: &SchemaManager) -> std::result::Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("project_meta"))
                    .add_column(ColumnDef::new(Alias::new("label")).text())
                    .to_owned(),
            )
            .await
    }
    async fn down(&self, _: &SchemaManager) -> std::result::Result<(), DbErr> {
        Err(DbErr::Custom("Test retains its upgrade history.".into()))
    }
    fn use_transaction(&self) -> Option<bool> {
        Some(true)
    }
}

struct ExtendedMigrator;
#[async_trait::async_trait]
impl MigratorTrait for ExtendedMigrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        let mut migrations = Migrator::migrations();
        migrations.push(Box::new(AddLabel));
        migrations
    }
}

#[tokio::test]
async fn later_named_migration_preserves_initial_data_and_records_each_name_once() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("agentchat.sqlite3");
    let id = identity();
    Store::open(&path, id.clone())
        .await
        .unwrap()
        .close()
        .await
        .unwrap();
    let conn = connect(&path).await;
    let manager = SchemaManager::new(&conn);
    assert!(!manager.has_column("project_meta", "label").await.unwrap());
    for _ in 0..2 {
        let tx = conn
            .begin_with_options(TransactionOptions {
                sqlite_transaction_mode: Some(SqliteTransactionMode::Immediate),
                ..Default::default()
            })
            .await
            .unwrap();
        ExtendedMigrator::up(&tx, None).await.unwrap();
        tx.commit().await.unwrap();
    }
    assert!(manager.has_column("project_meta", "label").await.unwrap());
    let statuses = ExtendedMigrator::get_migration_with_status_read_only(&conn)
        .await
        .unwrap();
    assert_eq!(statuses.len(), agentchat_store::SCHEMA_VERSION as usize + 1);
    assert_eq!(statuses[0].name(), "m20261015_000001_initial");
    assert_eq!(statuses[1].name(), "m20261015_000002_read_confirmations");
    assert_eq!(statuses[2].name(), "m20261015_000003_history_indexes");
    assert_eq!(statuses[3].name(), "m20261018_000004_launches");
    assert_eq!(statuses[4].name(), "m20261018_000005_message_streams");
    assert_eq!(statuses[5].name(), "m20261018_000006_message_acceptance");
    assert_eq!(statuses[6].name(), "m20261019_000007_native_submission");
    assert_eq!(statuses[7].name(), "m20261019_000008_claude_uncertainty");
    assert_eq!(
        statuses[8].name(),
        "m20261026_000009_project_channel_policy"
    );
    assert_eq!(
        statuses[9].name(),
        "m20261028_000010_project_receipt_settings"
    );
    assert_eq!(statuses[10].name(), "m20261028_000011_common_actors");
    assert_eq!(statuses[11].name(), "m20261028_000012_receipt_policy");
    assert_eq!(statuses[12].name(), "m20261031_000013_delivery_lifecycle");
    assert_eq!(statuses[13].name(), "m20261101_000014_dispatch_history");
    assert_eq!(statuses[14].name(), "m20261015_000004_test_add_label");
    assert!(
        statuses
            .iter()
            .all(|m| m.status() == MigrationStatus::Applied)
    );
    let row = conn
        .query_one(
            &Query::select()
                .column(Alias::new("project_id"))
                .from(Alias::new("project_meta"))
                .to_owned(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        row.try_get::<String>("", "project_id").unwrap(),
        id.id.to_string()
    );
    let rows = conn
        .query_all(
            &Query::select()
                .column(Alias::new("version"))
                .from(Alias::new("seaql_migrations"))
                .to_owned(),
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), agentchat_store::SCHEMA_VERSION as usize + 1);
    conn.close().await.unwrap();
    // This test registry is deliberately outside the supported product schema.
    let before = std::fs::read(&path).unwrap();
    assert_eq!(
        Store::open(&path, id).await.err().unwrap().code,
        ErrorCode::UnsupportedSchema
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
}
