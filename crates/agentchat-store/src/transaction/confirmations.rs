//! Bounded lifecycle expiry reads and immutable author-notice coalescing.
use super::StoreTransaction;
use crate::{
    decode,
    entities::{actor, delivery, delivery_timeout_notice as notice},
    error::{database, integrity},
};
use agentchat_core::{
    Result,
    model::{Delivery, ExpiredRecipients, NoticeKind},
    time::Timestamp,
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder,
    QuerySelect, Set, sea_query::Expr,
};
use uuid::Uuid;

pub(super) async fn due(tx: &StoreTransaction, now: Timestamp) -> Result<Vec<Delivery>> {
    let rows = delivery::Entity::find()
        .filter(delivery::Column::State.eq("processing"))
        .filter(delivery::Column::DeadlineAt.lt(now.to_string()))
        .order_by_asc(delivery::Column::DeadlineAt)
        .order_by_asc(delivery::Column::Id)
        .limit(200)
        .all(&tx.db)
        .await
        .map_err(database)?;
    crate::delivery_history::decode_many(&tx.db, rows).await
}

pub(super) async fn notice(tx: &StoreTransaction, message: Uuid) -> Result<Option<Uuid>> {
    notice::Entity::find_by_id(message.to_string())
        .one(&tx.db)
        .await
        .map_err(database)?
        .map(|row| decode::uuid(&row.notice_dispatch_id))
        .transpose()
}

pub(super) async fn save_notice(tx: &StoreTransaction, message: Uuid, event: Uuid) -> Result<()> {
    if let Some(old) = notice(tx, message).await? {
        return if old == event {
            Ok(())
        } else {
            Err(integrity("Original timeout notice is immutable."))
        };
    }
    let original = crate::dispatches::notice(&tx.db, event)
        .await?
        .ok_or_else(|| integrity("Timeout Notice Dispatch is missing."))?;
    if original.kind != NoticeKind::MessageDeliveryConfirmationTimeout
        || original.data["message_id"].as_str() != Some(message.to_string().as_str())
    {
        return Err(integrity(
            "Timeout Notice differs from its original Message.",
        ));
    }
    notice::ActiveModel {
        message_id: Set(message.to_string()),
        notice_dispatch_id: Set(event.to_string()),
    }
    .insert(&tx.db)
    .await
    .map_err(database)?;
    Ok(())
}

pub(super) async fn expired(tx: &StoreTransaction, message: Uuid) -> Result<ExpiredRecipients> {
    if crate::dispatches::message(&tx.db, message).await?.is_none() {
        return Err(integrity(
            "Expired-recipient source is not a Message Dispatch.",
        ));
    }
    let query = delivery::Entity::find()
        .filter(delivery::Column::DispatchId.eq(message.to_string()))
        .filter(delivery::Column::State.is_in(["failed_timeout", "delivered_late"]))
        .filter(delivery::Column::TimedOutAt.is_not_null());
    let count = query.clone().count(&tx.db).await.map_err(database)?;
    let rows = query
        .order_by_asc(delivery::Column::RecipientId)
        .limit(20)
        .all(&tx.db)
        .await
        .map_err(database)?;
    let delivery_ids = crate::delivery_history::decode_many(&tx.db, rows)
        .await?
        .into_iter()
        .map(|d| d.id)
        .collect();
    Ok(ExpiredRecipients {
        count,
        delivery_ids,
    })
}

/// This scalar scope fence is not a second delivery/receipt representation.
/// In particular, explicit Leave after owner retirement can cancel authored
/// notices without creating another agent.left event or scanning message rows.
pub(super) async fn cancel_author(tx: &StoreTransaction, id: Uuid) -> Result<()> {
    let head = super::sequence::head(&tx.db).await?.get();
    let changed = actor::Entity::update_many()
        .col_expr(actor::Column::TimeoutNoticeAfterSeq, Expr::value(head))
        .filter(actor::Column::Id.eq(id.to_string()))
        .filter(actor::Column::TimeoutNoticeAfterSeq.lte(head))
        .exec(&tx.db)
        .await
        .map_err(database)?;
    if changed.rows_affected != 1 {
        return Err(integrity(
            "Author notification cancellation boundary cannot regress.",
        ));
    }
    Ok(())
}

pub(super) async fn author_allowed(
    tx: &StoreTransaction,
    id: Uuid,
    created: agentchat_core::sequence::Sequence,
) -> Result<bool> {
    let author = actor::Entity::find_by_id(id.to_string())
        .one(&tx.db)
        .await
        .map_err(database)?
        .ok_or_else(|| integrity("Notification author is missing."))?;
    if author.timeout_notice_after_seq < 0 {
        return Err(integrity(
            "Invalid author notification cancellation boundary.",
        ));
    }
    Ok(created.get() > author.timeout_notice_after_seq)
}
