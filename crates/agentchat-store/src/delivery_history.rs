//! Bounded chain validation and atomic current-state/history persistence.
use crate::{
    decode,
    entities::{delivery, delivery_transition},
    error::{database, integrity},
};
use agentchat_core::{
    Result,
    model::{Delivery, DeliveryTransition, MAX_DELIVERY_TRANSITIONS},
};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseTransaction, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, Set, TransactionTrait, sea_query::Expr,
};
use std::collections::BTreeMap;
use uuid::Uuid;

fn version(value: i64) -> Result<u64> {
    let value = u64::try_from(value).map_err(|_| integrity("Invalid delivery state version."))?;
    if value == 0 || value > MAX_DELIVERY_TRANSITIONS as u64 {
        return Err(integrity(
            "Delivery state version is outside its bounded chain.",
        ));
    }
    Ok(value)
}

fn transition(row: delivery_transition::Model) -> Result<DeliveryTransition> {
    Ok(DeliveryTransition {
        id: decode::uuid(&row.id)?,
        delivery_id: decode::uuid(&row.delivery_id)?,
        ordinal: version(row.ordinal)?,
        from_state: row.from_state.map(|s| s.parse()).transpose()?,
        to_state: row.to_state.parse()?,
        changed_at: decode::time(&row.changed_at)?,
        deadline_at: row.deadline_at.as_deref().map(decode::time).transpose()?,
    })
}

pub(crate) async fn decode_many<C: ConnectionTrait>(
    db: &C,
    rows: Vec<delivery::Model>,
) -> Result<Vec<Delivery>> {
    if rows.len() > 201 {
        return Err(integrity(
            "Delivery history fetch exceeds its bounded page.",
        ));
    }
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let history = delivery_transition::Entity::find()
        .filter(delivery_transition::Column::DeliveryId.is_in(rows.iter().map(|r| r.id.clone())))
        .order_by_asc(delivery_transition::Column::DeliveryId)
        .order_by_asc(delivery_transition::Column::Ordinal)
        .limit((rows.len() * MAX_DELIVERY_TRANSITIONS + 1) as u64)
        .all(db)
        .await
        .map_err(database)?;
    if history.len() > rows.len() * MAX_DELIVERY_TRANSITIONS {
        return Err(integrity(
            "Delivery histories exceed the maximum legal chain.",
        ));
    }
    let mut groups: BTreeMap<Uuid, Vec<DeliveryTransition>> = BTreeMap::new();
    for row in history {
        let value = transition(row)?;
        groups.entry(value.delivery_id).or_default().push(value);
    }
    rows.into_iter()
        .map(|row| {
            let stored_version = version(row.state_version)?;
            let value = decode::delivery(row)?;
            let chain = groups.remove(&value.id).unwrap_or_default();
            DeliveryTransition::validate_chain(&value, stored_version, &chain)?;
            Ok(value)
        })
        .collect()
}

pub(crate) async fn get<C: ConnectionTrait>(db: &C, id: Uuid) -> Result<Option<Delivery>> {
    let Some(row) = delivery::Entity::find_by_id(id.to_string())
        .one(db)
        .await
        .map_err(database)?
    else {
        return Ok(None);
    };
    Ok(decode_many(db, vec![row]).await?.into_iter().next())
}

