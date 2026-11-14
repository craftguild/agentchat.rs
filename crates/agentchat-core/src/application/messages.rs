//! Immutable message/mention and independent explicit-read transaction policy.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};
use uuid::Uuid;

use super::work::{Work, not_found};
use crate::{
    Error, ErrorCode, Result,
    model::{DispatchKind, Mention, Message, ReadKind, Receipt},
};

pub(super) async fn post(
    work: &mut Work<'_>,
    actor: Uuid,
    channel_name: Option<&str>,
    root: Option<Uuid>,
    body: &str,
) -> Result<Value> {
    let channel = if let Some(root) = root {
        let message =
            work.tx.message(root).await?.ok_or_else(|| {
                Error::new(ErrorCode::InvalidThread, "Thread root does not exist.")
            })?;
        if message.root_message_id.is_some() {
            return Err(Error::new(
                ErrorCode::InvalidThread,
                "A reply must reference a root directly.",
            ));
        }
        message.channel_id
    } else {
        let fallback;
        let channel_name = if let Some(channel) = channel_name {
            channel
        } else {
            fallback = work
                .tx
                .channel_policy()
                .await?
                .unwrap_or_else(crate::project::ProjectChannelPolicy::legacy);
            fallback.primary()
        };
        work.tx
            .channel(channel_name)
            .await?
            .ok_or_else(|| not_found("Channel does not exist."))?
            .id
    };
    if !work.tx.is_member(channel, actor).await? {
        return Err(Error::new(
            ErrorCode::NotJoined,
            "Join this channel before posting to it.",
        ));
    }
    // Validate and hash the submitted arguments before this action so retained
    // request receipts keep their identity. Only new stored bodies are stripped;
    // mentions, previews and byte counts must use that same immutable text.
    let body = body.trim();
    let resolution = super::message_mentions::resolve(work, actor, channel, body).await?;
    let recipients = resolution.recipients;
    // The Header has its own committed order. Its later audit occurrence makes
    // the content visible beneath an observer snapshot's audit head.
    let dispatch = work
        .tx
        .allocate_dispatch(DispatchKind::Message, Some(channel), work.now)
        .await?;
    let id = dispatch.id;
    let thread = root.unwrap_or(id);
    let message = Message {
        id,
        channel_id: channel,
        author_id: actor,
        root_message_id: root,
        body: body.into(),
        created_at: work.now,
        creation_seq: dispatch.creation_seq,
    };
    let mut event = work.allocate("message.created").await?;
    let wake: BTreeSet<_> = recipients.keys().copied().collect();
    let target_count = work.send_message(&dispatch, &message, &wake).await?;
    for (recipient, sources) in &recipients {
        work.tx
            .insert_mention(&Mention {
                message_id: id,
                recipient_id: *recipient,
                sources: sources.clone(),
            })
            .await?;
    }
    event.actor_id = Some(actor);
    event.channel_id = Some(channel);
    event.message_id = Some(id);
    event.data = json!({"thread_id":thread,"mentioned_agent_ids":wake,"preview":body.chars().take(512).collect::<String>(),"body_bytes":body.len(),"mention_results":resolution.individuals,"warnings":resolution.warnings});
    work.tx.publish_event(&event).await?;
    Ok(
        json!({"message_id":id,"thread_id":thread,"seq":event.seq,"recipient_count":target_count,"mentioned_agent_ids":wake,"mention_results":resolution.individuals,"warnings":resolution.warnings}),
    )
}

pub(super) async fn read(work: &mut Work<'_>, actor: Uuid, ids: &[Uuid]) -> Result<Value> {
    // Validate the entire set before the first receipt: an invalid last ID must
    // not leave earlier IDs read or emit a partial read event.
    let mut messages = BTreeMap::new();
    for message in work.tx.messages(ids).await? {
        let id = message.id;
        let channel = message.channel_id;
        if !ids.contains(&id) || messages.insert(id, channel).is_some() {
            return Err(Error::new(
                ErrorCode::IntegrityError,
                "Read lookup returned an unrelated or repeated message.",
            ));
        }
        if !work.tx.is_member(channel, actor).await? {
            return Err(Error::new(
                ErrorCode::NotJoined,
                "Join every referenced message channel before marking messages read.",
            ));
        }
    }
    for id in ids {
        if !messages.contains_key(id) {
            return Err(not_found("A requested message does not exist."));
        }
    }
    let mut receipts = BTreeSet::new();
    for receipt in work.tx.receipts(actor, ids).await? {
        if receipt.actor_id != actor
            || !ids.contains(&receipt.message_id)
            || !receipts.insert(receipt.message_id)
        {
            return Err(Error::new(
                ErrorCode::IntegrityError,
                "Read lookup returned an unrelated or repeated receipt.",
            ));
        }
    }
    let mut changed = Vec::new();
    let mut already = Vec::new();
    for id in ids {
        if receipts.contains(id) {
            already.push(*id);
        } else {
            changed.push(*id);
        }
    }
    if !changed.is_empty() {
        let receipts = changed
            .iter()
            .map(|id| Receipt {
                message_id: *id,
                actor_id: actor,
                kind: ReadKind::Explicit,
                first_read_at: work.now,
            })
            .collect::<Vec<_>>();
        work.tx.insert_receipts(&receipts).await?;
    }
    Ok(json!({"changed":changed,"already_read":already}))
}
