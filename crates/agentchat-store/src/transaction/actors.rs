//! Common actor persistence; native authority remains outside storage.

use agentchat_core::{
    Result,
    model::{Actor, ActorIdentity},
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, QueryTrait, Set,
};
use uuid::Uuid;

use crate::{
    decode,
    entities::{actor, membership},
    error::{database, integrity},
};

pub(crate) async fn get<C: ConnectionTrait>(db: &C, id: Uuid) -> Result<Option<Actor>> {
    actor::Entity::find_by_id(id.to_string())
        .one(db)
        .await
        .map_err(database)?
        .map(decode::actor)
        .transpose()
}

pub(crate) async fn user<C: ConnectionTrait>(db: &C) -> Result<Option<Actor>> {
    actor::Entity::find()
        .filter(actor::Column::Kind.eq("user"))
        .one(db)
        .await
        .map_err(database)?
        .map(decode::actor)
        .transpose()
}

pub(crate) async fn joined<C: ConnectionTrait>(
    db: &C,
    channel: Option<Uuid>,
) -> Result<Vec<Actor>> {
    let mut query = actor::Entity::find().filter(actor::Column::Participation.eq("joined"));
    if let Some(channel) = channel {
        query = query.filter(
            actor::Column::Id.in_subquery(
                membership::Entity::find()
                    .select_only()
                    .column(membership::Column::ActorId)
                    .filter(membership::Column::ChannelId.eq(channel.to_string()))
                    .into_query(),
            ),
        );
    }
    query
        .order_by_asc(actor::Column::JoinSeq)
        .all(db)
        .await
        .map_err(database)?
        .into_iter()
        .map(decode::actor)
        .collect()
}

pub(crate) async fn save<C: ConnectionTrait>(db: &C, value: &Actor) -> Result<()> {
    if let Some(old) = get(db, value.actor_id).await?
        && (old.identity != value.identity
            || old.handle != value.handle
            || old.first_join_at != value.first_join_at)
    {
        return Err(integrity("Retained actor identity fields are immutable."));
    }
    let (kind, host, chat) = match &value.identity {
        ActorIdentity::Agent { host, chat_id } => (
            "agent",
            Some(host.as_str().to_owned()),
            Some(chat_id.clone()),
        ),
        ActorIdentity::User => ("user", None, None),
    };
    let row = actor::ActiveModel {
        id: Set(value.actor_id.to_string()),
        kind: Set(kind.into()),
        host_kind: Set(host),
        host_session_id: Set(chat),
        handle: Set(value.handle.clone()),
        participation: Set(if value.joined { "joined" } else { "left" }.into()),
        first_join_at: Set(value.first_join_at.to_string()),
        join_seq: Set(value.join_seq.get()),
        leave_seq: Set(value.leave_seq.map(|s| s.get())),
        // Leave cancellation is a separate monotonic private boundary; ordinary
        // participation updates, including rejoin, never overwrite it.
        ..Default::default()
    };
    if get(db, value.actor_id).await?.is_some() {
        row.update(db).await.map_err(database)?;
    } else {
        row.insert(db).await.map_err(database)?;
    }
    Ok(())
}
