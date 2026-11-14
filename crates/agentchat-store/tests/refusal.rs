//! Foreign/incomplete databases must not be mistaken for empty agentchat databases.

use agentchat_core::{
    ErrorCode,
    project::{ProjectIdentity, Scope},
};
use agentchat_store::Store;
use sea_orm::{
    ConnectOptions, ConnectionTrait, Database,
    sea_query::{Alias, ColumnDef, Expr, ExprTrait, Query, Table},
};
use tempfile::TempDir;
use uuid::Uuid;

#[tokio::test]
async fn foreign_tables_are_preserved_including_literal_sqlite_prefixes() {
    for name in ["foreign_state", "sqliteXforeign"] {
        let root = TempDir::new().unwrap();
        let path = root.path().join("agentchat.sqlite3");
        let filename = path.clone();
        let mut options = ConnectOptions::new("sqlite::memory:");
        options.sqlx_logging(false).map_sqlx_sqlite_opts(move |o| {
            o.filename(&filename)
                .in_memory(false)
                .shared_cache(false)
                .create_if_missing(true)
        });
        let conn = Database::connect(options).await.unwrap();
        let table = Table::create()
            .table(Alias::new(name))
            .col(ColumnDef::new(Alias::new("id")).integer().primary_key())
            .to_owned();
        conn.execute(&table).await.unwrap();
        conn.close().await.unwrap();
        let before = std::fs::read(&path).unwrap();
        let identity = ProjectIdentity {
            id: Uuid::new_v4(),
            scope: Scope::Directory,
        };
        assert_eq!(
            Store::open(&path, identity).await.err().unwrap().code,
            ErrorCode::IntegrityError
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}

#[tokio::test]
async fn filename_is_not_interpreted_as_a_database_url() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("a ?mode=memory#'&name.sqlite3");
    let identity = ProjectIdentity {
        id: Uuid::new_v4(),
        scope: Scope::Directory,
    };
    let store = Store::open(&path, identity.clone()).await.unwrap();
    store.integrity_check().await.unwrap();
    store.close().await.unwrap();
    assert!(std::fs::metadata(&path).unwrap().len() > 0);
    Store::open(&path, identity)
        .await
        .unwrap()
        .close()
        .await
        .unwrap();
}

#[tokio::test]
async fn newer_migration_ledger_is_refused_without_modification() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("agentchat.sqlite3");
    let identity = ProjectIdentity {
        id: Uuid::new_v4(),
        scope: Scope::Directory,
    };
    Store::open(&path, identity.clone())
        .await
        .unwrap()
        .close()
        .await
        .unwrap();
    let filename = path.clone();
    let mut options = ConnectOptions::new("sqlite::memory:");
    options.sqlx_logging(false).map_sqlx_sqlite_opts(move |o| {
        o.filename(&filename)
            .in_memory(false)
            .shared_cache(false)
            .create_if_missing(false)
    });
    let conn = Database::connect(options).await.unwrap();
    let query = Query::update()
        .table(Alias::new("schema_migrations"))
        .value(Alias::new("version"), agentchat_store::SCHEMA_VERSION + 1)
        .and_where(Expr::col(Alias::new("version")).eq(2))
        .to_owned();
    conn.execute(&query).await.unwrap();
    conn.close().await.unwrap();
    let before = std::fs::read(&path).unwrap();
    assert_eq!(
        Store::open(&path, identity).await.err().unwrap().code,
        ErrorCode::UnsupportedSchema
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
}
