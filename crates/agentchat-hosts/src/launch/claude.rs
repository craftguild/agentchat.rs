//! A child birth alone cannot complete the exact native startup handshake.
use super::{NativeChild, NativeProof};
use crate::claude::{NativeLifecycle, adapter::NativeAdapter, module, namespace::OwnerNamespace};
use agentchat_core::{
    Error, ErrorCode, Result,
    model::{Context, HostKind, LaunchOwnerState, Phase},
    ports::HostState,
};
use std::path::PathBuf;
use uuid::Uuid;

/// Explicit launch choices, independent of physical discovery or cwd identity.
/// Construction performs no host/configuration I/O and grants no membership.
#[derive(Clone)]
pub struct ClaudeStartup {
    root: Uuid,
    project: Uuid,
    directory: PathBuf,
    version: String,
}
/// One successful exact-child native integration challenge. Controls remain as
/// observed, including startup pause/approval; this never releases them. Shared
/// application startup must reverify authority before committing membership.
#[derive(Clone)]
pub struct ClaudeReady {
    context: Context,
    native_generation: u64,
    state: HostState,
}
impl ClaudeReady {
    /// Ask the original native stream to recapture its current registration.
    /// Only the live original supervisor can request this separate operation.
    /// Transport acceptance grants no readiness, participant or input permission;
    /// the caller must await a fresh native challenge and shared Attach again.
    pub async fn refresh_context(&self, home: &std::path::Path, child: &NativeChild) -> Result<()> {
        self.refresh_from_proof(home, &child.proof()).await
    }
    /// Request a fresh actual native registration using the original child proof.
    pub async fn refresh_from_proof(
        &self,
        home: &std::path::Path,
        child: &NativeProof,
    ) -> Result<()> {
        let registry = super::Registry::new(home);
        let binding = registry.resolve(&self.context)?.ok_or_else(unavailable)?;
        if child.state() != LaunchOwnerState::Current
            || binding.identity.owner != child.identity().owner
            || binding.endpoint != super::Endpoint::Claude
            || !module::has_startup_gate_source(&self.context.host_version)
            || registry.root_changed(
                self.context.project_id,
                self.context.host,
                &self.context.host_session_id,
                &binding.identity,
            )
        {
            return Err(unavailable());
        }
        let namespace = OwnerNamespace::existing(home, child.owner().clone()).await?;
        crate::claude::observer::relay::request_context_refresh(
            &namespace,
            &binding,
            self.native_generation,
        )
        .await
    }
    /// Actual immutable descriptor supplied by this child's native callbacks.
    pub fn context(&self) -> &Context {
        &self.context
    }
    /// Exact native callback reservation, separate from the AGENTCHAT launch UUID.
    pub fn native_generation(&self) -> u64 {
        self.native_generation
    }
    /// Fresh original native controls; readiness is not wake eligibility.
    pub fn state(&self) -> &HostState {
        &self.state
    }
}
impl ClaudeStartup {
    /// Pin launcher-selected UUID, initialized project, canonical directory and
    /// exact candidate engine. No host/OS profile is admitted by this selection.
    pub fn new(root: Uuid, project: Uuid, directory: PathBuf, version: String) -> Result<Self> {
        if root.is_nil()
            || project.is_nil()
            || !directory.is_absolute()
            || !module::candidate_version(&version)
        {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "Exact Claude startup choices are required.",
            ));
        }
        Ok(Self {
            root,
            project,
            directory,
            version,
        })
    }
    /// Check only this supervisor's actual native child. Missing/unavailable
    /// initialization returns None, never ready from a PID, old file or timeout.
    /// A positive different root/project is a startup mismatch, not permission
    /// to adopt another conversation. The caller owns waiting, exit and rollback.
    pub async fn observe(
        &self,
        home: &std::path::Path,
        child: &NativeChild,
        adapter: &NativeAdapter,
    ) -> Result<Option<ClaudeReady>> {
        self.observe_proof(home, &child.proof(), adapter).await
    }
    /// Observe a captured original child while its supervisor polls actual exit.
    pub async fn observe_proof(
        &self,
        home: &std::path::Path,
        child: &NativeProof,
        adapter: &NativeAdapter,
    ) -> Result<Option<ClaudeReady>> {
        if child.state() != LaunchOwnerState::Current {
            return Err(unavailable());
        }
        let ready = self.observe_current_proof(home, child, adapter).await?;
        if ready
            .as_ref()
            .is_some_and(|ready| !self.matches_initial(&ready.context))
        {
            return Err(Error::new(
                ErrorCode::ProjectBindingMismatch,
                "The native startup root or directory differs from the requested launch.",
            ));
        }
        Ok(ready)
    }

    /// Observe the latest exact lifecycle owned by the original physical child.
    /// Unlike startup admission this permits a new native root and another
    /// canonical directory resolving to the authorized project. It retains the
    /// project and engine; the caller must still check platform, correlate the
    /// departure and commit a distinct-root activation.
    pub async fn observe_current_proof(
        &self,
        home: &std::path::Path,
        child: &NativeProof,
        adapter: &NativeAdapter,
    ) -> Result<Option<ClaudeReady>> {
        if child.state() != LaunchOwnerState::Current {
            return Err(unavailable());
        }
        let ns = match OwnerNamespace::existing(home, child.owner().clone()).await {
            Ok(ns) => ns,
            Err(_) => return Ok(None),
        };
        let before = match ns.read_current().await {
            Ok(Some(before)) => before,
            // A concurrent original callback/transport update may hold the
            // cursor. Unavailable initialization is not a completed handshake.
            Ok(None) | Err(_) => return Ok(None),
        };
        let NativeLifecycle::Project(native) = &before.lifecycle else {
            return Ok(None);
        };
        if native.context.host_session_id != native.root.to_string()
            || !self.matches_observed(&native.context)
        {
            return Err(Error::new(
                ErrorCode::ProjectBindingMismatch,
                "The native project, directory or engine differs from the managed launch.",
            ));
        }
        let state = match adapter.observe_owned(&native.context, child.owner()).await {
            Ok(state) if state.phase != Phase::Disconnected => state,
            _ => return Ok(None),
        };
        if child.state() != LaunchOwnerState::Current
            || ns.read_current().await.ok().flatten().as_ref() != Some(&before)
        {
            return Ok(None);
        }
        Ok(Some(ClaudeReady {
            context: native.context.clone(),
            native_generation: native.generation,
            state,
        }))
    }

    fn matches_observed(&self, context: &Context) -> bool {
        context.host == HostKind::Claude
            && context.project_id == self.project
            && context.host_version == self.version
    }

    fn matches_initial(&self, context: &Context) -> bool {
        self.matches_observed(context)
            && context.host_session_id == self.root.to_string()
            && context.working_directory == self.directory
    }
}

#[cfg(test)]
#[path = "../../tests/unit/claude_launch.rs"]
mod tests;
fn unavailable() -> Error {
    Error::new(
        ErrorCode::SessionRequired,
        "The original native startup owner is unavailable or changed.",
    )
}
