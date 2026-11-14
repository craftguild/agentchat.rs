//! A business commit may expose only fully sealed events and recipient rows.

use agentchat_core::{
    Result,
    model::{Event, EventKind},
    ports::IdempotencyRecord,
    time::Timestamp,
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set, TransactionTrait, sea_query::Expr,
};
use uuid::Uuid;

use super::{StoreTransaction, decode};
use crate::{
    entities::{event, idempotency},
    error::{database, integrity},
};

pub(super) async fn idempotency(
    tx: &StoreTransaction,
    id: Uuid,
) -> Result<Option<IdempotencyRecord>> {
    idempotency::Entity::find_by_id(id.to_string())
        .one(&tx.db)
        .await
        .map_err(database)?
        .map(decode::idempotency)
        .transpose()
}

pub(super) async fn save_idempotency(
    tx: &StoreTransaction,
    value: &IdempotencyRecord,
) -> Result<()> {
    idempotency::ActiveModel {
        request_id: Set(value.request_id.to_string()),
        actor_key: Set(value.actor_key.clone()),
        operation: Set(value.operation.clone()),
        argument_hash: Set(value.argument_hash.clone()),
        result: Set(value.result.clone()),
        committed_at: Set(value.committed_at.to_string()),
    }
    .insert(&tx.db)
    .await
    .map_err(database)?;
    Ok(())
}

pub(super) async fn allocate(
    tx: &StoreTransaction,
    kind: EventKind,
    now: Timestamp,
) -> Result<Event> {
    let nested = tx.db.begin().await.map_err(database)?;
    let result = async {
        let seq = super::sequence::allocate(&nested).await?;
        let id = Uuid::new_v4();
        event::ActiveModel {
            seq: Set(seq.get()),
            event_id: Set(id.to_string()),
            version: Set(1),
            kind: Set(kind.as_str().to_owned()),
            occurred_at: Set(now.to_string()),
            actor_id: Set(None),
            channel_id: Set(None),
            message_id: Set(None),
            data: Set(serde_json::json!({})),
            sealed: Set(false),
            ..Default::default()
        }
        .insert(&nested)
        .await
        .map_err(database)?;
        Ok(Event {
            version: 1,
            event_id: id,
            seq,
            kind,
            occurred_at: now,
            actor_id: None,
            channel_id: None,
            message_id: None,
            data: serde_json::json!({}),
        })
    }
    .await;
    match result {
        Ok(value) => {
            nested.commit().await.map_err(database)?;
            Ok(value)
        }
        Err(error) => {
            nested.rollback().await.map_err(database)?;
            Err(error)
        }
    }
}

/// Audit publication has no recipient or native-scheduling side effect.
pub(super) async fn publish(tx: &StoreTransaction, value: &Event) -> Result<()> {
    value.validate()?;
    let sealed = event::Entity::update_many()
        .col_expr(
            event::Column::ActorId,
            Expr::value(value.actor_id.map(|v| v.to_string())),
        )
        .col_expr(
            event::Column::ChannelId,
            Expr::value(value.channel_id.map(|v| v.to_string())),
        )
        .col_expr(
            event::Column::MessageId,
            Expr::value(value.message_id.map(|v| v.to_string())),
        )
        .col_expr(event::Column::Data, Expr::value(value.data.clone()))
        .col_expr(event::Column::Sealed, Expr::value(true))
        .filter(event::Column::EventId.eq(value.event_id.to_string()))
        .filter(event::Column::Seq.eq(value.seq.get()))
        .filter(event::Column::Kind.eq(value.kind.as_str()))
        .filter(event::Column::OccurredAt.eq(value.occurred_at.to_string()))
        .filter(event::Column::Version.eq(value.version))
        .filter(event::Column::Sealed.eq(false))
        .exec(&tx.db)
        .await
        .map_err(database)?;
    if sealed.rows_affected != 1 {
        return Err(integrity("Event is absent, changed or already published."));
    }
    Ok(())
}

pub(super) async fn get(tx: &StoreTransaction, id: Uuid) -> Result<Option<Event>> {
    event::Entity::find()
        .filter(event::Column::EventId.eq(id.to_string()))
        .filter(event::Column::Sealed.eq(true))
        .one(&tx.db)
        .await
        .map_err(database)?
        .map(decode::event)
        .transpose()
}
