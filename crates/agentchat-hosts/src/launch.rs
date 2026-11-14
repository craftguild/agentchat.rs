//! Original physical owner/supervisor tokens, separate from exact root binding.
mod activation_socket;
mod child;
mod claude;
mod claude_profile;
mod quiescence;
mod readiness;
mod readiness_scope;
mod reap;
mod registry;
mod root_change;
mod startup;
mod terminal;
use crate::claude::process::ProcessIdentity;
pub use activation_socket::ActivationSocket;
use agentchat_core::{
    Error, ErrorCode, Result,
    model::{LaunchIdentity, LaunchOwnerState},
};
pub use child::{NativeChild, NativeProof, NativeTerminal};
pub use claude::{ClaudeReady, ClaudeStartup};
pub use claude_profile::{MANAGED_CLAUDE, managed_claude};
pub use readiness::ReadyAuthorization;
pub(crate) use reap::LaunchReaper;
pub use registry::{Binding, Endpoint, Registry};
use serde::{Deserialize, Serialize};
pub use startup::{StartupScope, StartupState};
use uuid::Uuid;

/// Refuse script/shim processes before selecting a launch owner. This validates
/// an executable Mach-O/ELF file, not its host version, trust or conversation.
pub fn native_executable(path: &std::path::Path) -> Result<()> {
    use std::{io::Read, os::unix::fs::PermissionsExt};
    let mut file = std::fs::File::open(path).map_err(|_| unavailable())?;
    let meta = file.metadata().map_err(|_| unavailable())?;
    let mut magic = [0; 4];
    file.read_exact(&mut magic).map_err(|_| unavailable())?;
    if !path.is_absolute()
        || !meta.is_file()
        || meta.permissions().mode() & 0o111 == 0
        || !matches!(
            magic,
            [0x7f, b'E', b'L', b'F']
                | [0xcf, 0xfa, 0xed, 0xfe]
                | [0xfe, 0xed, 0xfa, 0xcf]
                | [0xca, 0xfe, 0xba, 0xbe]
                | [0xbe, 0xba, 0xfe, 0xca]
        )
    {
        return Err(unavailable());
    }
    Ok(())
}
fn unavailable() -> Error {
    Error::new(
        ErrorCode::UnsupportedHost,
        "A real native executable is required; launcher scripts are not native owners.",
    )
}

/// Versioned physical proof for an AGENTCHAT-managed native launch. Construct this
/// only after selecting the actual native executable, never an npm shim. These
/// tokens do not establish a native root, trust, startup handshake or admission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchOwners {
    version: u32,
    owner: ProcessIdentity,
    supervisor: ProcessIdentity,
}

impl LaunchOwners {
    pub(crate) fn native_owner(identity: &LaunchIdentity) -> Result<ProcessIdentity> {
        let proof: Self = serde_json::from_str(&identity.owner).map_err(|_| unavailable())?;
        if proof.version != 1
            || identity.generation.is_nil()
            || proof.owner.pid() == proof.supervisor.pid()
        {
            return Err(unavailable());
        }
        Ok(proof.owner)
    }
    /// Whether the current process is the original live AGENTCHAT supervisor. Used by
    /// immutable binding publication; a native helper cannot publish its own
    /// replacement launch authority merely because it knows a root or token.
    pub fn is_supervisor(identity: &LaunchIdentity) -> bool {
        serde_json::from_str::<Self>(&identity.owner).is_ok_and(|proof| {
            proof.version == 1
                && proof.supervisor.pid() == std::process::id()
                && proof.supervisor.is_continuing()
                && proof.state() == LaunchOwnerState::Current
        }) && !identity.generation.is_nil()
    }
    pub(crate) fn supervisor_peer(
        identity: &LaunchIdentity,
        owner: &ProcessIdentity,
        pid: u32,
    ) -> bool {
        // The PID comes from kernel Unix peer credentials, not request JSON.
        // Both original births and user credentials must still be current.
        serde_json::from_str::<Self>(&identity.owner).is_ok_and(|proof| {
            proof.version == 1
                && !identity.generation.is_nil()
                && proof.owner == *owner
                && proof.supervisor.pid() == pid
                && proof.state() == LaunchOwnerState::Current
        })
    }
    /// Pin both already-identified processes and allocate a fresh generation.
    /// Caller must independently bind the host-provided root before publishing
    /// this proof through a launch-aware HostGate.
    pub fn capture(owner: u32, supervisor: u32) -> Result<LaunchIdentity> {
        let proof = Self {
            version: 1,
            owner: ProcessIdentity::capture(owner)?,
            supervisor: ProcessIdentity::capture(supervisor)?,
        };
        // Initial association retains full image validation. Only later
        // observations use image-independent original-process continuity.
        if owner == supervisor
            || !proof.owner.is_current()
            || !proof.supervisor.is_current()
            || proof.state() != LaunchOwnerState::Current
        {
            return Err(Error::new(
                ErrorCode::SessionRequired,
                "Distinct current native owner and supervisor are required.",
            ));
        }
        let owner = serde_json::to_string(&proof)
            .map_err(|_| Error::new(ErrorCode::IntegrityError, "Launch owner encoding failed."))?;
        if owner.len() > 32_768 {
            return Err(Error::new(
                ErrorCode::SessionRequired,
                "Launch owner proof exceeds its byte bound.",
            ));
        }
        Ok(LaunchIdentity {
            generation: Uuid::new_v4(),
            owner,
        })
    }

    /// Kernel-only bounded continuity. Positive birth loss retires; user changes
    /// and unreadability suspend. Root/cache/helper activity cannot renew
    /// these original tokens. This performs no native or application callback.
    pub fn observe(identity: &LaunchIdentity) -> LaunchOwnerState {
        if identity.generation.is_nil() || identity.owner.len() > 32_768 {
            return LaunchOwnerState::Unknown;
        }
        match serde_json::from_str::<Self>(&identity.owner) {
            Ok(proof) if proof.version == 1 && proof.owner.pid() != proof.supervisor.pid() => {
                proof.state()
            }
            _ => LaunchOwnerState::Unknown,
        }
    }

    fn state(&self) -> LaunchOwnerState {
        use crate::claude::process::ContinuityState;
        let states = [
            self.owner.continuity_state(),
            self.supervisor.continuity_state(),
        ];
        if states.contains(&ContinuityState::Gone) {
            LaunchOwnerState::Gone
        } else if states.contains(&ContinuityState::Unknown) {
            LaunchOwnerState::Unknown
        } else {
            LaunchOwnerState::Current
        }
    }
}
