//! Shared native startup completion; executable transports do not join themselves.
mod authorization;
mod claude_spawn;
pub use authorization::{authorize_claude_start, authorize_codex_start};
mod codex_start;
pub use codex_start::{PreparedCodexLaunch, prepare_codex_start, spawn_codex};
mod events;
mod reattach;
use agentchat_core::{
    Error, ErrorCode,
    model::{Context, LaunchIdentity},
    sequence::Sequence,
};
pub use claude_spawn::spawn_claude;
pub use events::{ClaudeLaunchRequest, CodexLaunchRequest, LaunchEvents};
pub use reattach::{reattach_claude_launch, reattach_launch};
use uuid::Uuid;

/// Committed explicit participation, separate from physical startup readiness
/// or later authority. Caller retains the original child and runtime connection and must
/// supervise/reconcile them; this value never renews participation on its own.
#[derive(Clone)]
pub struct StartedLaunch {
    /// Exact native context verified for this original child.
    pub context: Context,
    /// Immutable physical lifetime and fresh participation generation.
    pub identity: LaunchIdentity,
    /// Retained participant returned by the shared application operation.
    pub agent_id: Uuid,
    /// New committed delivery boundary; no offline catch-up is implied.
    pub boundary: Sequence,
}

fn required() -> Error {
    Error::new(
        ErrorCode::SessionRequired,
        "The exact native launch startup did not complete.",
    )
}
