//! Latest/backward complete-body windows with independent root/reply cursors.

use super::*;
use crate::{
    model::{Channel, Message},
    read::{Checkpoint, MessageWindow, PinnedRoot, WindowRequest},
};

pub(super) async fn window(
    db: &mut dyn Snapshot,
    project: Uuid,
    channel: String,
    root: Option<Uuid>,
    input: WindowRequest,
) -> Result<MessageWindow> {
    let limit = input.limit()?;
    let snapshot = checkpoint::snapshot(db, project, input.snapshot).await?;
    let channel = channel_at(db, &channel, snapshot).await?;
    if let Some(root) = root {
        root_at(db, channel.id, root, snapshot).await?;
    }
    if input.before.is_some_and(|before| before > snapshot.seq) {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            "Backward cursor exceeds its snapshot.",
        ));
    }
    let candidates = db
        .message_window(channel.id, root, input.before, snapshot.seq, limit + 1)
        .await?;
    let mut result = MessageWindow {
        snapshot,
        items: Vec::new(),
        next_before: None,
    };
    let mut bytes = 512;
    let mut previous = input.before;
    for item in candidates {
        let message = &item.message.message;
        let key = message.creation_seq.cursor();
        if key > snapshot.seq
            || previous.is_some_and(|previous| key >= previous)
            || message.channel_id != channel.id
            || message.root_message_id != root
        {
            return Err(Error::new(
                ErrorCode::IntegrityError,
                "Backward window returned an unordered or unrelated record.",
            ));
        }
        let size = serde_json::to_vec(&item)
            .map_err(|_| serialization())?
            .len()
            + 1;
        if size + 512 > MAX_PAGE_BYTES {
            return Err(Error::new(
                ErrorCode::IntegrityError,
                "A stored message exceeds its finite window budget.",
            ));
        }
        if result.items.len() == limit || bytes + size > MAX_PAGE_BYTES {
            // The rejected record remains strictly before the last returned
            // key. A byte-limited page never advances to that rejected record.
            result.next_before = previous;
            break;
        }
        previous = Some(key);
        bytes += size;
        result.items.push(item);
    }
    result.items.reverse();
    Ok(result)
}

pub(super) async fn pinned(
    db: &mut dyn Snapshot,
    project: Uuid,
    channel: String,
    root: Uuid,
    supplied: Option<Checkpoint>,
) -> Result<PinnedRoot> {
    let snapshot = checkpoint::snapshot(db, project, supplied).await?;
    let channel = channel_at(db, &channel, snapshot).await?;
    root_at(db, channel.id, root, snapshot).await?;
    let mut rows = db.observed_messages(&[root], snapshot.seq).await?;
    let root = rows.pop().ok_or_else(|| {
        Error::new(
            ErrorCode::IntegrityError,
            "Pinned root projection is missing.",
        )
    })?;
    Ok(PinnedRoot { snapshot, root })
}

async fn channel_at(db: &mut dyn Snapshot, name: &str, snapshot: Checkpoint) -> Result<Channel> {
    db.channel(name)
        .await?
        .filter(|c| c.creation_seq.cursor() <= snapshot.seq)
        .ok_or_else(|| {
            Error::new(
                ErrorCode::NotFound,
                "Channel does not exist at this snapshot.",
            )
        })
}

async fn root_at(
    db: &mut dyn Snapshot,
    channel: Uuid,
    root: Uuid,
    snapshot: Checkpoint,
) -> Result<Message> {
    db.message(root)
        .await?
        .filter(|m| {
            m.root_message_id.is_none()
                && m.channel_id == channel
                && m.creation_seq.cursor() <= snapshot.seq
        })
        .ok_or_else(|| {
            Error::new(
                ErrorCode::InvalidThread,
                "Thread must be a retained root in the selected channel and snapshot.",
            )
        })
}
