//! Manual profiling and independent restore audit of a closed workload backup.
mod support;

use agentchat_core::project::{ProjectIdentity, Scope};
use agentchat_store::Store;
use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseConnection,
    sea_query::{Alias, Asterisk, Expr, ExprTrait, Func, Order, Query, SelectStatement},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::Path,
    time::Instant,
};
use uuid::Uuid;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

async fn connect(path: &Path) -> DatabaseConnection {
    let path = path.to_owned();
    let mut options = ConnectOptions::new("sqlite::memory:");
    options
        .max_connections(1)
        .sqlx_logging(false)
        .map_sqlx_sqlite_opts(move |o| {
            o.filename(&path)
                .in_memory(false)
                .create_if_missing(false)
                .pragma("journal_mode", "WAL")
                .pragma("synchronous", "FULL")
        });
    Database::connect(options).await.unwrap()
}

async fn digest(db: &DatabaseConnection, table: &str) -> Value {
    let columns = db
        .query_all(
            &Query::select()
                .column(Asterisk)
                .from_function(
                    Func::cust(Alias::new("pragma_table_info")).arg(table),
                    Alias::new("info"),
                )
                .order_by(Alias::new("cid"), Order::Asc)
                .to_owned(),
        )
        .await
        .unwrap();
    assert!(!columns.is_empty());
    let mut fields = Vec::new();
    let mut keys = BTreeMap::new();
    for column in columns {
        let name = column.try_get::<String>("", "name").unwrap();
        let pk = column.try_get::<i64>("", "pk").unwrap();
        fields.push(Expr::col(Alias::new(&name)));
        if pk > 0 {
            keys.insert(pk, name);
        }
    }
    let mut query = Query::select();
    query
        .expr_as(
            Func::cust(Alias::new("json_array")).args(fields),
            Alias::new("row"),
        )
        .from(Alias::new(table));
    if table == "sqlite_sequence" {
        keys.insert(1, "name".into());
    }
    assert!(!keys.is_empty());
    for key in keys.values() {
        query.order_by(Alias::new(key), Order::Asc);
    }
    // Compare the unchanged historical ledger prefix independently of the new
    // third migration; the pre-upgrade backup must retain these exact rows too.
    if table == "schema_migrations" {
        query.and_where(Expr::col(Alias::new("version")).lte(2));
    } else if table == "seaql_migrations" {
        query.and_where(Expr::col(Alias::new("version")).is_in([
            "m20261015_000001_initial",
            "m20261015_000002_read_confirmations",
        ]));
    }
    let mut count = 0;
    let mut hash = Sha256::new();
    loop {
        let rows = db
            .query_all(&query.clone().offset(count).limit(200).to_owned())
            .await
            .unwrap();
        let size = rows.len();
        for row in rows {
            hash.update(row.try_get::<String>("", "row").unwrap().as_bytes());
            hash.update(b"\n");
            count += 1;
        }
        if size < 200 {
            break;
        }
    }
    json!({"rows":count,"sha256":hex(&hash.finalize())})
}

async fn audit(db: &DatabaseConnection) -> Value {
    let mut result = serde_json::Map::new();
    for table in [
        "agents",
        "sessions",
        "channels",
        "channel_memberships",
        "messages",
        "mentions",
        "read_receipts",
        "read_confirmations",
        "events",
        "deliveries",
        "wake_batches",
        "reservations",
        "reservation_patterns",
        "idempotency",
        "schema_migrations",
        "seaql_migrations",
        "sqlite_sequence",
    ] {
        result.insert(table.into(), digest(db, table).await);
    }
    Value::Object(result)
}

