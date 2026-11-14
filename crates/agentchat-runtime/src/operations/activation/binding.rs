//! Current participation tracking precedes restoration or delivery enablement.
use super::{Control, Native, Operations, Route, StartedLaunch};
use agentchat_core::{ErrorCode, Result, application::ManagedActivationResult};
use agentchat_hosts::launch::Binding;
use agentchat_protocol::operations::Target;
impl Control {
    pub(super) fn track_codex_binding(&self, binding: &Binding) -> Result<()> {
        if let Native::Codex { proxy } = &self.native {
            proxy.track(binding)?;
        }
        Ok(())
    }
    pub(super) async fn finish_activation(
        &mut self,
        operations: &Operations,
        route: &Route,
        activated: &ManagedActivationResult,
    ) -> Result<()> {
        let context_id = activated.context_id;
        let generation = activated.generation;
        if let Some(pending) = &self.pending
            && pending.identity.generation == generation
            && pending.context.context_id == context_id
        {
            self.track_codex_binding(pending)?;
            self.binding = pending.clone();
            self.pending = None;
            self.pending_request = None;
        }
        if self.binding.identity.generation == generation
            && self.binding.context.context_id == context_id
        {
            self.attach_committed_binding(operations, route, activated)
                .await?;
        }
        Ok(())
    }

    /// A recovered descriptor may finish the known initial receipt only for
    /// its exact retained agent, launch and boundary. Recheck current state even
    /// if the start-marker response was lost after successful attachment.
    pub(super) async fn finish_recovered_initial(
        &mut self,
        operations: &Operations,
        route: &Route,
        activated: &ManagedActivationResult,
    ) -> Result<()> {
        if self.binding.identity.generation != activated.generation
            || self.started.as_ref().is_none_or(|started| {
                started.context != self.binding.context
                    || started.identity != self.binding.identity
                    || started.agent_id != activated.agent_id
                    || started.boundary != activated.join_boundary
            })
        {
            return Err(super::required(Some(&self.binding.context)));
        }
        self.attach_committed_binding(operations, route, activated)
            .await
    }

    async fn attach_committed_binding(
        &mut self,
        operations: &Operations,
        route: &Route,
        activated: &ManagedActivationResult,
    ) -> Result<()> {
        let context_id = self.binding.context.context_id;
        {
            let mut read = operations.connect(route).await?;
            let state = read.native_binding(context_id).await?;
            if state.participation != agentchat_core::read::Participation::Joined {
                self.attachment = None;
                self.started = None;
                return Ok(());
            }
            self.started = Some(StartedLaunch {
                context: self.binding.context.clone(),
                identity: self.binding.identity.clone(),
                agent_id: activated.agent_id,
                boundary: activated.join_boundary,
            });
            if let Some(attachment) = &self.attachment
                && attachment.check().await.is_ok()
            {
                return Ok(());
            }
            // Attach only current joined state. A replayed historical success
            // after Leave returns its receipt without restoration/heartbeats.
            let target = Target {
                context_id: Some(context_id),
                ..Target::default()
            };
            match operations.attachment(target).await {
                Ok(attachment) => {
                    self.attachment = Some(attachment);
                }
                Err(error) if error.code == ErrorCode::NotJoined => {
                    self.attachment = None;
                    self.started = None;
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    /// Restore a retained descriptor without issuing a participation mutation.
    /// Both initial and later recovery use the same publication/native checks.
    pub(super) async fn restore_pending_attachment(
        &mut self,
        operations: &Operations,
    ) -> Result<()> {
        let pending = self
            .pending
            .as_ref()
            .expect("retained recovery descriptor")
            .clone();
        self.publish(operations, &pending).await?;
        // Restoration can enable delivery before attachment returns. Tracking
        // first refuses moving a still-unresolved fence to this descriptor.
        self.track_codex_binding(&pending)?;
        let target = Target {
            context_id: Some(pending.context.context_id),
            ..Target::default()
        };
        let attachment = match operations.attachment(target).await {
            Ok(value) => value,
            Err(error) if error.code == ErrorCode::NotJoined => {
                self.binding = pending;
                self.pending = None;
                self.attachment = None;
                self.started = None;
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        self.binding = pending;
        if let Some(started) = self.started.as_mut() {
            started.context = self.binding.context.clone();
        }
        self.pending = None;
        self.attachment = Some(attachment);
        Ok(())
    }
}
