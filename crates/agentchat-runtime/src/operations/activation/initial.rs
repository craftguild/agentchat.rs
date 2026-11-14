//! Initial participation waits for exact native rollback without owning native lifetime.
use super::{Control, Operations, Route, required};
use agentchat_core::{
    Error, ErrorCode, Result,
    application::{ManagedActivationCause, ManagedActivationResult},
    model::LaunchOwnerState,
    read::Participation,
};
use agentchat_hosts::launch::{NativeProof, Registry};
use agentchat_protocol::rpc::{Call, success};
use std::time::Duration;

impl Control {
    /// Commit the original initial intent, or retain it while native observation
    /// is fenced. False grants no application authority: the physical-ready
    /// supervisor keeps its native child and retries this intent on later ticks.
    /// Historical success after Leave is also inactive and never reopens Join.
    pub async fn activate_initial(
        &mut self,
        operations: &Operations,
        route: &Route,
        proof: &NativeProof,
    ) -> Result<bool> {
        self.initial_waiting = true;
        match self.attempt_initial(operations, route, proof).await {
            Err(error)
                if error.retryable
                    || matches!(
                        error.code,
                        ErrorCode::IoError | ErrorCode::RuntimeUnavailable
                    ) =>
            {
                // Read/observation ACK loss is also after physical readiness.
                // Retain Control just like an ambiguous Core RPC response.
                Ok(false)
            }
            result => result,
        }
    }

