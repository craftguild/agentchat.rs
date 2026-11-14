//! One durable cursor orders complete messages and bounded native event context.

use super::{Application, policy, progress, reconcile::CompletionError, stream};
use crate::{
    Error, ErrorCode, Result,
    delivery::{
        MessageProgress, NativeCompletion, NativeDispatch, NativeInput, NativeItem, NoticeInput,
        Presentation,
    },
    model::{Context, DeliveryState, Launch},
    ports::Transaction,
};
use uuid::Uuid;

pub(super) enum NativeReceiptPoll {
    Unpublished,
    Waiting,
    Ready(Box<NativeDispatch>),
}

impl Application {
    /// Claim the earliest recipient event after this launch's single accepted
    /// cursor. Complete messages and typed non-message context cannot overtake
    /// one another. Claiming grants neither acceptance nor an explicit read.
    pub async fn prepare_native(&self, context: &Context) -> Result<Option<NativeDispatch>> {
        use crate::application::{CommitOutcome, CommitRequest, NativePreparation};
        let request = self.native_processing(context)?;
        let processed = match self
            .commit_ready(vec![CommitRequest::NativeProcessing(request)])
            .await?
            .pop()
            .expect("one processing outcome")?
        {
            CommitOutcome::NativeProcessing(value) => value,
            _ => unreachable!("processing-only preparation"),
        };
        match self.observe_native(processed).await? {
            NativePreparation::Original(value) => Ok(value),
            NativePreparation::Claim(request) => match self
                .commit_ready(vec![CommitRequest::NativeClaim(request)])
                .await?
                .pop()
                .expect("one claim outcome")?
            {
                CommitOutcome::NativeClaim(value) => Ok(value.into_dispatch()),
                _ => unreachable!("claim-only preparation"),
            },
        }
    }

    /// Record one mixed-stream result through the shared grouped commit path.
    pub async fn finish_native(
        &self,
        context: &Context,
        dispatch: &NativeDispatch,
        outcome: Result<MessageProgress>,
    ) -> Result<bool> {
        let outcome = self
            .commit_ready(vec![crate::application::CommitRequest::NativeCompletion(
                NativeCompletion {
                    context: context.clone(),
                    dispatch: dispatch.clone(),
                    outcome,
                },
            )])
            .await?
            .pop()
            .expect("one native completion")?;
        match outcome {
            crate::application::CommitOutcome::NativeCompletion(recorded) => Ok(recorded),
            _ => unreachable!("native-completion-only commit"),
        }
    }

    pub(in crate::application) async fn finish_native_in(
        &self,
        tx: &mut dyn Transaction,
        context: &Context,
        dispatch: &NativeDispatch,
        outcome: Result<MessageProgress>,
    ) -> std::result::Result<bool, CompletionError> {
        let Some((agent, launch)) = self.message_launch(tx, context).await? else {
            return Ok(false);
        };
        let same_session = tx.session(agent.id).await?.is_some_and(|session| {
            session.context_id == context.context_id && session.incarnation == context.incarnation
        });
        if dispatch.generation() != launch.identity.generation {
            return Ok(false);
        }
        let Some(mut state) = tx.message_stream(dispatch.generation()).await? else {
            return Ok(false);
        };
        let Some(head) = &state.head else {
            return Ok(false);
        };
        // Reconnect cannot turn an old callback into current-session output.
        // It may only resolve the unchanged original request with a positive
        // acceptance or a positive proof that native acceptance did not occur.
        if !same_session && !progress::resolves_after_reconnect(head, &outcome) {
            return Ok(false);
        }
        let receipt_poll =
            dispatch.awaiting_acceptance() && head.awaiting_acceptance && same_session;
        if state.cancelled
            || head.frame_revision != dispatch.frame_revision()
            || head.receipt_policy != dispatch.input().receipt_policy()
            || head.delivery_id != dispatch.delivery_id()
            || head.seq != dispatch.seq()
            || (head.incarnation != context.incarnation && !receipt_poll)
            || head.token != dispatch.token()
            || head.attempts != dispatch.attempt()
            || (head.failure_code.is_some()
                && !(head.submission_pending && progress::settles(&outcome)))
        {
            return Ok(false);
        }
        let item = tx
            .retained_native(agent.id, head.delivery_id, head.seq)
            .await?
            .ok_or_else(|| policy::integrity("Ordered native input head disappeared."))?;
        if native_input(
            context,
            &launch,
            &item,
            head.receipt_policy,
            head.frame_revision,
        )? != dispatch.input()
        {
            return Err(policy::integrity(
                "Native input completion differs from its immutable head.",
            )
            .into());
        }
        let removed_channel = if let Some(channel) = item.dispatch().channel_id {
            tx.membership_boundary(channel, agent.id)
                .await?
                .is_none_or(|boundary| boundary >= item.dispatch().creation_seq)
        } else {
            false
        };
        if matches!(&outcome, Ok(MessageProgress::NotAccepted(_))) && removed_channel {
            let mut delivery = item.into_delivery();
            delivery.depart(self.clock.now())?;
            tx.save_delivery(&delivery).await?;
            state.settled_seq = head.seq;
            state.head = None;
            tx.save_message_stream(&state).await?;
            return Ok(true);
        }
        progress::apply(
            tx,
            agent.id,
            &mut state,
            match &item {
                NativeItem::Message(item) => Some(item.message.id),
                NativeItem::Notice(_) => None,
            },
            outcome,
            self.clock.now(),
        )
        .await?;
        tx.save_message_stream(&state).await?;
        Ok(true)
    }

