//! Native reception atomically settles delivery and its frozen read policy.
use super::{Application, policy, progress, reconcile::CompletionError};
use crate::{
    Result,
    delivery::{
        HostReceipt, MessageDispatch, MessageHead, MessageProgress, NativeItem, SUBMISSION_SECONDS,
    },
    model::{Agent, Context, Launch, LaunchOwnerState, Session},
    ports::Transaction,
};
use uuid::Uuid;

impl Application {
    /// Claim the earliest non-echo message after this launch's accepted cursor.
    /// Both addressed and passive messages use the same head. The committed
    /// claim precedes all native I/O; a failed/orphaned head cannot be skipped.
    pub async fn prepare_message(&self, context: &Context) -> Result<Option<MessageDispatch>> {
        self.validate_context(context)?;
        let capabilities = self.host.input_capabilities(context)?;
        self.claim_processing(context, false).await?;
        match self.awaiting_message(context, capabilities).await? {
            super::receipt::ReceiptPoll::Ready(poll) => return Ok(Some(*poll)),
            super::receipt::ReceiptPoll::Waiting => return Ok(None),
            super::receipt::ReceiptPoll::Unpublished => {}
        }
        let host = self.host.verify(context).await?;
        let mut tx = self.storage.begin().await?;
        let Some((agent, launch, session)) = self.message_authority(tx.as_mut(), context).await?
        else {
            return Ok(None);
        };
        let now = self.clock.now();
        let Some(presentation) =
            policy::message_presentation(&session, &host, now, capabilities.active_input)
        else {
            return Ok(None);
        };
        if let crate::delivery::Presentation::Active { expected_turn_id } = &presentation {
            policy::valid_turn(expected_turn_id)?;
        }
        let mut stream = tx
            .message_stream(launch.identity.generation)
            .await?
            .ok_or_else(|| policy::integrity("Current launch has no message stream."))?;
        // The snapshot may precede a concurrent head committed during observe.
        // Never interpret that newer head using a different captured mechanism.
        if !stream.cancelled
            && let Some(head) = &stream.head
        {
            head.require_recovery(capabilities.recovery)?;
        }
        let awaiting = stream.head.as_ref().is_some_and(|h| h.awaiting_acceptance);
        if stream.cancelled
            || stream.head.as_ref().is_some_and(|h| {
                h.submission_pending || h.retry_at > now || (h.attempts >= 3 && !awaiting)
            })
        {
            return Ok(None);
        }
        let Some(item) = tx.next_message(agent.id, stream.settled_seq).await? else {
            if let Some(head) = &stream.head {
                if matches!(
                    tx.retained_native(agent.id, head.delivery_id, head.seq)
                        .await?,
                    Some(NativeItem::Notice(_))
                ) {
                    return Ok(None);
                }
                return Err(policy::integrity("Ordered message head disappeared."));
            }
            return Ok(None);
        };
        let mut delivery = item.delivery.clone();
        delivery.start_processing(
            now,
            tx.receipt_settings()
                .await?
                .delivery_confirmation_timeout_seconds,
        )?;
        if super::processing::expire_before_submission(
            tx.as_mut(),
            &mut delivery,
            &mut stream,
            Some(item.message.id),
            now,
        )
        .await?
        {
            tx.commit().await?;
            return Ok(None);
        }
        let receipt_policy = if let Some(head) = &stream.head {
            head.receipt_policy
        } else {
            tx.receipt_settings().await?.input_policy()
        };
        let frame_revision = stream
            .head
            .as_ref()
            .map_or(crate::project::NativeFrameRevision::DispatchV4, |head| {
                head.frame_revision
            });
        let input = input(context, &launch, &item, receipt_policy)?;
        capabilities.require_input(input.addressed)?;
        if let Some(head) = &stream.head
            && (head.delivery_id != input.delivery_id || head.seq != input.seq)
        {
            return Err(policy::integrity(
                "An earlier ordered message head cannot be skipped.",
            ));
        }
        if awaiting {
            let head = stream.head.as_ref().expect("awaiting head");
            let dispatch = MessageDispatch {
                input,
                token: head.token,
                attempt: head.attempts,
                presentation,
                awaiting_acceptance: true,
                frame_revision,
            };
            // Rechecking an existing publication neither submits another body
            // nor consumes an attempt. Release the writer before native I/O.
            tx.rollback().await?;
            return Ok(Some(dispatch));
        }
        let attempt = if let Some(head) = &stream.head {
            head.attempts + 1
        } else {
            1
        };
        let token = Uuid::new_v4();
        stream.head = Some(MessageHead {
            delivery_id: input.delivery_id,
            seq: input.seq,
            incarnation: context.incarnation,
            token,
            attempts: attempt,
            retry_at: now.after_seconds(SUBMISSION_SECONDS as i64)?,
            failure_code: None,
            receipt_policy,
            frame_revision,
            // The first caller still owns publication; recovery may only read
            // this original token, including a crash before its file appears.
            awaiting_acceptance: capabilities.recovery
                == crate::delivery::InputRecovery::OriginalReceipt,
            submission_pending: capabilities.recovery
                == crate::delivery::InputRecovery::RetainUncertainty,
        });
        tx.save_message_stream(&stream).await?;
        // Retain the first processing claim and deadline across every attempt;
        // a native token is a separate uncertainty fence, never a receipt.
        delivery.attempts = delivery
            .attempts
            .checked_add(1)
            .ok_or_else(|| policy::integrity("Message delivery attempts overflow."))?;
        tx.save_delivery(&delivery).await?;
        tx.commit().await?;
        Ok(Some(MessageDispatch {
            input,
            token,
            attempt,
            presentation,
            awaiting_acceptance: false,
            frame_revision,
        }))
    }

