//! Group rows and ordered patterns share the owning immediate write transaction.

use agentchat_core::{Result, model::Reservation};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set,
    sea_query::Expr,
};
use uuid::Uuid;

use super::{StoreTransaction, decode};
use crate::{
    entities::{reservation, reservation_pattern},
    error::{database, integrity},
};

pub(super) async fn get(tx: &StoreTransaction, id: Uuid) -> Result<Option<Reservation>> {
    match reservation::Entity::find_by_id(id.to_string())
        .one(&tx.db)
        .await
        .map_err(database)?
    {
        Some(row) => decode::reservation_group(&tx.db, row).await.map(Some),
        None => Ok(None),
    }
}

pub(super) async fn active(tx: &StoreTransaction, after: Option<Uuid>) -> Result<Vec<Reservation>> {
    let mut query = reservation::Entity::find().filter(reservation::Column::State.eq("active"));
    if let Some(after) = after {
        query = query.filter(reservation::Column::Id.gt(after.to_string()));
    }
    let rows = query
        .order_by_asc(reservation::Column::Id)
        .limit(200)
        .all(&tx.db)
        .await
        .map_err(database)?;
    let mut groups = Vec::with_capacity(rows.len());
    for row in rows {
        groups.push(decode::reservation_group(&tx.db, row).await?);
    }
    Ok(groups)
}

pub(super) async fn save(tx: &StoreTransaction, value: &Reservation) -> Result<()> {
    if let Some(old) = get(tx, value.id).await? {
        if old.owner_id != value.owner_id
            || old.generation != value.generation
            || old.patterns != value.patterns
            || old.ttl_seconds != value.ttl_seconds
            || old.acquired_at != value.acquired_at
            || old.reason != value.reason
        {
            return Err(integrity(
                "Reservation group ownership and patterns are immutable.",
            ));
        }
        if old.state != agentchat_core::model::LeaseState::Active && old != *value {
            return Err(integrity(
                "A terminal reservation cannot be changed or revived.",
            ));
        }
        reservation::Entity::update_many()
            .col_expr(
                reservation::Column::State,
                Expr::value(value.state.as_str()),
            )
            .col_expr(
                reservation::Column::ExpiresAt,
                Expr::value(value.expires_at.to_string()),
            )
            .col_expr(
                reservation::Column::ReleasedAt,
                Expr::value(value.released_at.map(|v| v.to_string())),
            )
            .filter(reservation::Column::Id.eq(value.id.to_string()))
            .exec(&tx.db)
            .await
            .map_err(database)?;
    } else {
        reservation::ActiveModel {
            id: Set(value.id.to_string()),
            owner_id: Set(value.owner_id.to_string()),
            generation: Set(value.generation),
            state: Set(value.state.as_str().into()),
            ttl_seconds: Set(value.ttl_seconds as i32),
            acquired_at: Set(value.acquired_at.to_string()),
            expires_at: Set(value.expires_at.to_string()),
            released_at: Set(value.released_at.map(|v| v.to_string())),
            reason: Set(value.reason.clone()),
        }
        .insert(&tx.db)
        .await
        .map_err(database)?;
        for (i, pattern) in value.patterns.iter().enumerate() {
            reservation_pattern::ActiveModel {
                reservation_id: Set(value.id.to_string()),
                ordinal: Set(i as i32),
                pattern: Set(pattern.clone()),
                grammar_version: Set(1),
            }
            .insert(&tx.db)
            .await
            .map_err(database)?;
        }
    }
    Ok(())
}
