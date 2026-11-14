//! Stable first-join ordering and retained identity lookups in one snapshot.

use super::{ReadSnapshot, fetch_limit};
use crate::{
    decode,
    entities::{
        actor as agent, channel, dispatch, event, launch, membership, project_default_channel,
        session,
    },
    error::{database, integrity},
};
use agentchat_core::{
    Result,
    model::*,
    project::ProjectChannelPolicy,
    read::{ChannelActivity, ChannelListCondition, Indexed},
    sequence::Cursor,
    time::Timestamp,
};
use sea_orm::{
    ColumnTrait, ConnectionTrait, EntityTrait, FromQueryResult, QueryFilter, QueryOrder,
    sea_query::{Alias, Asterisk, Expr, ExprTrait, Func, Order, Query, SimpleExpr},
};
use uuid::Uuid;

pub(super) async fn agent(s: &ReadSnapshot, id: Uuid) -> Result<Option<Agent>> {
    agent::Entity::find_by_id(id.to_string())
        .filter(agent::Column::Kind.eq("agent"))
        .one(&s.db)
        .await
        .map_err(database)?
        .map(decode::agent)
        .transpose()
}
pub(super) async fn conversation(
    s: &ReadSnapshot,
    host: HostKind,
    id: &str,
) -> Result<Option<Agent>> {
    agent::Entity::find()
        .filter(agent::Column::Kind.eq("agent"))
        .filter(agent::Column::HostKind.eq(host.as_str()))
        .filter(agent::Column::HostSessionId.eq(id))
        .one(&s.db)
        .await
        .map_err(database)?
        .map(decode::agent)
        .transpose()
}
pub(super) async fn handle(s: &ReadSnapshot, name: &str) -> Result<Option<Agent>> {
    agent::Entity::find()
        .filter(agent::Column::Kind.eq("agent"))
        .filter(agent::Column::Handle.eq(name))
        .one(&s.db)
        .await
        .map_err(database)?
        .map(decode::agent)
        .transpose()
}
pub(super) async fn session(s: &ReadSnapshot, id: Uuid) -> Result<Option<Session>> {
    session::Entity::find()
        .filter(session::Column::AgentId.eq(id.to_string()))
        .filter(session::Column::IsCurrent.eq(true))
        .one(&s.db)
        .await
        .map_err(database)?
        .map(decode::session)
        .transpose()
}
pub(super) async fn latest_launch(s: &ReadSnapshot, agent: Uuid) -> Result<Option<Launch>> {
    launch::Entity::find()
        .filter(launch::Column::AgentId.eq(agent.to_string()))
        .order_by_desc(launch::Column::StartSeq)
        .one(&s.db)
        .await
        .map_err(database)?
        .map(decode::launch)
        .transpose()
}
pub(super) async fn channel(s: &ReadSnapshot, name: &str) -> Result<Option<Channel>> {
    channel::Entity::find()
        .filter(channel::Column::Name.eq(name))
        .one(&s.db)
        .await
        .map_err(database)?
        .map(decode::channel)
        .transpose()
}
pub(super) async fn channel_by_id(s: &ReadSnapshot, id: Uuid) -> Result<Option<Channel>> {
    channel::Entity::find_by_id(id.to_string())
        .one(&s.db)
        .await
        .map_err(database)?
        .map(decode::channel)
        .transpose()
}
pub(super) async fn is_member(s: &ReadSnapshot, channel: Uuid, agent: Uuid) -> Result<bool> {
    Ok(
        membership::Entity::find_by_id((channel.to_string(), agent.to_string()))
            .one(&s.db)
            .await
            .map_err(database)?
            .is_some(),
    )
}
pub(super) async fn member_channels(s: &ReadSnapshot, agent: Uuid) -> Result<Vec<Uuid>> {
    membership::Entity::find()
        .filter(membership::Column::ActorId.eq(agent.to_string()))
        .order_by_asc(membership::Column::JoinSeq)
        .all(&s.db)
        .await
        .map_err(database)?
        .into_iter()
        .map(|row| {
            Uuid::parse_str(&row.channel_id)
                .map_err(|_| integrity("Invalid membership channel UUID."))
        })
        .collect()
}
pub(super) async fn channel_policy(s: &ReadSnapshot) -> Result<Option<ProjectChannelPolicy>> {
    let rows = project_default_channel::Entity::find()
        .order_by_asc(project_default_channel::Column::Position)
        .all(&s.db)
        .await
        .map_err(database)?;
    if rows.is_empty() {
        return Ok(None);
    }
    let retained = channel::Entity::find()
        .all(&s.db)
        .await
        .map_err(database)?
        .into_iter()
        .map(|channel| (channel.id, channel.name))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut names = Vec::with_capacity(rows.len());
    for (index, row) in rows.into_iter().enumerate() {
        if row.position != index as i32 {
            return Err(integrity(
                "Project default channels are not in a contiguous order.",
            ));
        }
        names.push(
            retained.get(&row.channel_id).cloned().ok_or_else(|| {
                integrity("Project default channel references a missing channel.")
            })?,
        );
    }
    ProjectChannelPolicy::new(names).map(Some)
}
pub(super) async fn channels(
    s: &ReadSnapshot,
    after: Cursor,
    limit: usize,
    condition: ChannelListCondition,
    cutoff: Timestamp,
) -> Result<Vec<ChannelActivity>> {
    let last_conversation = last_conversation(s.head.get());
    let mut query = Query::select()
        .column(Asterisk)
        .expr_as(
            last_conversation.clone(),
            Alias::new("last_conversation_at"),
        )
        .from(channel::Entity)
        .and_where(channel::Column::CreationSeq.gt(after.get()))
        .and_where(channel::Column::CreationSeq.lte(s.head.get()))
        .order_by(channel::Column::CreationSeq, Order::Asc)
        .limit(fetch_limit(limit)?)
        .to_owned();
    match condition {
        ChannelListCondition::Active => {
            query.and_where(last_conversation.gt(cutoff.to_string()));
        }
        ChannelListCondition::Archived => {
            query.and_where(last_conversation.lte(cutoff.to_string()));
        }
        ChannelListCondition::All => {}
    }
    s.db.query_all(&query)
        .await
        .map_err(database)?
        .into_iter()
        .map(|row| {
            let stored = channel::Model::from_query_result(&row, "").map_err(database)?;
            let value: Option<String> =
                row.try_get("", "last_conversation_at").map_err(database)?;
            let value = value.ok_or_else(|| {
                integrity("Channel is missing its creation event and conversation time.")
            })?;
            Ok(ChannelActivity {
                channel: decode::channel(stored)?,
                last_conversation_at: Timestamp::parse(&value)
                    .map_err(|_| integrity("Channel conversation time is invalid."))?,
            })
        })
        .collect()
}

