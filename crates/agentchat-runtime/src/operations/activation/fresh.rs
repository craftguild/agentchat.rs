//! Native facts precede publication of a fresh participation descriptor.
use super::{Control, Operations, required};
use crate::context::{ContextRegistry, ContextSeed};
use agentchat_core::{
    Result,
    model::{LaunchIdentity, LaunchOwnerState},
};
use agentchat_hosts::{
    claude::adapter::NativeAdapter as ClaudeAdapter,
    codex::{NativeAdapter as CodexAdapter, NativeRegistry},
    launch::{Binding, ClaudeReady, ClaudeStartup, Endpoint, NativeProof, Registry},
};
use std::time::Duration;
use uuid::Uuid;

pub(in crate::operations) enum Native {
    Codex {
        proxy: agentchat_hosts::codex::ProxyEvents,
    },
    Claude {
        startup: ClaudeStartup,
        ready: Box<ClaudeReady>,
    },
    #[cfg(test)]
    Inert,
}
impl Control {
    pub(super) async fn fresh(
        &mut self,
        operations: &Operations,
        proof: &NativeProof,
        new_generation: bool,
    ) -> Result<Binding> {
        let home = operations.manager.home();
        let context = match &mut self.native {
            Native::Codex { .. } => {
                let old = &self.binding.context;
                ContextRegistry::new(operations.manager.clone())
                    .prepare(ContextSeed {
                        host: old.host,
                        host_session_id: old.host_session_id.clone(),
                        host_version: old.host_version.clone(),
                        os_profile: old.os_profile.clone(),
                        working_directory: old.working_directory.clone(),
                    })
                    .await?
            }
            Native::Claude { startup, ready } => {
                let adapter = ClaudeAdapter::new(home.into());
                // Publish no invented application descriptor for Claude. The
                // actual SDK must recapture its lifecycle and reserve a fresh
                // native callback generation before Core can bind that context.
                if let Some(observed) = startup.observe_current_proof(home, proof, &adapter).await?
                {
                    **ready = observed;
                }
                if ready.context().context_id == self.binding.context.context_id {
                    ready.refresh_from_proof(home, proof).await?;
                }
                let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
                loop {
                    if let Some(observed) =
                        startup.observe_current_proof(home, proof, &adapter).await?
                        && observed.context().context_id != self.binding.context.context_id
                    {
                        **ready = observed;
                        break ready.context().clone();
                    }
                    if tokio::time::Instant::now() >= deadline {
                        return Err(required(Some(&self.binding.context)));
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            }
            #[cfg(test)]
            Native::Inert => return Err(required(Some(&self.binding.context))),
        };
        if proof.state() != LaunchOwnerState::Current
            || !super::wire::same_scope(&context, &self.binding.context)
        {
            return Err(required(Some(&context)));
        }
        Binding::new(
            context,
            LaunchIdentity {
                generation: if new_generation {
                    Uuid::new_v4()
                } else {
                    self.binding.identity.generation
                },
                owner: proof.identity().owner.clone(),
            },
            self.binding.endpoint.clone(),
        )
    }
    pub(super) async fn publish(&self, operations: &Operations, binding: &Binding) -> Result<()> {
        ContextRegistry::new(operations.manager.clone())
            .publish(binding.context.clone())
            .await?;
        match binding.endpoint {
            Endpoint::Claude => Registry::new(operations.manager.home()).publish(binding),
            Endpoint::Codex { .. } => NativeRegistry::new(operations.manager.home())
                .publish(binding)
                .map(|_| ()),
        }?;
        match binding.endpoint {
            Endpoint::Claude => {
                ClaudeAdapter::new(operations.manager.home().into())
                    .observe(&binding.context)
                    .await?;
            }
            Endpoint::Codex { .. } => {
                CodexAdapter::new(operations.manager.home().into())
                    .observe(&binding.context)
                    .await?;
            }
        }
        Ok(())
    }

    pub(super) fn mark_delivery_started(&self, registry: &Registry) -> Result<()> {
        self.track_codex_binding(&self.binding)?;
        let started = self
            .started
            .as_ref()
            .ok_or_else(|| required(Some(&self.binding.context)))?;
        registry.mark_started(&self.binding, started.boundary)
    }

    pub(super) fn mark_delivery_if_started(&self, registry: &Registry) -> Result<()> {
        self.started
            .as_ref()
            .map_or(Ok(()), |_| self.mark_delivery_started(registry))
    }
}

impl Control {
    /// Restore already-active participation after runtime loss. The query is
    /// read-only; never issue Join, renew a left session, or allocate a boundary.
    pub async fn tick(
        &mut self,
        operations: &Operations,
        route: &super::Route,
        proof: &NativeProof,
    ) -> Result<()> {
        if self.pending_request.is_some() || proof.state() != LaunchOwnerState::Current {
            return Ok(());
        }
        if self.initial_waiting {
            self.activate_initial(operations, route, proof).await?;
            return Ok(());
        }
        if self
            .advance_codex_transition(operations, route, proof)
            .await?
        {
            return Ok(());
        }
        if self.prepare_claude_transition(operations, proof).await? {
            self.activate_transition(operations, route).await?;
            return Ok(());
        }
        if self.started.is_none() {
            return Ok(());
        }
        if self.pending.is_none()
            && let Native::Claude { startup, ready } = &mut self.native
            && let Some(observed) = startup
                .observe_current_proof(
                    operations.manager.home(),
                    proof,
                    &ClaudeAdapter::new(operations.manager.home().into()),
                )
                .await?
            && observed.context().context_id != self.binding.context.context_id
        {
            **ready = observed;
            self.pending = Some(Binding::new(
                ready.context().clone(),
                self.binding.identity.clone(),
                self.binding.endpoint.clone(),
            )?);
        }
        if self.pending.is_some() {
            self.restore_pending_attachment(operations).await?;
            return Ok(());
        }
        let mut client = operations.connect(route).await?;
        let state = client
            .native_binding(self.binding.context.context_id)
            .await?;
        if state.participation != agentchat_core::read::Participation::Joined {
            self.attachment = None;
            self.started = None;
            return Ok(());
        }
        if let Some(attachment) = &self.attachment
            && attachment.check().await.is_ok()
        {
            return Ok(());
        }
        self.attachment = None;
        let target = agentchat_protocol::operations::Target {
            context_id: Some(self.binding.context.context_id),
            ..Default::default()
        };
        match operations.attachment(target).await {
            Ok(attachment) => self.attachment = Some(attachment),
            Err(error)
                if matches!(
                    error.code,
                    agentchat_core::ErrorCode::IdentityConflict
                        | agentchat_core::ErrorCode::SessionRequired
                ) =>
            {
                // A dead owned MCP can remove its native callback before Core
                // can rebind the session. Ask the original live SDK to refresh;
                // only a new exact native challenge may restore this active row.
                let replacement = self.fresh(operations, proof, false).await?;
                // Retain before publication. Later ticks retry this exact
                // incarnation; an ambiguous Attach cannot mint another one.
                self.pending = Some(replacement);
            }
            Err(error) => return Err(error),
        }
        Ok(())
    }

    async fn prepare_claude_transition(
        &mut self,
        operations: &Operations,
        proof: &NativeProof,
    ) -> Result<bool> {
        let (startup, previous_epoch) = match &self.native {
            Native::Codex { .. } => return Ok(false),
            Native::Claude { startup, ready } => (startup.clone(), ready.native_generation()),
            #[cfg(test)]
            Native::Inert => return Ok(false),
        };
        let observed = match startup
            .observe_current_proof(
                operations.manager.home(),
                proof,
                &ClaudeAdapter::new(operations.manager.home().into()),
            )
            .await?
        {
            Some(observed) => observed,
            None => return Ok(self.transition.is_some()),
        };
        if let Some(pending) = &self.transition
            && pending.binding.context == *observed.context()
            && pending
                .observed
                .as_ref()
                .is_some_and(|prior| prior.native_generation() == observed.native_generation())
        {
            return Ok(true);
        }
        if self.transition.is_some() {
            self.transition = None;
            self.pending = None;
        }
        let registry = Registry::new(operations.manager.home());
        let departed = registry.root_changed(
            self.binding.context.project_id,
            self.binding.context.host,
            &self.binding.context.host_session_id,
            &self.binding.identity,
        );
        if !departed {
            return Ok(false);
        }
        if observed.native_generation() <= previous_epoch
            || observed.context().os_profile != self.binding.context.os_profile
            || observed.context().context_id == self.binding.context.context_id
        {
            return Err(required(Some(observed.context())));
        }
        let binding = Binding::new(
            observed.context().clone(),
            LaunchIdentity {
                generation: Uuid::new_v4(),
                owner: proof.identity().owner.clone(),
            },
            self.binding.endpoint.clone(),
        )?;
        self.publish(operations, &binding).await?;
        let authorization = registry.publish_ready(&binding, self.socket.path().into(), None)?;
        self.transition = Some(super::PendingTransition {
            request_id: Uuid::new_v4(),
            cause: agentchat_core::application::ManagedActivationCause::DistinctRoot {
                predecessor_context: self.binding.context.context_id,
                predecessor_generation: self.binding.identity.generation,
                transition_epoch: observed.native_generation(),
            },
            binding,
            authorization,
            observed: Some(observed),
        });
        Ok(true)
    }
}
