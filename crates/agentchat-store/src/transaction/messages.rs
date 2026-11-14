//! Immutable message/receipt rows and acknowledgement bookkeeping.

use agentchat_core::{
    Result,
    model::{Mention, Message, Receipt},
};
use sea_orm::{ActiveModelTrait, EntityTrait, Set};
use uuid::Uuid;

use super::{StoreTransaction, decode};
use crate::{
    entities::{mention, receipt},
    error::{database, integrity},
};

pub(super) async fn message(tx: &StoreTransaction, id: Uuid) -> Result<Option<Message>> {
    crate::dispatches::message(&tx.db, id).await
}

pub(super) async fn insert_mention(tx: &StoreTransaction, value: &Mention) -> Result<()> {
    mention::ActiveModel {
        message_id: Set(value.message_id.to_string()),
        recipient_id: Set(value.recipient_id.to_string()),
        source_kinds: Set(serde_json::to_value(&value.sources)
            .map_err(|_| integrity("Invalid mention sources."))?),
    }
    .insert(&tx.db)
    .await
    .map_err(database)?;
    Ok(())
}

pub(super) async fn receipt(
    tx: &StoreTransaction,
    message: Uuid,
    agent: Uuid,
) -> Result<Option<Receipt>> {
    receipt_kind(
        tx,
        message,
        agent,
        agentchat_core::model::ReadKind::Explicit,
    )
    .await
}
pub(super) async fn receipt_kind(
    tx: &StoreTransaction,
    message: Uuid,
    actor: Uuid,
    kind: agentchat_core::model::ReadKind,
) -> Result<Option<Receipt>> {
    receipt::Entity::find_by_id((
        message.to_string(),
        actor.to_string(),
        kind.as_str().to_owned(),
    ))
    .one(&tx.db)
    .await
    .map_err(database)?
    .map(decode::receipt)
    .transpose()
}

pub(super) async fn insert_receipt(tx: &StoreTransaction, value: &Receipt) -> Result<()> {
    receipt::ActiveModel {
        message_id: Set(value.message_id.to_string()),
        actor_id: Set(value.actor_id.to_string()),
        kind: Set(value.kind.as_str().to_owned()),
        first_read_at: Set(value.first_read_at.to_string()),
    }
    .insert(&tx.db)
    .await
    .map_err(database)?;
    Ok(())
}
