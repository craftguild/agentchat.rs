//! Original process continuity never opens or resolves an executable image.
use super::{ProcessIdentity, birth::Birth};
use agentchat_core::Result;
use rustix::{io::Errno, process::Pid};
use uuid::Uuid;

/// Ongoing original-process observation, independent of initial image validation.
/// None of these states establishes a conversation, generation or native permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContinuityState {
    /// Stable original boot/PID/start and supported same-user credentials, alive.
    Current,
    /// Positive original birth loss, kernel exit or typed process absence.
    Gone,
    /// Unreadable, malformed, racing or user-inconsistent facts. Preserve state.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Observation {
    pub birth: Birth,
    pub uid: u32,
}

pub(super) fn observe(identity: &ProcessIdentity) -> ContinuityState {
    #[cfg(target_os = "linux")]
    use super::linux as kernel;
    #[cfg(target_os = "macos")]
    use super::macos as kernel;
    inspect(
        identity,
        rustix::process::geteuid().as_raw(),
        kernel::boot,
        kernel::continuity,
        |pid| {
            Pid::from_raw(pid as i32)
                .ok_or(Errno::INVAL)
                .and_then(rustix::process::test_kill_process)
        },
    )
}

fn inspect(
    identity: &ProcessIdentity,
    current_uid: u32,
    mut boot: impl FnMut() -> Result<Uuid>,
    mut snapshot: impl FnMut(u32) -> Result<Observation>,
    mut permission: impl FnMut(u32) -> rustix::io::Result<()>,
) -> ContinuityState {
    if !super::birth::valid(identity) || identity.uid != current_uid {
        return ContinuityState::Unknown;
    }
    let Ok(before) = boot() else {
        return ContinuityState::Unknown;
    };
    if before.is_nil() {
        return ContinuityState::Unknown;
    }
    let state = if before != identity.boot {
        ContinuityState::Gone
    } else {
        match snapshot(identity.pid) {
            Ok(first) => match snapshot(identity.pid) {
                Ok(second) if first == second => {
                    if first.birth.start != identity.start || first.birth.exited {
                        ContinuityState::Gone
                    } else if first.uid == identity.uid {
                        ContinuityState::Current
                    } else {
                        ContinuityState::Unknown
                    }
                }
                _ => ContinuityState::Unknown,
            },
            Err(_) if permission(identity.pid) == Err(Errno::SRCH) => ContinuityState::Gone,
            Err(_) => ContinuityState::Unknown,
        }
    };
    // At most two bounded credential/birth snapshots and two boot reads. A
    // changed or unavailable final boot invalidates even apparent positive loss.
    if boot().ok() == Some(before) {
        state
    } else {
        ContinuityState::Unknown
    }
}

#[cfg(test)]
#[path = "../../../tests/process/continuity.rs"]
mod tests;
