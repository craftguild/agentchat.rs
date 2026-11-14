//! Test-only retained-schema fixtures, with unchanged historical DDL and ledgers.
use super::Fixture;
use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseConnection, TransactionTrait,
    sea_query::{Alias, Asterisk, Expr, ExprTrait, Index, Query as Sql, Table},
};
pub async fn rows(db: &DatabaseConnection, table: &str) -> Vec<sea_orm::QueryResult> {
    db.query_all(
        &Sql::select()
            .column(Asterisk)
            .from(Alias::new(table))
            .to_owned(),
    )
    .await
    .unwrap()
}
pub async fn inspect(path: &std::path::Path) -> DatabaseConnection {
    let path = path.to_owned();
    let mut options = ConnectOptions::new("sqlite::memory:");
    options.sqlx_logging(false).map_sqlx_sqlite_opts(move |o| {
        o.filename(&path)
            .in_memory(false)
            .create_if_missing(false)
            .read_only(true)
    });
    Database::connect(options).await.unwrap()
}
pub async fn legacy(f: &Fixture) {
    retained(f, 1).await;
}
pub async fn retained(f: &Fixture, version: i32) {
    assert!((1..=11).contains(&version));
    // This fixture removes only later migration objects and their ledger rows.
    // The product deliberately exposes no schema downgrade path.
    let path = f.root.path().join("agentchat.sqlite3");
    let mut options = ConnectOptions::new("sqlite::memory:");
    options
        .max_connections(1)
        .sqlx_logging(false)
        .map_sqlx_sqlite_opts(move |o| {
            o.filename(&path)
                .in_memory(false)
                .create_if_missing(false)
                .foreign_keys(false)
        });
    let rebuild = Database::connect(options).await.unwrap();
    let tx = rebuild.begin().await.unwrap();
    super::receipt_legacy::restore(&tx).await;
    if version < 11 {
        super::actor_legacy::restore(&tx).await;
    }
    if version < 10 {
        tx.execute(
            &Table::drop()
                .table(Alias::new("project_receipt_settings"))
                .to_owned(),
        )
        .await
        .unwrap();
    }
    if version < 9 {
        tx.execute(
            &Table::drop()
                .table(Alias::new("project_default_channels"))
                .to_owned(),
        )
        .await
        .unwrap();
    }
    if version < 5 {
        tx.execute(
            &Table::drop()
                .table(Alias::new("message_streams"))
                .to_owned(),
        )
        .await
        .unwrap();
    } else if version < 7 {
        tx.execute(
            &Table::alter()
                .table(Alias::new("message_streams"))
                .drop_column(Alias::new("submission_pending"))
                .to_owned(),
        )
        .await
        .unwrap();
        if version < 6 {
            tx.execute(
                &Table::alter()
                    .table(Alias::new("message_streams"))
                    .drop_column(Alias::new("awaiting_acceptance"))
                    .to_owned(),
            )
            .await
            .unwrap();
        }
    }
    if version < 4 {
        tx.execute(&Table::drop().table(Alias::new("launches")).to_owned())
            .await
            .unwrap();
    }
    if version < 3 {
        for name in ["idx_event_sealed", "idx_batch_recipient_state_incarnation"] {
            tx.execute(&Index::drop().name(name).to_owned())
                .await
                .unwrap();
        }
    }
    if version == 1 {
        tx.execute(
            &Table::drop()
                .table(Alias::new("read_confirmations"))
                .to_owned(),
        )
        .await
        .unwrap();
    }
    let mut names = vec!["m20261028_000012_receipt_policy"];
    if version < 11 {
        names.push("m20261028_000011_common_actors");
    }
    if version < 10 {
        names.push("m20261028_000010_project_receipt_settings");
    }
    if version < 9 {
        names.push("m20261026_000009_project_channel_policy");
    }
    if version < 8 {
        names.push("m20261019_000008_claude_uncertainty");
    }
    if version < 7 {
        names.push("m20261019_000007_native_submission");
    }
    if version < 6 {
        names.push("m20261018_000006_message_acceptance");
    }
    if version < 5 {
        names.push("m20261018_000005_message_streams");
    }
    if version < 4 {
        names.push("m20261018_000004_launches");
    }
    if version < 3 {
        names.push("m20261015_000003_history_indexes");
    }
    if version < 2 {
        names.push("m20261015_000002_read_confirmations");
    }
    tx.execute(
        &Sql::delete()
            .from_table(Alias::new("seaql_migrations"))
            .and_where(Expr::col(Alias::new("version")).is_in(names))
            .to_owned(),
    )
    .await
    .unwrap();
    tx.execute(
        &Sql::delete()
            .from_table(Alias::new("schema_migrations"))
            .and_where(Expr::col(Alias::new("version")).gt(version))
            .to_owned(),
    )
    .await
    .unwrap();
    tx.execute(
        &Sql::update()
            .table(Alias::new("project_meta"))
            .value(Alias::new("schema_version"), version)
            .to_owned(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    rebuild.close().await.unwrap();
}
pub fn backups(f: &Fixture) -> Vec<std::path::PathBuf> {
    std::fs::read_dir(f.root.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .contains(".schema-v")
                && p.extension().is_some_and(|s| s == "sqlite3")
        })
        .collect()
}
