//! Installed routing must agree with the current owner cursor and endpoint.
use super::Bridge;
use agentchat_core::{Error, ErrorCode, Result};
use agentchat_hosts::claude::NativeLifecycle;

impl Bridge {
    pub(super) fn pending_launch(&self, lifecycle: &NativeLifecycle) -> Result<bool> {
        let Some(scope) = agentchat_hosts::launch::StartupScope::from_environment()? else {
            return Ok(false);
        };
        let registry = agentchat_hosts::launch::Registry::new(self.manager.home());
        if let NativeLifecycle::Project(target) = lifecycle
            && registry.managed_transition_pending(&scope, &self.physical_owner, &target.context)?
        {
            return Ok(true);
        }
        // Locators only permit a bounded Attach wait while the original
        // supervisor commits. They cannot join, publish a binding, clear pause
        // or override core launch authority. Other roots/owners stay ordinary.
        let state = registry.startup_state(
            &scope,
            &self.physical_owner,
            lifecycle.root(),
            lifecycle.working_directory(),
            lifecycle.host_version(),
        )?;
        Ok(
            matches!(state, agentchat_hosts::launch::StartupState::Waiting)
                && scope.root == lifecycle.root(),
        )
    }
    pub(super) async fn check_cursor(&self, lifecycle: &NativeLifecycle) -> Result<()> {
        if let Some(cursor) = &self.cursor {
            let current = cursor.current().await?.ok_or_else(required)?;
            if current.lifecycle != *lifecycle
                || current.reservation.transport_nonce != Some(self.transport_nonce)
            {
                return Err(required());
            }
        }
        Ok(())
    }
}
fn required() -> Error {
    Error::new(
        ErrorCode::SessionRequired,
        "The native lifecycle needs fresh registration for this transport.",
    )
}
