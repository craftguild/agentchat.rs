//! Relational identity, membership and exact-incarnation persistence.

use agentchat_core::{
    Error, ErrorCode, Result,
    model::{Agent, Channel, HostKind, Session},
    project::ProjectChannelPolicy,
    sequence::Sequence,
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, Set,
    sea_query::{Expr, OnConflict},
};
use uuid::Uuid;

use super::{StoreTransaction, decode};
use crate::{
    entities::{actor as agent, channel, membership, project_default_channel, session},
    error::{database, integrity},
};

pub(super) async fn disconnect_sessions(tx: &StoreTransaction) -> Result<()> {
    session::Entity::update_many()
        .col_expr(session::Column::State, Expr::value("disconnected"))
        .filter(session::Column::IsCurrent.eq(true))
        .exec(&tx.db)
        .await
        .map_err(database)?;
    Ok(())
}

pub(super) async fn agent(tx: &StoreTransaction, id: Uuid) -> Result<Option<Agent>> {
    agent::Entity::find_by_id(id.to_string())
        .filter(agent::Column::Kind.eq("agent"))
        .one(&tx.db)
        .await
        .map_err(database)?
        .map(decode::agent)
        .transpose()
}

pub(super) async fn conversation(
    tx: &StoreTransaction,
    host: HostKind,
    id: &str,
) -> Result<Option<Agent>> {
    agent::Entity::find()
        .filter(agent::Column::Kind.eq("agent"))
        .filter(agent::Column::HostKind.eq(host.as_str()))
        .filter(agent::Column::HostSessionId.eq(id))
        .one(&tx.db)
        .await
        .map_err(database)?
        .map(decode::agent)
        .transpose()
}

pub(super) async fn handle(tx: &StoreTransaction, name: &str) -> Result<Option<Agent>> {
    agent::Entity::find()
        .filter(agent::Column::Kind.eq("agent"))
        .filter(agent::Column::Handle.eq(name))
        .one(&tx.db)
        .await
        .map_err(database)?
        .map(decode::agent)
        .transpose()
}

pub(super) async fn joined(tx: &StoreTransaction) -> Result<Vec<Agent>> {
    agent::Entity::find()
        .filter(agent::Column::Kind.eq("agent"))
        .filter(agent::Column::Participation.eq("joined"))
        .order_by_asc(agent::Column::JoinSeq)
        .all(&tx.db)
        .await
        .map_err(database)?
        .into_iter()
        .map(decode::agent)
        .collect()
}

pub(super) async fn save_agent(tx: &StoreTransaction, value: &Agent) -> Result<()> {
    super::actors::save(&tx.db, &value.clone().into()).await
}

pub(super) async fn session(tx: &StoreTransaction, id: Uuid) -> Result<Option<Session>> {
    session::Entity::find()
        .filter(session::Column::AgentId.eq(id.to_string()))
        .filter(session::Column::IsCurrent.eq(true))
        .one(&tx.db)
        .await
        .map_err(database)?
        .map(decode::session)
        .transpose()
}

pub(super) async fn save_session(tx: &StoreTransaction, value: &Session) -> Result<()> {
    if let Some(old) = session::Entity::find_by_id(value.incarnation.to_string())
        .one(&tx.db)
        .await
        .map_err(database)?
    {
        if old.agent_id != value.agent_id.to_string()
            || old.context_id != value.context_id.to_string()
        {
            return Err(integrity("Connection incarnation identity is immutable."));
        }
        if !old.is_current {
            return Err(agentchat_core::Error::new(
                agentchat_core::ErrorCode::IdentityConflict,
                "An obsolete connection incarnation cannot become current again.",
            ));
        }
    }
    session::Entity::update_many()
        .col_expr(session::Column::IsCurrent, Expr::value(false))
        .col_expr(session::Column::State, Expr::value("disconnected"))
        .filter(session::Column::AgentId.eq(value.agent_id.to_string()))
        .filter(session::Column::Incarnation.ne(value.incarnation.to_string()))
        .exec(&tx.db)
        .await
        .map_err(database)?;
    session::Entity::insert(session::ActiveModel {
        incarnation: Set(value.incarnation.to_string()),
        agent_id: Set(value.agent_id.to_string()),
        context_id: Set(value.context_id.to_string()),
        state: Set(value.phase.as_str().into()),
        last_heartbeat: Set(value.heartbeat_at.to_string()),
        active_turn_id: Set(value.active_turn_id.clone()),
        wake_paused: Set(value.wake_paused),
        is_current: Set(true),
    })
    .on_conflict(
        OnConflict::column(session::Column::Incarnation)
            .update_columns([
                session::Column::State,
                session::Column::LastHeartbeat,
                session::Column::ActiveTurnId,
                session::Column::WakePaused,
                session::Column::IsCurrent,
            ])
            .to_owned(),
    )
    .exec(&tx.db)
    .await
    .map_err(database)?;
    Ok(())
}

pub(super) async fn channel(tx: &StoreTransaction, name: &str) -> Result<Option<Channel>> {
    channel::Entity::find()
        .filter(channel::Column::Name.eq(name))
        .one(&tx.db)
        .await
        .map_err(database)?
        .map(decode::channel)
        .transpose()
}

