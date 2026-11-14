//! Bounded processing expiry and one author notice per original message.
use super::work::Work;
use crate::{
    Result,
    model::{ActorKind, DispatchKind},
    read::IdentityLabel,
};
use serde_json::json;
use std::collections::BTreeSet;
use uuid::Uuid;

pub(super) async fn expire(work: &mut Work<'_>) -> Result<usize> {
    let mut messages = BTreeSet::new();
    // A single bounded ordinary maintenance pass shares the serialized writer
    // with completion. Pending time and not-yet-expired recipients never enter.
    for mut delivery in work.tx.due_deliveries(work.now).await? {
        if delivery.expire(work.now)? {
            work.tx.save_delivery(&delivery).await?;
            let dispatch = work
                .tx
                .dispatch(delivery.dispatch_id)
                .await?
                .ok_or_else(|| integrity("Delivery Dispatch is missing."))?;
            if dispatch.kind == DispatchKind::Message {
                messages.insert(dispatch.id);
            }
        }
    }
    let mut notices = 0;
    for message in messages {
        notices += usize::from(notify(work, message).await?);
    }
    Ok(notices)
}

/// Late completion calls the same coalescing operation in its acceptance commit.
/// It can materialize both expiry history and the author notice without a tick.
pub(super) async fn notify(work: &mut Work<'_>, message_id: Uuid) -> Result<bool> {
    if work.tx.delivery_timeout_notice(message_id).await?.is_some() {
        return Ok(false);
    }
    let message = work
        .tx
        .message(message_id)
        .await?
        .ok_or_else(|| integrity("Timeout message is missing."))?;
    if !work
        .tx
        .author_timeout_notice_allowed(message.author_id, message.creation_seq)
        .await?
    {
        return Ok(false);
    }
    let expired = work.tx.expired_recipients(message_id).await?;
    if expired.count == 0 {
        return Ok(false);
    }
    let mut sample = Vec::new();
    for id in expired.delivery_ids {
        let delivery = work
            .tx
            .delivery(id)
            .await?
            .ok_or_else(|| integrity("Expired delivery is missing."))?;
        let actor = work
            .tx
            .actor(delivery.recipient_id)
            .await?
            .ok_or_else(|| integrity("Timeout recipient is missing."))?;
        sample.push(json!({"recipient":IdentityLabel::from(&actor),
            "delivery_id":delivery.id,"state":delivery.state,
            "deadline_at":delivery.history.deadline_at,"timed_out_at":delivery.history.timed_out_at}));
    }
    let mut event = work
        .allocate("message.delivery_confirmation_timeout")
        .await?;
    event.channel_id = Some(message.channel_id);
    event.message_id = Some(message.id);
    event.data = json!({"message_id":message.id,"thread_id":message.thread_id(),
        "channel_id":message.channel_id,"author_id":message.author_id,"observed_at":work.now,
        "expired_count":expired.count,"sample_recipients":sample,
        "preview":"Delivery was not confirmed by its processing deadline; this does not prove non-delivery."});
    let author = work
        .tx
        .actor(message.author_id)
        .await?
        .ok_or_else(|| integrity("Timeout author is missing."))?;
    let recipients = if author.joined
        && author.kind() == ActorKind::Agent
        && work
            .tx
            .is_member(message.channel_id, author.actor_id)
            .await?
    {
        vec![author.actor_id]
    } else {
        Vec::new()
    };
    let notice_id = work.record_direct_notice(&event, &recipients).await?;
    work.tx
        .save_delivery_timeout_notice(message_id, notice_id)
        .await?;
    Ok(true)
}

fn integrity(message: &str) -> crate::Error {
    crate::Error::new(crate::ErrorCode::IntegrityError, message)
}
