//! Fresh designated-owner proof plus the exact shared server's current controls.
mod input;
use super::{Client, NativeRegistry, NativeRoute, required};
use crate::launch::Endpoint;
use agentchat_core::{Result, model::Context, ports::HostState};
use std::path::PathBuf;

/// Non-admitting Codex observation and complete-body handoff component. Every
/// operation rechecks original native authority; constructor, loaded metadata
/// and MCP activity grant none. MessagePort consumes an application-committed
/// head; the application serializes both routes and owns retry/sequence policy.
#[derive(Clone)]
pub struct NativeAdapter {
    registry: NativeRegistry,
}
impl NativeAdapter {
    /// Declare mechanisms without probing a native host or granting permission.
    pub fn input_capabilities(&self) -> agentchat_core::delivery::InputCapabilities {
        use agentchat_core::delivery::{ActiveInput, InputCapabilities, InputRecovery};
        InputCapabilities::complete(InputRecovery::RetainUncertainty, ActiveInput::ExactTurnId)
    }
    /// Capture the explicit state home without probing or creating anything.
    pub fn new(home: PathBuf) -> Self {
        Self {
            registry: NativeRegistry::new(home),
        }
    }
    /// Verify the fixed launch association and original owners before and after
    /// metadata-only native observation. Esc creates no application pause latch;
    /// unknown native controls or owner observations never authorize output.
    pub async fn observe(&self, context: &Context) -> Result<HostState> {
        self.observe_route(context).await.map(|(_, state)| state)
    }
    /// Return the exact currently verified immutable route for native callers.
    /// This is not participation or version admission and never renews either.
    pub async fn observe_route(&self, context: &Context) -> Result<(NativeRoute, HostState)> {
        let before = self.registry.observe(context)?;
        let Endpoint::Codex { socket, .. } = &before.binding().endpoint else {
            return Err(required());
        };
        let mut client = Client::connect(socket, &context.host_version).await?;
        let state = client.observe(&context.host_session_id).await?;
        let after = self.registry.observe(context)?;
        if before != after {
            return Err(required());
        }
        Ok((after, state))
    }
}