pub(super) async fn channels(tx: &StoreTransaction) -> Result<Vec<Channel>> {
    channel::Entity::find()
        .order_by_asc(channel::Column::CreationSeq)
        .all(&tx.db)
        .await
        .map_err(database)?
        .into_iter()
        .map(decode::channel)
        .collect()
}

pub(super) async fn channel_policy(tx: &StoreTransaction) -> Result<Option<ProjectChannelPolicy>> {
    let rows = project_default_channel::Entity::find()
        .order_by_asc(project_default_channel::Column::Position)
        .all(&tx.db)
        .await
        .map_err(database)?;
    if rows.is_empty() {
        return Ok(None);
    }
    let retained = channels(tx)
        .await?
        .into_iter()
        .map(|channel| (channel.id.to_string(), channel.name))
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

pub(super) async fn save_channel_policy(
    tx: &StoreTransaction,
    policy: &ProjectChannelPolicy,
) -> Result<()> {
    let retained = channels(tx)
        .await?
        .into_iter()
        .map(|channel| (channel.name, channel.id))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut rows = Vec::with_capacity(policy.ordered_defaults().len());
    for (position, name) in policy.ordered_defaults().iter().enumerate() {
        let id = retained.get(name).ok_or_else(|| {
            Error::new(
                ErrorCode::IntegrityError,
                "Project policy cannot reference a missing channel.",
            )
        })?;
        rows.push(project_default_channel::ActiveModel {
            position: Set(position as i32),
            channel_id: Set(id.to_string()),
        });
    }
    project_default_channel::Entity::delete_many()
        .exec(&tx.db)
        .await
        .map_err(database)?;
    project_default_channel::Entity::insert_many(rows)
        .exec(&tx.db)
        .await
        .map_err(database)?;
    Ok(())
}

pub(super) async fn insert_channel(tx: &StoreTransaction, value: &Channel) -> Result<()> {
    channel::ActiveModel {
        id: Set(value.id.to_string()),
        name: Set(value.name.clone()),
        creator_id: Set(value.creator_id.map(|id| id.to_string())),
        creation_seq: Set(value.creation_seq.get()),
    }
    .insert(&tx.db)
    .await
    .map_err(database)?;
    Ok(())
}

pub(super) async fn add_member(
    tx: &StoreTransaction,
    c: Uuid,
    a: Uuid,
    seq: Sequence,
) -> Result<()> {
    membership::Entity::insert(membership::ActiveModel {
        channel_id: Set(c.to_string()),
        actor_id: Set(a.to_string()),
        join_seq: Set(seq.get()),
    })
    .on_conflict(
        OnConflict::columns([membership::Column::ChannelId, membership::Column::ActorId])
            .do_nothing()
            .to_owned(),
    )
    .try_insert()
    .exec(&tx.db)
    .await
    .map_err(database)?;
    Ok(())
}

pub(super) async fn is_member(tx: &StoreTransaction, channel: Uuid, agent: Uuid) -> Result<bool> {
    Ok(
        membership::Entity::find_by_id((channel.to_string(), agent.to_string()))
            .one(&tx.db)
            .await
            .map_err(database)?
            .is_some(),
    )
}

pub(super) async fn membership_boundary(
    tx: &StoreTransaction,
    channel: Uuid,
    agent: Uuid,
) -> Result<Option<Sequence>> {
    membership::Entity::find_by_id((channel.to_string(), agent.to_string()))
        .one(&tx.db)
        .await
        .map_err(database)?
        .map(|row| Sequence::new(row.join_seq))
        .transpose()
}

pub(super) async fn remove_member(tx: &StoreTransaction, channel: Uuid, agent: Uuid) -> Result<()> {
    membership::Entity::delete_by_id((channel.to_string(), agent.to_string()))
        .exec(&tx.db)
        .await
        .map_err(database)?;
    Ok(())
}

pub(super) async fn channel_member_count(tx: &StoreTransaction, channel: Uuid) -> Result<u64> {
    membership::Entity::find()
        .filter(membership::Column::ChannelId.eq(channel.to_string()))
        .count(&tx.db)
        .await
        .map_err(database)
}

pub(super) async fn channel_members(tx: &StoreTransaction, channel: Uuid) -> Result<Vec<Agent>> {
    let rows = membership::Entity::find()
        .filter(membership::Column::ChannelId.eq(channel.to_string()))
        .order_by_asc(membership::Column::JoinSeq)
        .all(&tx.db)
        .await
        .map_err(database)?;
    let mut agents = Vec::with_capacity(rows.len());
    for row in rows {
        let id = Uuid::parse_str(&row.actor_id)
            .map_err(|_| integrity("Invalid membership agent UUID."))?;
        if let Some(agent) = agent(tx, id).await?.filter(|a| a.joined) {
            agents.push(agent);
        }
    }
    Ok(agents)
}

pub(super) async fn remove_memberships(tx: &StoreTransaction, id: Uuid) -> Result<()> {
    membership::Entity::delete_many()
        .filter(membership::Column::ActorId.eq(id.to_string()))
        .exec(&tx.db)
        .await
        .map_err(database)?;
    Ok(())
}
