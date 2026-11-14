//! Immutable generation facts and one-way retirement under the writer boundary.
use super::StoreTransaction;
use crate::{
    decode,
    entities::launch,
    error::{database, integrity},
};
use agentchat_core::{Result, model::Launch};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set,
    TryIntoModel,
};
use uuid::Uuid;

pub(super) async fn get(tx: &StoreTransaction, generation: Uuid) -> Result<Option<Launch>> {
    launch::Entity::find_by_id(generation.to_string())
        .one(&tx.db)
        .await
        .map_err(database)?
        .map(decode::launch)
        .transpose()
}
pub(super) async fn latest(tx: &StoreTransaction, agent: Uuid) -> Result<Option<Launch>> {
    launch::Entity::find()
        .filter(launch::Column::AgentId.eq(agent.to_string()))
        .order_by_desc(launch::Column::StartSeq)
        .one(&tx.db)
        .await
        .map_err(database)?
        .map(decode::launch)
        .transpose()
}
pub(super) async fn active(tx: &StoreTransaction, after: Option<Uuid>) -> Result<Vec<Launch>> {
    let mut query = launch::Entity::find().filter(launch::Column::RetiredAt.is_null());
    if let Some(after) = after {
        query = query.filter(launch::Column::Generation.gt(after.to_string()));
    }
    query
        .order_by_asc(launch::Column::Generation)
        .limit(32)
        .all(&tx.db)
        .await
        .map_err(database)?
        .into_iter()
        .map(decode::launch)
        .collect()
}
pub(super) async fn save(tx: &StoreTransaction, value: &Launch) -> Result<()> {
    let row = launch::ActiveModel {
        generation: Set(value.identity.generation.to_string()),
        agent_id: Set(value.agent_id.to_string()),
        context_id: Set(value.context_id.to_string()),
        incarnation: Set(value.incarnation.to_string()),
        owner: Set(value.identity.owner.clone()),
        start_seq: Set(value.start_seq.get()),
        started_at: Set(value.started_at.to_string()),
        retired_at: Set(value.retired_at.map(|t| t.to_string())),
        retirement_reason: Set(value.retirement_reason.clone()),
    };
    // Decode the same bounded representation on both reads and writes. Saved
    // metadata never becomes current authority merely because it parses.
    decode::launch(row.clone().try_into_model().map_err(database)?)?;
    if let Some(old) = get(tx, value.identity.generation).await? {
        let mut expected = old.clone();
        expected.retired_at = value.retired_at;
        expected.retirement_reason = value.retirement_reason.clone();
        if expected != *value || (old.retired_at.is_some() && old != *value) {
            return Err(integrity(
                "Launch facts are immutable and retirement is terminal.",
            ));
        }
        row.update(&tx.db).await.map_err(database)?;
    } else {
        row.insert(&tx.db).await.map_err(database)?;
    }
    Ok(())
}
