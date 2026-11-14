//! Shared completion transitions; uncertainty never manufactures a retry permit.
use super::{policy, reconcile::CompletionError};
use crate::{
    Result,
    application::work::Work,
    delivery::{MessageHead, MessageProgress, MessageStream, SUBMISSION_SECONDS},
    ports::Transaction,
    time::Timestamp,
};
use uuid::Uuid;

pub(super) async fn apply(
    tx: &mut dyn Transaction,
    recipient: Uuid,
    state: &mut MessageStream,
    message_id: Option<Uuid>,
    outcome: Result<MessageProgress>,
    now: Timestamp,
) -> std::result::Result<(), CompletionError> {
    let id = state.head.as_ref().expect("validated head").delivery_id;
    let mut current = tx
        .delivery(id)
        .await?
        .ok_or_else(|| policy::integrity("Original delivery disappeared."))?;
    if current.expire(now)? {
        tx.save_delivery(&current).await?;
        if let Some(message) = message_id {
            super::super::confirmations::notify(
                &mut Work {
                    tx,
                    now,
                    last_seq: None,
                },
                message,
            )
            .await?;
        }
    }
    match outcome {
        Ok(MessageProgress::Accepted(receipt)) => {
            policy::valid_turn(&receipt.turn_id).map_err(CompletionError::Outcome)?;
            let head = state.head.as_mut().expect("validated head");
            let mut delivery = tx
                .delivery(head.delivery_id)
                .await?
                .ok_or_else(|| policy::integrity("Accepted delivery disappeared."))?;
            // Acceptance, frozen-policy effects and FIFO state share the same
            // FULL transaction. Nothing here emits a receipt event or invalidation.
            delivery.confirm_delivery(now)?;
            let at = delivery.history.delivered_at.expect("confirmed delivery");
            tx.save_delivery(&delivery).await?;
            let mut work = Work {
                tx,
                now: at,
                last_seq: None,
            };
            if let Some(batch_id) = delivery.wake_batch_id {
                let batch = work
                    .tx
                    .wake_batch(batch_id)
                    .await?
                    .ok_or_else(|| policy::integrity("Assigned hint batch disappeared."))?;
                super::reconcile::settled_batch(work.tx, &batch).await?;
            }
            if let Some(message) = message_id {
                if head.receipt_policy.auto_read
                    && work
                        .tx
                        .receipt_kind(message, recipient, crate::model::ReadKind::Automatic)
                        .await?
                        .is_none()
                {
                    work.tx
                        .insert_receipt(&crate::model::Receipt {
                            message_id: message,
                            actor_id: recipient,
                            kind: crate::model::ReadKind::Automatic,
                            first_read_at: at,
                        })
                        .await?;
                }
                if delivery.history.timed_out_at.is_some() {
                    super::super::confirmations::notify(&mut work, message).await?;
                }
            }
            state.settled_seq = head.seq;
            state.head = None;
        }
        Ok(MessageProgress::AwaitingAcceptance) => {
            let head = state.head.as_mut().expect("validated head");
            // Local publication cannot invent a receipt mechanism for a
            // differently claimed original attempt, even after adapter changes.
            if !head.awaiting_acceptance || head.submission_pending {
                return Err(CompletionError::Outcome(crate::Error::new(
                    crate::ErrorCode::UnsupportedProtocol,
                    "The original native claim has no receipt-only recovery mechanism.",
                )));
            }
            head.failure_code = None;
            head.retry_at = now.after_seconds(1)?;
        }
        result => {
            let head = state.head.as_mut().expect("validated head");
            let error = match result {
                Ok(MessageProgress::NotAccepted(error)) => {
                    // A positive no-acceptance result resolves this original
                    // attempt. Ordinary errors cannot undo its durable fence.
                    head.submission_pending = false;
                    head.awaiting_acceptance = false;
                    if current.state == crate::model::DeliveryState::FailedTimeout {
                        // A positive original refusal settles uncertainty after
                        // expiry. It cannot fabricate acceptance or reopen send.
                        state.settled_seq = head.seq;
                        state.head = None;
                        return Ok(());
                    }
                    error
                }
                Err(_) if head.awaiting_acceptance => {
                    // A receipt/read error proves no rejection. Keep the exact
                    // original publication and existing receipt-check interval.
                    head.retry_at = now.after_seconds(SUBMISSION_SECONDS as i64)?;
                    return Ok(());
                }
                Err(error) => error,
                _ => unreachable!("progress variants handled above"),
            };
            // A published receipt lane keeps its exact token through errors.
            // Only a definite non-entry result may reopen native publication.
            head.failure_code = Some(
                serde_json::to_value(error.code)
                    .expect("error code serialization")
                    .as_str()
                    .expect("string error code")
                    .into(),
            );
            head.retry_at = now.after_seconds(SUBMISSION_SECONDS as i64)?;
        }
    }
    Ok(())
}

pub(super) fn settles(outcome: &Result<MessageProgress>) -> bool {
    matches!(
        outcome,
        Ok(MessageProgress::Accepted(_) | MessageProgress::NotAccepted(_))
    )
}

pub(super) fn resolves_after_reconnect(
    head: &MessageHead,
    outcome: &Result<MessageProgress>,
) -> bool {
    matches!(outcome, Ok(MessageProgress::Accepted(_)))
        || ((head.submission_pending || head.awaiting_acceptance)
            && matches!(outcome, Ok(MessageProgress::NotAccepted(_))))
}
