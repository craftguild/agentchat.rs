//! Durable wake control types; submission never implies model receipt or read.

mod capabilities;
mod message;
mod native;
mod native_input;
mod notice;
mod stream;
pub use capabilities::{ActiveInput, InputCapabilities, InputRecovery};
pub use message::MessageInput;
pub use native::{NativeCompletion, NativeDispatch, NativeItem, NoticeDispatch};
pub use native_input::NativeInput;
pub use notice::NoticeInput;
pub use stream::{
    MessageCompletion, MessageDispatch, MessageHead, MessageItem, MessagePort, MessageProgress,
    MessageStream,
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    Result,
    model::{Context, Delivery, Dispatch, DispatchNotice, Message},
    sequence::Sequence,
    time::Timestamp,
};

/// Maximum complete serialized wake hint, before native transport wrapping.
pub const MAX_WAKE_BYTES: usize = 8 * 1024;
/// Maximum stable event/delivery references in one outstanding batch.
pub const MAX_WAKE_EVENTS: usize = 20;
/// Maximum native submission wait and durable orphan-claim grace.
pub const SUBMISSION_SECONDS: u64 = 5;
/// Maximum already-completed native outcomes sharing one durability boundary.
pub const MAX_WAKE_COMPLETION_GROUP: usize = 8;
/// Maximum exact conversation descriptors in one read-only wake candidate scan.
pub const MAX_WAKE_SCAN_CONTEXTS: usize = 32;

/// Retained lifecycle of one incarnation's stable outstanding batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BatchState {
    /// A durable attempt is reserved and may be executing outside the transaction.
    Pending,
    /// Hint submission completed; complete-input settlement remains outstanding.
    Inflight,
    /// The three-attempt budget was exhausted; retain work without further wake.
    Stalled,
    /// Every assigned delivery has settled through complete input or departure.
    Settled,
    /// Leave or a verified replacement incarnation retired this batch.
    Cancelled,
}
impl BatchState {
    /// Stable ORM spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Inflight => "inflight",
            Self::Stalled => "stalled",
            Self::Settled => "settled",
            Self::Cancelled => "cancelled",
        }
    }
    /// Whether this batch still serializes new targeted work behind itself.
    pub const fn outstanding(self) -> bool {
        matches!(self, Self::Pending | Self::Inflight | Self::Stalled)
    }
}
/// Persistent control row, distinct from the immutable business event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WakeBatch {
    /// Stable identity for this incarnation's bounded delivery set.
    pub id: Uuid,
    /// Exact recipient.
    pub recipient_id: Uuid,
    /// Verified connection lifetime; a reconnect gets a new budget/batch.
    pub incarnation: Uuid,
    /// Retained control state.
    pub state: BatchState,
    /// Unique token for the latest reserved attempt, fencing stale callbacks.
    pub dispatch_ref: Option<Uuid>,
    /// Host turn associated with the latest presentation, when known.
    pub turn_ref: Option<String>,
    /// Reserved presentation attempts in this batch/incarnation, at most three.
    pub attempts: u32,
    /// Orphan-claim deadline or next safe retry time, depending on state.
    pub next_eligible_at: Option<Timestamp>,
    /// Bounded machine failure code; never a request body or credential.
    pub failure_code: Option<String>,
}
impl WakeBatch {
    /// Whether persisted state/time postpones another outstanding attempt.
    /// Act on this only after reconciling an empty assigned set, or validating
    /// that pending assigned rows remain. Due exhausted attempts still need
    /// ordinary preparation to materialize their stalled transition.
    pub fn retry_deferred(&self, now: Timestamp) -> bool {
        self.state.outstanding()
            && (self.state == BatchState::Stalled
                || self.next_eligible_at.is_some_and(|due| due > now))
    }
}
/// Transaction-local candidate in sealed Dispatch Header order.
#[derive(Debug, Clone)]
pub struct WakeItem {
    /// Stable recipient row whose complete-input result remains unsettled.
    pub delivery: Delivery,
    /// Its immutable sending Header, independent of audit history.
    pub dispatch: Dispatch,
    /// Exact Message payload; Notice candidates never impersonate a Message.
    pub message: Option<Message>,
    /// Exact Notice payload; Message candidates never carry a Notice subtype.
    pub notice: Option<DispatchNotice>,
}
/// Compact reference; fetch full bodies using the ordinary bounded inbox API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WakeReference {
    /// message.created or an author-only confirmation timeout; fetch exact context.
    pub kind: String,
    /// Exact original delivery identity.
    pub delivery_id: Uuid,
    /// Stable sending Header identity, never an audit-event identity.
    pub dispatch_id: Uuid,
    /// Durable project ordering.
    pub seq: Sequence,
    /// Immutable explicitly mentioned message.
    pub message_id: Uuid,
}
/// Complete bounded native-host context; identifiers survive at-least-once replay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WakeHint {
    /// Supported envelope major.
    pub version: u32,
    /// Independent persistent project namespace.
    pub project_id: Uuid,
    /// Exact routing descriptor.
    pub context_id: Uuid,
    /// Current verified connection lifetime.
    pub incarnation: Uuid,
    /// Retained participant.
    pub agent_id: Uuid,
    /// Stable outstanding batch.
    pub batch_id: Uuid,
    /// At most twenty references in event order.
    pub events: Vec<WakeReference>,
    /// Total pending/inflight non-echo inbox rows, including passive context.
    pub pending_count: u64,
    /// Bounded first-message preview; never a substitute for complete retrieval.
    pub preview: String,
}
/// The native operation must preserve the current host settings and user task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Presentation {
    /// May begin a turn only while the genuinely connected host remains idle.
    Idle,
    /// Append context at a safe active boundary under the mechanism's turn rule.
    Active {
        /// Expected host turn, unavailable on some file-hook hosts.
        expected_turn_id: Option<String>,
    },
}
/// Committed authority to attempt presentation outside the SQLite transaction.
#[derive(Debug, Clone)]
pub struct WakeDispatch {
    /// Complete bounded context, containing stable batch/event/delivery IDs.
    pub hint: WakeHint,
    /// Unique attempt fence; callbacks from earlier attempts cannot mutate state.
    pub token: Uuid,
    /// Monotonic attempt number within this batch and incarnation.
    pub attempt: u32,
    /// Idle/active decision from freshly verified host state.
    pub presentation: Presentation,
}
/// Native transport acceptance; this alone does not settle delivery or mark messages read.
#[derive(Debug, Clone, Default)]
pub struct HostReceipt {
    /// Associated host turn, if the native route exposes it.
    pub turn_id: Option<String>,
}
/// One completed external attempt; recording it never implies an inbox read.
#[derive(Debug, Clone)]
pub struct WakeCompletion {
    /// Exact original recipient and connection incarnation.
    pub context: Context,
    /// Previously committed claim, including the callback's attempt fence.
    pub dispatch: WakeDispatch,
    /// Native acceptance or bounded transport failure, without implying full-input acceptance.
    pub outcome: Result<HostReceipt>,
}
/// Host implementation of an already committed, exact-recipient presentation.
#[async_trait]
pub trait DeliveryPort: Send + Sync {
    /// Recheck actual host eligibility, preserve its controls and submit the hint.
    /// A transport response is not model receipt. Do not load offline sessions.
    /// Bound the final model-facing context, including native framing text, to
    /// `MAX_WAKE_BYTES`; transport wrappers must not expand it past that limit.
    async fn present(&self, context: &Context, dispatch: &WakeDispatch) -> Result<HostReceipt>;
}
