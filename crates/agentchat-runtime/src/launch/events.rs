//! Diagnostic launch observation never substitutes for native/application proof.
use super::StartedLaunch;
use agentchat_core::{Error, Result};
use agentchat_hosts::launch::{NativeChild, NativeTerminal};
use std::process::ExitStatus;
use uuid::Uuid;

/// Exact Codex startup selection. No prompt, arbitrary native flag or policy
/// override is accepted; a fresh thread is materialized when resume is absent.
pub struct CodexLaunchRequest {
    /// Exact canonical native UUID to resume, otherwise create a fresh root.
    pub resume: Option<Uuid>,
    /// Optional participant handle, validated by shared application policy.
    pub name: Option<String>,
    /// Native terminal routing without changing host controls.
    pub terminal: NativeTerminal,
}

/// Selected native conversation and terminal inputs for configured supervision.
/// Core startup still independently verifies its actual original child/root.
pub struct ClaudeLaunchRequest {
    /// Exact selected native UUID, verified again by the native challenge.
    pub root: Uuid,
    /// Resume the selected existing UUID rather than start a fresh conversation.
    pub resume: bool,
    /// Optional immutable participant name, validated by shared core policy.
    pub name: Option<String>,
    /// Native terminal routing, independent of host permissions and admission.
    pub terminal: NativeTerminal,
}

/// Optional synchronous progress sink, called outside native/database locks.
/// It can observe proof but cannot supply identity, readiness or membership.
/// A reporting error cancels the owned operation like any caller-side failure.
pub trait LaunchEvents: Send + Sync {
    /// Native startup became ready while remaining unjoined. The original
    /// supervisor owns the control endpoint until actual native exit.
    fn authorized(
        &self,
        _child: &NativeChild,
        _ready: &agentchat_hosts::launch::ReadyAuthorization,
    ) -> Result<()> {
        Ok(())
    }
    /// An explicit Join committed or an already-active participation restored.
    fn activated(&self, _started: &StartedLaunch) -> Result<()> {
        Ok(())
    }
    /// Record the exact selected/materialized Codex UUID before native spawn.
    /// Selection alone proves no readiness, owner association or membership.
    fn codex_selected(&self, _root: &str) -> Result<()> {
        Ok(())
    }
    /// The actual child and complete original kernel identity were captured.
    fn spawned(&self, _child: &NativeChild) -> Result<()> {
        Ok(())
    }
    /// An application attachment stopped renewing; this is not native exit.
    fn connection_lost(&self, _error: &Error) -> Result<()> {
        Ok(())
    }
    /// A fresh native route attached the same participant/generation/boundary.
    fn reattached(&self, _started: &StartedLaunch) -> Result<()> {
        Ok(())
    }
    /// The original native stream accepted a separate registration request.
    /// A later native challenge and application Attach are still required.
    fn context_refresh_requested(&self, _context: &agentchat_core::model::Context) -> Result<()> {
        Ok(())
    }
    /// Native wait returned actual exit, separately from application retirement.
    fn exited(&self, _child: &NativeChild, _status: ExitStatus) -> Result<()> {
        Ok(())
    }
}
impl LaunchEvents for () {}
