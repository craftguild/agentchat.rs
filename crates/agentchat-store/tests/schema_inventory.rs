//! Actual fresh-format table, column, FK and uniqueness inventory via SeaORM.
use agentchat_core::project::{ProjectIdentity, Scope};
use agentchat_store::Store;
use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, QueryResult,
    sea_query::{Alias, Expr, ExprTrait, Func, Query},
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use tempfile::TempDir;
use uuid::Uuid;

async fn pragma(db: &impl ConnectionTrait, kind: &str, argument: &str) -> Vec<QueryResult> {
    db.query_all(
        &Query::select()
            .column(sea_orm::sea_query::Asterisk)
            .from_function(
                Func::cust(Alias::new(kind)).arg(Expr::val(argument)),
                Alias::new("schema_rows"),
            )
            .to_owned(),
    )
    .await
    .unwrap()
}
#[tokio::test]
async fn fresh_dispatch_catalog_has_required_ownership_and_unique_order_constraints() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("schema.sqlite3");
    let store = Store::open(
        &path,
        ProjectIdentity {
            id: Uuid::new_v4(),
            scope: Scope::Directory,
        },
    )
    .await
    .unwrap();
    let mut options = ConnectOptions::new("sqlite::memory:");
    options
        .max_connections(1)
        .sqlx_logging(false)
        .map_sqlx_sqlite_opts(move |o| {
            o.filename(&path)
                .in_memory(false)
                .create_if_missing(false)
                .read_only(true)
        });
    let db = Database::connect(options).await.unwrap();
    let names = db
        .query_all(
            &Query::select()
                .column(Alias::new("name"))
                .from(Alias::new("sqlite_schema"))
                .and_where(Expr::col(Alias::new("type")).eq("table"))
                .to_owned(),
        )
        .await
        .unwrap();
    let mut inventory = BTreeMap::new();
    for row in names {
        let name: String = row.try_get("", "name").unwrap();
        if name.starts_with("sqlite_") {
            continue;
        }
        let columns=pragma(&db,"pragma_table_info",&name).await.into_iter().map(|r|json!({
            "name":r.try_get::<String>("","name").unwrap(),"type":r.try_get::<String>("","type").unwrap(),
            "not_null":r.try_get::<i64>("","notnull").unwrap()!=0,"pk_order":r.try_get::<i64>("","pk").unwrap()
        })).collect::<Vec<_>>();
        let foreign_keys=pragma(&db,"pragma_foreign_key_list",&name).await.into_iter().map(|r|json!({
            "from":r.try_get::<String>("","from").unwrap(),"table":r.try_get::<String>("","table").unwrap(),
            "to":r.try_get::<String>("","to").unwrap(),"on_delete":r.try_get::<String>("","on_delete").unwrap(),
            "group":r.try_get::<i64>("","id").unwrap(),"position":r.try_get::<i64>("","seq").unwrap()
        })).collect::<Vec<_>>();
        let mut unique = Vec::new();
        for index in pragma(&db, "pragma_index_list", &name).await {
            if index.try_get::<i64>("", "unique").unwrap() == 0 {
                continue;
            }
            let index_name: String = index.try_get("", "name").unwrap();
            let mut fields = pragma(&db, "pragma_index_info", &index_name)
                .await
                .into_iter()
                .map(|r| {
                    (
                        r.try_get::<i64>("", "seqno").unwrap(),
                        r.try_get::<String>("", "name").unwrap(),
                    )
                })
                .collect::<Vec<_>>();
            fields.sort();
            unique.push(json!({"name":index_name,"columns":fields.into_iter().map(|(_,v)|v).collect::<Vec<_>>()}));
        }
        unique.sort_by_key(|i| i["name"].as_str().unwrap().to_owned());
        inventory.insert(
            name,
            json!({"columns":columns,"foreign_keys":foreign_keys,"unique_indexes":unique}),
        );
    }
    assert_eq!(inventory.len(), 24);
    assert!(!inventory.contains_key("messages"));
    assert!(!inventory.contains_key("agents"));
    assert!(!inventory.contains_key("read_confirmations"));
    for (table, column, target, key) in [
        ("dispatch_messages", "dispatch_id", "dispatches", "id"),
        (
            "dispatch_messages",
            "root_message_id",
            "dispatch_messages",
            "dispatch_id",
        ),
        ("dispatch_notices", "dispatch_id", "dispatches", "id"),
        ("deliveries", "dispatch_id", "dispatches", "id"),
        ("deliveries", "recipient_id", "actors", "id"),
        (
            "delivery_state_transitions",
            "delivery_id",
            "deliveries",
            "id",
        ),
        ("message_streams", "head_seq", "dispatches", "creation_seq"),
        ("events", "message_id", "dispatch_messages", "dispatch_id"),
        (
            "read_receipts",
            "message_id",
            "dispatch_messages",
            "dispatch_id",
        ),
        ("mentions", "message_id", "dispatch_messages", "dispatch_id"),
        (
            "delivery_timeout_notices",
            "notice_dispatch_id",
            "dispatch_notices",
            "dispatch_id",
        ),
    ] {
        assert!(
            inventory[table]["foreign_keys"]
                .as_array()
                .unwrap()
                .iter()
                .any(|fk| fk["from"] == column && fk["table"] == target && fk["to"] == key),
            "{table}.{column}"
        );
    }
    for (table, columns) in [
        ("dispatches", vec!["creation_seq"]),
        ("events", vec!["seq"]),
        ("deliveries", vec!["dispatch_id", "recipient_id"]),
        ("delivery_state_transitions", vec!["delivery_id", "ordinal"]),
    ] {
        assert!(
            inventory[table]["unique_indexes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|i| i["columns"] == json!(columns)),
            "{table}"
        );
    }
    for (table, column) in [
        ("deliveries", "dispatch_id"),
        ("delivery_state_transitions", "delivery_id"),
    ] {
        assert!(
            inventory[table]["columns"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["name"] == column && c["not_null"] == true)
        );
    }
    let result: Value = json!({"schema_version":14,"format_version":3,"sqlite_internal_tables_excluded":true,"tables":inventory});
    eprintln!(
        "SCHEMA_INVENTORY {}",
        serde_json::to_string(&result).unwrap()
    );
    store.integrity_check().await.unwrap();
    db.close().await.unwrap();
    store.close().await.unwrap();
}
