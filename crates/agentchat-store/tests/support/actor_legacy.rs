//! Test-only reconstruction of real pre-actor schema, never product downgrade.

#[path = "../../src/migrations/initial_entities/agent.rs"]
mod initial_agent;

use sea_orm::{
    ConnectionTrait, DatabaseTransaction, DbBackend, Schema,
    sea_query::{Alias, Expr, ExprTrait, ForeignKey, Index, Query, Table},
};

pub async fn restore(tx: &DatabaseTransaction) {
    let user = tx
        .query_one(
            &Query::select()
                .column(Alias::new("id"))
                .from(Alias::new("actors"))
                .and_where(Expr::col(Alias::new("kind")).eq("user"))
                .to_owned(),
        )
        .await
        .unwrap();
    assert!(
        user.is_none(),
        "legacy fixtures cannot discard anonymous users"
    );
    let temporary = Alias::new("agent_rebuild_v10");
    let mut table = Schema::new(DbBackend::Sqlite).create_table_from_entity(initial_agent::Entity);
    table
        .table(temporary.clone())
        .check(Expr::col(Alias::new("host_kind")).is_in(["codex", "claude"]))
        .check(Expr::col(Alias::new("participation")).is_in(["joined", "left"]));
    for column in ["join_seq", "leave_seq"] {
        table.foreign_key(
            ForeignKey::create()
                .from(temporary.clone(), Alias::new(column))
                .to(Alias::new("events"), Alias::new("seq")),
        );
    }
    tx.execute(&table).await.unwrap();
    let columns = [
        "id",
        "host_kind",
        "host_session_id",
        "handle",
        "participation",
        "first_join_at",
        "join_seq",
        "leave_seq",
    ];
    tx.execute(
        &Query::insert()
            .into_table(temporary.clone())
            .columns(columns.map(Alias::new))
            .select_from(
                Query::select()
                    .columns(columns.map(Alias::new))
                    .from(Alias::new("actors"))
                    .to_owned(),
            )
            .unwrap()
            .to_owned(),
    )
    .await
    .unwrap();
    tx.execute(&Table::drop().table(Alias::new("actors")).to_owned())
        .await
        .unwrap();
    tx.execute(
        &Table::rename()
            .table(temporary, Alias::new("actors"))
            .to_owned(),
    )
    .await
    .unwrap();
    tx.execute(
        &Table::rename()
            .table(Alias::new("actors"), Alias::new("agents"))
            .to_owned(),
    )
    .await
    .unwrap();
    for (name, columns) in [
        (
            "uq_agent_conversation",
            vec!["host_kind", "host_session_id"],
        ),
        ("uq_agent_handle", vec!["handle"]),
    ] {
        let mut index = Index::create();
        index.name(name).table(Alias::new("agents")).unique();
        for column in columns {
            index.col(Alias::new(column));
        }
        tx.execute(&index).await.unwrap();
    }
    for table in ["channel_memberships", "read_receipts"] {
        tx.execute(
            &Table::alter()
                .table(Alias::new(table))
                .rename_column(Alias::new("actor_id"), Alias::new("agent_id"))
                .to_owned(),
        )
        .await
        .unwrap();
    }
}
