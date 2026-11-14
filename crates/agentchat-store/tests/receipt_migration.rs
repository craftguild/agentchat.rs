//! Released receipt history is refused explicitly, never converted or reset.
mod support;
use agentchat_core::{
    ErrorCode,
    project::{ProjectIdentity, Scope},
};
use agentchat_store::{Store, migrations::Migrator};
use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseConnection, Value,
    sea_query::{Alias, Asterisk, Query},
};
use sea_orm_migration::MigratorTrait;
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use uuid::Uuid;

async fn insert(db: &DatabaseConnection, table: &str, columns: &[&str], values: Vec<Value>) {
    db.execute(
        &Query::insert()
            .into_table(Alias::new(table))
            .columns(columns.iter().copied().map(Alias::new))
            .values_panic(
                values
                    .into_iter()
                    .map(sea_orm::sea_query::SimpleExpr::Value),
            )
            .to_owned(),
    )
    .await
    .unwrap();
}
async fn rows(db: &DatabaseConnection, table: &str) -> Vec<sea_orm::QueryResult> {
    db.query_all(
        &Query::select()
            .column(Asterisk)
            .from(Alias::new(table))
            .to_owned(),
    )
    .await
    .unwrap()
}
struct Legacy {
    root: TempDir,
    path: PathBuf,
    identity: ProjectIdentity,
    token: Uuid,
    delivery: Uuid,
    body: String,
}
async fn released_v11() -> Legacy {
    let root = TempDir::new().unwrap();
    let path = root.path().join("agentchat.sqlite3");
    let target = path.clone();
    let identity = ProjectIdentity {
        id: Uuid::new_v4(),
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
    Migrator::up(&db, Some(11)).await.unwrap();
    let at = "2026-10-15T00:00:00.000Z";
    let read_at = "2026-10-15T00:00:01.000Z";
    let author = Uuid::new_v4().to_string();
    let recipient = Uuid::new_v4().to_string();
    let channel = Uuid::new_v4().to_string();
    let message = Uuid::new_v4().to_string();
    let events: Vec<String> = (0..6).map(|_| Uuid::new_v4().to_string()).collect();
    insert(
        &db,
        "project_meta",
        &[
            "singleton",
            "project_id",
            "scope",
            "created_at",
            "schema_version",
            "format_version",
        ],
        vec![
            1.into(),
            identity.id.to_string().into(),
            "directory".into(),
            at.into(),
            11.into(),
            1.into(),
        ],
    )
    .await;
    for (index, kind) in [
        "agent.joined",
        "agent.joined",
        "channel.created",
        "message.created",
        "message.read",
        "message.read",
    ]
    .into_iter()
    .enumerate()
    {
        insert(
            &db,
            "events",
            &[
                "seq",
                "event_id",
                "version",
                "kind",
                "occurred_at",
                "data",
                "sealed",
            ],
            vec![
                (index as i64 + 1).into(),
                events[index].clone().into(),
                1.into(),
                kind.into(),
                at.into(),
                serde_json::json!({"legacy":true}).into(),
                true.into(),
            ],
        )
        .await;
    }
    for (id, handle, seq) in [(&author, "author", 1i64), (&recipient, "recipient", 2i64)] {
        insert(
            &db,
            "actors",
            &[
                "id",
                "kind",
                "host_kind",
                "host_session_id",
                "handle",
                "participation",
                "first_join_at",
                "join_seq",
            ],
            vec![
                id.clone().into(),
                "agent".into(),
                "codex".into(),
                Uuid::new_v4().to_string().into(),
                handle.into(),
                "joined".into(),
                at.into(),
                seq.into(),
            ],
        )
        .await;
    }
    insert(
        &db,
        "channels",
        &["id", "name", "creator_id", "creation_seq"],
        vec![
            channel.clone().into(),
            "dev".into(),
            author.clone().into(),
            3i64.into(),
        ],
    )
    .await;
    let body = "retained\0body\r\n[AgentChat:Message] 日本語".to_owned();
    insert(
        &db,
        "messages",
        &[
            "id",
            "channel_id",
            "author_id",
            "body",
            "created_at",
            "creation_seq",
        ],
        vec![
            message.clone().into(),
            channel.into(),
            author.clone().into(),
            body.clone().into(),
            at.into(),
            4i64.into(),
        ],
    )
    .await;
    let delivery = Uuid::new_v4();
    for (id, event, state, attempts) in [
        (delivery.to_string(), events[3].clone(), "inflight", 1),
        (Uuid::new_v4().to_string(), events[4].clone(), "pending", 0),
        (Uuid::new_v4().to_string(), events[5].clone(), "inflight", 1),
    ] {
        insert(
            &db,
            "deliveries",
            &[
                "id",
                "event_id",
                "recipient_id",
                "state",
                "wake_eligible",
                "attempts",
            ],
            vec![
                id.into(),
                event.into(),
                recipient.clone().into(),
                state.into(),
                false.into(),
                attempts.into(),
            ],
        )
        .await;
    }
    let generation = Uuid::new_v4().to_string();
    let incarnation = Uuid::new_v4().to_string();
    let token = Uuid::new_v4();
    insert(
        &db,
        "launches",
        &[
            "generation",
            "agent_id",
            "context_id",
            "incarnation",
            "owner",
            "start_seq",
            "started_at",
        ],
        vec![
            generation.clone().into(),
            recipient.clone().into(),
            Uuid::new_v4().to_string().into(),
            incarnation.clone().into(),
            "legacy-test-owner".into(),
            3i64.into(),
            at.into(),
        ],
    )
    .await;
    insert(
        &db,
        "message_streams",
        &[
            "generation",
            "accepted_seq",
            "head_delivery",
            "head_seq",
            "incarnation",
            "token",
            "attempts",
            "retry_at",
            "awaiting_acceptance",
            "submission_pending",
            "cancelled",
        ],
        vec![
            generation.into(),
            3i64.into(),
            delivery.to_string().into(),
            4i64.into(),
            incarnation.into(),
            token.to_string().into(),
            1.into(),
            "2026-10-15T00:00:30.000Z".into(),
            true.into(),
            false.into(),
            false.into(),
        ],
    )
    .await;
    insert(
        &db,
        "read_receipts",
        &["message_id", "actor_id", "first_read_at"],
        vec![
            message.clone().into(),
            recipient.clone().into(),
            read_at.into(),
        ],
    )
    .await;
    insert(
        &db,
        "read_confirmations",
        &[
            "message_id",
            "recipient_id",
            "author_id",
            "deadline",
            "state",
        ],
        vec![
            message.into(),
            recipient.into(),
            author.into(),
            "2026-10-15T00:01:00.000Z".into(),
            "waiting".into(),
        ],
    )
    .await;
    db.close().await.unwrap();
    Legacy {
        root,
        path,
        identity,
        token,
        delivery,
        body,
    }
}
fn no_backup(root: &Path) {
    assert!(std::fs::read_dir(root).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains(".schema-v")
    }));
}

