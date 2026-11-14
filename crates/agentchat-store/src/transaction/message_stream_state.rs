//! Durable original input policy and uncertainty-fenced settlement cursor.
use super::StoreTransaction;
use crate::{
    decode,
    entities::message_stream,
    error::{database, integrity},
};
use agentchat_core::{
    Result,
    delivery::{MessageHead, MessageStream},
    project::{InputReceiptPolicy, NativeFrameRevision},
};
use sea_orm::{ActiveModelTrait, ConnectionTrait, EntityTrait, Set};
use uuid::Uuid;
pub(crate) async fn get<C: ConnectionTrait>(
    db: &C,
    generation: Uuid,
) -> Result<Option<MessageStream>> {
    message_stream::Entity::find_by_id(generation.to_string())
        .one(db)
        .await
        .map_err(database)?
        .map(decode_stream)
        .transpose()
}

fn decode_stream(row: message_stream::Model) -> Result<MessageStream> {
    let head = match (
        row.head_delivery,
        row.head_seq,
        row.incarnation,
        row.token,
        row.attempts,
        row.retry_at,
    ) {
        (Some(id), Some(seq), Some(incarnation), Some(token), Some(attempts), Some(retry_at))
            if (1..=3).contains(&attempts) && seq > row.settled_seq =>
        {
            Some(MessageHead {
                delivery_id: decode::uuid(&id)?,
                seq: decode::seq(seq)?,
                incarnation: decode::uuid(&incarnation)?,
                token: decode::uuid(&token)?,
                attempts: attempts as u32,
                retry_at: decode::time(&retry_at)?,
                failure_code: row.failure_code,
                awaiting_acceptance: row.awaiting_acceptance,
                submission_pending: row.submission_pending,
                receipt_policy: InputReceiptPolicy {
                    auto_read: row
                        .auto_read
                        .ok_or_else(|| integrity("Missing original read policy."))?,
                },
                frame_revision: NativeFrameRevision::from_code(
                    row.frame_revision
                        .ok_or_else(|| integrity("Missing original frame revision."))?,
                )?,
            })
        }
        (None, None, None, None, None, None)
            if row.failure_code.is_none()
                && !row.awaiting_acceptance
                && !row.submission_pending
                && row.auto_read.is_none()
                && row.frame_revision.is_none() =>
        {
            None
        }
        _ => return Err(integrity("Invalid ordered message head.")),
    };
    let generation = decode::uuid(&row.generation)?;
    if generation.is_nil()
        || head.as_ref().is_some_and(|h| {
            h.delivery_id.is_nil()
                || h.incarnation.is_nil()
                || h.token.is_nil()
                || (h.awaiting_acceptance && h.failure_code.is_some())
                || (h.awaiting_acceptance && h.submission_pending)
                || h.failure_code
                    .as_ref()
                    .is_some_and(|c| c.is_empty() || c.len() > 64)
        })
    {
        return Err(integrity(
            "Invalid message stream identity or failure code.",
        ));
    }
    Ok(MessageStream {
        generation,
        settled_seq: decode::seq(row.settled_seq)?,
        head,
        cancelled: row.cancelled,
    })
}

pub(super) async fn save(tx: &StoreTransaction, stream: &MessageStream) -> Result<()> {
    let row = message_stream::Model {
        generation: stream.generation.to_string(),
        settled_seq: stream.settled_seq.get(),
        head_delivery: stream.head.as_ref().map(|h| h.delivery_id.to_string()),
        head_seq: stream.head.as_ref().map(|h| h.seq.get()),
        incarnation: stream.head.as_ref().map(|h| h.incarnation.to_string()),
        token: stream.head.as_ref().map(|h| h.token.to_string()),
        attempts: stream.head.as_ref().map(|h| h.attempts as i32),
        retry_at: stream.head.as_ref().map(|h| h.retry_at.to_string()),
        failure_code: stream.head.as_ref().and_then(|h| h.failure_code.clone()),
        awaiting_acceptance: stream.head.as_ref().is_some_and(|h| h.awaiting_acceptance),
        submission_pending: stream.head.as_ref().is_some_and(|h| h.submission_pending),
        cancelled: stream.cancelled,
        auto_read: stream.head.as_ref().map(|h| h.receipt_policy.auto_read),
        frame_revision: stream.head.as_ref().map(|h| h.frame_revision.code()),
    };
    decode_stream(row.clone())?;
    let old = get(&tx.db, stream.generation).await?;
    if let Some(old) = &old {
        let valid = if old.cancelled {
            old == stream
        } else if stream.settled_seq == old.settled_seq {
            match (&old.head, &stream.head) {
                (None, _) => true,
                (Some(a), Some(b)) => {
                    a.delivery_id == b.delivery_id
                        && a.seq == b.seq
                        && a.receipt_policy == b.receipt_policy && a.frame_revision == b.frame_revision
                        && (a.attempts..=a.attempts + 1).contains(&b.attempts)
                        // Neither an unresolved native request nor a durable
                        // publication can be replaced by another attempt token.
                        && (a.attempts == b.attempts
                            || (!a.submission_pending && !a.awaiting_acceptance))
                        && (a.attempts != b.attempts
                            || (a.token == b.token && a.incarnation == b.incarnation))
                        // An unresolved original mechanism can remain unchanged
                        // or be definitely settled; it cannot become the other
                        // mechanism through configuration or callback drift.
                        && (!(a.awaiting_acceptance || a.submission_pending)
                            || (!b.awaiting_acceptance && !b.submission_pending)
                            || (a.awaiting_acceptance == b.awaiting_acceptance
                                && a.submission_pending == b.submission_pending))
                }
                _ => false,
            }
        } else {
            old.head
                .as_ref()
                .is_some_and(|h| stream.settled_seq == h.seq)
                && stream.head.is_none()
                && !stream.cancelled
        };
        if !valid {
            return Err(integrity(
                "Message stream cannot skip its head, regress or revive.",
            ));
        }
    } else {
        let launch = super::launches::get(tx, stream.generation)
            .await?
            .ok_or_else(|| integrity("Message stream has no launch."))?;
        if stream != &MessageStream::new(&launch) {
            return Err(integrity(
                "Message stream must begin at its launch boundary.",
            ));
        }
    }
    let active = message_stream::ActiveModel {
        generation: Set(row.generation),
        settled_seq: Set(row.settled_seq),
        head_delivery: Set(row.head_delivery),
        head_seq: Set(row.head_seq),
        incarnation: Set(row.incarnation),
        token: Set(row.token),
        attempts: Set(row.attempts),
        retry_at: Set(row.retry_at),
        failure_code: Set(row.failure_code),
        awaiting_acceptance: Set(row.awaiting_acceptance),
        submission_pending: Set(row.submission_pending),
        cancelled: Set(row.cancelled),
        auto_read: Set(row.auto_read),
        frame_revision: Set(row.frame_revision),
    };
    if old.is_some() {
        active.update(&tx.db).await.map_err(database)?;
    } else {
        active.insert(&tx.db).await.map_err(database)?;
    }
    Ok(())
}
