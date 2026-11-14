//! Independent bounded audit pages and Dispatch-ordered mixed Inbox hydration.
use super::{ReadSnapshot, fetch_limit};
use crate::{
    decode, delivery_history, dispatches,
    entities::{delivery, dispatch, event, wake_batch},
    error::{database, integrity},
};
use agentchat_core::{
    Error, ErrorCode, Result,
    model::{DispatchPayload, Event},
    read::InboxRecord,
    sequence::Cursor,
};
use sea_orm::{
    ColumnTrait, ConnectionTrait, EntityTrait, FromQueryResult, QueryFilter, QueryOrder,
    QuerySelect,
    sea_query::{Condition, Expr, ExprTrait, Order},
};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

pub(super) async fn list(
    s: &ReadSnapshot,
    after: Cursor,
    limit: usize,
    channels: Option<&[Uuid]>,
) -> Result<Vec<Event>> {
    let mut query = event::Entity::find()
        .filter(event::Column::Seq.gt(after.get()))
        .filter(event::Column::Seq.lte(s.head.get()))
        .filter(event::Column::Sealed.eq(true));
    if let Some(channels) = channels {
        query = query.filter(
            Condition::any()
                .add(event::Column::ChannelId.is_in(channels.iter().map(Uuid::to_string)))
                .add(
                    Condition::all()
                        .add(event::Column::ChannelId.is_null())
                        .add(event::Column::Kind.is_in([
                            "agent.joined",
                            "agent.left",
                            "agent.launch_retired",
                            "reservation.acquired",
                            "reservation.expired",
                            "reservation.released",
                            "project.channel_policy_changed",
                            "project.channel_policy_materialized",
                        ])),
                ),
        );
    }
    let rows = query
        .order_by_asc(event::Column::Seq)
        .limit(fetch_limit(limit)?)
        .all(&s.db)
        .await
        .map_err(database)?;
    if channels.is_none() {
        let previous = event::Entity::find()
            .select_only()
            .column(event::Column::Ordinal)
            .filter(event::Column::Seq.lte(after.get()))
            .filter(event::Column::Sealed.eq(true))
            .order_by_desc(event::Column::Seq)
            .into_tuple::<i64>()
            .one(&s.db)
            .await
            .map_err(database)?
            .unwrap_or(0);
        let mut ordinal = previous;
        for row in &rows {
            if ordinal.checked_add(1) != Some(row.ordinal) {
                return Err(invalidated());
            }
            ordinal = row.ordinal;
        }
    }
    rows.into_iter().map(decode::event).collect()
}

fn invalidated() -> Error {
    let mut error = Error::new(
        ErrorCode::InvalidArgument,
        "Retained audit history has a missing interval; reset the snapshot.",
    );
    error.details =
        serde_json::json!({"reason":"history_cursor_invalidated","reset_required":true});
    error
}

pub(super) async fn inbox(
    s: &ReadSnapshot,
    actor: Uuid,
    after: Cursor,
    limit: usize,
) -> Result<Vec<InboxRecord>> {
    let query = crate::queue::selected(actor)
        .cond_where(crate::scope::inbox_delivery(actor))
        .and_where(Expr::col((dispatch::Entity, dispatch::Column::CreationSeq)).gt(after.get()))
        .and_where(
            Expr::col((dispatch::Entity, dispatch::Column::CreationSeq)).lte(s.dispatch_head.get()),
        )
        .order_by(
            (dispatch::Entity, dispatch::Column::CreationSeq),
            Order::Asc,
        )
        .limit(fetch_limit(limit)?)
        .to_owned();
    let rows =
        s.db.query_all(&query)
            .await
            .map_err(database)?
            .into_iter()
            .map(|r| delivery::Model::from_query_result(&r, "").map_err(database))
            .collect::<Result<Vec<_>>>()?;
    let deliveries = delivery_history::decode_many(&s.db, rows).await?;
    let mut payloads = dispatches::hydrate(
        &s.db,
        &deliveries.iter().map(|v| v.dispatch_id).collect::<Vec<_>>(),
    )
    .await?;
    let ids = deliveries
        .iter()
        .filter_map(|d| d.wake_batch_id)
        .map(|id| id.to_string())
        .collect::<BTreeSet<_>>();
    let batches = if ids.is_empty() {
        BTreeMap::new()
    } else {
        wake_batch::Entity::find()
            .filter(wake_batch::Column::Id.is_in(ids))
            .all(&s.db)
            .await
            .map_err(database)?
            .into_iter()
            .map(|r| (r.id, r.state))
            .collect()
    };
    deliveries
        .into_iter()
        .map(|delivery| {
            let (dispatch, payload) = payloads
                .remove(&delivery.dispatch_id)
                .ok_or_else(|| integrity("Inbox Dispatch is missing."))?;
            let notice = match payload {
                DispatchPayload::Notice(v) => Some(v),
                DispatchPayload::Message(_) => None,
            };
            let batch_state = delivery
                .wake_batch_id
                .map(|id| {
                    batches
                        .get(&id.to_string())
                        .cloned()
                        .ok_or_else(|| integrity("Assigned wake batch is missing."))
                })
                .transpose()?;
            Ok(InboxRecord {
                delivery,
                dispatch,
                notice,
                batch_state,
            })
        })
        .collect()
}
