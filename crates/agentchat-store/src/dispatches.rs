//! Validated Header/subtype loading and Message-only relational projections.
use crate::{
    decode,
    entities::{dispatch, message, notice},
    error::{database, integrity},
};
use agentchat_core::{
    Result,
    model::{Dispatch, DispatchKind, DispatchNotice, DispatchPayload, Message},
};
use sea_orm::{
    ColumnTrait, ConnectionTrait, EntityTrait, FromQueryResult, QueryFilter,
    sea_query::{Alias, Expr, ExprTrait, JoinType, Query, SelectStatement},
};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

pub(crate) async fn get<C: ConnectionTrait>(db: &C, id: Uuid) -> Result<Option<Dispatch>> {
    let Some(row) = dispatch::Entity::find_by_id(id.to_string())
        .filter(dispatch::Column::Sealed.eq(true))
        .one(db)
        .await
        .map_err(database)?
    else {
        return Ok(None);
    };
    let header = decode::dispatch(row)?;
    payload(db, &header).await?;
    Ok(Some(header))
}

pub(crate) async fn payload<C: ConnectionTrait>(
    db: &C,
    header: &Dispatch,
) -> Result<DispatchPayload> {
    let message = message::Entity::find_by_id(header.id.to_string())
        .one(db)
        .await
        .map_err(database)?;
    let notice = notice::Entity::find_by_id(header.id.to_string())
        .one(db)
        .await
        .map_err(database)?;
    let value = match (header.kind, message, notice) {
        (DispatchKind::Message, Some(row), None) => {
            DispatchPayload::Message(decode::message(header, row)?)
        }
        (DispatchKind::Notice, None, Some(row)) => DispatchPayload::Notice(decode::notice(row)?),
        _ => {
            return Err(integrity(
                "Dispatch has missing, multiple or mismatched payload subtypes.",
            ));
        }
    };
    header.validate_payload(&value)?;
    Ok(value)
}

pub(crate) async fn notice<C: ConnectionTrait>(db: &C, id: Uuid) -> Result<Option<DispatchNotice>> {
    let Some(header) = get(db, id).await? else {
        return Ok(None);
    };
    if header.kind != DispatchKind::Notice {
        return Ok(None);
    }
    match payload(db, &header).await? {
        DispatchPayload::Notice(value) => Ok(Some(value)),
        _ => Err(integrity(
            "Notice projection has a different Dispatch kind.",
        )),
    }
}

/// Join only Message subtypes and detect a second, invalid Notice payload.
pub(crate) fn message_query() -> SelectStatement {
    let mut query = Query::select();
    query
        .from(message::Entity)
        .inner_join(
            dispatch::Entity,
            Expr::col((dispatch::Entity, dispatch::Column::Id))
                .equals((message::Entity, message::Column::DispatchId)),
        )
        .join(
            JoinType::LeftJoin,
            notice::Entity,
            Expr::col((notice::Entity, notice::Column::DispatchId))
                .equals((dispatch::Entity, dispatch::Column::Id)),
        )
        .column((dispatch::Entity, dispatch::Column::Id))
        .column((dispatch::Entity, dispatch::Column::ChannelId))
        .column((dispatch::Entity, dispatch::Column::CreatedAt))
        .column((dispatch::Entity, dispatch::Column::CreationSeq))
        .column((message::Entity, message::Column::AuthorId))
        .column((message::Entity, message::Column::RootMessageId))
        .column((message::Entity, message::Column::Body))
        .expr_as(
            Expr::col((notice::Entity, notice::Column::DispatchId)),
            Alias::new("invalid_notice"),
        )
        .and_where(Expr::col((dispatch::Entity, dispatch::Column::Kind)).eq("message"))
        .and_where(Expr::col((dispatch::Entity, dispatch::Column::Sealed)).eq(true));
    query.to_owned()
}

#[derive(FromQueryResult)]
struct MessageRow {
    id: String,
    channel_id: Option<String>,
    created_at: String,
    creation_seq: i64,
    author_id: String,
    root_message_id: Option<String>,
    body: String,
    invalid_notice: Option<String>,
}

pub(crate) async fn messages<C: ConnectionTrait>(
    db: &C,
    query: &SelectStatement,
) -> Result<Vec<Message>> {
    db.query_all(query)
        .await
        .map_err(database)?
        .into_iter()
        .map(|row| {
            let row = MessageRow::from_query_result(&row, "").map_err(database)?;
            if row.invalid_notice.is_some() {
                return Err(integrity("Message Dispatch has a second payload subtype."));
            }
            let header = Dispatch {
                id: decode::uuid(&row.id)?,
                kind: DispatchKind::Message,
                channel_id: row.channel_id.as_deref().map(decode::uuid).transpose()?,
                created_at: decode::time(&row.created_at)?,
                creation_seq: decode::seq(row.creation_seq)?,
            };
            decode::message(
                &header,
                message::Model {
                    dispatch_id: row.id,
                    author_id: row.author_id,
                    root_message_id: row.root_message_id,
                    body: row.body,
                },
            )
        })
        .collect()
}

pub(crate) async fn message<C: ConnectionTrait>(db: &C, id: Uuid) -> Result<Option<Message>> {
    let query = message_query()
        .and_where(Expr::col((dispatch::Entity, dispatch::Column::Id)).eq(id.to_string()))
        .limit(1)
        .to_owned();
    Ok(messages(db, &query).await?.into_iter().next())
}

/// Three bounded queries hydrate both kinds without per-recipient payload reads.
pub(crate) async fn hydrate<C: ConnectionTrait>(
    db: &C,
    ids: &[Uuid],
) -> Result<BTreeMap<Uuid, (Dispatch, DispatchPayload)>> {
    let ids = ids.iter().copied().collect::<BTreeSet<_>>();
    if ids.len() > 201 {
        return Err(integrity("Dispatch hydration exceeds its bounded page."));
    }
    if ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let values = ids.iter().map(Uuid::to_string).collect::<Vec<_>>();
    let headers = dispatch::Entity::find()
        .filter(dispatch::Column::Id.is_in(values.clone()))
        .filter(dispatch::Column::Sealed.eq(true))
        .all(db)
        .await
        .map_err(database)?;
    if headers.len() != ids.len() {
        return Err(integrity("Selected Dispatch is absent or unpublished."));
    }
    let mut messages = message::Entity::find()
        .filter(message::Column::DispatchId.is_in(values.clone()))
        .all(db)
        .await
        .map_err(database)?
        .into_iter()
        .map(|r| (r.dispatch_id.clone(), r))
        .collect::<BTreeMap<_, _>>();
    let mut notices = notice::Entity::find()
        .filter(notice::Column::DispatchId.is_in(values))
        .all(db)
        .await
        .map_err(database)?
        .into_iter()
        .map(|r| (r.dispatch_id.clone(), r))
        .collect::<BTreeMap<_, _>>();
    headers
        .into_iter()
        .map(|row| {
            let header = decode::dispatch(row)?;
            let id = header.id.to_string();
            let payload = match (header.kind, messages.remove(&id), notices.remove(&id)) {
                (DispatchKind::Message, Some(row), None) => {
                    DispatchPayload::Message(decode::message(&header, row)?)
                }
                (DispatchKind::Notice, None, Some(row)) => {
                    DispatchPayload::Notice(decode::notice(row)?)
                }
                _ => {
                    return Err(integrity(
                        "Selected Dispatch has invalid payload ownership.",
                    ));
                }
            };
            header.validate_payload(&payload)?;
            Ok((header.id, (header, payload)))
        })
        .collect()
}
