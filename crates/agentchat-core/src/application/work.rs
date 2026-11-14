//! Transaction-local event construction and durable fan-out.

use std::collections::BTreeSet;

use serde_json::Value;
use uuid::Uuid;

use crate::{
    Error, ErrorCode, Result,
    model::{
        Agent, Context, Delivery, DeliveryState, Dispatch, DispatchKind, DispatchNotice,
        DispatchPayload, Event, Message, NoticeKind,
    },
    ports::Transaction,
    sequence::Sequence,
    time::Timestamp,
};

pub(super) struct Work<'a> {
    pub tx: &'a mut dyn Transaction,
    pub now: Timestamp,
    pub last_seq: Option<Sequence>,
}

impl Work<'_> {
    pub async fn allocate(&mut self, kind: &str) -> Result<Event> {
        let event = self.tx.allocate_event(kind.parse()?, self.now).await?;
        self.last_seq = Some(event.seq);
        Ok(event)
    }

    /// Record an occurrence, then explicitly apply the existing notification policy.
    /// The storage event operation itself never creates recipient work.
    pub async fn record_and_notify(
        &mut self,
        event: &Event,
        wake: &BTreeSet<Uuid>,
    ) -> Result<usize> {
        // Message creation has a separate explicit sending aggregate path.
        NoticeKind::try_from(event.kind)?;
        self.tx.publish_event(event).await?;
        let recipients = self.recipients(event.channel_id, event.actor_id).await?;
        if recipients.is_empty() {
            return Ok(0);
        }
        self.send_notice(event, &recipients, wake).await?;
        Ok(recipients.len())
    }

    /// An explicitly selected author notification retains a coalescing identity
    /// even when its author currently has no native receiving obligation.
    pub async fn record_direct_notice(
        &mut self,
        event: &Event,
        recipients: &[Uuid],
    ) -> Result<Uuid> {
        self.tx.publish_event(event).await?;
        self.send_notice(event, recipients, &recipients.iter().copied().collect())
            .await
    }

    async fn send_notice(
        &mut self,
        event: &Event,
        recipients: &[Uuid],
        wake: &BTreeSet<Uuid>,
    ) -> Result<Uuid> {
        let kind = NoticeKind::try_from(event.kind)?;
        let dispatch = self
            .tx
            .allocate_dispatch(DispatchKind::Notice, event.channel_id, self.now)
            .await?;
        let payload = DispatchPayload::Notice(DispatchNotice {
            dispatch_id: dispatch.id,
            kind,
            data: event.data.clone(),
        });
        self.send(&dispatch, &payload, recipients, wake).await?;
        Ok(dispatch.id)
    }

    /// Conversation content has its own Header; the audit event is not its owner.
    pub async fn send_message(
        &mut self,
        dispatch: &Dispatch,
        message: &Message,
        wake: &BTreeSet<Uuid>,
    ) -> Result<usize> {
        let recipients = self
            .recipients(dispatch.channel_id, Some(message.author_id))
            .await?;
        self.send(
            dispatch,
            &DispatchPayload::Message(message.clone()),
            &recipients,
            wake,
        )
        .await?;
        Ok(recipients.len())
    }

    async fn recipients(
        &mut self,
        channel: Option<Uuid>,
        sender: Option<Uuid>,
    ) -> Result<Vec<Uuid>> {
        let recipients = if let Some(channel) = channel {
            self.tx.channel_members(channel).await?
        } else {
            self.tx.joined_agents().await?
        };
        Ok(recipients
            .into_iter()
            .filter(|actor| sender != Some(actor.id))
            .map(|actor| actor.id)
            .collect())
    }

    async fn send(
        &mut self,
        dispatch: &Dispatch,
        payload: &DispatchPayload,
        recipients: &[Uuid],
        wake: &BTreeSet<Uuid>,
    ) -> Result<()> {
        dispatch.validate_payload(payload)?;
        let deliveries = recipients
            .iter()
            .map(|recipient| Delivery {
                id: Uuid::new_v4(),
                dispatch_id: dispatch.id,
                recipient_id: *recipient,
                state: DeliveryState::Pending,
                wake_eligible: wake.contains(recipient),
                wake_batch_id: None,
                attempts: 0,
                history: crate::model::DeliveryHistory::queued(dispatch.created_at),
            })
            .collect::<Vec<_>>();
        self.tx
            .publish_dispatch(dispatch, payload, &deliveries)
            .await
    }

    pub async fn informational(
        &mut self,
        kind: &str,
        actor: Option<Uuid>,
        channel: Option<Uuid>,
        data: Value,
    ) -> Result<Event> {
        let mut event = self.allocate(kind).await?;
        event.actor_id = actor;
        event.channel_id = channel;
        event.data = data;
        self.record_and_notify(&event, &BTreeSet::new()).await?;
        Ok(event)
    }

    pub async fn actor(&mut self, context: &Context, joined: bool, live: bool) -> Result<Agent> {
        let agent = self
            .tx
            .conversation(context.host, &context.host_session_id)
            .await?
            .ok_or_else(|| {
                Error::new(
                    ErrorCode::NotJoined,
                    "This exact host conversation has not joined.",
                )
            })?;
        if joined && !agent.joined {
            return Err(Error::new(
                ErrorCode::NotJoined,
                "This identity explicitly left the project.",
            ));
        }
        let session = self.tx.session(agent.id).await?.ok_or_else(|| {
            Error::new(
                ErrorCode::SessionRequired,
                "A bound host connection is required.",
            )
        })?;
        if session.incarnation != context.incarnation || session.context_id != context.context_id {
            return Err(Error::new(
                ErrorCode::IdentityConflict,
                "Context belongs to an obsolete connection incarnation.",
            ));
        }
        if live && !session.is_connected(self.now) {
            return Err(Error::new(
                ErrorCode::SessionRequired,
                "Host conversation liveness is no longer valid.",
            ));
        }
        Ok(agent)
    }
}

pub(super) fn not_found(message: &str) -> Error {
    Error::new(ErrorCode::NotFound, message)
}
