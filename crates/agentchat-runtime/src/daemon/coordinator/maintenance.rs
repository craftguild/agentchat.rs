//! Existing housekeeping reconciles authority before allowing further native I/O.
use super::Worker;
use crate::diagnostics::Event;
use agentchat_core::{Error, ErrorCode};

impl Worker {
    pub(super) async fn maintain(&mut self) {
        self.collect_staging().await;
        let contexts = self
            .attached
            .iter()
            .map(|(id, c)| (*id, c.clone()))
            .collect::<Vec<_>>();
        for (connection, context) in contexts {
            let live = async {
                self.registry
                    .resolve(context.context_id, Some(self.project.identity().id))
                    .await?;
                let binding = self.app.native_binding(&context).await?;
                Ok::<_, Error>(binding.agent.is_some_and(|a| a.connected && a.agent.joined))
            }
            .await;
            if !matches!(live, Ok(true)) {
                self.remove(connection);
            }
        }
        let pending = self.pending.values().cloned().collect::<Vec<_>>();
        for context in pending {
            match self.app.detach(&context).await {
                Ok(()) => {
                    self.pending.remove(&context.context_id);
                }
                Err(error)
                    if matches!(
                        error.code,
                        ErrorCode::IdentityConflict
                            | ErrorCode::NotJoined
                            | ErrorCode::SessionRequired
                    ) =>
                {
                    self.pending.remove(&context.context_id);
                }
                Err(error) => {
                    self.control_error(Event::MaintenanceFailed { code: error.code }, error.code)
                }
            }
        }
        // A failed authoritative detach cannot turn into automatic renewal.
        if self.pending.is_empty() {
            match self.app.maintain().await {
                Ok(result) => {
                    if let Some(head) = result.event_head {
                        self.heads.send_replace(head.cursor());
                    }
                }
                Err(error) => {
                    self.control_error(Event::MaintenanceFailed { code: error.code }, error.code)
                }
            }
        }
        self.reconcile_delivery_contexts().await;
    }

    async fn collect_staging(&mut self) {
        let replacement = agentchat_hosts::staging::OriginReaper::new(self.manager.home())
            .with_context_validator(crate::context::ContextRegistry::validate_stage);
        let mut staging = std::mem::replace(&mut self.staging, replacement);
        match crate::project::blocking(move || {
            staging.step()?;
            Ok(staging)
        })
        .await
        {
            Ok(staging) => self.staging = staging,
            Err(error) => {
                self.control_error(Event::MaintenanceFailed { code: error.code }, error.code)
            }
        }
    }
}
