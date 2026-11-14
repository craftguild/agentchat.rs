//! Scoped departure preserves atomic per-recipient history and retained uncertainty.
use super::StoreTransaction;
use crate::{
    delivery_history,
    entities::{delivery, dispatch, wake_batch},
    error::database,
};
use agentchat_core::{Result, time::Timestamp};
use sea_orm::{
    ColumnTrait, EntityTrait, QueryFilter, QuerySelect, TransactionTrait,
    sea_query::{Expr, ExprTrait, Query},
};
use uuid::Uuid;

pub(super) async fn cancel(
    tx: &StoreTransaction,
    actor: Uuid,
    channel: Option<Uuid>,
    at: Timestamp,
) -> Result<()> {
    let nested = tx.db.begin().await.map_err(database)?;
    let scoped = tx.nested(nested);
    match cancel_inner(&scoped, actor, channel, at).await {
        Ok(()) => scoped.db.commit().await.map_err(database),
        Err(error) => {
            scoped.db.rollback().await.map_err(database)?;
            Err(error)
        }
    }
}

async fn cancel_inner(
    tx: &StoreTransaction,
    actor: Uuid,
    channel: Option<Uuid>,
    at: Timestamp,
) -> Result<()> {
    loop {
        let mut query = delivery::Entity::find()
            .filter(delivery::Column::RecipientId.eq(actor.to_string()))
            .filter(delivery::Column::State.is_in(["pending", "processing"]));
        if let Some(channel) = channel {
            let headers = Query::select()
                .column(dispatch::Column::Id)
                .from(dispatch::Entity)
                .and_where(Expr::col(dispatch::Column::ChannelId).eq(channel.to_string()))
                .to_owned();
            query = query.filter(Expr::col(delivery::Column::DispatchId).in_subquery(headers));
        }
        let rows = query.limit(200).all(&tx.db).await.map_err(database)?;
        if rows.is_empty() {
            break;
        }
        for mut value in delivery_history::decode_many(&tx.db, rows).await? {
            value.depart(at)?;
            delivery_history::save(&tx.db, &value).await?;
        }
    }
    let mut batches = wake_batch::Entity::update_many()
        .col_expr(wake_batch::Column::State, Expr::value("cancelled"))
        .filter(wake_batch::Column::RecipientId.eq(actor.to_string()))
        .filter(wake_batch::Column::State.is_in(["pending", "inflight", "stalled"]));
    if channel.is_some() {
        let active = Query::select()
            .column(delivery::Column::WakeBatchId)
            .from(delivery::Entity)
            .and_where(Expr::col(delivery::Column::RecipientId).eq(actor.to_string()))
            .and_where(Expr::col(delivery::Column::WakeBatchId).is_not_null())
            .and_where(Expr::col(delivery::Column::State).is_in(["pending", "processing"]))
            .to_owned();
        batches = batches.filter(Expr::col(wake_batch::Column::Id).not_in_subquery(active));
    }
    batches.exec(&tx.db).await.map_err(database)?;
    if channel.is_none() {
        super::message_streams::cancel(tx, actor).await?;
    }
    Ok(())
}
