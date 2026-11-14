//! Bounded delivery bookkeeping without changing routing, claims or receipts.

use super::StoreTransaction;
use crate::{
    entities::delivery,
    error::{database, integrity},
};
use agentchat_core::{Result, model::Delivery};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use std::collections::BTreeSet;
use uuid::Uuid;

fn checked(ids: &[Uuid]) -> Result<Vec<String>> {
    if ids.len() > 200 || ids.iter().collect::<BTreeSet<_>>().len() != ids.len() {
        return Err(integrity(
            "Delivery set must have at most 200 distinct IDs.",
        ));
    }
    Ok(ids.iter().map(ToString::to_string).collect())
}

pub(super) async fn get(tx: &StoreTransaction, ids: &[Uuid]) -> Result<Vec<Delivery>> {
    let ids = checked(ids)?;
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    // The primary-key predicate bounds rows by the checked input, including
    // when the requested set contains missing or another recipient's IDs.
    let rows = delivery::Entity::find()
        .filter(delivery::Column::Id.is_in(ids))
        .all(&tx.db)
        .await
        .map_err(database)?;
    crate::delivery_history::decode_many(&tx.db, rows).await
}
