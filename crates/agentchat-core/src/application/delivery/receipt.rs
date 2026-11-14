//! Receipt-only polls cannot emit input or be blocked by later native controls.
use super::{Application, policy, stream};
use crate::{
    Result,
    delivery::{MessageDispatch, NativeItem, Presentation},
    model::{Context, LaunchOwnerState},
};

pub(super) enum ReceiptPoll {
    Unpublished,
    Waiting,
    Ready(Box<MessageDispatch>),
}

impl Application {
    pub(super) async fn awaiting_message(
        &self,
        context: &Context,
        capabilities: crate::delivery::InputCapabilities,
    ) -> Result<ReceiptPoll> {
        // Only exact current local launch/owner/connection authority is needed
        // to read a result of earlier input. A native getter or later pause must
        // not make known accepted input wait for another model/user boundary.
        let Some(identity) = self.host.launch_delivery_identity(context).await? else {
            return Ok(ReceiptPoll::Unpublished);
        };
        if self.host.launch_owner(&identity) != LaunchOwnerState::Current
            || self.launch_root_changed(context, &identity)
        {
            return Ok(ReceiptPoll::Unpublished);
        }
        let mut snapshot = self.storage.snapshot().await?;
        let Some(agent) = snapshot
            .conversation(context.host, &context.host_session_id)
            .await?
            .filter(|a| a.joined)
        else {
            return Ok(ReceiptPoll::Unpublished);
        };
        let Some(launch) = snapshot
            .latest_launch(agent.id)
            .await?
            .filter(|l| l.identity == identity && l.retired_at.is_none())
        else {
            return Ok(ReceiptPoll::Unpublished);
        };
        if snapshot.session(agent.id).await?.is_none_or(|s| {
            s.context_id != context.context_id || s.incarnation != context.incarnation
        }) {
            return Ok(ReceiptPoll::Unpublished);
        }
        let Some(state) = snapshot.message_stream(launch.identity.generation).await? else {
            return Ok(ReceiptPoll::Unpublished);
        };
        if !state.cancelled
            && let Some(head) = &state.head
        {
            head.require_recovery(capabilities.recovery)?;
        }
        let Some(head) = state.head.filter(|head| {
            !state.cancelled && head.awaiting_acceptance && head.failure_code.is_none()
        }) else {
            return Ok(ReceiptPoll::Unpublished);
        };
        if head.retry_at > self.clock.now() {
            return Ok(ReceiptPoll::Waiting);
        }
        let item = match snapshot
            .retained_native(agent.id, head.delivery_id, head.seq)
            .await?
        {
            Some(NativeItem::Message(item)) => *item,
            // A body-only consumer cannot recover a typed notice as a message.
            // Keep its original head for the mixed receipt-only native route.
            Some(NativeItem::Notice(_)) => return Ok(ReceiptPoll::Waiting),
            _ => return Err(policy::integrity("Ordered receipt head disappeared.")),
        };
        let input = stream::input(context, &launch, &item, head.receipt_policy)?;
        if head.delivery_id != input.delivery_id || head.seq != input.seq {
            return Err(policy::integrity("Ordered receipt head cannot be skipped."));
        }
        Ok(ReceiptPoll::Ready(Box::new(MessageDispatch {
            input,
            token: head.token,
            attempt: head.attempts,
            // No new presentation is authorized. The port may only read this
            // original token's receipt; all actual output uses the fresh path.
            presentation: Presentation::Idle,
            awaiting_acceptance: true,
            frame_revision: head.frame_revision,
        })))
    }
}
