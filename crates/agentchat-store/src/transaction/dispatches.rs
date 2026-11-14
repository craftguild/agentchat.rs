//! Stage immutable sending Headers, then atomically publish payload and recipients.
use super::StoreTransaction;
use crate::{
    decode, delivery_history,
    entities::{dispatch, message, notice},
    error::{database, integrity},
};
use agentchat_core::{
    Result,
    model::{
        Delivery, DeliveryHistory, DeliveryState, DeliveryTransition, Dispatch, DispatchKind,
        DispatchPayload,
    },
    time::Timestamp,
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseTransaction, EntityTrait, QueryFilter,
    Set, TransactionTrait,
    sea_query::{Condition, Expr, ExprTrait, JoinType, Query},
};
use std::collections::BTreeSet;
use uuid::Uuid;

pub(super) async fn allocate(
    tx: &StoreTransaction,
    kind: DispatchKind,
    channel: Option<Uuid>,
    now: Timestamp,
) -> Result<Dispatch> {
    let nested = tx.db.begin().await.map_err(database)?;
    let result = async {
        let creation_seq = super::sequence::allocate(&nested).await?;
        let id = Uuid::new_v4();
        tx.guard.touch(id)?;
        dispatch::ActiveModel {
            id: Set(id.to_string()),
            kind: Set(kind.as_str().to_owned()),
            channel_id: Set(channel.map(|v| v.to_string())),
            created_at: Set(now.to_string()),
            creation_seq: Set(creation_seq.get()),
            sealed: Set(false),
        }
        .insert(&nested)
        .await
        .map_err(database)?;
        Ok(Dispatch {
            id,
            kind,
            channel_id: channel,
            created_at: now,
            creation_seq,
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

pub(super) async fn publish(
    tx: &StoreTransaction,
    header: &Dispatch,
    payload: &DispatchPayload,
    recipients: &[Delivery],
) -> Result<()> {
    tx.guard.touch(header.id)?;
    header.validate_payload(payload)?;
    let mut ids = BTreeSet::new();
    let mut actors = BTreeSet::new();
    for value in recipients {
        if !ids.insert(value.id)
            || !actors.insert(value.recipient_id)
            || value.dispatch_id != header.id
            || value.state != DeliveryState::Pending
            || value.history != DeliveryHistory::queued(header.created_at)
            || value.attempts != 0
            || value.wake_batch_id.is_some()
        {
            return Err(integrity(
                "Initial recipients disagree with their Dispatch obligation.",
            ));
        }
    }
    let nested = tx.db.begin().await.map_err(database)?;
    match publish_inner(&nested, header, payload, recipients).await {
        Ok(()) => nested.commit().await.map_err(database),
        Err(error) => {
            nested.rollback().await.map_err(database)?;
            Err(error)
        }
    }
}

async fn publish_inner(
    db: &DatabaseTransaction,
    header: &Dispatch,
    payload: &DispatchPayload,
    recipients: &[Delivery],
) -> Result<()> {
    let staged = dispatch::Entity::find_by_id(header.id.to_string())
        .one(db)
        .await
        .map_err(database)?
        .ok_or_else(|| integrity("Dispatch Header is absent."))?;
    if staged.sealed || decode::dispatch(staged)? != *header {
        return Err(integrity(
            "Dispatch Header is changed or already published.",
        ));
    }
    match payload {
        DispatchPayload::Message(value) => {
            if let Some(root) = value.root_message_id {
                let root = crate::dispatches::message(db, root)
                    .await?
                    .ok_or_else(|| integrity("Reply root is not a published Message."))?;
                if root.root_message_id.is_some() || root.channel_id != value.channel_id {
                    return Err(integrity(
                        "Reply must target a direct root in the same channel.",
                    ));
                }
            }
            message::ActiveModel {
                dispatch_id: Set(value.id.to_string()),
                author_id: Set(value.author_id.to_string()),
                root_message_id: Set(value.root_message_id.map(|v| v.to_string())),
                body: Set(value.body.clone()),
            }
            .insert(db)
            .await
            .map_err(database)?;
        }
        DispatchPayload::Notice(value) => {
            notice::ActiveModel {
                dispatch_id: Set(value.dispatch_id.to_string()),
                kind: Set(value.kind.as_str().to_owned()),
                data: Set(value.data.clone()),
            }
            .insert(db)
            .await
            .map_err(database)?;
        }
    }
    for chunk in recipients.chunks(16) {
        let values = chunk
            .iter()
            .map(|v| delivery_history::active(v, 1))
            .collect::<Result<Vec<_>>>()?;
        let inserted = crate::entities::delivery::Entity::insert_many(values)
            .exec_without_returning(db)
            .await
            .map_err(database)?;
        if inserted != chunk.len() as u64 {
            return Err(integrity("Dispatch fanout omitted recipients."));
        }
        let initial = chunk
            .iter()
            .map(DeliveryTransition::initial)
            .collect::<Result<Vec<_>>>()?;
        delivery_history::append(db, &initial).await?;
    }
    crate::dispatches::payload(db, header).await?;
    let changed = dispatch::Entity::update_many()
        .col_expr(dispatch::Column::Sealed, Expr::value(true))
        .filter(dispatch::Column::Id.eq(header.id.to_string()))
        .filter(dispatch::Column::Sealed.eq(false))
        .exec(db)
        .await
        .map_err(database)?;
    if changed.rows_affected != 1 {
        return Err(integrity("Dispatch disappeared before publication."));
    }
    Ok(())
}

/// Indexed existence check, not an unbounded hydration of all retained content.
pub(crate) async fn guard<C: ConnectionTrait>(db: &C) -> Result<()> {
    guard_selected(db, None).await
}

pub(super) async fn guard_changed<C: ConnectionTrait>(db: &C, ids: &[Uuid]) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    guard_selected(db, Some(ids)).await
}

async fn guard_selected<C: ConnectionTrait>(db: &C, ids: Option<&[Uuid]>) -> Result<()> {
    let invalid = Condition::any()
        .add(Expr::col((dispatch::Entity, dispatch::Column::Sealed)).eq(false))
        .add(
            Condition::all()
                .add(Expr::col((dispatch::Entity, dispatch::Column::Kind)).eq("message"))
                .add(
                    Condition::any()
                        .add(Expr::col((message::Entity, message::Column::DispatchId)).is_null())
                        .add(Expr::col((notice::Entity, notice::Column::DispatchId)).is_not_null())
                        .add(Expr::col((dispatch::Entity, dispatch::Column::ChannelId)).is_null()),
                ),
        )
        .add(
            Condition::all()
                .add(Expr::col((dispatch::Entity, dispatch::Column::Kind)).eq("notice"))
                .add(
                    Condition::any()
                        .add(Expr::col((notice::Entity, notice::Column::DispatchId)).is_null())
                        .add(
                            Expr::col((message::Entity, message::Column::DispatchId)).is_not_null(),
                        ),
                ),
        );
    let mut query = Query::select()
        .expr(1)
        .from(dispatch::Entity)
        .join(
            JoinType::LeftJoin,
            message::Entity,
            Expr::col((message::Entity, message::Column::DispatchId))
                .equals((dispatch::Entity, dispatch::Column::Id)),
        )
        .join(
            JoinType::LeftJoin,
            notice::Entity,
            Expr::col((notice::Entity, notice::Column::DispatchId))
                .equals((dispatch::Entity, dispatch::Column::Id)),
        )
        .cond_where(invalid)
        .limit(1)
        .to_owned();
    if let Some(ids) = ids {
        query.and_where(
            Expr::col((dispatch::Entity, dispatch::Column::Id))
                .is_in(ids.iter().map(Uuid::to_string)),
        );
    }
    if db.query_one(&query).await.map_err(database)?.is_some() {
        return Err(integrity(
            "Cannot commit an incomplete or mismatched Dispatch aggregate.",
        ));
    }
    Ok(())
}
