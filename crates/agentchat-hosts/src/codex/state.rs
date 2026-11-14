//! Native phase alone is never designated-process liveness evidence.
use super::{invalid, required};
use agentchat_core::{Result, model::Phase, ports::HostState};
use serde_json::Value;

pub(super) fn decode(root: &str, result: Value) -> Result<HostState> {
    let thread = result.get("thread").ok_or_else(invalid)?;
    if thread.get("id").and_then(Value::as_str) != Some(root) {
        return Err(required());
    }
    let status = thread.get("status").ok_or_else(invalid)?;
    let phase = match status.get("type").and_then(Value::as_str) {
        Some("idle") => Phase::Idle,
        Some("notLoaded" | "systemError") => Phase::Disconnected,
        Some("active") => {
            let flags = status
                .get("activeFlags")
                .and_then(Value::as_array)
                .ok_or_else(invalid)?;
            if flags
                .iter()
                .any(|f| f.as_str() == Some("waitingOnApproval"))
            {
                Phase::ApprovalBlocked
            } else if flags
                .iter()
                .any(|f| f.as_str() == Some("waitingOnUserInput"))
            {
                Phase::Paused
            } else if flags.is_empty() {
                Phase::Active
            } else {
                return Err(invalid());
            }
        }
        _ => return Err(invalid()),
    };
    // Native waiting/approval facts remain authoritative. Esc has no separate
    // AGENTCHAT latch, and an active phase without an exact turn ID cannot authorize
    // a turn-bound offer through the shared policy.
    Ok(HostState {
        phase,
        active_turn_id: None,
        wake_paused: phase == Phase::Paused,
    })
}