#[tokio::test]
async fn released_original_uncertainty_and_explicit_first_read_are_preserved_by_refusal() {
    let f = released_v11().await;
    let before = std::fs::read(&f.path).unwrap();
    let error = Store::open(&f.path, f.identity.clone())
        .await
        .err()
        .unwrap();
    assert_eq!(error.code, ErrorCode::UnsupportedSchema);
    assert!(error.message.contains("agentchat install --new-project"));
    assert_eq!(std::fs::read(&f.path).unwrap(), before);
    no_backup(f.root.path());
    let db = support::upgrade::inspect(&f.path).await;
    let stream = rows(&db, "message_streams").await;
    assert_eq!(stream.len(), 1);
    assert_eq!(
        stream[0].try_get::<String>("", "token").unwrap(),
        f.token.to_string()
    );
    assert_eq!(
        stream[0].try_get::<String>("", "head_delivery").unwrap(),
        f.delivery.to_string()
    );
    assert_eq!(stream[0].try_get::<i32>("", "attempts").unwrap(), 1);
    assert!(
        stream[0]
            .try_get::<bool>("", "awaiting_acceptance")
            .unwrap()
    );
    assert_eq!(
        rows(&db, "messages").await[0]
            .try_get::<String>("", "body")
            .unwrap(),
        f.body
    );
    assert_eq!(
        rows(&db, "read_receipts").await[0]
            .try_get::<String>("", "first_read_at")
            .unwrap(),
        "2026-10-15T00:00:01.000Z"
    );
    assert!(stream[0].try_get::<i64>("", "settled_seq").is_err());
    db.close().await.unwrap();
}

#[tokio::test]
async fn repeated_old_format_open_never_retires_positive_read_rows_or_invents_new_receipts() {
    let f = released_v11().await;
    let before = std::fs::read(&f.path).unwrap();
    for _ in 0..2 {
        assert_eq!(
            Store::open(&f.path, f.identity.clone())
                .await
                .err()
                .unwrap()
                .code,
            ErrorCode::UnsupportedSchema
        );
    }
    assert_eq!(std::fs::read(&f.path).unwrap(), before);
    no_backup(f.root.path());
    let db = support::upgrade::inspect(&f.path).await;
    assert!(
        !sea_orm_migration::SchemaManager::new(&db)
            .has_column("deliveries", "delivered_at")
            .await
            .unwrap()
    );
    let mut deliveries = rows(&db, "deliveries")
        .await
        .into_iter()
        .map(|row| {
            assert!(
                row.try_get::<Option<String>>("", "acknowledged_at")
                    .unwrap()
                    .is_none()
            );
            (
                row.try_get::<String>("", "state").unwrap(),
                row.try_get::<i32>("", "attempts").unwrap(),
            )
        })
        .collect::<Vec<_>>();
    deliveries.sort();
    assert_eq!(
        deliveries,
        vec![
            ("inflight".into(), 1),
            ("inflight".into(), 1),
            ("pending".into(), 0)
        ]
    );
    assert_eq!(
        rows(&db, "read_confirmations").await[0]
            .try_get::<String>("", "state")
            .unwrap(),
        "waiting"
    );
    assert_eq!(
        rows(&db, "project_meta").await[0]
            .try_get::<i32>("", "schema_version")
            .unwrap(),
        11
    );
    assert_eq!(rows(&db, "seaql_migrations").await.len(), 11);
    db.close().await.unwrap();
}