fn last_conversation(head: i64) -> SimpleExpr {
    let messages = Query::select()
        .expr(Func::max(Expr::col((
            Alias::new("activity_dispatch"),
            dispatch::Column::CreatedAt,
        ))))
        .from_as(dispatch::Entity, Alias::new("activity_dispatch"))
        .and_where(
            Expr::col((Alias::new("activity_dispatch"), dispatch::Column::ChannelId))
                .equals((channel::Entity, channel::Column::Id)),
        )
        .and_where(
            Expr::col((
                Alias::new("activity_dispatch"),
                dispatch::Column::CreationSeq,
            ))
            .lte(head),
        )
        .and_where(
            Expr::col((Alias::new("activity_dispatch"), dispatch::Column::Kind)).eq("message"),
        )
        .and_where(Expr::col((Alias::new("activity_dispatch"), dispatch::Column::Sealed)).eq(true))
        .to_owned();
    let created = Query::select()
        .column((Alias::new("creation_event"), event::Column::OccurredAt))
        .from_as(event::Entity, Alias::new("creation_event"))
        .and_where(
            Expr::col((Alias::new("creation_event"), event::Column::Seq))
                .equals((channel::Entity, channel::Column::CreationSeq)),
        )
        .and_where(
            Expr::col((Alias::new("creation_event"), event::Column::Kind)).eq("channel.created"),
        )
        .and_where(Expr::col((Alias::new("creation_event"), event::Column::Sealed)).eq(true))
        .to_owned();
    Func::if_null(
        Expr::SubQuery(None, Box::new(messages.into())),
        Expr::SubQuery(None, Box::new(created.into())),
    )
    .into()
}

