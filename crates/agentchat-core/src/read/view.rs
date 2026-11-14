//! Compact bounded result records; large nested reader sets are separately paged.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::Page;
use crate::{model::*, time::Timestamp};

/// Presentation-only channel classification selected at list time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelListCondition {
    /// Channels whose conversation deadline has not elapsed.
    #[default]
    Active,
    /// Channels at or beyond the inclusive 30-day boundary.
    Archived,
    /// Both groups, with classification retained on every item.
    All,
}

/// Storage-derived conversation time paired with the immutable channel entity.
///
/// This is an internal snapshot-port record, not a persisted entity or wire DTO.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelActivity {
    /// Immutable retained channel.
    pub channel: Channel,
    /// Latest root/reply time, or channel-creation event time when empty.
    pub last_conversation_at: Timestamp,
}

/// Typed derived channel-list projection, separate from persisted `Channel`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelView {
    /// Immutable retained channel fields.
    #[serde(flatten)]
    pub channel: Channel,
    /// Latest root/reply time, or channel-creation event time when empty.
    pub last_conversation_at: Timestamp,
    /// Inclusive transition instant (`last_conversation_at + 30 days`).
    pub archive_at: Timestamp,
    /// Classification at the containing page's captured `as_of` time.
    pub archived: bool,
    /// Whether the channel is a current mandatory project default.
    pub mandatory: bool,
}

/// Finite channel page evaluated against one captured server UTC time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChannelPage {
    /// Existing cursor/head semantics with derived channel items.
    #[serde(flatten)]
    pub page: Page<ChannelView>,
    /// Server UTC observation used for every classification in this page.
    pub as_of: Timestamp,
}

/// Current participant view; liveness does not imply automatic wake permission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentView {
    /// Retained conversation identity and explicit participation.
    #[serde(flatten)]
    pub agent: Agent,
    /// Effective phase, disconnected once valid host evidence expires.
    pub presence: Phase,
    /// Presence at the snapshot time, including paused/approval-blocked sessions.
    pub connected: bool,
    /// Current exact incarnation if this identity has ever attached.
    pub incarnation: Option<Uuid>,
    /// Pending visible reads, excluding this author's own messages.
    pub unread_count: u64,
}

/// Complete bounded message with author/channel/thread metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageView {
    /// Immutable message including complete body and creation sequence.
    #[serde(flatten)]
    pub message: Message,
    /// Root identity, without introducing another thread object.
    pub thread_id: Uuid,
    /// Retained author.
    pub author: IdentityLabel,
    /// Immutable channel slug.
    pub channel: String,
}

/// Single-message lookup with a bounded nested receipt page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageDetail {
    /// Full immutable message, without duplicate receipt projections.
    pub message: MessageView,
    /// Actual receiving targets and history-only readers, each appearing once.
    pub receipts: Page<super::RecipientReceipt>,
}

/// Raw snapshot inbox row, before the application adds message metadata.
#[derive(Debug, Clone)]
pub struct InboxRecord {
    /// Exact original recipient-delivery row.
    pub delivery: Delivery,
    /// Immutable sending Header; audit records are independent.
    pub dispatch: Dispatch,
    /// Complete typed Notice payload, absent for Message Headers.
    pub notice: Option<DispatchNotice>,
    /// Outstanding batch's visible control state, including stalled.
    pub batch_state: Option<String>,
}

/// Full finite inbox item; retrieval never settles delivery or implies reading.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InboxView {
    /// Stable delivery and current at-least-once bookkeeping.
    pub delivery: Delivery,
    /// Original sending Header, including its Message/Notice kind.
    pub dispatch: Dispatch,
    /// Typed Notice payload only for Notice Headers.
    pub notice: Option<DispatchNotice>,
    /// Complete conversation content only for Message Headers.
    pub message: Option<MessageView>,
    /// Visible outstanding batch state, if assigned.
    pub batch_state: Option<String>,
}

/// Scalar project status; runtime and integration health are added by adapters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectStatistics {
    /// Retained identities, including left identities.
    pub agents: u64,
    /// Currently joined identities, independent of presence.
    pub joined: u64,
    /// Retained channels.
    pub channels: u64,
    /// Immutable messages.
    pub messages: u64,
    /// Committed business events, including passive information.
    pub events: u64,
    /// Every retained recipient row, including actor echoes and terminal rows.
    pub deliveries: u64,
    /// Sparse kind-specific first-read transitions, independent from delivery.
    pub read_receipts: u64,
    /// Pending/inflight deliveries across all recipients.
    pub pending_deliveries: u64,
    /// Outstanding batches that exhausted their safe attempt budget.
    pub stalled_batches: u64,
}
