//! Bounded event-ordered wake reads and atomic control-row persistence.

use super::StoreTransaction;
use crate::{
    decode,
    entities::{delivery, dispatch, wake_batch},
    error::{database, integrity},
};
use agentchat_core::{
    Result,
    delivery::{MAX_WAKE_EVENTS, WakeBatch, WakeItem},
    model::DispatchPayload,
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, FromQueryResult, QueryFilter, Set,
    sea_query::{Alias, Expr, ExprTrait, Order, Query},
};
use uuid::Uuid;

pub(crate) async fn get<C: ConnectionTrait>(db: &C, id: Uuid) -> Result<Option<WakeBatch>> {
    wake_batch::Entity::find_by_id(id.to_string())
        .one(db)
        .await
        .map_err(database)?
        .map(decode::batch)
        .transpose()
}
pub(super) async fn outstanding(
    tx: &StoreTransaction,
    agent: Uuid,
    incarnation: Uuid,
) -> Result<Option<WakeBatch>> {
    wake_batch::Entity::find()
        .filter(wake_batch::Column::RecipientId.eq(agent.to_string()))
        .filter(wake_batch::Column::Incarnation.eq(incarnation.to_string()))
        .filter(wake_batch::Column::State.is_in(["pending", "inflight", "stalled"]))
        .one(&tx.db)
        .await
        .map_err(database)?
        .map(decode::batch)
        .transpose()
}
pub(super) async fn save(tx: &StoreTransaction, value: &WakeBatch) -> Result<()> {
    let row = wake_batch::Model {
        id: value.id.to_string(),
        recipient_id: value.recipient_id.to_string(),
        incarnation: value.incarnation.to_string(),
        state: value.state.as_str().into(),
        dispatch_ref: value.dispatch_ref.map(|u| u.to_string()),
        turn_ref: value.turn_ref.clone(),
        attempts: value
            .attempts
            .try_into()
            .map_err(|_| integrity("Wake attempts overflow."))?,
        next_eligible_at: value.next_eligible_at.map(|t| t.to_string()),
        failure_code: value.failure_code.clone(),
    };
    decode::batch(row.clone())?;
    let active = wake_batch::ActiveModel {
        id: Set(row.id),
        recipient_id: Set(row.recipient_id),
        incarnation: Set(row.incarnation),
        state: Set(row.state),
        dispatch_ref: Set(row.dispatch_ref),
        turn_ref: Set(row.turn_ref),
        attempts: Set(row.attempts),
        next_eligible_at: Set(row.next_eligible_at),
        failure_code: Set(row.failure_code),
    };
    if let Some(old) = get(&tx.db, value.id).await? {
        if old.recipient_id != value.recipient_id || old.incarnation != value.incarnation {
            return Err(integrity(
                "Wake batch ownership and incarnation are immutable.",
            ));
        }
        active.update(&tx.db).await.map_err(database)?;
    } else {
        active.insert(&tx.db).await.map_err(database)?;
    }
    Ok(())
}
pub(crate) async fn selected_deliveries<C: ConnectionTrait>(
    db: &C,
    agent: Uuid,
    batch: Option<Uuid>,
    ordered: bool,
    scoped: bool,
) -> Result<Vec<agentchat_core::model::Delivery>> {
    let mut query = crate::queue::selected(agent);
    if scoped {
        query.cond_where(crate::scope::delivery(agent));
    }
    if ordered {
        query.order_by(
            (dispatch::Entity, dispatch::Column::CreationSeq),
            Order::Asc,
        );
    }
    if let Some(batch) = batch {
        query
            .and_where(
                Expr::col((Alias::new("deliveries"), Alias::new("wake_batch_id")))
                    .eq(batch.to_string()),
            )
            .limit(MAX_WAKE_EVENTS as u64 + 1);
    } else {
        query
            .and_where(Expr::col((Alias::new("deliveries"), Alias::new("wake_batch_id"))).is_null())
            .and_where(Expr::col((Alias::new("deliveries"), Alias::new("wake_eligible"))).eq(true))
            .limit(MAX_WAKE_EVENTS as u64);
    }
    let rows = db.query_all(&query.to_owned()).await.map_err(database)?;
    if rows.len() > MAX_WAKE_EVENTS {
        return Err(integrity("Persisted wake batch exceeds twenty references."));
    }
    let rows = rows
        .into_iter()
        .map(|row| delivery::Model::from_query_result(&row, "").map_err(database))
        .collect::<Result<Vec<_>>>()?;
    crate::delivery_history::decode_many(db, rows).await
}