pub(crate) async fn append<C: ConnectionTrait>(db: &C, rows: &[DeliveryTransition]) -> Result<()> {
    for chunk in rows.chunks(64) {
        let values = chunk
            .iter()
            .map(|v| {
                Ok(delivery_transition::ActiveModel {
                    id: Set(v.id.to_string()),
                    delivery_id: Set(v.delivery_id.to_string()),
                    ordinal: Set(i64::try_from(v.ordinal)
                        .map_err(|_| integrity("Transition ordinal overflow."))?),
                    from_state: Set(v.from_state.map(|s| s.as_str().to_owned())),
                    to_state: Set(v.to_state.as_str().to_owned()),
                    changed_at: Set(v.changed_at.to_string()),
                    deadline_at: Set(v.deadline_at.map(|t| t.to_string())),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let expected = values.len() as u64;
        let inserted = delivery_transition::Entity::insert_many(values)
            .exec_without_returning(db)
            .await
            .map_err(database)?;
        if inserted != expected {
            return Err(integrity("Transition insertion omitted history rows."));
        }
    }
    Ok(())
}

pub(crate) fn active(value: &Delivery, state_version: u64) -> Result<delivery::ActiveModel> {
    Ok(delivery::ActiveModel {
        id: Set(value.id.to_string()),
        dispatch_id: Set(value.dispatch_id.to_string()),
        recipient_id: Set(value.recipient_id.to_string()),
        state: Set(value.state.as_str().to_owned()),
        state_version: Set(
            i64::try_from(state_version).map_err(|_| integrity("Delivery version overflow."))?
        ),
        wake_eligible: Set(value.wake_eligible),
        wake_batch_id: Set(value.wake_batch_id.map(|v| v.to_string())),
        attempts: Set(value
            .attempts
            .try_into()
            .map_err(|_| integrity("Delivery attempts overflow."))?),
        queued_at: Set(value.history.queued_at.to_string()),
        processing_started_at: Set(value.history.processing_started_at.map(|v| v.to_string())),
        deadline_at: Set(value.history.deadline_at.map(|v| v.to_string())),
        timed_out_at: Set(value.history.timed_out_at.map(|v| v.to_string())),
        delivered_at: Set(value.history.delivered_at.map(|v| v.to_string())),
        left_at: Set(value.history.left_at.map(|v| v.to_string())),
    })
}

/// Local rollback preserves the pair even if a caller ignores an error and commits.
pub(crate) async fn save(db: &DatabaseTransaction, value: &Delivery) -> Result<()> {
    let nested = db.begin().await.map_err(database)?;
    match save_inner(&nested, value).await {
        Ok(()) => nested.commit().await.map_err(database),
        Err(error) => {
            nested.rollback().await.map_err(database)?;
            Err(error)
        }
    }
}

async fn save_inner(db: &DatabaseTransaction, value: &Delivery) -> Result<()> {
    let row = delivery::Entity::find_by_id(value.id.to_string())
        .one(db)
        .await
        .map_err(database)?
        .ok_or_else(|| integrity("Delivery is missing."))?;
    let old_version = version(row.state_version)?;
    let old = decode_many(db, vec![row]).await?.remove(0);
    if value.attempts < old.attempts || value.wake_eligible != old.wake_eligible {
        return Err(integrity(
            "Delivery attempts or original wake policy conflicts.",
        ));
    }
    let steps = DeliveryTransition::plan_update(&old, value, old_version)?;
    let next_version = old_version + steps.len() as u64;
    let attempts: i32 = value
        .attempts
        .try_into()
        .map_err(|_| integrity("Delivery attempts overflow."))?;
    let changed = delivery::Entity::update_many()
        .col_expr(delivery::Column::State, Expr::value(value.state.as_str()))
        .col_expr(
            delivery::Column::StateVersion,
            Expr::value(
                i64::try_from(next_version).map_err(|_| integrity("Delivery version overflow."))?,
            ),
        )
        .col_expr(
            delivery::Column::WakeBatchId,
            Expr::value(value.wake_batch_id.map(|v| v.to_string())),
        )
        .col_expr(delivery::Column::Attempts, Expr::value(attempts))
        .col_expr(
            delivery::Column::QueuedAt,
            Expr::value(value.history.queued_at.to_string()),
        )
        .col_expr(
            delivery::Column::ProcessingStartedAt,
            Expr::value(value.history.processing_started_at.map(|v| v.to_string())),
        )
        .col_expr(
            delivery::Column::DeadlineAt,
            Expr::value(value.history.deadline_at.map(|v| v.to_string())),
        )
        .col_expr(
            delivery::Column::TimedOutAt,
            Expr::value(value.history.timed_out_at.map(|v| v.to_string())),
        )
        .col_expr(
            delivery::Column::DeliveredAt,
            Expr::value(value.history.delivered_at.map(|v| v.to_string())),
        )
        .col_expr(
            delivery::Column::LeftAt,
            Expr::value(value.history.left_at.map(|v| v.to_string())),
        )
        .filter(delivery::Column::Id.eq(value.id.to_string()))
        .filter(delivery::Column::DispatchId.eq(old.dispatch_id.to_string()))
        .filter(delivery::Column::RecipientId.eq(old.recipient_id.to_string()))
        .filter(delivery::Column::State.eq(old.state.as_str()))
        .filter(
            delivery::Column::StateVersion
                .eq(i64::try_from(old_version)
                    .map_err(|_| integrity("Delivery version overflow."))?),
        )
        .exec(db)
        .await
        .map_err(database)?;
    if changed.rows_affected != 1 {
        return Err(integrity(
            "Delivery version or immutable identity conflicts.",
        ));
    }
    append(db, &steps).await?;
    get(db, value.id)
        .await?
        .ok_or_else(|| integrity("Saved delivery disappeared."))?;
    Ok(())
}