    /// Snapshot-only final hook for either native input kind.
    pub async fn native_is_current(
        &self,
        context: &Context,
        generation: Uuid,
        token: Uuid,
        delivery: Uuid,
    ) -> Result<bool> {
        self.stream_is_current(context, generation, token, delivery, true)
            .await
    }

    pub(super) async fn awaiting_native(
        &self,
        context: &Context,
        capabilities: crate::delivery::InputCapabilities,
    ) -> Result<NativeReceiptPoll> {
        let Some(identity) = self.host.launch_delivery_identity(context).await? else {
            return Ok(NativeReceiptPoll::Unpublished);
        };
        if self.host.launch_owner(&identity) != crate::model::LaunchOwnerState::Current
            || self.launch_root_changed(context, &identity)
        {
            return Ok(NativeReceiptPoll::Unpublished);
        }
        let mut snapshot = self.storage.snapshot().await?;
        let Some(agent) = snapshot
            .conversation(context.host, &context.host_session_id)
            .await?
            .filter(|agent| agent.joined)
        else {
            return Ok(NativeReceiptPoll::Unpublished);
        };
        let Some(launch) = snapshot
            .latest_launch(agent.id)
            .await?
            .filter(|launch| launch.identity == identity && launch.retired_at.is_none())
        else {
            return Ok(NativeReceiptPoll::Unpublished);
        };
        if snapshot.session(agent.id).await?.is_none_or(|session| {
            session.context_id != context.context_id || session.incarnation != context.incarnation
        }) {
            return Ok(NativeReceiptPoll::Unpublished);
        }
        let Some(state) = snapshot.message_stream(identity.generation).await? else {
            return Ok(NativeReceiptPoll::Unpublished);
        };
        if !state.cancelled
            && let Some(head) = &state.head
        {
            head.require_recovery(capabilities.recovery)?;
        }
        let Some(head) = state.head.filter(|head| {
            !state.cancelled && head.awaiting_acceptance && head.failure_code.is_none()
        }) else {
            return Ok(NativeReceiptPoll::Unpublished);
        };
        if head.retry_at > self.clock.now() {
            return Ok(NativeReceiptPoll::Waiting);
        }
        let item = snapshot
            .retained_native(agent.id, head.delivery_id, head.seq)
            .await?
            .ok_or_else(|| policy::integrity("Ordered native receipt head disappeared."))?;
        let input = native_input(
            context,
            &launch,
            &item,
            head.receipt_policy,
            head.frame_revision,
        )?;
        if head.delivery_id != input.delivery_id() || head.seq != input.seq() {
            return Err(policy::integrity(
                "Ordered native receipt head cannot be skipped.",
            ));
        }
        Ok(NativeReceiptPoll::Ready(Box::new(
            NativeDispatch::new(input, head.token, head.attempts, Presentation::Idle, true)
                .with_frame_revision(head.frame_revision),
        )))
    }
}

pub(super) fn native_input(
    context: &Context,
    launch: &Launch,
    item: &NativeItem,
    policy: crate::project::InputReceiptPolicy,
    _revision: crate::project::NativeFrameRevision,
) -> Result<NativeInput> {
    match item {
        NativeItem::Message(item) => {
            stream::input(context, launch, item, policy).map(NativeInput::Message)
        }
        NativeItem::Notice(item) => {
            let notice = item
                .notice
                .as_ref()
                .ok_or_else(|| policy::integrity("Notice subtype is missing."))?;
            if item.message.is_some()
                || item.dispatch.creation_seq <= launch.start_seq
                || item.delivery.dispatch_id != item.dispatch.id
                || item.delivery.recipient_id != launch.agent_id
                || item.delivery.state == DeliveryState::FailedLeft
            {
                return Err(Error::new(
                    ErrorCode::IntegrityError,
                    "Invalid native Notice candidate.",
                ));
            }
            NoticeInput::new(
                context.project_id,
                launch.identity.generation,
                item.delivery.id,
                &item.dispatch,
                notice,
                item.delivery.wake_eligible,
                policy,
            )
            .map(NativeInput::Notice)
        }
    }
}
