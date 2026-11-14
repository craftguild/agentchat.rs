//! Parameter-bound literal search and sparse receipt retrieval through SeaORM.

use super::{ReadSnapshot, fetch_limit};
use crate::{
    decode,
    entities::{dispatch, message, receipt},
    error::database,
};
use agentchat_core::{
    Result,
    model::{Message, Receipt},
    read::MessageFilter,
    sequence::Cursor,
};
use sea_orm::{
    EntityTrait,
    sea_query::{Alias, Expr, ExprTrait, Func, Order},
};
use uuid::Uuid;

pub(super) async fn message(s: &ReadSnapshot, id: Uuid) -> Result<Option<Message>> {
    crate::dispatches::message(&s.db, id).await
}
pub(super) async fn receipt(
    s: &ReadSnapshot,
    message: Uuid,
    agent: Uuid,
) -> Result<Option<Receipt>> {
    receipt::Entity::find_by_id((
        message.to_string(),
        agent.to_string(),
        "explicit".to_owned(),
    ))
    .one(&s.db)
    .await
    .map_err(database)?
    .map(decode::receipt)
    .transpose()
}
pub(super) async fn list(
    s: &ReadSnapshot,
    filter: &MessageFilter,
    after: Cursor,
    limit: usize,
) -> Result<Vec<Message>> {
    let mut query = crate::dispatches::message_query();
    query
        .and_where(Expr::col((dispatch::Entity, dispatch::Column::CreationSeq)).gt(after.get()))
        .and_where(Expr::col((dispatch::Entity, dispatch::Column::CreationSeq)).lte(s.head.get()));
    if let Some(channel) = filter.channel {
        query.and_where(
            Expr::col((dispatch::Entity, dispatch::Column::ChannelId)).eq(channel.to_string()),
        );
    }
    if let Some(channels) = &filter.channels {
        query.and_where(
            Expr::col((dispatch::Entity, dispatch::Column::ChannelId))
                .is_in(channels.iter().map(Uuid::to_string)),
        );
    }
    if let Some(thread) = filter.thread {
        query.and_where(
            Expr::col((message::Entity, message::Column::DispatchId))
                .eq(thread.to_string())
                .or(Expr::col((message::Entity, message::Column::RootMessageId))
                    .eq(thread.to_string())),
        );
    }
    if let Some(text) = &filter.query {
        query.and_where(
            Expr::expr(Func::cust(Alias::new("instr")).args([
                Expr::col((message::Entity, message::Column::Body)),
                Expr::value(text.clone()),
            ]))
            .gt(0),
        );
    }
    query
        .order_by(
            (dispatch::Entity, dispatch::Column::CreationSeq),
            Order::Asc,
        )
        .limit(fetch_limit(limit)?);
    crate::dispatches::messages(&s.db, &query).await
}
