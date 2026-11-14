//! Scalar unread/status queries and bounded retained reservation pages.

use super::{ReadSnapshot, fetch_limit};
use crate::{
    decode,
    entities::reservation,
    error::{database, integrity},
};
use agentchat_core::{
    Result, model::Reservation, read::ProjectStatistics, sequence::Cursor, time::Timestamp,
};
use sea_orm::{
    ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect,
    sea_query::{Alias, Asterisk, Expr, ExprTrait, Func, Query},
};
use uuid::Uuid;

async fn count(s: &ReadSnapshot, table: &str, predicate: Option<Expr>) -> Result<u64> {
    let mut query = Query::select()
        .expr_as(Func::count(Expr::col(Asterisk)), Alias::new("count"))
        .from(Alias::new(table))
        .to_owned();
    if let Some(predicate) = predicate {
        query.and_where(predicate);
    }
    let row =
        s.db.query_one(&query)
            .await
            .map_err(database)?
            .ok_or_else(|| integrity("Missing count aggregate."))?;
    let value: i64 = row.try_get("", "count").map_err(database)?;
    value
        .try_into()
        .map_err(|_| integrity("Invalid count aggregate."))
}
pub(super) async fn unread(s: &ReadSnapshot, agent: Uuid) -> Result<u64> {
    let receipt = Query::select()
        .expr(1)
        .from(Alias::new("read_receipts"))
        .and_where(
            Expr::col((Alias::new("read_receipts"), Alias::new("message_id")))
                .equals((Alias::new("dispatch_messages"), Alias::new("dispatch_id"))),
        )
        .and_where(
            Expr::col((Alias::new("read_receipts"), Alias::new("actor_id"))).eq(agent.to_string()),
        )
        .to_owned();
    let membership = Query::select()
        .expr(1)
        .from(Alias::new("channel_memberships"))
        .and_where(
            Expr::col((Alias::new("channel_memberships"), Alias::new("channel_id")))
                .equals((Alias::new("dispatches"), Alias::new("channel_id"))),
        )
        .and_where(
            Expr::col((Alias::new("channel_memberships"), Alias::new("actor_id")))
                .eq(agent.to_string()),
        )
        .and_where(
            Expr::col((Alias::new("channel_memberships"), Alias::new("join_seq"))).lt(Expr::col((
                Alias::new("dispatches"),
                Alias::new("creation_seq"),
            ))),
        )
        .to_owned();
    let visible = Query::select()
        .column(Alias::new("id"))
        .from(Alias::new("dispatches"))
        .and_where(Expr::col(Alias::new("kind")).eq("message"))
        .and_where(Expr::col(Alias::new("sealed")).eq(true))
        .and_where(Expr::col(Alias::new("creation_seq")).lte(s.head.get()))
        .and_where(Expr::exists(membership))
        .to_owned();
    count(
        s,
        "dispatch_messages",
        Some(
            Expr::col(Alias::new("author_id"))
                .ne(agent.to_string())
                .and(Expr::not_exists(receipt))
                .and(Expr::col(Alias::new("dispatch_id")).in_subquery(visible)),
        ),
    )
    .await
}
pub(super) async fn statistics(s: &ReadSnapshot) -> Result<ProjectStatistics> {
    Ok(ProjectStatistics {
        agents: count(s, "actors", Some(Expr::col(Alias::new("kind")).eq("agent"))).await?,
        joined: count(
            s,
            "actors",
            Some(
                Expr::col(Alias::new("participation"))
                    .eq("joined")
                    .and(Expr::col(Alias::new("kind")).eq("agent")),
            ),
        )
        .await?,
        channels: count(s, "channels", None).await?,
        messages: count(s, "dispatch_messages", None).await?,
        events: count(s, "events", None).await?,
        deliveries: count(s, "deliveries", None).await?,
        read_receipts: count(s, "read_receipts", None).await?,
        pending_deliveries: count(
            s,
            "deliveries",
            Some(Expr::col(Alias::new("state")).is_in(["pending", "processing"])),
        )
        .await?,
        stalled_batches: count(
            s,
            "wake_batches",
            Some(Expr::col(Alias::new("state")).eq("stalled")),
        )
        .await?,
    })
}
pub(super) async fn reservations(
    s: &ReadSnapshot,
    owners: Option<Vec<Uuid>>,
    all: bool,
    now: Timestamp,
    after: Cursor,
    limit: usize,
) -> Result<Vec<Reservation>> {
    let mut query = reservation::Entity::find()
        .filter(reservation::Column::Generation.gt(after.get()))
        .filter(reservation::Column::Generation.lte(s.head.get()));
    if let Some(owners) = owners {
        if owners.is_empty() || owners.len() > 2 {
            return Err(integrity(
                "Owner label expansion exceeds its two-kind bound.",
            ));
        }
        query = query
            .filter(reservation::Column::OwnerId.is_in(owners.iter().map(ToString::to_string)));
    }
    if !all {
        query = query
            .filter(reservation::Column::State.eq("active"))
            .filter(reservation::Column::ExpiresAt.gt(now.to_string()));
    }
    let mut records = Vec::new();
    for row in query
        .order_by_asc(reservation::Column::Generation)
        .limit(fetch_limit(limit)?)
        .all(&s.db)
        .await
        .map_err(database)?
    {
        records.push(decode::reservation_group(&s.db, row).await?);
    }
    Ok(records)
}