    /// Record native acceptance or failure for the exact current head/token.
    /// An unchanged original head may accept its known success after reconnect
    /// within the same current launch; unrelated failures and replaced tokens cannot.
    /// Native acceptance and selected policy effects share one FULL commit.
    pub async fn finish_message(
        &self,
        context: &Context,
        dispatch: &MessageDispatch,
        outcome: Result<HostReceipt>,
    ) -> Result<bool> {
        self.finish_message_progress(context, dispatch, outcome.map(MessageProgress::Accepted))
            .await
    }

    /// Record local publication progress or explicit native input acceptance.
    /// Publication retains the original head/token; only acceptance advances FIFO.
    pub async fn finish_message_progress(
        &self,
        context: &Context,
        dispatch: &MessageDispatch,
        outcome: Result<MessageProgress>,
    ) -> Result<bool> {
        self.commit_ready(vec![crate::application::CommitRequest::MessageCompletion(
            crate::delivery::MessageCompletion {
                context: context.clone(),
                dispatch: dispatch.clone(),
                outcome,
            },
        )])
        .await?
        .pop()
        .expect("one message completion")?
        .message_completion()
    }

    pub(in crate::application) async fn finish_message_in(
        &self,
        tx: &mut dyn Transaction,
        context: &Context,
        dispatch: &MessageDispatch,
        outcome: Result<MessageProgress>,
    ) -> std::result::Result<bool, CompletionError> {
        let Some((agent, launch)) = self.message_launch(tx, context).await? else {
            return Ok(false);
        };
        let same_session = tx.session(agent.id).await?.is_some_and(|session| {
            session.context_id == context.context_id && session.incarnation == context.incarnation
        });
        if dispatch.input.generation != launch.identity.generation {
            return Ok(false);
        }
        let Some(mut stream) = tx.message_stream(launch.identity.generation).await? else {
            return Ok(false);
        };
        let Some(head) = &stream.head else {
            return Ok(false);
        };
        // A result from the original connection may settle only the exact
        // unresolved native request. Acceptance cannot be replayed as output;
        // a positive non-acceptance proof may release its retry fence. Ordinary
        // errors and local publication progress still require the live session.
        if !same_session && !progress::resolves_after_reconnect(head, &outcome) {
            return Ok(false);
        }
        // A new connection may only resolve the unchanged original publication,
        // never borrow its token for output. Its receipt-only failure can release
        // a bounded retry; obsolete connection failures remain fenced above.
        let receipt_poll = dispatch.awaiting_acceptance && head.awaiting_acceptance && same_session;
        if stream.cancelled
            || head.frame_revision != dispatch.frame_revision
            || head.receipt_policy.auto_read != dispatch.input.auto_read
            || head.delivery_id != dispatch.input.delivery_id
            || head.seq != dispatch.input.seq
            || (head.incarnation != context.incarnation && !receipt_poll)
            || head.token != dispatch.token
            || head.attempts != dispatch.attempt
            || (head.failure_code.is_some()
                && !(head.submission_pending && progress::settles(&outcome)))
        {
            return Ok(false);
        }
        let item = match tx
            .retained_native(agent.id, head.delivery_id, head.seq)
            .await?
        {
            Some(NativeItem::Message(item)) => *item,
            _ => return Err(policy::integrity("Ordered message head disappeared.").into()),
        };
        if input(context, &launch, &item, head.receipt_policy)? != dispatch.input {
            return Err(
                policy::integrity("Message completion differs from its immutable head.").into(),
            );
        }
        let eligible = tx
            .membership_boundary(item.channel.id, agent.id)
            .await?
            .is_some_and(|boundary| boundary < item.dispatch.creation_seq);
        if matches!(&outcome, Ok(MessageProgress::NotAccepted(_))) && !eligible {
            let mut delivery = item.delivery;
            delivery.depart(self.clock.now())?;
            tx.save_delivery(&delivery).await?;
            stream.settled_seq = head.seq;
            stream.head = None;
            tx.save_message_stream(&stream).await?;
            return Ok(true);
        }
        progress::apply(
            tx,
            agent.id,
            &mut stream,
            Some(item.message.id),
            outcome,
            self.clock.now(),
        )
        .await?;
        tx.save_message_stream(&stream).await?;
        Ok(true)
    }

