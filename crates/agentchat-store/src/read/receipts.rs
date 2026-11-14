//! Bound recipient selection before loading each actor's delivery/read details.

use agentchat_core::{
    Result,
    read::{IdentityLabel, Indexed, ReceiptDelivery, ReceiptRead, RecipientReceipt},
    sequence::Cursor,
};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseTransaction, EntityTrait, FromQueryResult, QueryFilter,
    QueryOrder, QuerySelect,
    sea_query::{Alias, Asterisk, Expr, ExprTrait, Order, Query},
};
use uuid::Uuid;

use super::{fetch_limit, identity::first_join};
use crate::{
    decode,
    entities::{actor, delivery, receipt},
    error::database,
};

pub(super) async fn page(
    db: &DatabaseTransaction,
    head: Cursor,
    message: Uuid,
    after: Cursor,
    limit: usize,
) -> Result<Vec<Indexed<RecipientReceipt>>> {
    let original_dispatch = Query::select()
        .column(Alias::new("id"))
        .from(Alias::new("dispatches"))
        .and_where(Expr::col(Alias::new("id")).eq(message.to_string()))
        .and_where(Expr::col(Alias::new("kind")).eq("message"))
        .and_where(Expr::col(Alias::new("sealed")).eq(true))
        .to_owned();
    let delivered = Query::select()
        .column(Alias::new("recipient_id"))
        .from(Alias::new("deliveries"))
        .and_where(Expr::col(Alias::new("dispatch_id")).in_subquery(original_dispatch.clone()))
        .to_owned();
    let read = Query::select()
        .column(Alias::new("actor_id"))
        .from(Alias::new("read_receipts"))
        .and_where(Expr::col(Alias::new("message_id")).eq(message.to_string()))
        .to_owned();
    let key = first_join(Expr::col((actor::Entity, actor::Column::Id)));
    let selected = Query::select()
        .column(Asterisk)
        .expr_as(key.clone(), Alias::new("page_key"))
        .from(actor::Entity)
        .and_where(
            Expr::col(actor::Column::Id)
                .in_subquery(delivered)
                .or(Expr::col(actor::Column::Id).in_subquery(read)),
        )
        .and_where(key.clone().gt(after.get()))
        .and_where(key.lte(head.get()))
        .order_by(Alias::new("page_key"), Order::Asc)
        .limit(fetch_limit(limit)?)
        .to_owned();
    let mut items = Vec::new();
    for row in db.query_all(&selected).await.map_err(database)? {
        let key = decode::seq(row.try_get("", "page_key").map_err(database)?)?;
        let actor = decode::actor(actor::Model::from_query_result(&row, "").map_err(database)?)?;
        let original_rows = delivery::Entity::find()
            .filter(delivery::Column::DispatchId.in_subquery(original_dispatch.clone()))
            .filter(delivery::Column::RecipientId.eq(actor.actor_id.to_string()))
            .limit(1)
            .all(db)
            .await
            .map_err(database)?;
        let original = crate::delivery_history::decode_many(db, original_rows)
            .await?
            .into_iter()
            .next();
        let reads = receipt::Entity::find()
            .filter(receipt::Column::MessageId.eq(message.to_string()))
            .filter(receipt::Column::ActorId.eq(actor.actor_id.to_string()))
            .order_by_asc(receipt::Column::Kind)
            .limit(3)
            .all(db)
            .await
            .map_err(database)?;
        if reads.len() > 2 {
            return Err(crate::error::integrity(
                "Read kinds exceed the canonical bound.",
            ));
        }
        let reads = reads
            .into_iter()
            .map(decode::receipt)
            .collect::<Result<Vec<_>>>()?;
        let recipient = IdentityLabel {
            actor_id: actor.actor_id,
            kind: actor.kind(),
            handle: actor.handle,
        };
        items.push(Indexed {
            key,
            item: RecipientReceipt {
                recipient,
                delivery: original.map(|delivery| ReceiptDelivery {
                    delivery_id: delivery.id,
                    state: delivery.state,
                    history: delivery.history,
                }),
                reads: reads
                    .into_iter()
                    .map(|r| ReceiptRead {
                        kind: r.kind,
                        at: r.first_read_at,
                    })
                    .collect(),
            },
        });
    }
    Ok(items)
}
