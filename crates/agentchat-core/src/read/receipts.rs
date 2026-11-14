//! The single bounded public receipt representation for a message recipient.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::IdentityLabel;
use crate::{
    model::{DeliveryHistory, DeliveryState, ReadKind},
    time::Timestamp,
};

/// One first committed read time; public results order automatic then explicit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptRead {
    /// Verified automatic reading or an optional explicit declaration.
    pub kind: ReadKind,
    /// First committed time for this exact message, actor and kind.
    pub at: Timestamp,
}

/// Facts for an actual original receiving target; no read implies acceptance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "UncheckedDelivery")]
pub struct ReceiptDelivery {
    /// Exact stable original-recipient delivery identity.
    pub delivery_id: Uuid,
    /// Current durable recipient-delivery bookkeeping.
    pub state: DeliveryState,
    /// Canonical first-time facts, flattened into the shared wire receipt.
    #[serde(flatten)]
    pub history: DeliveryHistory,
}

// This deserialization boundary validates the canonical flattened receipt. It
// is neither another public projection nor independently interpreted policy.
#[derive(Deserialize)]
struct UncheckedDelivery {
    delivery_id: Uuid,
    state: DeliveryState,
    #[serde(flatten)]
    history: DeliveryHistory,
}

impl TryFrom<UncheckedDelivery> for ReceiptDelivery {
    type Error = crate::Error;

    fn try_from(value: UncheckedDelivery) -> crate::Result<Self> {
        value.history.validate(value.state)?;
        Ok(Self {
            delivery_id: value.delivery_id,
            state: value.state,
            history: value.history,
        })
    }
}

/// Each actual recipient/history-only reader appears once per finite detail page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecipientReceipt {
    /// Typed retained native or anonymous identity.
    pub recipient: IdentityLabel,
    /// None for a history-only explicit reader, including anonymous users.
    pub delivery: Option<ReceiptDelivery>,
    /// At most two independent first times, automatic before explicit.
    pub reads: Vec<ReceiptRead>,
}
