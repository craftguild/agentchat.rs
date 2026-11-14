//! Read-only binding and native progress projections, independent of adapters.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::AgentView;
use crate::{sequence::Sequence, time::Timestamp};

/// Exact descriptor's retained participation, without changing membership.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Participation {
    /// This exact conversation has never joined.
    Unjoined,
    /// Current explicit participation.
    Joined,
    /// Retained identity has left.
    Left,
}

/// Exact binding returned by whoami and contextual status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingView {
    /// Stable namespace.
    pub project_id: Uuid,
    /// Immutable descriptor, never a cwd/PID inference.
    pub context_id: Uuid,
    /// Explicit participation state.
    pub participation: Participation,
    /// Retained identity, absent for a never-joined conversation.
    pub agent: Option<AgentView>,
    /// Latest retained managed stream, if present.
    pub native_delivery: Option<NativeDeliveryView>,
}

/// Effective ordered-delivery state at the captured read time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeDeliveryState {
    /// Generation retired.
    Cancelled,
    /// Original native acceptance cannot safely be determined.
    AcceptanceUnknown,
    /// Safe attempt budget exhausted.
    Stalled,
    /// An unsettled original head remains.
    Pending,
    /// No unsettled head.
    Ready,
}

/// Snapshot of an ordered stream; observing it never advances settlement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeDeliveryView {
    /// Original launch generation.
    pub generation: Uuid,
    /// Last committed stream settlement boundary, including definite failure.
    pub settled_seq: Sequence,
    /// Effective state of the retained stream.
    pub state: NativeDeliveryState,
    /// Current head, including unknown original outcomes.
    pub head: Option<NativeHeadView>,
}

/// Safe public head metadata, excluding native payloads and authority tokens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeHeadView {
    /// Stable recipient delivery.
    pub delivery_id: Uuid,
    /// Immutable project order.
    pub seq: Sequence,
    /// Retained attempt count.
    pub attempts: u32,
    /// Next eligible retry boundary.
    pub next_eligible_at: Timestamp,
    /// Last bounded failure classification.
    pub failure_code: Option<String>,
    /// Frozen original metadata policy, unaffected by current settings.
    pub receipt_policy: crate::project::InputReceiptPolicy,
    /// Original result is pending.
    pub awaiting_acceptance: bool,
    /// Original submission may have entered the native host.
    pub submission_pending: bool,
}
