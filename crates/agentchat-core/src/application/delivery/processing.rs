//! Processing bookkeeping precedes native observation and never grants a write.
use super::{Application, policy};
use crate::{
    Result,
    application::work::Work,
    model::{Context, DeliveryState, Phase},
};

impl Application {
    pub(super) async fn claim_processing(&self, context: &Context, mixed: bool) -> Result<()> {
        let mut tx = self.storage.begin().await?;
        if self
            .claim_processing_in(tx.as_mut(), context, mixed, None)
            .await?
            .changed
        {
            tx.commit().await?;
        } else {
            tx.rollback().await?;
        }
        Ok(())
    }

    pub(super) async fn claim_processing_in(
        &self,
        tx: &mut dyn crate::ports::Transaction,
        context: &Context,
        mixed: bool,
        admission: Option<&super::native_preparation::Admission>,
    ) -> Result<super::native_preparation::ProcessingEffect> {
        use super::native_preparation::{ProcessingEffect, Selection};
        let Some((agent, launch, session)) = self.message_authority(tx, context).await? else {
            return Ok(ProcessingEffect::unchanged(None));
        };
        let now = self.clock.now();
        let Some(mut stream) = tx.message_stream(launch.identity.generation).await? else {
            return Err(policy::integrity("Current launch has no native stream."));
        };
        if stream.cancelled {
            return Ok(ProcessingEffect::unchanged(Some(
                launch.identity.generation,
            )));
        }
        let item = if let Some(head) = &stream.head {
            tx.retained_native(agent.id, head.delivery_id, head.seq)
                .await?
        } else if mixed {
            tx.next_native(agent.id, stream.settled_seq).await?
        } else {
            tx.next_message(agent.id, stream.settled_seq)
                .await?
                .map(|item| crate::delivery::NativeItem::Message(Box::new(item)))
        };
        let Some(item) = item else {
            return Ok(ProcessingEffect::unchanged(Some(
                launch.identity.generation,
            )));
        };
        let selection = Selection {
            generation: Some(launch.identity.generation),
            delivery: Some((item.delivery().id, item.dispatch().creation_seq)),
        };
        let message = (item.dispatch().kind == crate::model::DispatchKind::Message)
            .then_some(item.dispatch().id);
        let mut delivery = item.into_delivery();
        let mut changed = false;
        if session.is_connected(now)
            && !session.wake_paused
            && matches!(session.phase, Phase::Idle | Phase::Active)
        {
            changed |= delivery.start_processing(
                now,
                tx.receipt_settings()
                    .await?
                    .delivery_confirmation_timeout_seconds,
            )?;
        }
        changed |= delivery.expire(now)?;
        if changed {
            super::native_preparation::Admission::check(admission)?;
            tx.save_delivery(&delivery).await?;
            if delivery.history.timed_out_at.is_some()
                && let Some(message) = message
            {
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
        if delivery.state == DeliveryState::FailedTimeout
            && stream
                .head
                .as_ref()
                .is_some_and(|head| !head.submission_pending && !head.awaiting_acceptance)
        {
            // Only a positively settled non-entry has neither uncertainty flag.
            // Expiry alone can never remove an unknown original head.
            super::native_preparation::Admission::check(admission)?;
            stream.settled_seq = stream.head.as_ref().expect("settled original").seq;
            stream.head = None;
            tx.save_message_stream(&stream).await?;
            changed = true;
        }
        Ok(ProcessingEffect { changed, selection })
    }
}

/// Observation can cross a deadline. Recheck under the writer before reserving
/// another token, keeping any existing unknown original input recoverable.
pub(super) async fn expire_before_submission(
    tx: &mut dyn crate::ports::Transaction,
    delivery: &mut crate::model::Delivery,
    stream: &mut crate::delivery::MessageStream,
    message: Option<uuid::Uuid>,
    now: crate::time::Timestamp,
) -> Result<bool> {
    if !delivery.expire(now)? {
        return Ok(false);
    }
    tx.save_delivery(delivery).await?;
    if let Some(message) = message {
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
    if let Some(head) = &stream.head
        && head.delivery_id == delivery.id
        && !head.submission_pending
        && !head.awaiting_acceptance
    {
        stream.settled_seq = head.seq;
        stream.head = None;
        tx.save_message_stream(stream).await?;
    }
    Ok(true)
}

/// Channel departure retires only this head's original scope. No backlog moves
/// to a later membership boundary, and obsolete callbacks lose their token.
pub(in crate::application) async fn retire_channel_head(
    work: &mut Work<'_>,
    actor: uuid::Uuid,
    channel: uuid::Uuid,
) -> Result<()> {
    let Some(launch) = work
        .tx
        .latest_launch(actor)
        .await?
        .filter(|launch| launch.retired_at.is_none())
    else {
        return Ok(());
    };
    let Some(mut stream) = work.tx.message_stream(launch.identity.generation).await? else {
        return Ok(());
    };
    if stream.cancelled {
        return Ok(());
    }
    let Some(head) = &stream.head else {
        return Ok(());
    };
    let delivery = work
        .tx
        .delivery(head.delivery_id)
        .await?
        .ok_or_else(|| policy::integrity("Departed head disappeared."))?;
    let dispatch = work
        .tx
        .dispatch(delivery.dispatch_id)
        .await?
        .ok_or_else(|| policy::integrity("Departed head Dispatch disappeared."))?;
    if dispatch.channel_id == Some(channel) {
        stream.settled_seq = head.seq;
        stream.head = None;
        work.tx.save_message_stream(&stream).await?;
    }
    Ok(())
}
