//! Omit only validated nonempty sets blocked by the shared retry predicate.

use super::Candidate;
use crate::{
    decode,
    entities::wake_batch,
    error::{database, integrity},
    transaction::wake::selected_deliveries,
};
use agentchat_core::{Result, delivery::MAX_WAKE_SCAN_CONTEXTS, time::Timestamp};
use sea_orm::{
    ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QuerySelect, sea_query::Condition,
};
use std::collections::BTreeSet;
use uuid::Uuid;

pub(super) async fn blocked<C: ConnectionTrait>(
    db: &C,
    candidates: &[Candidate<'_>],
    now: Timestamp,
) -> Result<BTreeSet<(Uuid, Uuid)>> {
    let keys = candidates
        .iter()
        .map(|(agent, context)| (*agent, context.incarnation))
        .collect::<BTreeSet<_>>();
    if keys.is_empty() {
        return Ok(BTreeSet::new());
    }
    let mut scope = Condition::any();
    for (agent, incarnation) in &keys {
        scope = scope.add(
            Condition::all()
                .add(wake_batch::Column::RecipientId.eq(agent.to_string()))
                .add(wake_batch::Column::Incarnation.eq(incarnation.to_string())),
        );
    }
    let rows = wake_batch::Entity::find()
        .filter(scope)
        .filter(wake_batch::Column::State.is_in(["pending", "inflight", "stalled"]))
        .limit(MAX_WAKE_SCAN_CONTEXTS as u64 + 1)
        .all(db)
        .await
        .map_err(database)?;
    if rows.len() > keys.len() {
        return Err(integrity("Outstanding wake sets exceed their exact scope."));
    }
    let mut seen = BTreeSet::new();
    let mut blocked = BTreeSet::new();
    for row in rows {
        let batch = decode::batch(row)?;
        let key = (batch.recipient_id, batch.incarnation);
        if !keys.contains(&key) || !seen.insert(key) {
            return Err(integrity(
                "Outstanding wake set has an unrelated or repeated owner.",
            ));
        }
        if batch.retry_deferred(now)
            && !selected_deliveries(db, batch.recipient_id, Some(batch.id), false, true)
                .await?
                .is_empty()
        {
            // Reuse the ordinary recipient/batch/state/sealed filters, full
            // delivery decoder and 21-row overflow guard. Never prune an empty
            // assigned set: ordinary preparation must still reconcile it.
            blocked.insert(key);
        }
    }
    Ok(blocked)
}
