//! Dispatch-ordered input heads and exact retained original-result recovery.
use super::StoreTransaction;
use crate::{
    decode,
    entities::{actor as agent, channel, delivery, dispatch, launch, message_stream},
    error::{database, integrity},
};
use agentchat_core::{
    Result,
    delivery::{MessageItem, NativeItem, WakeItem},
    model::DispatchPayload,
    sequence::Sequence,
};
use sea_orm::{
    ConnectionTrait, EntityTrait, FromQueryResult, QueryFilter,
    sea_query::{Asterisk, Expr, ExprTrait, Order, Query},
};
use uuid::Uuid;

pub(crate) use super::message_stream_state::get;
pub(super) use super::message_stream_state::save;

pub(crate) async fn next<C: ConnectionTrait>(
    db: &C,
    recipient: Uuid,
    after: Sequence,
) -> Result<Option<MessageItem>> {
    match next_item(db, recipient, after).await? {
        Some(NativeItem::Message(item)) => Ok(Some(*item)),
        // Body-only consumers cannot bypass an earlier Notice in the same FIFO.
        None | Some(NativeItem::Notice(_)) => Ok(None),
    }
}
pub(crate) async fn next_native<C: ConnectionTrait>(
    db: &C,
    recipient: Uuid,
    after: Sequence,
) -> Result<Option<NativeItem>> {
    next_item(db, recipient, after).await
}

pub(crate) async fn retained<C: ConnectionTrait>(
    db: &C,
    recipient: Uuid,
    delivery_id: Uuid,
    seq: Sequence,
) -> Result<Option<NativeItem>> {
    // Recovery must retain terminal/unscoped original heads as well: this read
    // authorizes no new publication and cannot select a successor instead.
    let query = Query::select()
        .column((delivery::Entity, Asterisk))
        .from(delivery::Entity)
        .inner_join(
            dispatch::Entity,
            Expr::col((dispatch::Entity, dispatch::Column::Id))
                .equals((delivery::Entity, delivery::Column::DispatchId)),
        )
        .and_where(Expr::col((delivery::Entity, delivery::Column::Id)).eq(delivery_id.to_string()))
        .and_where(
            Expr::col((delivery::Entity, delivery::Column::RecipientId)).eq(recipient.to_string()),
        )
        .and_where(Expr::col((dispatch::Entity, dispatch::Column::CreationSeq)).eq(seq.get()))
        .and_where(Expr::col((dispatch::Entity, dispatch::Column::Sealed)).eq(true))
        .to_owned();
    let row = db.query_one(&query).await.map_err(database)?;
    match row {
        Some(row) => decode_item(db, row).await.map(Some),
        None => Ok(None),
    }
}
async fn next_item<C: ConnectionTrait>(
    db: &C,
    recipient: Uuid,
    after: Sequence,
) -> Result<Option<NativeItem>> {
    let prefix = crate::queue::native_prefix(recipient, after)
        .cond_where(crate::scope::delivery(recipient))
        .order_by(
            (dispatch::Entity, dispatch::Column::CreationSeq),
            Order::Asc,
        )
        .limit(1)
        .to_owned();
    if let Some(row) = db.query_one(&prefix).await.map_err(database)? {
        return decode_item(db, row).await.map(Some);
    }
    // A sparse or out-of-scope prefix proves no absence. Preserve the complete
    // query and the exact original cursor, without skipping any later work.
    let query = crate::queue::selected(recipient)
        .and_where(Expr::col((dispatch::Entity, dispatch::Column::CreationSeq)).gt(after.get()))
        .cond_where(crate::scope::delivery(recipient))
        .order_by(
            (dispatch::Entity, dispatch::Column::CreationSeq),
            Order::Asc,
        )
        .limit(1)
        .to_owned();
    match db.query_one(&query).await.map_err(database)? {
        Some(row) => decode_item(db, row).await.map(Some),
        None => Ok(None),
    }
}
async fn decode_item<C: ConnectionTrait>(db: &C, row: sea_orm::QueryResult) -> Result<NativeItem> {
    let mut deliveries = crate::delivery_history::decode_many(
        db,
        vec![delivery::Model::from_query_result(&row, "").map_err(database)?],
    )
    .await?;
    let delivery = deliveries
        .pop()
        .ok_or_else(|| integrity("Missing native delivery."))?;
    let mut payloads = crate::dispatches::hydrate(db, &[delivery.dispatch_id]).await?;
    let (dispatch, payload) = payloads
        .remove(&delivery.dispatch_id)
        .ok_or_else(|| integrity("Missing native Dispatch."))?;
    let DispatchPayload::Message(message) = payload else {
        let DispatchPayload::Notice(notice) = payload else {
            unreachable!()
        };
        return Ok(NativeItem::Notice(Box::new(WakeItem {
            delivery,
            dispatch,
            message: None,
            notice: Some(notice),
        })));
    };
    let author = agent::Entity::find_by_id(message.author_id.to_string())
        .one(db)
        .await
        .map_err(database)?
        .ok_or_else(|| integrity("Missing stream author."))?;
    let channel = channel::Entity::find_by_id(message.channel_id.to_string())
        .one(db)
        .await
        .map_err(database)?
        .ok_or_else(|| integrity("Missing stream channel."))?;
    Ok(NativeItem::Message(Box::new(MessageItem {
        delivery,
        dispatch,
        message,
        author: decode::actor(author)?,
        channel: decode::channel(channel)?,
    })))
}

pub(super) async fn cancel(tx: &StoreTransaction, agent: Uuid) -> Result<()> {
    let generations = Query::select()
        .column(launch::Column::Generation)
        .from(launch::Entity)
        .and_where(Expr::col(launch::Column::AgentId).eq(agent.to_string()))
        .to_owned();
    message_stream::Entity::update_many()
        .col_expr(message_stream::Column::Cancelled, Expr::value(true))
        .filter(Expr::col(message_stream::Column::Generation).in_subquery(generations))
        .exec(&tx.db)
        .await
        .map_err(database)?;
    Ok(())
}
