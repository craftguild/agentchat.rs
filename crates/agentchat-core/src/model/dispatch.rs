//! Immutable sending Header, with exactly one matching payload subtype.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::{Message, NoticeKind};
use crate::{Error, ErrorCode, Result, sequence::Sequence, time::Timestamp};

/// Conversation content and typed notification are the only sending kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchKind {
    /// Conversation content with an author, immutable body and optional root.
    Message,
    /// Typed notification content outside conversation reads and read receipts.
    Notice,
}

impl DispatchKind {
    /// Canonical shared-wire and relational spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::Notice => "notice",
        }
    }
}

impl std::str::FromStr for DispatchKind {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "message" => Ok(Self::Message),
            "notice" => Ok(Self::Notice),
            _ => Err(invalid("Unknown Dispatch kind.")),
        }
    }
}

/// Shared immutable Header. Its sequence is independent of the audit log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dispatch {
    /// Stable Header identity, also the public Message UUID for Message kind.
    pub id: Uuid,
    /// The only payload subtype that may own this Header.
    pub kind: DispatchKind,
    /// Applicable receiving channel, absent for project-wide notification.
    pub channel_id: Option<Uuid>,
    /// Immutable committed creation time.
    pub created_at: Timestamp,
    /// Independent Dispatch order from the shared transactional allocator.
    pub creation_seq: Sequence,
}

/// Typed notification content; it is never a reply root or read target.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DispatchNotice {
    /// Exact Header that owns this payload.
    pub dispatch_id: Uuid,
    /// Typed notification semantics, distinct from an audit EventKind.
    pub kind: NoticeKind,
    /// Complete structured content validated against the notification kind.
    pub data: Value,
}

impl DispatchNotice {
    /// Persist complete kind-matching content, never a truncated native view.
    pub fn validate(&self) -> Result<()> {
        if self.dispatch_id.is_nil() {
            return Err(invalid("A Notice requires its owning Dispatch identity."));
        }
        self.kind.validate_data(&self.data)
    }
}

/// Exactly one payload belongs to a Header. Message remains its public DTO.
#[derive(Debug, Clone, PartialEq)]
pub enum DispatchPayload {
    /// Conversation payload projected with the Header's public identity/order.
    Message(Message),
    /// Typed notification payload owned by the matching Notice Header.
    Notice(DispatchNotice),
}

impl Dispatch {
    /// Validate the aggregate before publishing any recipient or initial history.
    pub fn validate_payload(&self, payload: &DispatchPayload) -> Result<()> {
        if self.id.is_nil() || self.channel_id.is_some_and(|id| id.is_nil()) {
            return Err(invalid("Dispatch identity or scope is invalid."));
        }
        match (self.kind, payload) {
            (DispatchKind::Message, DispatchPayload::Message(message)) => {
                if self.id != message.id
                    || self.channel_id != Some(message.channel_id)
                    || self.created_at != message.created_at
                    || self.creation_seq != message.creation_seq
                    || message.author_id.is_nil()
                    || message
                        .root_message_id
                        .is_some_and(|id| id.is_nil() || id == self.id)
                {
                    return Err(invalid(
                        "Message payload disagrees with its Dispatch Header.",
                    ));
                }
                crate::validation::body(&message.body)
            }
            (DispatchKind::Notice, DispatchPayload::Notice(notice)) => {
                if self.id != notice.dispatch_id {
                    return Err(invalid("Notice belongs to another Dispatch Header."));
                }
                notice.kind.validate_dispatch(self.channel_id, &notice.data)
            }
            _ => Err(invalid("Dispatch kind and payload subtype disagree.")),
        }
    }
}

fn invalid(message: &str) -> Error {
    Error::new(ErrorCode::IntegrityError, message)
}
