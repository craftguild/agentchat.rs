//! Optional post-commit instrumentation, separate from policy and diagnostics.
use agentchat_core::sequence::Sequence;
use uuid::Uuid;

/// Observe the runtime's application-commit return boundary before publication
/// of the committed head and before any native scheduling for that operation.
/// A callback must be short/nonblocking and must not invoke application work.
/// It cannot change results, acknowledge delivery or establish host admission.
pub trait CommitObserver: Send + Sync {
    /// The successful mutation's last newly committed sequence. Idempotent
    /// replays and successful operations without a new event are not reported.
    fn committed(&self, request_id: Uuid, head: Sequence);
}
