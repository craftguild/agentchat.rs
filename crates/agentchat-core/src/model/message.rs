//! Immutable messages, durable events, frozen mentions and sparse read state.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{mentions::MentionSource, sequence::Sequence, time::Timestamp};

/// Immutable root or direct-to-root reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    /// Message UUID and root thread UUID when this is a root.
    pub id: Uuid,
    /// Channel namespace.
    pub channel_id: Uuid,
    /// Retained author identity.
    pub author_id: Uuid,
    /// Direct root reference; replies never reference another reply.
    pub root_message_id: Option<Uuid>,
    /// Complete bounded UTF-8 body.
    pub body: String,
    /// Wall-clock metadata; sequence numbers define ordering.
    pub created_at: Timestamp,
    /// Committed Dispatch order in the shared causal sequence.
    pub creation_seq: Sequence,
}

impl Message {
    /// Public thread identity, with no independent thread object.
    pub fn thread_id(&self) -> Uuid {
        self.root_message_id.unwrap_or(self.id)
    }
}

/// Frozen selection retained even if presence or channel membership changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mention {
    /// Immutable mentioned message.
    pub message_id: Uuid,
    /// Selected recipient; never the sender.
    pub recipient_id: Uuid,
    /// All contributing explicit mention kinds.
    pub sources: BTreeSet<MentionSource>,
}

/// Explicit first-read transition, independent of delivery settlement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    /// Actual message read.
    pub message_id: Uuid,
    /// Reading native or anonymous actor.
    pub actor_id: Uuid,
    /// Independently retained origin; public manual operations always use Explicit.
    pub kind: super::ReadKind,
    /// Original read time, preserved by retries.
    pub first_read_at: Timestamp,
}

/// One original recipient Detail, committed with its Dispatch Header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "UncheckedDelivery")]
pub struct Delivery {
    /// Stable original input and recovery identity.
    pub id: Uuid,
    /// Owning immutable Dispatch UUID.
    pub dispatch_id: Uuid,
    /// Owning recipient.
    pub recipient_id: Uuid,
    /// Current canonical delivery outcome.
    pub state: super::DeliveryState,
    /// True only for a frozen explicit message mention.
    pub wake_eligible: bool,
    /// Stable outstanding batch assignment, when scheduled.
    pub wake_batch_id: Option<Uuid>,
    /// Presentation attempts, unrelated to read receipts.
    pub attempts: u32,
    /// Transactional read projection of the append-only transition history.
    #[serde(flatten)]
    pub history: super::DeliveryHistory,
}

#[derive(Deserialize)]
struct UncheckedDelivery {
    id: Uuid,
    dispatch_id: Uuid,
    recipient_id: Uuid,
    state: super::DeliveryState,
    wake_eligible: bool,
    wake_batch_id: Option<Uuid>,
    attempts: u32,
    #[serde(flatten)]
    history: super::DeliveryHistory,
}

impl TryFrom<UncheckedDelivery> for Delivery {
    type Error = crate::Error;

    fn try_from(value: UncheckedDelivery) -> crate::Result<Self> {
        value.history.validate(value.state)?;
        Ok(Self {
            id: value.id,
            dispatch_id: value.dispatch_id,
            recipient_id: value.recipient_id,
            state: value.state,
            wake_eligible: value.wake_eligible,
            wake_batch_id: value.wake_batch_id,
            attempts: value.attempts,
            history: value.history,
        })
    }
}
