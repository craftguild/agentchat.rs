//! Complete immutable message views, bounded receipts and explicit inbox reads.

use super::channel_id;
use crate::{
    Error, ErrorCode, Result,
    model::Message,
    read::{
        IdentityLabel, InboxView, MessageDetail, MessageFilter, MessageView, Page, PageBuilder,
        PageRequest, Snapshot,
    },
};
use std::collections::BTreeMap;
use uuid::Uuid;

// A finite page visits at most 201 candidates. Keep only immutable display
// names in this request-scoped cache; receipt/viewer state is always read from
// the same snapshot and no cache survives the next page or operation.
#[derive(Default)]
struct Names {
    authors: BTreeMap<Uuid, IdentityLabel>,
    channels: BTreeMap<Uuid, String>,
}

async fn view(
    db: &mut dyn Snapshot,
    message: Message,
    _viewer: Option<Uuid>,
    names: &mut Names,
) -> Result<MessageView> {
    if !names.authors.contains_key(&message.author_id) {
        let author = db
            .actor(message.author_id)
            .await?
            .ok_or_else(|| integrity("Retained message author is missing."))?;
        names.authors.insert(
            author.actor_id,
            IdentityLabel {
                actor_id: author.actor_id,
                kind: author.kind(),
                handle: author.handle,
            },
        );
    }
    if !names.channels.contains_key(&message.channel_id) {
        let channel = db
            .channel_by_id(message.channel_id)
            .await?
            .ok_or_else(|| integrity("Retained message channel is missing."))?;
        names.channels.insert(channel.id, channel.name);
    }
    Ok(MessageView {
        thread_id: message.thread_id(),
        author: names.authors[&message.author_id].clone(),
        channel: names.channels[&message.channel_id].clone(),
        message,
    })
}

pub(super) async fn detail(
    db: &mut dyn Snapshot,
    viewer: Option<Uuid>,
    allowed: Option<&[Uuid]>,
    id: Uuid,
    receipts: PageRequest,
) -> Result<MessageDetail> {
    let message = db
        .message(id)
        .await?
        .ok_or_else(|| Error::new(ErrorCode::NotFound, "Message does not exist."))?;
    require_channel(allowed, message.channel_id)?;
    let mut page = PageBuilder::new(receipts, db.head())?;
    for row in db
        .recipient_receipts(id, receipts.after, receipts.limit()? + 1)
        .await?
    {
        if !page.push(row.key.cursor(), row.item)? {
            break;
        }
    }
    Ok(MessageDetail {
        message: view(db, message, viewer, &mut Names::default()).await?,
        receipts: page.finish(),
    })
}

pub(super) async fn history(
    db: &mut dyn Snapshot,
    viewer: Option<Uuid>,
    allowed: Option<&[Uuid]>,
    channel: Option<String>,
    thread: Option<Uuid>,
    query: Option<String>,
    input: PageRequest,
) -> Result<Page<MessageView>> {
    let mut filter = MessageFilter {
        channel: None,
        channels: allowed.map(<[Uuid]>::to_vec),
        thread,
        query,
    };
    if let Some(channel) = channel {
        let channel = channel_id(db, &channel).await?;
        require_channel(allowed, channel)?;
        filter.channel = Some(channel);
    }
    if let Some(thread) = thread {
        let root = db
            .message(thread)
            .await?
            .ok_or_else(|| Error::new(ErrorCode::InvalidThread, "Thread root does not exist."))?;
        if root.root_message_id.is_some()
            || filter
                .channel
                .is_some_and(|channel| channel != root.channel_id)
        {
            return Err(Error::new(
                ErrorCode::InvalidThread,
                "Thread must be a root in the selected channel.",
            ));
        }
        require_channel(allowed, root.channel_id)?;
    }
    let mut page = PageBuilder::new(input, db.head())?;
    let mut names = Names::default();
    for message in db
        .messages(&filter, input.after, input.limit()? + 1)
        .await?
    {
        let key = message.creation_seq.cursor();
        if !page.push(key, view(db, message, viewer, &mut names).await?)? {
            break;
        }
    }
    Ok(page.finish())
}

fn require_channel(allowed: Option<&[Uuid]>, channel: Uuid) -> Result<()> {
    if allowed.is_some_and(|channels| !channels.contains(&channel)) {
        return Err(Error::new(
            ErrorCode::NotJoined,
            "Join this channel before reading its content.",
        ));
    }
    Ok(())
}

pub(super) async fn inbox(
    db: &mut dyn Snapshot,
    recipient: Uuid,
    input: PageRequest,
) -> Result<Page<InboxView>> {
    let mut page = PageBuilder::new(input, db.dispatch_head())?;
    let mut names = Names::default();
    for row in db.inbox(recipient, input.after, input.limit()? + 1).await? {
        if row.delivery.dispatch_id != row.dispatch.id {
            return Err(integrity("Inbox delivery belongs to another Dispatch."));
        }
        let message = if row.dispatch.kind == crate::model::DispatchKind::Message {
            if row.notice.is_some() {
                return Err(integrity("Message inbox row contains a Notice subtype."));
            }
            let message = db
                .message(row.dispatch.id)
                .await?
                .ok_or_else(|| integrity("Inbox message is missing."))?;
            row.dispatch
                .validate_payload(&crate::model::DispatchPayload::Message(message.clone()))?;
            Some(view(db, message, Some(recipient), &mut names).await?)
        } else {
            let notice = row
                .notice
                .as_ref()
                .ok_or_else(|| integrity("Inbox Notice payload is missing."))?;
            row.dispatch
                .validate_payload(&crate::model::DispatchPayload::Notice(notice.clone()))?;
            None
        };
        let key = row.dispatch.creation_seq.cursor();
        if !page.push(
            key,
            InboxView {
                delivery: row.delivery,
                dispatch: row.dispatch,
                notice: row.notice,
                message,
                batch_state: row.batch_state,
            },
        )? {
            break;
        }
    }
    Ok(page.finish())
}
fn integrity(message: &str) -> Error {
    Error::new(ErrorCode::IntegrityError, message)
}