pub(crate) async fn has_pending<C: ConnectionTrait>(
    db: &C,
    agent: Uuid,
    batch: Uuid,
) -> Result<bool> {
    // Keep the same recipient/state/sealed filters, row decoding and 21-row
    // overflow guard as full selection. An emptiness check needs neither sort
    // order nor event JSON, and must not conceal invalid/oversized deliveries.
    Ok(!selected_deliveries(db, agent, Some(batch), false, true)
        .await?
        .is_empty())
}
pub(crate) async fn scoped_delivery_ids<C: ConnectionTrait>(
    db: &C,
    agent: Uuid,
    batch: Uuid,
) -> Result<Vec<Uuid>> {
    Ok(selected_deliveries(db, agent, Some(batch), true, true)
        .await?
        .into_iter()
        .map(|delivery| delivery.id)
        .collect())
}
pub(super) async fn has_pending_exact(
    tx: &StoreTransaction,
    agent: Uuid,
    batch: Uuid,
) -> Result<bool> {
    Ok(
        !selected_deliveries(&tx.db, agent, Some(batch), false, false)
            .await?
            .is_empty(),
    )
}
pub(super) async fn items(
    tx: &StoreTransaction,
    agent: Uuid,
    batch: Option<Uuid>,
) -> Result<Vec<WakeItem>> {
    let deliveries = selected_deliveries(&tx.db, agent, batch, true, true).await?;
    if deliveries.is_empty() {
        return Ok(Vec::new());
    }
    let mut payloads = crate::dispatches::hydrate(
        &tx.db,
        &deliveries.iter().map(|d| d.dispatch_id).collect::<Vec<_>>(),
    )
    .await?;
    let mut items = Vec::with_capacity(deliveries.len());
    for delivery in deliveries {
        let (dispatch, payload) = payloads
            .remove(&delivery.dispatch_id)
            .ok_or_else(|| integrity("Wake Dispatch is missing or unpublished."))?;
        let (message, notice) = match payload {
            DispatchPayload::Message(value) => (Some(value), None),
            DispatchPayload::Notice(value) => (None, Some(value)),
        };
        items.push(WakeItem {
            delivery,
            dispatch,
            message,
            notice,
        });
    }
    Ok(items)
}
pub(super) async fn count(tx: &StoreTransaction, agent: Uuid) -> Result<u64> {
    let mut query = crate::queue::selected(agent);
    query
        .clear_selects()
        .expr_as(
            Expr::col((delivery::Entity, delivery::Column::Id)).count(),
            Alias::new("count"),
        )
        .cond_where(crate::scope::delivery(agent));
    let row = tx
        .db
        .query_one(&query)
        .await
        .map_err(database)?
        .ok_or_else(|| integrity("Missing inbox count."))?;
    let count: i64 = row.try_get("", "count").map_err(database)?;
    count
        .try_into()
        .map_err(|_| integrity("Invalid inbox count."))
}

pub(super) async fn retire(tx: &StoreTransaction, agent: Uuid, current: Uuid) -> Result<()> {
    let old = Query::select()
        .column(wake_batch::Column::Id)
        .from(wake_batch::Entity)
        .and_where(Expr::col(wake_batch::Column::RecipientId).eq(agent.to_string()))
        .and_where(Expr::col(wake_batch::Column::Incarnation).ne(current.to_string()))
        .and_where(Expr::col(wake_batch::Column::State).is_in(["pending", "inflight", "stalled"]))
        .to_owned();
    delivery::Entity::update_many()
        .col_expr(
            delivery::Column::WakeBatchId,
            Expr::value(Option::<String>::None),
        )
        .filter(delivery::Column::RecipientId.eq(agent.to_string()))
        .filter(delivery::Column::State.is_in(["pending", "processing"]))
        .filter(Expr::col(delivery::Column::WakeBatchId).in_subquery(old))
        .exec(&tx.db)
        .await
        .map_err(database)?;
    wake_batch::Entity::update_many()
        .col_expr(wake_batch::Column::State, Expr::value("cancelled"))
        .filter(wake_batch::Column::RecipientId.eq(agent.to_string()))
        .filter(wake_batch::Column::Incarnation.ne(current.to_string()))
        .filter(wake_batch::Column::State.is_in(["pending", "inflight", "stalled"]))
        .exec(&tx.db)
        .await
        .map_err(database)?;
    Ok(())
}
