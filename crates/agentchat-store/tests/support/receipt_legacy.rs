//! Disposable v11 reconstruction keeps retained original frame/receipt evidence.
#[path = "../../src/backup/receipt_v11.rs"]
mod receipt_v11;
use sea_orm::{
    ConnectionTrait, DatabaseTransaction, DbBackend, Schema,
    sea_query::{Alias, Expr, ExprTrait, ForeignKey, Query, Table},
};
pub async fn restore(tx: &DatabaseTransaction) {
    // A historical fixture cannot erase automatic reads or native arrival proof.
    let new_reads = tx
        .query_one(
            &Query::select()
                .column(Alias::new("message_id"))
                .from(Alias::new("read_receipts"))
                .and_where(Expr::col(Alias::new("kind")).ne("explicit"))
                .to_owned(),
        )
        .await
        .unwrap();
    assert!(new_reads.is_none());
    let accepted = tx
        .query_one(
            &Query::select()
                .column(Alias::new("id"))
                .from(Alias::new("deliveries"))
                .and_where(Expr::col(Alias::new("accepted_at")).is_not_null())
                .to_owned(),
        )
        .await
        .unwrap();
    assert!(accepted.is_none());
    for name in ["confirmation_targets", "confirmation_watches"] {
        tx.execute(&Table::drop().table(Alias::new(name)).to_owned())
            .await
            .unwrap();
    }
    let temporary = Alias::new("receipts_rebuild_v11");
    let mut table = Schema::new(DbBackend::Sqlite).create_table_from_entity(receipt_v11::Entity);
    table.table(temporary.clone());
    for (col, target) in [("message_id", "messages"), ("actor_id", "actors")] {
        table.foreign_key(
            ForeignKey::create()
                .from(temporary.clone(), Alias::new(col))
                .to(Alias::new(target), Alias::new("id")),
        );
    }
    tx.execute(&table).await.unwrap();
    let columns = ["message_id", "actor_id", "first_read_at"].map(Alias::new);
    tx.execute(
        &Query::insert()
            .into_table(temporary.clone())
            .columns(columns.clone())
            .select_from(
                Query::select()
                    .columns(columns)
                    .from(Alias::new("read_receipts"))
                    .to_owned(),
            )
            .unwrap()
            .to_owned(),
    )
    .await
    .unwrap();
    tx.execute(&Table::drop().table(Alias::new("read_receipts")).to_owned())
        .await
        .unwrap();
    tx.execute(
        &Table::rename()
            .table(temporary, Alias::new("read_receipts"))
            .to_owned(),
    )
    .await
    .unwrap();
    tx.execute(
        &Table::alter()
            .table(Alias::new("deliveries"))
            .drop_column(Alias::new("accepted_at"))
            .to_owned(),
    )
    .await
    .unwrap();
    for col in ["auto_ack", "auto_read", "frame_revision", "accepted_at"] {
        tx.execute(
            &Table::alter()
                .table(Alias::new("message_streams"))
                .drop_column(Alias::new(col))
                .to_owned(),
        )
        .await
        .unwrap();
    }
}