    /// Snapshot-only final-hook fence, without the writer or native callbacks.
    /// Expired unpublished claims or uncertain processes cannot authorize output.
    /// A published head's poll time is not its native-acceptance deadline.
    pub async fn message_is_current(
        &self,
        context: &Context,
        generation: Uuid,
        token: Uuid,
        delivery: Uuid,
    ) -> Result<bool> {
        self.stream_is_current(context, generation, token, delivery, false)
            .await
    }

    pub(super) async fn stream_is_current(
        &self,
        context: &Context,
        generation: Uuid,
        token: Uuid,
        delivery: Uuid,
        mixed: bool,
    ) -> Result<bool> {
        self.validate_context(context)?;
        if [generation, token, delivery]
            .into_iter()
            .any(|id| id.is_nil())
        {
            return Ok(false);
        }
        let Some(identity) = self.host.launch_delivery_identity(context).await? else {
            return Ok(false);
        };
        if identity.generation != generation
            || self.host.launch_owner(&identity) != LaunchOwnerState::Current
            || self.launch_root_changed(context, &identity)
        {
            return Ok(false);
        }
        let now = self.clock.now();
        let mut snapshot = self.storage.snapshot().await?;
        let Some(agent) = snapshot
            .conversation(context.host, &context.host_session_id)
            .await?
            .filter(|a| a.joined)
        else {
            return Ok(false);
        };
        if snapshot
            .latest_launch(agent.id)
            .await?
            .is_none_or(|l| l.identity != identity || l.retired_at.is_some())
            || snapshot.session(agent.id).await?.is_none_or(|s| {
                s.context_id != context.context_id
                    || s.incarnation != context.incarnation
                    || !s.is_connected(now)
                    || s.wake_paused
                    || !matches!(
                        s.phase,
                        crate::model::Phase::Idle | crate::model::Phase::Active
                    )
            })
        {
            return Ok(false);
        }
        let Some(stream) = snapshot.message_stream(generation).await? else {
            return Ok(false);
        };
        let Some(head) = stream.head.as_ref().filter(|head| {
            !stream.cancelled
                && head.token == token
                && head.delivery_id == delivery
                && head.incarnation == context.incarnation
                && head.failure_code.is_none()
                && (head.awaiting_acceptance || now < head.retry_at)
        }) else {
            return Ok(false);
        };
        // Re-read the exact head through the same snapshot and recipient scope.
        // This is the last pre-native-I/O fence: channel Leave removes it, and a
        // later rejoin boundary cannot revive a delivery created before rejoin.
        let item = if mixed {
            snapshot.next_native(agent.id, stream.settled_seq).await?
        } else {
            snapshot
                .next_message(agent.id, stream.settled_seq)
                .await?
                .map(|item| NativeItem::Message(Box::new(item)))
        };
        let Some(item) = item else {
            return Ok(false);
        };
        let channel = item.dispatch().channel_id;
        Ok(item.delivery().id == head.delivery_id
            && item.delivery().within_processing_deadline(now)
            && item.dispatch().creation_seq == head.seq
            && match channel {
                Some(channel) => snapshot.is_member(channel, agent.id).await?,
                None => true,
            })
    }

    pub(super) async fn message_authority(
        &self,
        tx: &mut dyn Transaction,
        context: &Context,
    ) -> Result<Option<(Agent, Launch, Session)>> {
        let Some((agent, launch)) = self.message_launch(tx, context).await? else {
            return Ok(None);
        };
        let Some(session) = tx
            .session(agent.id)
            .await?
            .filter(|s| s.context_id == context.context_id && s.incarnation == context.incarnation)
        else {
            return Ok(None);
        };
        Ok(Some((agent, launch, session)))
    }

    pub(super) async fn message_launch(
        &self,
        tx: &mut dyn Transaction,
        context: &Context,
    ) -> Result<Option<(Agent, Launch)>> {
        let Some(binding) = self.host.launch_delivery_identity(context).await? else {
            return Ok(None);
        };
        if self.host.launch_owner(&binding) != LaunchOwnerState::Current
            || self.launch_root_changed(context, &binding)
        {
            return Ok(None);
        }
        let Some(agent) = tx
            .conversation(context.host, &context.host_session_id)
            .await?
            .filter(|a| a.joined)
        else {
            return Ok(None);
        };
        let Some(launch) = tx
            .latest_launch(agent.id)
            .await?
            .filter(|l| l.identity == binding && l.retired_at.is_none())
        else {
            return Ok(None);
        };
        Ok(Some((agent, launch)))
    }
}

pub(super) use super::input::message as input;
