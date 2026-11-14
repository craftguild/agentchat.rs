//! Fresh original-head reservation; no host verify, native I/O or outer commit.
use super::{
    Application,
    native::native_input,
    native_preparation::{ClaimedNative, NativeClaim},
    policy,
};
use crate::{
    Result,
    delivery::{MessageHead, NativeDispatch, Presentation, SUBMISSION_SECONDS},
    ports::Transaction,
};
use uuid::Uuid;

impl Application {
    pub(in crate::application) async fn reserve_native_in(
        &self,
        tx: &mut dyn Transaction,
        request: NativeClaim,
    ) -> Result<ClaimedNative> {
        let NativeClaim {
            selection,
            context,
            capabilities,
            host,
            admission,
        } = request;
        super::native_preparation::Admission::check(admission.as_ref())?;
        let context = &context;

        let Some((agent, launch, session)) = self.message_authority(tx, context).await? else {
            return Ok(ClaimedNative::none(false));
        };
        if selection.generation != Some(launch.identity.generation) {
            return Ok(ClaimedNative::none(false));
        }
        let now = self.clock.now();
        let Some(presentation) =
            policy::message_presentation(&session, &host, now, capabilities.active_input)
        else {
            return Ok(ClaimedNative::none(false));
        };
        if let Presentation::Active { expected_turn_id } = &presentation {
            policy::valid_turn(expected_turn_id)?;
        }
        let mut state = tx
            .message_stream(launch.identity.generation)
            .await?
            .ok_or_else(|| policy::integrity("Current launch has no native input stream."))?;
        // Another preparation can commit a head while native observation awaits.
        // Check its original meaning again under the writer using captured facts,
        // before deciding to submit, wait or poll. This performs no host callback.
        if !state.cancelled
            && let Some(head) = &state.head
        {
            head.require_recovery(capabilities.recovery)?;
        }
        if state.cancelled
            || state.head.as_ref().is_some_and(|head| {
                head.submission_pending
                    || head.retry_at > now
                    || (head.attempts >= 3 && !head.awaiting_acceptance)
            })
        {
            return Ok(ClaimedNative::none(false));
        }
        let Some(item) = tx.next_native(agent.id, state.settled_seq).await? else {
            if state.head.is_some() {
                return Err(policy::integrity("Ordered native input head disappeared."));
            }
            return Ok(ClaimedNative::none(false));
        };
        if selection.delivery != Some((item.delivery().id, item.dispatch().creation_seq)) {
            return Ok(ClaimedNative::none(false));
        }
        let mut delivery = item.delivery().clone();
        delivery.start_processing(
            now,
            tx.receipt_settings()
                .await?
                .delivery_confirmation_timeout_seconds,
        )?;
        let message = (item.dispatch().kind == crate::model::DispatchKind::Message)
            .then_some(item.dispatch().id);
        super::native_preparation::Admission::check(admission.as_ref())?;
        if super::processing::expire_before_submission(tx, &mut delivery, &mut state, message, now)
            .await?
        {
            return Ok(ClaimedNative::none(true));
        }
        let receipt_policy = if let Some(head) = &state.head {
            head.receipt_policy
        } else {
            tx.receipt_settings().await?.input_policy()
        };
        let frame_revision = state
            .head
            .as_ref()
            .map_or(crate::project::NativeFrameRevision::DispatchV4, |head| {
                head.frame_revision
            });
        let input = native_input(context, &launch, &item, receipt_policy, frame_revision)?;
        capabilities.require_input(item.delivery().wake_eligible)?;
        if let Some(head) = &state.head
            && (head.delivery_id != input.delivery_id() || head.seq != input.seq())
        {
            return Err(policy::integrity(
                "An earlier native input head cannot be skipped.",
            ));
        }
        if state
            .head
            .as_ref()
            .is_some_and(|head| head.awaiting_acceptance)
        {
            return Err(policy::integrity(
                "A published native input must be polled through its original receipt lane.",
            ));
        }
        let attempt = state.head.as_ref().map_or(1, |head| head.attempts + 1);
        let token = Uuid::new_v4();
        state.head = Some(MessageHead {
            delivery_id: input.delivery_id(),
            seq: input.seq(),
            incarnation: context.incarnation,
            token,
            attempts: attempt,
            retry_at: now.after_seconds(SUBMISSION_SECONDS as i64)?,
            failure_code: None,
            receipt_policy,
            frame_revision,
            // Persist the mechanism before I/O, independently of host labels.
            // A crash or new declaration cannot turn uncertainty into a resend.
            awaiting_acceptance: capabilities.recovery
                == crate::delivery::InputRecovery::OriginalReceipt,
            submission_pending: capabilities.recovery
                == crate::delivery::InputRecovery::RetainUncertainty,
        });
        super::native_preparation::Admission::check(admission.as_ref())?;
        tx.save_message_stream(&state).await?;
        delivery.attempts = delivery
            .attempts
            .checked_add(1)
            .ok_or_else(|| policy::integrity("Native input delivery attempts overflow."))?;
        tx.save_delivery(&delivery).await?;
        Ok(ClaimedNative::some(
            NativeDispatch::new(input, token, attempt, presentation, false)
                .with_frame_revision(frame_revision),
        ))
    }
}
