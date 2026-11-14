//! One retained claim shape spans message bodies and non-message event context.
use super::{
    MessageDispatch, MessageItem, MessageProgress, NativeInput, NoticeInput, Presentation, WakeItem,
};
use crate::{
    Result,
    model::{Context, Delivery, Dispatch},
    sequence::Sequence,
};
use uuid::Uuid;

/// Transaction-local immutable candidate for the next native acceptance slot.
#[derive(Debug, Clone)]
pub enum NativeItem {
    /// Complete original message and retained labels.
    Message(Box<MessageItem>),
    /// Original non-message event and its frozen recipient decision.
    Notice(Box<WakeItem>),
}
impl NativeItem {
    /// Exact original recipient row, including settled rows.
    pub fn delivery(&self) -> &Delivery {
        match self {
            Self::Message(item) => &item.delivery,
            Self::Notice(item) => &item.delivery,
        }
    }
    /// Immutable sending Header carrying native FIFO ordering.
    pub fn dispatch(&self) -> &Dispatch {
        match self {
            Self::Message(item) => &item.dispatch,
            Self::Notice(item) => &item.dispatch,
        }
    }
    /// Consume only the delivery bookkeeping after constructing native input.
    pub fn into_delivery(self) -> Delivery {
        match self {
            Self::Message(item) => item.delivery,
            Self::Notice(item) => item.delivery,
        }
    }
}
/// One committed non-message claim on the same per-generation native stream.
#[derive(Debug, Clone)]
pub struct NoticeDispatch {
    /// Original bounded event context and frozen wake decision.
    pub input: NoticeInput,
    /// Exact committed callback fence.
    pub token: Uuid,
    /// Cumulative attempt, shared across input kinds and reconnects.
    pub attempt: u32,
    /// Fresh preparation phase; the adapter independently rechecks it.
    pub presentation: Presentation,
    /// Read only the original published native receipt, without new output.
    pub awaiting_acceptance: bool,
    /// Original frame contract, independent of the current project settings.
    pub frame_revision: crate::project::NativeFrameRevision,
}
/// One current stream head; separate native transports cannot overtake it.
#[derive(Debug, Clone)]
pub enum NativeDispatch {
    /// Complete original body.
    Message(MessageDispatch),
    /// Non-message control context.
    Notice(NoticeDispatch),
}
impl NativeDispatch {
    /// Build an exact input-kind dispatch from one committed claim.
    pub fn new(
        input: NativeInput,
        token: Uuid,
        attempt: u32,
        presentation: Presentation,
        awaiting_acceptance: bool,
    ) -> Self {
        match input {
            NativeInput::Message(input) => Self::Message(MessageDispatch {
                input,
                token,
                attempt,
                presentation,
                awaiting_acceptance,
                frame_revision: crate::project::NativeFrameRevision::DispatchV4,
            }),
            NativeInput::Notice(input) => Self::Notice(NoticeDispatch {
                input,
                token,
                attempt,
                presentation,
                awaiting_acceptance,
                frame_revision: crate::project::NativeFrameRevision::DispatchV4,
            }),
        }
    }
    /// Retain immutable input for completion validation and native codecs.
    pub fn input(&self) -> NativeInput {
        match self {
            Self::Message(value) => NativeInput::Message(value.input.clone()),
            Self::Notice(value) => NativeInput::Notice(value.input.clone()),
        }
    }
    /// Exact current callback token, never a delivery acknowledgement.
    pub fn token(&self) -> Uuid {
        match self {
            Self::Message(value) => value.token,
            Self::Notice(value) => value.token,
        }
    }
    /// Cumulative head attempt, unchanged by receipt-only polling.
    pub fn attempt(&self) -> u32 {
        match self {
            Self::Message(value) => value.attempt,
            Self::Notice(value) => value.attempt,
        }
    }
    /// Native phase captured at preparation, not ongoing authority.
    pub fn presentation(&self) -> &Presentation {
        match self {
            Self::Message(value) => &value.presentation,
            Self::Notice(value) => &value.presentation,
        }
    }
    /// Whether this claim may only read a prior native result.
    pub fn awaiting_acceptance(&self) -> bool {
        match self {
            Self::Message(value) => value.awaiting_acceptance,
            Self::Notice(value) => value.awaiting_acceptance,
        }
    }
    /// Original frame identity used only for exact receipt settlement.
    pub fn frame_revision(&self) -> crate::project::NativeFrameRevision {
        match self {
            Self::Message(value) => value.frame_revision,
            Self::Notice(value) => value.frame_revision,
        }
    }

    /// Carry the revision from an existing durable head into receipt-only work.
    /// Selecting this value never grants submission authority.
    pub fn with_frame_revision(mut self, revision: crate::project::NativeFrameRevision) -> Self {
        match &mut self {
            Self::Message(value) => value.frame_revision = revision,
            Self::Notice(value) => value.frame_revision = revision,
        }
        self
    }
}
/// One original native outcome, committed through the shared ready-write engine.
#[derive(Debug, Clone)]
pub struct NativeCompletion {
    /// Exact original application connection.
    pub context: Context,
    /// Previously committed native head and immutable input.
    pub dispatch: NativeDispatch,
    /// Explicit acceptance, durable publication or native failure.
    pub outcome: Result<MessageProgress>,
}

/// Strictly ordered metadata helpers without cloning full input bodies.
impl NativeDispatch {
    /// Original generation, independent of attachment replacement.
    pub fn generation(&self) -> Uuid {
        match self {
            Self::Message(value) => value.input.generation,
            Self::Notice(value) => value.input.generation,
        }
    }
    /// Exact stable original recipient-delivery identity.
    pub fn delivery_id(&self) -> Uuid {
        match self {
            Self::Message(value) => value.input.delivery_id,
            Self::Notice(value) => value.input.delivery_id,
        }
    }
    /// One project cursor orders both messages and events.
    pub fn seq(&self) -> Sequence {
        match self {
            Self::Message(value) => value.input.seq,
            Self::Notice(value) => value.input.dispatch.creation_seq,
        }
    }
}