async fn timings(db: &DatabaseConnection) -> Value {
    let agents = support::upgrade::rows(db, "sessions").await;
    let mut queries = vec![(
        "unpublished_event",
        Query::select()
            .column(Asterisk)
            .from(Alias::new("events"))
            .and_where(Expr::col(Alias::new("sealed")).eq(false))
            .limit(1)
            .to_owned(),
    )];
    for agent in agents {
        if !agent.try_get::<bool>("", "is_current").unwrap() {
            continue;
        }
        let recipient = agent.try_get::<String>("", "agent_id").unwrap();
        let incarnation = agent.try_get::<String>("", "incarnation").unwrap();
        queries.push((
            "outstanding_batch",
            Query::select()
                .column(Asterisk)
                .from(Alias::new("wake_batches"))
                .and_where(Expr::col(Alias::new("recipient_id")).eq(&recipient))
                .and_where(Expr::col(Alias::new("state")).is_in(["pending", "inflight", "stalled"]))
                .and_where(Expr::col(Alias::new("incarnation")).eq(&incarnation))
                .limit(1)
                .to_owned(),
        ));
    }
    let mut samples = BTreeMap::<&str, Vec<f64>>::new();
    for _ in 0..100 {
        for (name, query) in &queries {
            let start = Instant::now();
            assert!(
                db.query_one(query as &SelectStatement)
                    .await
                    .unwrap()
                    .is_none()
            );
            samples
                .entry(name)
                .or_default()
                .push(start.elapsed().as_secs_f64() * 1000.0);
        }
    }
    json!(
        samples
            .into_iter()
            .map(|(name, mut values)| {
                values.sort_by(f64::total_cmp);
                (
                    name,
                    json!({"samples":values.len(),"p50_ms":values[values.len()/2],
            "p95_ms":values[(values.len()*95).div_ceil(100)-1]}),
                )
            })
            .collect::<BTreeMap<_, _>>()
    )
}

#[tokio::test]
#[ignore = "manual: requires a closed schema-v2 workload backup and an owned new output directory"]
async fn restore_and_profile_retained_v2_history() {
    let source = std::path::PathBuf::from(std::env::var_os("AGENTCHAT_RETAINED_BACKUP").unwrap());
    let output = std::path::PathBuf::from(std::env::var_os("AGENTCHAT_RETAINED_OUTPUT").unwrap());
    assert!(source.is_absolute() && output.is_absolute());
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&output)
        .unwrap();
    let bytes = std::fs::read(&source).unwrap();
    let source_hash = hex(&Sha256::digest(&bytes));
    let path = output.join("restored.sqlite3");
    // The input is an already closed, validated logical backup, not a live DB.
    std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&path)
        .unwrap()
        .write_all(&bytes)
        .unwrap();
    drop(bytes);
    let db = connect(&path).await;
    let meta = support::upgrade::rows(&db, "project_meta").await;
    assert_eq!(meta[0].try_get::<i32>("", "schema_version").unwrap(), 2);
    let identity = ProjectIdentity {
        id: Uuid::parse_str(&meta[0].try_get::<String>("", "project_id").unwrap()).unwrap(),
        scope: Scope::Directory,
    };
    let before = audit(&db).await;
    assert_eq!(before["messages"]["rows"], 6000);
    assert_eq!(before["events"]["rows"], 12742);
    let initial_timings = timings(&db).await;
    let started = Instant::now();
    let store = Store::open(&path, identity.clone()).await.unwrap();
    let upgrade_seconds = started.elapsed().as_secs_f64();
    store.integrity_check().await.unwrap();
    store.close().await.unwrap();
    let after_timings = timings(&db).await;
    assert_eq!(audit(&db).await, before);
    db.close().await.unwrap();
    let backups = std::fs::read_dir(&output)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .contains("schema-v2-backup-")
                && p.extension().is_some_and(|e| e == "sqlite3")
        })
        .collect::<Vec<_>>();
    assert_eq!(backups.len(), 1);
    let backup = support::upgrade::inspect(&backups[0]).await;
    assert_eq!(audit(&backup).await, before);
    assert_eq!(
        support::upgrade::rows(&backup, "project_meta").await[0]
            .try_get::<i32>("", "schema_version")
            .unwrap(),
        2
    );
    backup.close().await.unwrap();
    Store::open(&path, identity)
        .await
        .unwrap()
        .close()
        .await
        .unwrap();
    assert_eq!(
        hex(&Sha256::digest(std::fs::read(&source).unwrap())),
        source_hash
    );
    let report = json!({"source_sha256":source_hash,"before":initial_timings,"after":after_timings,
        "upgrade_seconds":upgrade_seconds,"business_tables":before,"restored":path,"backup":backups[0]});
    std::fs::write(
        output.join("profile.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
    println!("{}", serde_json::to_string(&report).unwrap());
}
