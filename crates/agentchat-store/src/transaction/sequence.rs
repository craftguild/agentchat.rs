//! One rollback-safe causal namespace shared by audit events and Dispatches.
use crate::{
    decode,
    entities::project,
    error::{database, integrity},
};
use agentchat_core::{
    Result,
    sequence::{Cursor, Sequence},
};
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, sea_query::Expr};

pub(super) async fn allocate<C: ConnectionTrait>(db: &C) -> Result<Sequence> {
    let row = project::Entity::find_by_id(1)
        .one(db)
        .await
        .map_err(database)?
        .ok_or_else(|| integrity("Project sequence allocator is missing."))?;
    let value = decode::seq(row.next_sequence)?;
    let next = row
        .next_sequence
        .checked_add(1)
        .ok_or_else(|| integrity("Project sequence exhausted."))?;
    let changed = project::Entity::update_many()
        .col_expr(project::Column::NextSequence, Expr::value(next))
        .filter(project::Column::Singleton.eq(1))
        .filter(project::Column::NextSequence.eq(row.next_sequence))
        .exec(db)
        .await
        .map_err(database)?;
    if changed.rows_affected != 1 {
        return Err(integrity("Project sequence allocator conflicts."));
    }
    Ok(value)
}

pub(crate) async fn head<C: ConnectionTrait>(db: &C) -> Result<Cursor> {
    let row = project::Entity::find_by_id(1)
        .one(db)
        .await
        .map_err(database)?
        .ok_or_else(|| integrity("Project sequence allocator is missing."))?;
    if row.next_sequence <= 0 {
        return Err(integrity("Project sequence allocator is invalid."));
    }
    Cursor::new(row.next_sequence - 1).map_err(|_| integrity("Invalid Dispatch snapshot head."))
}
