//! Typed complete messages and bounded events retain distinct attribution.
use super::{MessageInput, NoticeInput};
use crate::{Result, project::InputReceiptPolicy, sequence::Sequence};
use uuid::Uuid;

/// Native input kind. An event is never encoded with a fictitious message,
/// channel or author; complete message framing remains unchanged.
#[derive(Debug, Clone, PartialEq)]
pub enum NativeInput {
    /// Original complete message and immutable author/channel attribution.
    Message(MessageInput),
    /// Bounded non-message context with explicit omitted-data diagnostics.
    Notice(NoticeInput),
}
impl NativeInput {
    /// Frozen policy from the original claim, never current configuration or
    /// evidence that acceptance or read bookkeeping has already occurred.
    pub fn receipt_policy(&self) -> InputReceiptPolicy {
        InputReceiptPolicy {
            auto_read: match self {
                Self::Message(value) => value.auto_read,
                Self::Notice(value) => value.auto_read,
            },
        }
    }
    /// Persistent project namespace, independent of native conversation ID.
    pub fn project_id(&self) -> Uuid {
        match self {
            Self::Message(value) => value.project_id,
            Self::Notice(value) => value.project_id,
        }
    }
    /// Original participation generation; reconnecting cannot replace it.
    pub fn generation(&self) -> Uuid {
        match self {
            Self::Message(value) => value.generation,
            Self::Notice(value) => value.generation,
        }
    }
    /// Exact stable delivery identity, independent of explicit reading.
    pub fn delivery_id(&self) -> Uuid {
        match self {
            Self::Message(value) => value.delivery_id,
            Self::Notice(value) => value.delivery_id,
        }
    }
    /// Original immutable sending Header, independent of audit occurrences.
    pub fn dispatch_id(&self) -> Uuid {
        match self {
            Self::Message(value) => value.dispatch_id,
            Self::Notice(value) => value.dispatch.id,
        }
    }
    /// Committed project ordering for both input kinds.
    pub fn seq(&self) -> Sequence {
        match self {
            Self::Message(value) => value.seq,
            Self::Notice(value) => value.dispatch.creation_seq,
        }
    }
    /// Frozen recipient wake decision, never reparsed from rendered text.
    pub fn addressed(&self) -> bool {
        match self {
            Self::Message(value) => value.addressed,
            Self::Notice(value) => value.addressed,
        }
    }
    /// Canonical complete body or bounded event frame for native input.
    pub fn text(&self) -> Result<String> {
        match self {
            Self::Message(value) => value.text(),
            Self::Notice(value) => value.text(),
        }
    }
    /// Decode one canonical frame without interpreting events as messages.
    pub fn from_text(text: &str) -> Result<Self> {
        if text.starts_with("[AgentChat:Event]\n") {
            return Err(crate::Error::new(
                crate::ErrorCode::UnsupportedProtocol,
                "Audit-event native frames are unsupported; use agentchat install --new-project for fresh setup.",
            ));
        }
        if text.starts_with("[AgentChat:Notice]\n") {
            NoticeInput::from_text(text).map(Self::Notice)
        } else {
            MessageInput::from_text(text).map(Self::Message)
        }
    }
}
