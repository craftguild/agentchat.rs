//! Read-only final delivery fence; native freshness is independently required.

use super::Application;
use crate::{
    Result,
    delivery::BatchState,
    model::{Context, Phase},
};
use uuid::Uuid;

impl Application {
    /// Check whether an exact dispatch still belongs to the current attachment.
    ///
    /// Static and final native hooks use this after receiving their fresh native
    /// permit. File metadata alone can outlive leave, reconnect, ACK or retry.
    /// This snapshot never renews presence, reconciles a batch, invokes native
    /// getters or acquires the writer. In particular it cannot deadlock against
    /// the native callback whose own short-lived permit is being checked.
    pub async fn wake_is_current(
        &self,
        context: &Context,
        batch: Uuid,
        token: Uuid,
        delivery_ids: &[Uuid],
    ) -> Result<bool> {
        self.validate_context(context)?;
        if batch.is_nil()
            || token.is_nil()
            || delivery_ids.is_empty()
            || delivery_ids.len() > crate::delivery::MAX_WAKE_EVENTS
            || delivery_ids.iter().any(Uuid::is_nil)
            || delivery_ids.windows(2).any(|pair| pair[0] == pair[1])
            || delivery_ids
                .iter()
                .enumerate()
                .any(|(index, id)| delivery_ids[..index].contains(id))
        {
            return Ok(false);
        }
        let identity = self.host.launch_delivery_identity(context).await?;
        match &identity {
            Some(identity)
                if self.host.launch_owner(identity) == crate::model::LaunchOwnerState::Current => {}
            None if self.host.allows_unmanaged_participation() => {}
            _ => return Ok(false),
        }
        let now = self.clock.now();
        let mut snapshot = self.storage.snapshot().await?;
        let Some(agent) = snapshot
            .conversation(context.host, &context.host_session_id)
            .await?
            .filter(|agent| agent.joined)
        else {
            return Ok(false);
        };
        match (snapshot.latest_launch(agent.id).await?, identity) {
            (Some(launch), Some(identity))
                if launch.identity == identity && launch.retired_at.is_none() => {}
            (None, None) if self.host.allows_unmanaged_participation() => {}
            _ => return Ok(false),
        }
        let Some(session) = snapshot.session(agent.id).await? else {
            return Ok(false);
        };
        if session.context_id != context.context_id
            || session.incarnation != context.incarnation
            || !session.is_connected(now)
            || session.wake_paused
            || !matches!(session.phase, Phase::Idle | Phase::Active)
        {
            return Ok(false);
        }
        let Some(batch) = snapshot.wake_batch(batch).await? else {
            return Ok(false);
        };
        if batch.recipient_id != agent.id
            || batch.incarnation != context.incarnation
            || batch.dispatch_ref != Some(token)
            || super::policy::dispatch_token(&batch, delivery_ids) != token
            || batch.failure_code.is_some()
            || !match batch.state {
                // An orphaned/expired submission must not regain authority
                // merely because a later shared file signal reaches its hook.
                BatchState::Pending => batch.next_eligible_at.is_some_and(|end| now < end),
                // Successful acceptance may precede the final prompt callback;
                // only the current, unfailed token remains valid in that phase.
                BatchState::Inflight => true,
                _ => false,
            }
        {
            return Ok(false);
        }
        let exact = snapshot
            .wake_batch_exact_deliveries(agent.id, batch.id)
            .await?;
        if exact
            .iter()
            .any(|delivery| !delivery.within_processing_deadline(now))
        {
            return Ok(false);
        }
        let exact = exact
            .into_iter()
            .map(|delivery| delivery.id)
            .collect::<Vec<_>>();
        if exact != delivery_ids {
            return Ok(false);
        }
        Ok(snapshot.wake_batch_deliveries(agent.id, batch.id).await? == exact)
    }
}
