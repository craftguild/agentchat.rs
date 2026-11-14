//! Bounded read-only validation of Dispatch aggregates, history and causal order.
use crate::{
    decode,
    entities::{delivery, dispatch, event},
    error::{database, integrity},
};
use agentchat_core::{Result, model::DispatchPayload};
use sea_orm::{
    ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect,
    sea_query::{Alias, Expr, ExprTrait, Func, Query},
};

pub(crate) async fn check<C: ConnectionTrait>(db: &C) -> Result<()> {
    crate::transaction::dispatches::guard(db).await?;
    let shared_head = crate::transaction::sequence::head(db).await?.get();
    let audit_max = maximum(db, event::Entity, event::Column::Seq).await?;
    let dispatch_max = maximum(db, dispatch::Entity, dispatch::Column::CreationSeq).await?;
    if shared_head != std::cmp::max(audit_max, dispatch_max) {
        return Err(integrity(
            "Causal allocator differs from the last committed fact.",
        ));
    }
    let duplicate = Query::select()
        .expr(1)
        .from(dispatch::Entity)
        .inner_join(
            event::Entity,
            Expr::col((event::Entity, event::Column::Seq))
                .equals((dispatch::Entity, dispatch::Column::CreationSeq)),
        )
        .limit(1)
        .to_owned();
    if db.query_one(&duplicate).await.map_err(database)?.is_some() {
        return Err(integrity(
            "Audit and Dispatch records reuse a causal sequence.",
        ));
    }
    dispatches(db).await?;
    deliveries(db).await?;
    audits(db).await
}
async fn maximum<C: ConnectionTrait, T: sea_orm::Iden + 'static, K: sea_orm::Iden + 'static>(
    db: &C,
    table: T,
    column: K,
) -> Result<i64> {
    let query = Query::select()
        .expr_as(Func::max(Expr::col(column)), Alias::new("maximum"))
        .from(table)
        .to_owned();
    let row = db
        .query_one(&query)
        .await
        .map_err(database)?
        .ok_or_else(|| integrity("Missing causal maximum."))?;
    Ok(row
        .try_get::<Option<i64>>("", "maximum")
        .map_err(database)?
        .unwrap_or(0))
}
async fn dispatches<C: ConnectionTrait>(db: &C) -> Result<()> {
    let mut after = 0;
    loop {
        let rows = dispatch::Entity::find()
            .filter(dispatch::Column::CreationSeq.gt(after))
            .order_by_asc(dispatch::Column::CreationSeq)
            .limit(64)
            .all(db)
            .await
            .map_err(database)?;
        if rows.is_empty() {
            return Ok(());
        }
        let ids = rows
            .iter()
            .map(|r| decode::uuid(&r.id))
            .collect::<Result<Vec<_>>>()?;
        let payloads = crate::dispatches::hydrate(db, &ids).await?;
        for (_, (_, payload)) in payloads {
            if let DispatchPayload::Message(message) = payload
                && let Some(root) = message.root_message_id
            {
                let root = crate::dispatches::message(db, root)
                    .await?
                    .ok_or_else(|| integrity("Message root is absent."))?;
                if root.root_message_id.is_some() || root.channel_id != message.channel_id {
                    return Err(integrity(
                        "Message root is not a direct root in the same channel.",
                    ));
                }
            }
        }
        after = rows
            .last()
            .ok_or_else(|| integrity("Missing Dispatch page tail."))?
            .creation_seq;
    }
}
async fn deliveries<C: ConnectionTrait>(db: &C) -> Result<()> {
    let mut after = String::new();
    loop {
        let rows = delivery::Entity::find()
            .filter(delivery::Column::Id.gt(&after))
            .order_by_asc(delivery::Column::Id)
            .limit(64)
            .all(db)
            .await
            .map_err(database)?;
        if rows.is_empty() {
            return Ok(());
        }
        after = rows
            .last()
            .ok_or_else(|| integrity("Missing delivery page tail."))?
            .id
            .clone();
        crate::delivery_history::decode_many(db, rows).await?;
    }
}
async fn audits<C: ConnectionTrait>(db: &C) -> Result<()> {
    let mut ordinal = 0i64;
    let mut sequence = 0;
    loop {
        let rows = event::Entity::find()
            .filter(event::Column::Ordinal.gt(ordinal))
            .order_by_asc(event::Column::Ordinal)
            .limit(64)
            .all(db)
            .await
            .map_err(database)?;
        if rows.is_empty() {
            break;
        }
        for row in rows {
            if !row.sealed || row.seq <= sequence || ordinal.checked_add(1) != Some(row.ordinal) {
                return Err(integrity(
                    "Audit publication, order or retained interval is invalid.",
                ));
            }
            ordinal = row.ordinal;
            sequence = row.seq;
            decode::event(row)?;
        }
    }
    let query = Query::select()
        .column(Alias::new("seq"))
        .from(Alias::new("sqlite_sequence"))
        .and_where(Expr::col(Alias::new("name")).eq("events"))
        .to_owned();
    let recorded = db
        .query_one(&query)
        .await
        .map_err(database)?
        .map(|r| r.try_get::<i64>("", "seq").map_err(database))
        .transpose()?
        .unwrap_or(0);
    if recorded != ordinal {
        return Err(integrity(
            "Audit ordinal high-water mark contains a missing tail.",
        ));
    }
    Ok(())
}
