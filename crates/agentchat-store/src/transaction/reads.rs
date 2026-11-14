//! Bounded explicit-read sets share the caller's existing immediate transaction.

use super::StoreTransaction;
use crate::{
    decode,
    entities::{dispatch, receipt},
    error::{database, integrity},
};
use agentchat_core::{
    Result,
    model::{Message, Receipt},
};
use sea_orm::{
    ColumnTrait, EntityTrait, QueryFilter, Set,
    sea_query::{Expr, ExprTrait},
};
use std::collections::BTreeSet;
use uuid::Uuid;

fn checked(ids: &[Uuid]) -> Result<Vec<String>> {
    if ids.len() > 200 || ids.iter().collect::<BTreeSet<_>>().len() != ids.len() {
        return Err(integrity(
            "Read set must have at most 200 distinct message IDs.",
        ));
    }
    Ok(ids.iter().map(ToString::to_string).collect())
}

pub(super) async fn messages(tx: &StoreTransaction, ids: &[Uuid]) -> Result<Vec<Message>> {
    let ids = checked(ids)?;
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let query = crate::dispatches::message_query()
        .and_where(Expr::col((dispatch::Entity, dispatch::Column::Id)).is_in(ids))
        .to_owned();
    crate::dispatches::messages(&tx.db, &query).await
}

pub(super) async fn receipts(
    tx: &StoreTransaction,
    agent: Uuid,
    ids: &[Uuid],
) -> Result<Vec<Receipt>> {
    let ids = checked(ids)?;
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    receipt::Entity::find()
        .filter(receipt::Column::ActorId.eq(agent.to_string()))
        .filter(receipt::Column::Kind.eq("explicit"))
        .filter(receipt::Column::MessageId.is_in(ids))
        .all(&tx.db)
        .await
        .map_err(database)?
        .into_iter()
        .map(decode::receipt)
        .collect()
}

pub(super) async fn insert(tx: &StoreTransaction, receipts: &[Receipt]) -> Result<()> {
    if receipts.len() > 200
        || receipts
            .iter()
            .map(|r| (r.message_id, r.actor_id, r.kind))
            .collect::<BTreeSet<_>>()
            .len()
            != receipts.len()
    {
        return Err(integrity(
            "First-read set must have at most 200 distinct message/agent pairs.",
        ));
    }
    if receipts.is_empty() {
        return Ok(());
    }
    // At most 800 binds: one ordinary INSERT statement, with no conflict-ignore
    // or upsert that could erase first times or conceal an incomplete set.
    // Its success is still provisional until the enclosing FULL commit.
    let rows = receipts
        .iter()
        .map(|r| receipt::ActiveModel {
            message_id: Set(r.message_id.to_string()),
            actor_id: Set(r.actor_id.to_string()),
            kind: Set(r.kind.as_str().to_owned()),
            first_read_at: Set(r.first_read_at.to_string()),
        })
        .collect::<Vec<_>>();
    let inserted = receipt::Entity::insert_many(rows)
        .exec_without_returning(&tx.db)
        .await
        .map_err(database)?;
    if inserted != receipts.len() as u64 {
        return Err(integrity("First-read insertion omitted requested rows."));
    }
    Ok(())
}