pub(super) fn first_join(id: Expr) -> Expr {
    // Latest join_seq changes after rejoining. Retained history supplies the
    // immutable first event, so pagination cannot repeat a renamed ordering key.
    let query = Query::select()
        .expr(Func::min(Expr::col((
            Alias::new("first_join_event"),
            Alias::new("seq"),
        ))))
        .from_as(Alias::new("events"), Alias::new("first_join_event"))
        .and_where(Expr::col((Alias::new("first_join_event"), Alias::new("actor_id"))).eq(id))
        .and_where(
            Expr::col((Alias::new("first_join_event"), Alias::new("kind")))
                .is_in(["agent.joined", "user.joined"]),
        )
        .and_where(Expr::col((Alias::new("first_join_event"), Alias::new("sealed"))).eq(true))
        .to_owned();
    Expr::SubQuery(None, Box::new(query.into()))
}

pub(super) async fn agents(
    s: &ReadSnapshot,
    after: Cursor,
    limit: usize,
    channel: Option<Uuid>,
) -> Result<Vec<Indexed<Agent>>> {
    let key = first_join(Expr::col((Alias::new("actors"), Alias::new("id"))));
    let mut query = Query::select()
        .column(Asterisk)
        .expr_as(key.clone(), Alias::new("page_key"))
        .from(Alias::new("actors"))
        .and_where(Expr::col(Alias::new("kind")).eq("agent"))
        .and_where(key.clone().gt(after.get()))
        .and_where(key.lte(s.head.get()))
        .order_by(Alias::new("page_key"), Order::Asc)
        .limit(fetch_limit(limit)?)
        .to_owned();
    if let Some(channel) = channel {
        query.and_where(
            Expr::col(Alias::new("id")).in_subquery(
                Query::select()
                    .column(Alias::new("actor_id"))
                    .from(Alias::new("channel_memberships"))
                    .and_where(Expr::col(Alias::new("channel_id")).eq(channel.to_string()))
                    .to_owned(),
            ),
        );
        query.and_where(Expr::col(Alias::new("participation")).eq("joined"));
    }
    s.db.query_all(&query)
        .await
        .map_err(database)?
        .into_iter()
        .map(|row| {
            Ok(Indexed {
                key: decode::seq(row.try_get("", "page_key").map_err(database)?)?,
                item: decode::agent(agent::Model::from_query_result(&row, "").map_err(database)?)?,
            })
        })
        .collect()
}

pub(super) async fn actors(
    s: &ReadSnapshot,
    after: Cursor,
    limit: usize,
    channel: Uuid,
) -> Result<Vec<Indexed<Actor>>> {
    let key = first_join(Expr::col((Alias::new("actors"), Alias::new("id"))));
    let query = Query::select()
        .column(Asterisk)
        .expr_as(key.clone(), Alias::new("page_key"))
        .from(Alias::new("actors"))
        .and_where(key.clone().gt(after.get()))
        .and_where(key.lte(s.head.get()))
        .and_where(Expr::col(Alias::new("participation")).eq("joined"))
        .and_where(
            Expr::col(Alias::new("id")).in_subquery(
                Query::select()
                    .column(Alias::new("actor_id"))
                    .from(Alias::new("channel_memberships"))
                    .and_where(Expr::col(Alias::new("channel_id")).eq(channel.to_string()))
                    .to_owned(),
            ),
        )
        .order_by(Alias::new("page_key"), Order::Asc)
        .limit(fetch_limit(limit)?)
        .to_owned();
    s.db.query_all(&query)
        .await
        .map_err(database)?
        .into_iter()
        .map(|row| {
            Ok(Indexed {
                key: decode::seq(row.try_get("", "page_key").map_err(database)?)?,
                item: decode::actor(agent::Model::from_query_result(&row, "").map_err(database)?)?,
            })
        })
        .collect()
}
