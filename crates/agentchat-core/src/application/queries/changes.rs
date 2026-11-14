//! Retained replay: finite scans, complete projections and explicit safe progress.

use super::*;
use crate::{
    model::Event,
    read::{ChangeBatch, ChangeRequest, MAX_CHANGE_SCAN, ViewerChange, ViewerChangeKind},
};
use std::collections::BTreeMap;

pub(super) async fn changes(
    db: &mut dyn Snapshot,
    project: Uuid,
    input: ChangeRequest,
) -> Result<ChangeBatch> {
    let limit = input.limit()?;
    let after = checkpoint::validate(db, project, input.after).await?;
    let head = checkpoint::snapshot(db, project, input.through).await?;
    if after.seq > head.seq {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            "Change cursor exceeds its captured catch-up head.",
        ));
    }
    if after == head {
        return Ok(ChangeBatch {
            after,
            scan: after,
            head,
            items: Vec::new(),
            has_more: false,
        });
    }
    let events = db.events(after.seq, MAX_CHANGE_SCAN + 1, None).await?;
    if events.len() > MAX_CHANGE_SCAN + 1 {
        return Err(integrity("Unbounded change scan."));
    }
    let events = events
        .into_iter()
        .take_while(|e| e.seq.cursor() <= head.seq)
        .take(MAX_CHANGE_SCAN)
        .collect::<Vec<_>>();
    let message_ids = events
        .iter()
        .filter(|e| e.kind.as_str() == "message.created")
        .map(|e| {
            e.message_id
                .ok_or_else(|| integrity("Message event is missing its identity."))
        })
        .collect::<Result<Vec<_>>>()?;
    let channel_ids = events
        .iter()
        .filter(|e| e.kind.as_str() == "channel.created")
        .map(|e| {
            e.channel_id
                .ok_or_else(|| integrity("Channel event is missing its identity."))
        })
        .collect::<Result<Vec<_>>>()?;
    let messages = db
        .observed_messages(&message_ids, head.seq)
        .await?
        .into_iter()
        .map(|m| (m.message.message.id, m))
        .collect::<BTreeMap<_, _>>();
    let channels = db
        .channels_by_ids(&channel_ids)
        .await?
        .into_iter()
        .map(|c| (c.id, c))
        .collect::<BTreeMap<_, _>>();
    let mut result = ChangeBatch {
        after,
        scan: after,
        head,
        items: Vec::new(),
        has_more: false,
    };
    let mut bytes = 1024;
    for event in events {
        // The shared causal allocator also orders Dispatches. Gaps in the audit
        // log are legitimate; backwards or repeated audit rows are not.
        if event.seq.cursor() <= result.scan.seq || event.seq.cursor() > head.seq {
            return Err(checkpoint::invalidated());
        }
        let change = match event.kind.as_str() {
            "message.created" => {
                let message = messages
                    .get(
                        &event
                            .message_id
                            .ok_or_else(|| integrity("Message identity is missing."))?,
                    )
                    .cloned()
                    .ok_or_else(|| integrity("Retained change message is missing."))?;
                if message.message.message.creation_seq >= event.seq
                    || Some(message.message.message.channel_id) != event.channel_id
                {
                    return Err(integrity(
                        "Retained change message disagrees with its event.",
                    ));
                }
                Some(ViewerChangeKind::MessageCreated { message })
            }
            "channel.created" => {
                let channel = channels
                    .get(
                        &event
                            .channel_id
                            .ok_or_else(|| integrity("Channel identity is missing."))?,
                    )
                    .cloned()
                    .ok_or_else(|| integrity("Retained change channel is missing."))?;
                if channel.creation_seq != event.seq {
                    return Err(integrity("Retained channel disagrees with its event."));
                }
                Some(ViewerChangeKind::ChannelCreated { channel })
            }
            _ => None,
        };
        if let Some(change) = change {
            let item = ViewerChange {
                event_id: event.event_id,
                seq: event.seq,
                change,
            };
            let size = serde_json::to_vec(&item)
                .map_err(|_| serialization())?
                .len()
                + 1;
            if size + 1024 > MAX_PAGE_BYTES {
                return Err(integrity(
                    "A retained change exceeds its finite batch budget.",
                ));
            }
            if result.items.len() == limit || bytes + size > MAX_PAGE_BYTES {
                break;
            }
            bytes += size;
            result.items.push(item);
        }
        result.scan = event_checkpoint(project, &event);
    }
    result.has_more = result.scan.seq < head.seq;
    if result.has_more && result.scan == after {
        return Err(checkpoint::invalidated());
    }
    Ok(result)
}

fn event_checkpoint(project_id: Uuid, event: &Event) -> crate::read::Checkpoint {
    crate::read::Checkpoint {
        project_id,
        seq: event.seq.cursor(),
        event_id: Some(event.event_id),
    }
}
fn integrity(message: &str) -> Error {
    Error::new(ErrorCode::IntegrityError, message)
}
