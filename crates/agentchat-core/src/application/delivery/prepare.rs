//! A reserved attempt is durable before a native host can observe its hint.

use super::{policy, reconcile};
use crate::{
    Result,
    delivery::*,
    model::Context,
    ports::{HostState, Transaction},
    time::Timestamp,
};
use uuid::Uuid;

pub(super) async fn prepare(
    tx: &mut dyn Transaction,
    context: &Context,
    host: &HostState,
    now: Timestamp,
    requirement: ActiveInput,
) -> Result<Option<WakeDispatch>> {
    let Some(agent) = tx
        .conversation(context.host, &context.host_session_id)
        .await?
        .filter(|a| a.joined)
    else {
        return Ok(None);
    };
    let Some(session) = tx.session(agent.id).await? else {
        return Ok(None);
    };
    if session.incarnation != context.incarnation || session.context_id != context.context_id {
        return Ok(None);
    }
    let Some(presentation) = policy::presentation(&session, host, now, requirement) else {
        return Ok(None);
    };
    if let Presentation::Active { expected_turn_id } = &presentation {
        policy::valid_turn(expected_turn_id)?;
    }
    let existing = tx.outstanding_wake(agent.id, context.incarnation).await?;
    let mut batch = if let Some(batch) = existing {
        if reconcile::settled_batch(tx, &batch).await? {
            None
        } else {
            Some(batch)
        }
    } else {
        None
    };
    // The bounded delivery-only check above reconciles empty assigned sets
    // even during backoff/stall. Hydrate full events only when another attempt
    // can actually proceed, retaining all exact host/turn and retry controls.
    if let Some(existing) = &mut batch {
        if existing.retry_deferred(now) || !policy::retry_safe(existing, &presentation) {
            return Ok(None);
        }
        if existing.attempts >= 3 {
            existing.state = BatchState::Stalled;
            existing.next_eligible_at = None;
            existing.failure_code = Some("unconfirmed_input".into());
            tx.save_wake_batch(existing).await?;
            return Ok(None);
        }
    }
    let mut items = tx
        .wake_items(agent.id, batch.as_ref().map(|b| b.id))
        .await?;
    // A final write cannot depend on housekeeping having observed expiry.
    // Retain original deadlines across hint retries; this never settles the
    // separate complete-input stream or discards an unknown original receipt.
    for item in &mut items {
        if item.delivery.expire(now)? {
            tx.save_delivery(&item.delivery).await?;
            if item.dispatch.kind == crate::model::DispatchKind::Message {
                let message = item.dispatch.id;
                super::super::confirmations::notify(
                    &mut crate::application::work::Work {
                        tx,
                        now,
                        last_seq: None,
                    },
                    message,
                )
                .await?;
            }
        }
    }
    items.retain(|item| {
        matches!(item.delivery.state, crate::model::DeliveryState::Pending)
            || item.delivery.within_processing_deadline(now)
    });
    if items.is_empty() {
        if let Some(batch) = &batch {
            reconcile::settled_batch(tx, batch).await?;
        }
        return Ok(None);
    }
    let mut batch = batch.unwrap_or_else(|| WakeBatch {
        id: Uuid::new_v4(),
        recipient_id: agent.id,
        incarnation: context.incarnation,
        state: BatchState::Pending,
        dispatch_ref: None,
        turn_ref: None,
        attempts: 0,
        next_eligible_at: None,
        failure_code: None,
    });
    let hint = policy::hint(
        context,
        agent.id,
        batch.id,
        &items,
        tx.pending_delivery_count(agent.id).await?,
    )?;
    batch.attempts += 1;
    let references = hint
        .events
        .iter()
        .map(|event| event.delivery_id)
        .collect::<Vec<_>>();
    let token = policy::dispatch_token(&batch, &references);
    batch.state = BatchState::Pending;
    batch.dispatch_ref = Some(token);
    batch.turn_ref = match &presentation {
        Presentation::Idle => None,
        Presentation::Active { expected_turn_id } => expected_turn_id.clone(),
    };
    // An orphaned attempt must not be issued twice immediately after a crash.
    // The runtime also caps native I/O at this deadline; callbacks are fenced.
    batch.next_eligible_at = Some(now.after_seconds(SUBMISSION_SECONDS as i64)?);
    batch.failure_code = None;
    tx.save_wake_batch(&batch).await?;
    for item in items {
        let mut delivery = item.delivery;
        delivery.start_processing(
            now,
            tx.receipt_settings()
                .await?
                .delivery_confirmation_timeout_seconds,
        )?;
        delivery.wake_batch_id = Some(batch.id);
        delivery.attempts = delivery
            .attempts
            .checked_add(1)
            .ok_or_else(|| policy::integrity("Delivery attempt counter overflow."))?;
        tx.save_delivery(&delivery).await?;
    }
    Ok(Some(WakeDispatch {
        hint,
        token,
        attempt: batch.attempts,
        presentation,
    }))
}
