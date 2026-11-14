//! Native transport metadata; accepting a notice never acknowledges its inbox.
use agentchat_core::delivery::WakeHint;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One bounded exact-context hint, published before its metadata-only file signal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WakeRecord {
    /// Native root supplied by its verified context.
    pub root: Uuid,
    /// Exact immutable application descriptor.
    pub context_id: Uuid,
    /// Exact connection incarnation; never renewed from cached metadata.
    pub incarnation: Uuid,
    /// Positive signal order, exactly representable by native JavaScript.
    pub generation: u64,
    /// Durable dispatch attempt token.
    pub token: Uuid,
    /// Exact root turn for active delivery, absent for idle delivery.
    pub expected_turn_id: Option<String>,
    /// Bounded event references; full message bodies use application operations.
    pub hint: WakeHint,
}
/// Fixed per-context submission receipt. Its fields attest only native command
/// acceptance; explicit application ACK and per-message read remain separate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Submission {
    /// Native static command accepted the bounded notice.
    pub accepted: bool,
    /// Exact durable dispatch attempt.
    pub token: Uuid,
    /// Exact immutable context.
    pub context_id: Uuid,
    /// Exact context incarnation.
    pub incarnation: Uuid,
    /// Exact native root.
    pub root: Uuid,
    /// Signal order for this notice.
    pub generation: u64,
    /// Diagnostic wall time in milliseconds; never native liveness authority.
    pub unix_ms: u64,
}
