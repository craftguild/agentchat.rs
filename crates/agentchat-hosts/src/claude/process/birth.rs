//! Positive original-birth loss, distinct from unavailable liveness evidence.
use super::{ProcessIdentity, Start};
use agentchat_core::Result;
use rustix::{io::Errno, process::Pid};
use uuid::Uuid;

/// Read-only physical-birth observation; none of these states grants host authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BirthState {
    /// Boot/PID/start still match a non-exited kernel process. Image, user,
    /// conversation, observer and host-control proof are deliberately separate.
    Present,
    /// Positive boot/start replacement, observed kernel exit or typed ESRCH.
    /// Only a separately validated saved owner record may be cleaned on this basis.
    Gone,
    /// Malformed, unreadable, denied or racing observations. Preserve metadata.
    Unknown,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Birth {
    pub start: Start,
    pub exited: bool,
}
pub(super) fn observe(identity: &ProcessIdentity) -> BirthState {
    #[cfg(target_os = "linux")]
    use super::linux as kernel;
    #[cfg(target_os = "macos")]
    use super::macos as kernel;
    inspect(identity, kernel::boot, kernel::birth, |pid| {
        Pid::from_raw(pid as i32)
            .ok_or(Errno::INVAL)
            .and_then(rustix::process::test_kill_process)
    })
}
pub(super) fn valid(identity: &ProcessIdentity) -> bool {
    if identity.pid == 0 || identity.pid > i32::MAX as u32 || identity.boot.is_nil() {
        return false;
    }
    match identity.start {
        #[cfg(target_os = "linux")]
        Start::Linux { ticks } => ticks > 0,
        #[cfg(target_os = "macos")]
        Start::Macos {
            seconds,
            microseconds,
        } => seconds > 0 && microseconds < 1_000_000,
        _ => false,
    }
}
fn inspect(
    identity: &ProcessIdentity,
    mut boot: impl FnMut() -> Result<Uuid>,
    mut snapshot: impl FnMut(u32) -> Result<Birth>,
    mut permission: impl FnMut(u32) -> rustix::io::Result<()>,
) -> BirthState {
    if !valid(identity) {
        return BirthState::Unknown;
    }
    let Ok(before) = boot() else {
        return BirthState::Unknown;
    };
    if before.is_nil() {
        return BirthState::Unknown;
    }
    let state = if before != identity.boot {
        BirthState::Gone
    } else {
        match snapshot(identity.pid) {
            Ok(first) => match snapshot(identity.pid) {
                Ok(second) if first == second => {
                    if first.start != identity.start || first.exited {
                        BirthState::Gone
                    } else {
                        BirthState::Present
                    }
                }
                _ => BirthState::Unknown,
            },
            // Failure to read /proc or libproc is never absence by itself.
            // The non-signalling kernel query must return exactly ESRCH; EPERM,
            // successful existence and every other error preserve uncertainty.
            Err(_) if permission(identity.pid) == Err(Errno::SRCH) => BirthState::Gone,
            Err(_) => BirthState::Unknown,
        }
    };
    // Do not combine observations from different boot namespaces. At most two
    // birth reads, two boot reads and one non-signalling query are performed.
    if boot().ok() == Some(before) {
        state
    } else {
        BirthState::Unknown
    }
}

#[cfg(test)]
#[path = "../../../tests/process/birth.rs"]
mod tests;
