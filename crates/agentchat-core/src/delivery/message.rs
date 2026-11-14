//! Attributed complete message input, independent of native wake mechanisms.
use crate::{Error, ErrorCode, Result, read::IdentityLabel, sequence::Sequence, validation};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One committed message selected for one current launch generation.
/// Frozen recipient resolution supplies `addressed`; host adapters never parse
/// mentions or invent recipients. Rendering does not acknowledge or mark read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageInput {
    /// Persistent project namespace.
    pub project_id: Uuid,
    /// Current native launch lifetime, distinct from a reconnect incarnation.
    pub generation: Uuid,
    /// Stable recipient-delivery identity, independent of reading.
    pub delivery_id: Uuid,
    /// Explicit fresh-project framing contract; older policy revisions are refused.
    pub frame_revision: crate::project::NativeFrameRevision,
    /// Frozen policy for this complete body; rendering records no read.
    pub auto_read: bool,
    /// Sending Header identity, equal to the immutable public Message identity.
    pub dispatch_id: Uuid,
    /// Committed project ordering, never a timestamp or lexical UUID order.
    pub seq: Sequence,
    /// Exact immutable message.
    pub message_id: Uuid,
    /// Root message; equals message_id for root posts.
    pub thread_id: Uuid,
    /// Immutable author UUID and handle.
    pub author: IdentityLabel,
    /// Exact channel identity.
    pub channel_id: Uuid,
    /// Immutable normalized channel name.
    pub channel: String,
    /// Complete valid UTF-8 body, including all whitespace and Markdown.
    pub body: String,
    /// Whether frozen wake/mention resolution selected this recipient.
    pub addressed: bool,
}
impl MessageInput {
    /// Validate and render all original body bytes with unambiguous attribution.
    /// The old 8-KiB wake-hint budget does not apply to this full-body context.
    /// Only attribution is JSON quoted. The body remains raw UTF-8 between a
    /// length-bearing header and a terminal marker: escaping every control byte
    /// could expand a valid body beyond a host notification's character limit.
    /// The terminal marker protects original trailing whitespace from host trim.
    pub fn text(&self) -> Result<String> {
        if [
            self.project_id,
            self.generation,
            self.delivery_id,
            self.dispatch_id,
            self.message_id,
            self.thread_id,
            self.author.actor_id,
            self.channel_id,
        ]
        .into_iter()
        .any(|id| id.is_nil())
            || self.dispatch_id != self.message_id
            || (self.author.kind == crate::model::ActorKind::User && self.author.handle != "user")
            || validation::handle(&self.author.handle)? != self.author.handle
            || validation::channel(&self.channel)? != self.channel
        {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "Complete original message attribution is required.",
            ));
        }
        validation::body(&self.body)?;
        let mut header = serde_json::to_value(self).map_err(|_| {
            Error::new(
                ErrorCode::IntegrityError,
                "Full message context encoding failed.",
            )
        })?;
        let fields = header.as_object_mut().expect("message object");
        fields.remove("body");
        fields.insert("body_utf8_bytes".into(), self.body.len().into());
        let header = serde_json::to_string(&header).map_err(|_| invalid())?;
        Ok(format!("{PREFIX}{header}\n--\n{}{END}", self.body))
    }
    /// Decode the compact frame or its exact retained DispatchV4 spelling.
    /// Body bytes and saved capsules are never normalized. Marker lookalikes
    /// do not delimit the body; its byte length and canonical metadata must match.
    pub fn from_text(text: &str) -> Result<Self> {
        // Already published inputs keep their original bytes for receipt recovery.
        let (rest, end) = if let Some(rest) = text.strip_prefix(RETAINED_PREFIX) {
            (rest, RETAINED_END)
        } else {
            (text.strip_prefix(PREFIX).ok_or_else(invalid)?, END)
        };
        let (header, rest) = rest.split_once("\n--\n").ok_or_else(invalid)?;
        // Only metadata is delimited. The body can contain any marker or
        // separator; its declared byte count locates the sole terminal suffix.
        let mut value: serde_json::Value = serde_json::from_str(header).map_err(|_| invalid())?;
        let fields = value.as_object_mut().ok_or_else(invalid)?;
        if fields
            .get("frame_revision")
            .and_then(serde_json::Value::as_str)
            != Some("dispatch_v4")
        {
            return Err(Error::new(
                ErrorCode::UnsupportedProtocol,
                "The native message frame is unsupported; use agentchat install --new-project for fresh setup.",
            ));
        }
        let count = fields
            .remove("body_utf8_bytes")
            .and_then(|v| v.as_u64())
            .and_then(|v| usize::try_from(v).ok())
            .ok_or_else(invalid)?;
        let body = rest.get(..count).ok_or_else(invalid)?;
        if fields.contains_key("body") || rest.get(count..) != Some(end) {
            return Err(invalid());
        }
        fields.insert("body".into(), body.into());
        let input: Self = serde_json::from_value(value).map_err(|_| invalid())?;
        if input.text()? != format!("{PREFIX}{header}\n--\n{body}{END}") {
            return Err(invalid());
        }
        Ok(input)
    }
}
const PREFIX: &str = "[AgentChat:Message]\n";
const END: &str = "\n[/AgentChat:Message]";
const RETAINED_PREFIX: &str = "[AgentChat:Message]\n--\n";
const RETAINED_END: &str = "\n--\n[/AgentChat:Message]";
fn invalid() -> Error {
    Error::new(
        ErrorCode::InvalidArgument,
        "The complete original native message frame is invalid.",
    )
}