    async fn attempt_initial(
        &mut self,
        operations: &Operations,
        route: &Route,
        proof: &NativeProof,
    ) -> Result<bool> {
        let registry = Registry::new(operations.manager.home());
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            // A transition retry cannot borrow a replacement descriptor or owner.
            if registry.authorization(&self.binding.context)?.as_ref() != Some(&self.ready)
                || proof.identity() != &self.binding.identity
                || proof.state() != LaunchOwnerState::Current
            {
                return Err(required(Some(&self.binding.context)));
            }
            self.drain_proxy_events(&registry)?;
            if self.initial_fenced(&registry) {
                if self.codex_candidate.is_some_and(|candidate| candidate.2)
                    || self.codex_release.is_some()
                    || self.transition.is_some()
                    || registry.root_departed(
                        self.binding.context.project_id,
                        self.binding.context.host,
                        &self.binding.context.host_session_id,
                        &self.binding.identity,
                    )
                    || tokio::time::Instant::now() >= deadline
                {
                    return Ok(false);
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
            if self.pending.is_none() {
                self.track_codex_binding(&self.binding)?;
            }
            match self
                .commit_initial(operations, route, &registry, proof)
                .await
            {
                Ok(active) => return Ok(active),
                Err(error) => {
                    // A candidate can arrive after the pre-call drain, during
                    // either Core verification or attachment. Consume its exact
                    // outcome before retrying the unchanged durable intent.
                    let rolled_back = self.drain_proxy_events(&registry)?;
                    if error.code == ErrorCode::SessionRequired
                        && (rolled_back || self.initial_fenced(&registry))
                    {
                        continue;
                    }
                    if !error.retryable
                        && !matches!(
                            error.code,
                            ErrorCode::IoError | ErrorCode::RuntimeUnavailable
                        )
                    {
                        return Err(error);
                    }
                    if tokio::time::Instant::now() >= deadline {
                        // A lost commit/attach reply keeps the exact request in
                        // this physical-ready Control, not the native exit path.
                        return Ok(false);
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    }

    fn initial_fenced(&self, registry: &Registry) -> bool {
        self.codex_candidate.is_some()
            || self.codex_release.is_some()
            || self.transition.is_some()
            || registry.root_changed(
                self.binding.context.project_id,
                self.binding.context.host,
                &self.binding.context.host_session_id,
                &self.binding.identity,
            )
    }

    async fn commit_initial(
        &mut self,
        operations: &Operations,
        route: &Route,
        registry: &Registry,
        proof: &NativeProof,
    ) -> Result<bool> {
        if self.initial_committed.is_none() {
            let mut client = operations.connect(route).await?;
            let value = success(
                client
                    .call(
                        self.initial_request,
                        Call::ActivateManaged {
                            request_id: self.initial_request,
                            context_id: self.binding.context.context_id,
                            cause: ManagedActivationCause::Initial,
                            name: self.ready.preferred_name().map(str::to_owned),
                        },
                    )
                    .await?,
            )?;
            let activated: ManagedActivationResult =
                serde_json::from_value(value).map_err(|_| required(Some(&self.binding.context)))?;
            self.initial_committed = Some(activated);
        }
        if self.pending.is_some() {
            // Retain this single replacement across ambiguous publish/Attach.
            // The receipt's original context/name/cause are never rewritten.
            match self.restore_pending_attachment(operations).await {
                Err(error) if error.code == ErrorCode::IdentityConflict => {
                    return self
                        .replace_disconnected_pending(operations, route, proof, error)
                        .await;
                }
                result => result?,
            }
        } else {
            let activated = self
                .initial_committed
                .as_ref()
                .expect("known initial receipt")
                .clone();
            let attachment = if self.binding.context.context_id == activated.context_id {
                self.finish_activation(operations, route, &activated).await
            } else {
                self.finish_recovered_initial(operations, route, &activated)
                    .await
            };
            match attachment {
                Err(error)
                    if matches!(
                        error.code,
                        ErrorCode::IdentityConflict | ErrorCode::SessionRequired
                    ) && self.started.as_ref().is_some_and(|started| {
                        started.context == self.binding.context
                            && started.identity == self.binding.identity
                            && started.identity.generation == activated.generation
                            && started.agent_id == activated.agent_id
                            && started.boundary == activated.join_boundary
                    }) =>
                {
                    // Core correctly refuses a disconnected incarnation. Only
                    // this committed, still-joined activation may recover with
                    // a fresh verified descriptor under its original live owner.
                    self.attachment = None;
                    self.pending = Some(self.fresh(operations, proof, false).await?);
                    return Ok(false);
                }
                outcome => outcome?,
            }
        }
        if self.started.is_none() {
            self.initial_waiting = false;
            return Ok(false);
        }
        // Startup ordering is historical for this immutable launch/scope. A
        // same-generation incarnation repair preserves its proved first marker
        // instead of rewriting it with the replacement descriptor.
        if !registry.was_started(&self.binding)? {
            self.mark_delivery_started(registry)?;
        }
        self.initial_waiting = false;
        Ok(true)
    }

    async fn replace_disconnected_pending(
        &mut self,
        operations: &Operations,
        route: &Route,
        proof: &NativeProof,
        refused: Error,
    ) -> Result<bool> {
        let pending = self
            .pending
            .as_ref()
            .expect("retained attachment intent")
            .clone();
        let committed = self
            .initial_committed
            .as_ref()
            .expect("known initial receipt");
        self.track_codex_binding(&pending)?;
        let mut read = operations.connect(route).await?;
        let state = read.native_binding(pending.context.context_id).await?;
        if state.participation == Participation::Left {
            self.binding = pending;
            self.pending = None;
            self.attachment = None;
            self.started = None;
            self.initial_waiting = false;
            return Ok(false);
        }
        if state.participation != Participation::Joined
            || state.agent.as_ref().is_none_or(|agent| {
                agent.agent.id != committed.agent_id
                    || agent.connected
                    || agent.incarnation != Some(pending.context.incarnation)
            })
            || state
                .native_delivery
                .as_ref()
                .is_none_or(|delivery| delivery.generation != committed.generation)
            || pending.identity.generation != committed.generation
        {
            return Err(refused);
        }
        // This is a positive read of the exact published incarnation's death,
        // not a retry classification for arbitrary identity errors. An unknown
        // Attach/read keeps its one pending descriptor; this definite stale
        // incarnation instead requires a fresh verified same-generation route.
        self.pending = Some(self.fresh(operations, proof, false).await?);
        Ok(false)
    }
}
