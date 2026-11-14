//! Bounded immutable windows and batched metadata/counts in one WAL snapshot.

use agentchat_core::{
    Result,
    model::{Channel, Message},
    read::{IdentityLabel, MessageView, ObservedMessage},
    sequence::Cursor,
};
use sea_orm::{
    ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QuerySelect,
    sea_query::{Alias, Expr, ExprTrait, Func, Order, Query},
};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

use super::{ReadSnapshot, fetch_limit};
use crate::{
    decode,
    entities::{actor as agent, channel, dispatch, event, message},
    error::{database, integrity},
};

pub(super) async fn event_identity(s: &ReadSnapshot, seq: Cursor) -> Result<Option<Uuid>> {
    if seq == Cursor::START || seq > s.head {
        return Ok(None);
    }
    event::Entity::find()
        .select_only()
        .column(event::Column::EventId)
        .filter(event::Column::Seq.eq(seq.get()))
        .filter(event::Column::Sealed.eq(true))
        .into_tuple::<String>()
        .one(&s.db)
        .await
        .map_err(database)?
        .map(|id| decode::uuid(&id))
        .transpose()
}

pub(super) async fn window(
    s: &ReadSnapshot,
    channel_id: Uuid,
    root: Option<Uuid>,
    before: Option<Cursor>,
    through: Cursor,
    limit: usize,
) -> Result<Vec<ObservedMessage>> {
    upper(s, through)?;
    let mut query = crate::dispatches::message_query();
    query
        .and_where(
            Expr::col((dispatch::Entity, dispatch::Column::ChannelId)).eq(channel_id.to_string()),
        )
        .and_where(Expr::col((dispatch::Entity, dispatch::Column::CreationSeq)).lte(through.get()));
    match root {
        Some(root) => {
            query.and_where(
                Expr::col((message::Entity, message::Column::RootMessageId)).eq(root.to_string()),
            );
        }
        None => {
            query.and_where(Expr::col((message::Entity, message::Column::RootMessageId)).is_null());
        }
    }
    if let Some(before) = before {
        query.and_where(
            Expr::col((dispatch::Entity, dispatch::Column::CreationSeq)).lt(before.get()),
        );
    }
    query
        .order_by(
            (dispatch::Entity, dispatch::Column::CreationSeq),
            Order::Desc,
        )
        .limit(fetch_limit(limit)?);
    let messages = crate::dispatches::messages(&s.db, &query).await?;
    project(s, messages, through).await
}

pub(super) async fn by_ids(
    s: &ReadSnapshot,
    ids: &[Uuid],
    through: Cursor,
) -> Result<Vec<ObservedMessage>> {
    upper(s, through)?;
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    fetch_limit(ids.len())?;
    let query = crate::dispatches::message_query()
        .and_where(
            Expr::col((dispatch::Entity, dispatch::Column::Id))
                .is_in(ids.iter().map(Uuid::to_string)),
        )
        .and_where(Expr::col((dispatch::Entity, dispatch::Column::CreationSeq)).lte(through.get()))
        .to_owned();
    let messages = crate::dispatches::messages(&s.db, &query).await?;
    project(s, messages, through).await
}

pub(super) async fn channels(s: &ReadSnapshot, ids: &[Uuid]) -> Result<Vec<Channel>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    fetch_limit(ids.len())?;
    channel::Entity::find()
        .filter(channel::Column::Id.is_in(ids.iter().map(Uuid::to_string)))
        .all(&s.db)
        .await
        .map_err(database)?
        .into_iter()
        .map(decode::channel)
        .collect()
}

async fn project(
    s: &ReadSnapshot,
    messages: Vec<Message>,
    through: Cursor,
) -> Result<Vec<ObservedMessage>> {
    if messages.is_empty() {
        return Ok(Vec::new());
    }
    fetch_limit(messages.len())?;
    let authors = messages
        .iter()
        .map(|m| m.author_id)
        .collect::<BTreeSet<_>>();
    let channels = messages
        .iter()
        .map(|m| m.channel_id)
        .collect::<BTreeSet<_>>();
    let roots = messages
        .iter()
        .map(Message::thread_id)
        .collect::<BTreeSet<_>>();
    // At most 201 selected messages imply at most 201 keys for each query.
    // No author, channel or reply aggregate is fetched once per message.
    let authors = agent::Entity::find()
        .filter(agent::Column::Id.is_in(authors.iter().map(Uuid::to_string)))
        .all(&s.db)
        .await
        .map_err(database)?
        .into_iter()
        .map(|row| {
            let actor = decode::actor(row)?;
            let id = actor.actor_id;
            Ok((
                id,
                IdentityLabel {
                    actor_id: id,
                    kind: actor.kind(),
                    handle: actor.handle,
                },
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let channels = channel::Entity::find()
        .filter(channel::Column::Id.is_in(channels.iter().map(Uuid::to_string)))
        .all(&s.db)
        .await
        .map_err(database)?
        .into_iter()
        .map(|row| Ok((decode::uuid(&row.id)?, row.name)))
        .collect::<Result<BTreeMap<_, _>>>()?;
    let counts = reply_counts(s, &roots, through).await?;
    messages
        .into_iter()
        .map(|message| {
            let thread_id = message.thread_id();
            Ok(ObservedMessage {
                reply_count: counts.get(&thread_id).copied().unwrap_or(0),
                message: MessageView {
                    author: authors
                        .get(&message.author_id)
                        .cloned()
                        .ok_or_else(|| integrity("Retained message author is missing."))?,
                    channel: channels
                        .get(&message.channel_id)
                        .cloned()
                        .ok_or_else(|| integrity("Retained message channel is missing."))?,
                    thread_id,
                    message,
                },
            })
        })
        .collect()
}

async fn reply_counts(
    s: &ReadSnapshot,
    roots: &BTreeSet<Uuid>,
    through: Cursor,
) -> Result<BTreeMap<Uuid, u64>> {
    let query = Query::select()
        .column(message::Column::RootMessageId)
        .expr_as(
            Func::count(Expr::col((message::Entity, message::Column::DispatchId))),
            Alias::new("reply_count"),
        )
        .from(message::Entity)
        .inner_join(
            dispatch::Entity,
            Expr::col((dispatch::Entity, dispatch::Column::Id))
                .equals((message::Entity, message::Column::DispatchId)),
        )
        .and_where(Expr::col((dispatch::Entity, dispatch::Column::Kind)).eq("message"))
        .and_where(Expr::col((dispatch::Entity, dispatch::Column::Sealed)).eq(true))
        .and_where(message::Column::RootMessageId.is_in(roots.iter().map(Uuid::to_string)))
        .and_where(Expr::col((dispatch::Entity, dispatch::Column::CreationSeq)).lte(through.get()))
        .group_by_col(message::Column::RootMessageId)
        .to_owned();
    s.db.query_all(&query)
        .await
        .map_err(database)?
        .into_iter()
        .map(|row| {
            let id: String = row.try_get("", "root_message_id").map_err(database)?;
            let count: i64 = row.try_get("", "reply_count").map_err(database)?;
            Ok((
                decode::uuid(&id)?,
                u64::try_from(count).map_err(|_| integrity("Invalid reply count aggregate."))?,
            ))
        })
        .collect()
}

fn upper(s: &ReadSnapshot, through: Cursor) -> Result<()> {
    if through > s.head {
        return Err(integrity(
            "Viewer projection exceeds its captured snapshot.",
        ));
    }
    Ok(())
}
