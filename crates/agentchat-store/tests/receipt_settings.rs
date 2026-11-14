//! Real Store configuration, consistent snapshots, guarded upgrade and backups.

mod support;

use agentchat_core::{
    ErrorCode,
    ports::Storage,
    project::{ProjectReceiptSettings, ReceiptSettingsUpdate},
};
use agentchat_store::Store;
use sea_orm::{
    ConnectionTrait,
    sea_query::{Alias, Query},
};
use support::Fixture;

#[tokio::test]
async fn new_settings_reconfigure_atomically_and_preserve_snapshot_and_backup() {
    let f = Fixture::new().await;
    // Common fixtures explicitly disable automatic reading; this test selects it.
    f.app
        .initialize_with_settings(
            None,
            ReceiptSettingsUpdate {
                auto_read: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let mut before = f.store.snapshot().await.unwrap();
    assert_eq!(
        before.receipt_settings().await.unwrap(),
        ProjectReceiptSettings::default()
    );
    let initial_head = before.head();
    let snapshot = before.receipt_settings().await.unwrap().input_policy();
    let result = f
        .app
        .initialize_with_settings(
            None,
            ReceiptSettingsUpdate {
                auto_read: Some(false),
                delivery_confirmation_timeout_seconds: Some(123),
            },
        )
        .await
        .unwrap();
    let expected = ProjectReceiptSettings {
        auto_read: false,
        delivery_confirmation_timeout_seconds: 123,
    };
    assert_eq!(
        result.result["receipt_settings"],
        serde_json::to_value(expected).unwrap()
    );
    assert!(result.event_head.is_none());
    assert_eq!(
        before.receipt_settings().await.unwrap(),
        ProjectReceiptSettings::default()
    );
    assert!(snapshot.auto_read);
    drop(before);
    let mut after = f.store.snapshot().await.unwrap();
    assert_eq!(after.head(), initial_head);
    assert_eq!(after.receipt_settings().await.unwrap(), expected);
    drop(after);
    let omitted = f
        .app
        .initialize_with_settings(None, Default::default())
        .await
        .unwrap();
    assert_eq!(omitted.result["receipt_settings_changed"], false);
    assert_eq!(
        omitted.result["receipt_settings"],
        serde_json::to_value(expected).unwrap()
    );
    let bad = f
        .app
        .initialize_with_settings(
            Some(vec!["other".into()]),
            ReceiptSettingsUpdate {
                delivery_confirmation_timeout_seconds: Some(0),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(bad.code, ErrorCode::InvalidArgument);
    assert_eq!(f.count("channels").await, 1);
    let backup = f.root.path().join("receipt-policy-backup.sqlite3");
    f.store.backup(&backup).await.unwrap();
    let restored = Store::open(&backup, f.store.identity().clone())
        .await
        .unwrap();
    assert_eq!(
        restored
            .snapshot()
            .await
            .unwrap()
            .receipt_settings()
            .await
            .unwrap(),
        expected
    );
    restored.integrity_check().await.unwrap();
    restored.close().await.unwrap();
}

#[tokio::test]
async fn released_settings_are_refused_without_automatic_upgrade_or_backup() {
    use agentchat_core::project::{ProjectIdentity, Scope};
    use agentchat_store::migrations::Migrator;
    use sea_orm::{ConnectOptions, Database};
    use sea_orm_migration::MigratorTrait;
    let root = tempfile::TempDir::new().unwrap();
    let path = root.path().join("agentchat.sqlite3");
    let target = path.clone();
    let expected = ProjectIdentity {
        id: uuid::Uuid::new_v4(),
        scope: Scope::Directory,
    };
    let mut options = ConnectOptions::new("sqlite::memory:");
    options
        .max_connections(1)
        .sqlx_logging(false)
        .map_sqlx_sqlite_opts(move |o| {
            o.filename(&target)
                .in_memory(false)
                .create_if_missing(true)
                .foreign_keys(false)
        });
    let db = Database::connect(options).await.unwrap();
    Migrator::up(&db, Some(10)).await.unwrap();
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
                "2026-10-15T00:00:00.000Z".into(),
                10.into(),
                1.into(),
            ])
            .to_owned(),
    )
    .await
    .unwrap();
    db.execute(
        &Query::update()
            .table(Alias::new("project_receipt_settings"))
            .value(Alias::new("auto_ack"), false)
            .value(Alias::new("auto_read"), false)
            .value(Alias::new("delivery_confirmation_timeout_seconds"), 123)
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
    assert!(std::fs::read_dir(root.path()).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains(".schema-v")
    }));
}

#[tokio::test]
async fn settings_write_rolls_back_with_the_surrounding_transaction() {
    let f = Fixture::new().await;
    let mut tx = f.store.begin().await.unwrap();
    let original = tx.receipt_settings().await.unwrap();
    let changed = ProjectReceiptSettings {
        auto_read: true,
        delivery_confirmation_timeout_seconds: 42,
    };
    tx.save_receipt_settings(&changed).await.unwrap();
    assert_eq!(tx.receipt_settings().await.unwrap(), changed);
    tx.rollback().await.unwrap();
    assert_eq!(
        f.store
            .snapshot()
            .await
            .unwrap()
            .receipt_settings()
            .await
            .unwrap(),
        original
    );
}
