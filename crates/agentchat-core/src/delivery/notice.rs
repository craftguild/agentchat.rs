//! Typed bounded notification input, independent of immutable audit records.
use super::MAX_WAKE_BYTES;
use crate::{
    Error, ErrorCode, Result,
    model::{Dispatch, DispatchKind, DispatchNotice, DispatchPayload},
    project::{InputReceiptPolicy, NativeFrameRevision},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// One immutable Notice subtype selected for one exact recipient. The complete
/// payload remains in dispatch_notices when the bounded native view omits data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoticeInput {
    /// Persistent project namespace.
    pub project_id: Uuid,
    /// Original native launch lifetime.
    pub generation: Uuid,
    /// Exact original recipient-delivery identity.
    pub delivery_id: Uuid,
    /// Explicit fresh-project frame; old Event envelopes cannot decode here.
    pub frame_revision: NativeFrameRevision,
    /// Frozen policy; a Notice never creates an automatic Message read.
    pub auto_read: bool,
    /// Sealed sending identity, scope, time and queue order.
    pub dispatch: Dispatch,
    /// Original typed notification content, independent of an audit Event.
    pub notice: DispatchNotice,
    /// Frozen notification wake decision.
    pub addressed: bool,
    /// Only the bounded native view omits oversized data, never stored content.
    pub data_omitted: bool,
}
impl NoticeInput {
    /// Validate the complete immutable aggregate before constructing its view.
    pub fn new(
        project_id: Uuid,
        generation: Uuid,
        delivery_id: Uuid,
        dispatch: &Dispatch,
        notice: &DispatchNotice,
        addressed: bool,
        policy: InputReceiptPolicy,
    ) -> Result<Self> {
        dispatch.validate_payload(&DispatchPayload::Notice(notice.clone()))?;
        let mut input = Self {
            project_id,
            generation,
            delivery_id,
            frame_revision: NativeFrameRevision::DispatchV4,
            auto_read: policy.auto_read,
            dispatch: dispatch.clone(),
            notice: notice.clone(),
            addressed,
            data_omitted: false,
        };
        input.validate()?;
        if input.encoded()?.len() > MAX_WAKE_BYTES {
            input.notice.data = Value::Null;
            input.data_omitted = true;
        }
        input.text()?;
        Ok(input)
    }
    /// Render canonical control context within the established 8-KiB budget.
    pub fn text(&self) -> Result<String> {
        self.validate()?;
        let text = self.encoded()?;
        if text.len() > MAX_WAKE_BYTES {
            return Err(invalid());
        }
        Ok(text)
    }
    /// Decode one exact new Notice frame, preserving explicit omission.
    pub fn from_text(text: &str) -> Result<Self> {
        if text.len() > MAX_WAKE_BYTES {
            return Err(invalid());
        }
        let data = text
            .strip_prefix(PREFIX)
            .and_then(|value| value.strip_suffix(END))
            .ok_or_else(invalid)?;
        let input: Self = serde_json::from_str(data).map_err(|_| invalid())?;
        if input.text()? != text {
            return Err(invalid());
        }
        Ok(input)
    }
    fn encoded(&self) -> Result<String> {
        let data = serde_json::to_string(self).map_err(|_| invalid())?;
        Ok(format!("{PREFIX}{data}{END}"))
    }
    fn validate(&self) -> Result<()> {
        self.notice
            .kind
            .validate_channel_presence(self.dispatch.channel_id)?;
        if [
            self.project_id,
            self.generation,
            self.delivery_id,
            self.dispatch.id,
        ]
        .into_iter()
        .any(|id| id.is_nil())
            || self.dispatch.kind != DispatchKind::Notice
            || self.notice.dispatch_id != self.dispatch.id
            || self.dispatch.channel_id.is_some_and(|id| id.is_nil())
            || (self.data_omitted && !self.notice.data.is_null())
        {
            return Err(invalid());
        }
        if !self.data_omitted {
            self.dispatch
                .validate_payload(&DispatchPayload::Notice(self.notice.clone()))?;
        }
        Ok(())
    }
}
const PREFIX: &str = "[AgentChat:Notice]\n--\n";
const END: &str = "\n--\n[/AgentChat:Notice]";
fn invalid() -> Error {
    Error::new(
        ErrorCode::InvalidArgument,
        "The original bounded native Notice is invalid.",
    )
}
