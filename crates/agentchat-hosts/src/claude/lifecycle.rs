//! Native lifecycle authority exists before an application project or context.

use super::{CallScope, NativeRegistration};
use agentchat_core::{Error, ErrorCode, Result, model::HostKind};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Native-only initialization lifecycle; never an application context or agent.
/// The callback captures these facts independently of tool arguments. A fresh
/// one-use call observation and live kernel/native challenge are still required.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeBootstrap {
    /// Physical namespace locator retained across clear and module reload.
    pub owner_root: Uuid,
    /// Native observer belonging to that exact physical owner.
    pub observer: String,
    /// Shared positive monotonic lifecycle generation, including promotion.
    pub generation: u64,
    /// Actual current native root, independent of the physical locator.
    pub root: Uuid,
    /// Fresh opaque lifecycle nonce; not issued by the context registry.
    pub lifecycle_id: Uuid,
    /// Exact engine version selected and independently checked by the adapter.
    pub host_version: String,
    /// Exact operating-system profile selected by the adapter.
    pub os_profile: String,
    /// Native working directory; a locator, never conversation identity.
    pub working_directory: PathBuf,
}

/// Immutable native facts for either side of project initialization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "stage",
    content = "registration",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum NativeLifecycle {
    /// No application context, presence, participation or restoration authority.
    Bootstrap(NativeBootstrap),
    /// Registry-issued context agreeing with the exact current native root.
    Project(NativeRegistration),
}

/// Native routing result, deliberately independent of the MCP transport crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeScope {
    /// Exact initialized application attachment.
    Project(CallScope),
    /// Initialization-only attachment; cannot be cast into a project context.
    Bootstrap {
        /// Fresh native lifecycle nonce.
        lifecycle_id: Uuid,
        /// Shared monotonic native generation.
        generation: u64,
    },
}
impl NativeScope {
    pub(super) fn token(self) -> Uuid {
        match self {
            Self::Project(scope) => scope.context_id,
            Self::Bootstrap { lifecycle_id, .. } => lifecycle_id,
        }
    }
}
impl NativeLifecycle {
    /// Physical namespace locator; never sufficient authority on its own.
    pub fn owner_root(&self) -> Uuid {
        match self {
            Self::Bootstrap(r) => r.owner_root,
            Self::Project(r) => r.owner_root,
        }
    }
    /// Observer namespace independently pinned to the kernel owner.
    pub fn observer(&self) -> &str {
        match self {
            Self::Bootstrap(r) => &r.observer,
            Self::Project(r) => &r.observer,
        }
    }
    /// Positive monotonic native generation shared by both lifecycle stages.
    pub fn generation(&self) -> u64 {
        match self {
            Self::Bootstrap(r) => r.generation,
            Self::Project(r) => r.generation,
        }
    }
    /// Current exact native root captured by the lifecycle callback.
    pub fn root(&self) -> Uuid {
        match self {
            Self::Bootstrap(r) => r.root,
            Self::Project(r) => r.root,
        }
    }
    /// Expected engine version; fresh native answers must match it.
    pub fn host_version(&self) -> &str {
        match self {
            Self::Bootstrap(r) => &r.host_version,
            Self::Project(r) => &r.context.host_version,
        }
    }
    /// Native working directory, used only for explicit project resolution.
    pub fn working_directory(&self) -> &Path {
        match self {
            Self::Bootstrap(r) => &r.working_directory,
            Self::Project(r) => &r.context.working_directory,
        }
    }
    /// Select a lifecycle scope without claiming liveness or host admission.
    pub fn scope(&self) -> NativeScope {
        match self {
            Self::Bootstrap(r) => NativeScope::Bootstrap {
                lifecycle_id: r.lifecycle_id,
                generation: r.generation,
            },
            Self::Project(r) => NativeScope::Project(r.scope()),
        }
    }
    pub(super) fn validate(&self) -> Result<()> {
        let os_profile = match self {
            Self::Bootstrap(r) if !r.lifecycle_id.is_nil() => &r.os_profile,
            Self::Project(r)
                if r.context.host == HostKind::Claude
                    && r.context.host_session_id == r.root.to_string() =>
            {
                &r.context.os_profile
            }
            _ => return Err(required()),
        };
        if self.generation() == 0
            || !identifier(self.observer(), 100)
            || self.host_version().is_empty()
            || self.host_version().len() > 128
            || os_profile.is_empty()
            || os_profile.len() > 128
            || !self.working_directory().is_absolute()
            || self.working_directory().as_os_str().len() > 16_384
        {
            return Err(required());
        }
        Ok(())
    }
    pub(super) fn reuses_identity(&self, old: &Self) -> bool {
        self.scope().token() == old.scope().token()
            || matches!((self, old), (Self::Project(a), Self::Project(b))
                if a.context.incarnation == b.context.incarnation)
    }
}
fn identifier(value: &str, bound: usize) -> bool {
    !value.is_empty()
        && value.len() <= bound
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
pub(super) fn required() -> Error {
    Error::new(
        ErrorCode::SessionRequired,
        "Fresh exact native lifecycle evidence is required.",
    )
}
