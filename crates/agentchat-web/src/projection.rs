//! Typed GraphQL presentation and complete-record cuts for rendering expansion.

use crate::render::{RenderedBody, Renderer};
use agentchat_core::{
    Result,
    model::Channel,
    read::{ChangeBatch, Checkpoint, MessageWindow, ObservedMessage, PinnedRoot, ViewerChangeKind},
    sequence::{Cursor, Sequence},
};
use serde::Serialize;
use std::sync::Arc;
use uuid::Uuid;

/// Headroom below Chigiri's hard 1 MiB complete-event limit. Every normal
/// projection plus metadata/framing fits; aliases still obey SDK output bounds.
pub(crate) const PAGE_BYTES: usize = 896 * 1024;
const ENVELOPE_RESERVE: usize = 16 * 1024;

pub(crate) struct Message {
    pub raw: ObservedMessage,
    pub body: Arc<RenderedBody>,
}
impl Message {
    async fn prepare(raw: ObservedMessage, renderer: &Renderer) -> Arc<Self> {
        let body = renderer.render(&raw.message.message.body).await;
        let mut message = Self { raw, body };
        // A maximum JSON-expanding body plus normal HTML can exceed the page
        // target. Escaped original text has a known finite complete-body bound.
        if message.bytes() > PAGE_BYTES - ENVELOPE_RESERVE {
            message.body = Renderer::plain_text(&message.raw.message.message.body);
        }
        Arc::new(message)
    }
    fn bytes(&self) -> usize {
        #[derive(Serialize)]
        struct Measure<'a> {
            #[serde(flatten)]
            raw: &'a ObservedMessage,
            #[serde(rename = "bodyHtml")]
            html: &'a str,
            #[serde(rename = "renderFallback")]
            fallback: bool,
        }
        // Typed serialization only measures the eventual presentation. Core's
        // flattened metadata is slightly larger than these GraphQL projections.
        serde_json::to_vec(&Measure {
            raw: &self.raw,
            html: &self.body.html,
            fallback: self.body.fallback,
        })
        .expect("serializable typed presentation")
        .len()
            + 1024
    }
}

pub(crate) struct Window {
    pub snapshot: Checkpoint,
    pub items: Vec<Arc<Message>>,
    pub next_before: Option<Cursor>,
}
impl Window {
    pub async fn prepare(mut page: MessageWindow, renderer: &Renderer) -> Result<Arc<Self>> {
        let mut items = Vec::new();
        let mut bytes = ENVELOPE_RESERVE;
        for raw in page.items.iter().rev() {
            let message = Message::prepare(raw.clone(), renderer).await;
            let size = message.bytes();
            if !items.is_empty() && bytes + size > PAGE_BYTES {
                break;
            }
            bytes += size;
            items.push(message);
        }
        page.retain_latest(items.len())?;
        items.reverse();
        Ok(Arc::new(Self {
            snapshot: page.snapshot,
            items,
            next_before: page.next_before,
        }))
    }
}

pub(crate) struct Pinned {
    pub snapshot: Checkpoint,
    pub root: Arc<Message>,
}
impl Pinned {
    pub async fn prepare(page: PinnedRoot, renderer: &Renderer) -> Arc<Self> {
        Arc::new(Self {
            snapshot: page.snapshot,
            root: Message::prepare(page.root, renderer).await,
        })
    }
}

pub(crate) struct Change {
    pub event_id: Uuid,
    pub seq: Sequence,
    pub channel: Option<Channel>,
    pub message: Option<Arc<Message>>,
}
pub(crate) struct Changes {
    pub after: Checkpoint,
    pub scan: Checkpoint,
    pub head: Checkpoint,
    pub items: Vec<Arc<Change>>,
    pub has_more: bool,
}
impl Changes {
    /// Retain an owned raw remainder for a source, or let a finite caller resume
    /// from `scan` through `head`. Shared core code creates the exact boundary.
    pub async fn prepare(
        batch: ChangeBatch,
        renderer: &Renderer,
    ) -> Result<(Arc<Self>, Option<ChangeBatch>)> {
        let mut items = Vec::new();
        let mut bytes = ENVELOPE_RESERVE;
        for item in &batch.items {
            let (channel, message, size) = match &item.change {
                ViewerChangeKind::ChannelCreated { channel } => (Some(channel.clone()), None, 1024),
                ViewerChangeKind::MessageCreated { message } => {
                    let message = Message::prepare(message.clone(), renderer).await;
                    let size = message.bytes() + 256;
                    (None, Some(message), size)
                }
            };
            if !items.is_empty() && bytes + size > PAGE_BYTES {
                break;
            }
            bytes += size;
            items.push(Arc::new(Change {
                event_id: item.event_id,
                seq: item.seq,
                channel,
                message,
            }));
        }
        let (batch, rest) = batch.split_prefix(items.len())?;
        Ok((
            Arc::new(Self {
                after: batch.after,
                scan: batch.scan,
                head: batch.head,
                has_more: batch.has_more,
                items,
            }),
            rest,
        ))
    }
}
