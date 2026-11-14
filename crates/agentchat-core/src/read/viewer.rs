//! Context-free history windows and retained changes, independent of Web/GraphQL.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::MessageView;
use crate::{
    Result,
    model::Channel,
    sequence::{Cursor, Sequence},
    validation,
};

/// A retained history boundary. The event identity detects a restored or
/// divergent history even when its decimal sequence happens to be unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Checkpoint {
    /// Fixed project namespace, never selected by an HTTP path.
    pub project_id: Uuid,
    /// Last scanned project sequence, encoded as a decimal string.
    pub seq: Cursor,
    /// Event at that sequence; absent exactly at sequence zero.
    pub event_id: Option<Uuid>,
}

impl Checkpoint {
    /// Before retained history in an already selected namespace.
    pub fn start(project_id: Uuid) -> Self {
        Self {
            project_id,
            seq: Cursor::START,
            event_id: None,
        }
    }
}

/// Backward pagination is separate from the existing ascending `after` API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct WindowRequest {
    /// Target maximum; default 50 and maximum 200, additionally byte bounded.
    pub limit: Option<usize>,
    /// Exclusive creation boundary. Omission starts with the latest records.
    pub before: Option<Cursor>,
    /// Reuse this captured history boundary across older pages.
    pub snapshot: Option<Checkpoint>,
}
impl WindowRequest {
    /// Validate the item budget before opening a database snapshot.
    pub fn limit(self) -> Result<usize> {
        validation::page_limit(self.limit)
    }
}

/// Complete unbound presentation plus the thread's snapshot-bounded reply count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservedMessage {
    /// Full body and immutable author/channel/root metadata; no viewer receipt.
    #[serde(flatten)]
    pub message: MessageView,
    /// Total retained direct replies to this message's root at the snapshot.
    pub reply_count: u64,
}

/// Complete records in ascending display order, selected from a backward window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageWindow {
    /// Captured logical history boundary, retained across paging.
    pub snapshot: Checkpoint,
    /// Complete bodies fitting the item and encoded-byte budgets.
    pub items: Vec<ObservedMessage>,
    /// Exclusive boundary for the next older page; never skips an omitted row.
    pub next_before: Option<Cursor>,
}

/// A thread root is fetched independently from its reply window. Keeping them
/// separate lets each maximum-sized body fit without truncating either body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PinnedRoot {
    /// Same boundary used for related reply/count reads.
    pub snapshot: Checkpoint,
    /// Root and its complete snapshot-bounded reply count.
    pub root: ObservedMessage,
}

/// Finite retained catch-up. A continuation keeps `through` until drained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangeRequest {
    /// Last safely applied/scanned boundary, exclusive.
    pub after: Checkpoint,
    /// Fixed catch-up head; omission captures the current committed head.
    pub through: Option<Checkpoint>,
    /// Matching-item budget, default 50 and maximum 200.
    pub limit: Option<usize>,
}
impl ChangeRequest {
    /// Validate the item budget before opening a database snapshot.
    pub fn limit(self) -> Result<usize> {
        validation::page_limit(self.limit)
    }
}

/// Maximum event candidates inspected by one change read, including irrelevant
/// events. Empty filtered batches can still advance this explicitly scanned span.
pub const MAX_CHANGE_SCAN: usize = 200;

/// One complete retained viewer change. Event identities/sequences deduplicate
/// replay; neither receipt of a batch nor its local write acknowledges delivery.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ViewerChange {
    /// Stable retained event identity.
    pub event_id: Uuid,
    /// Immutable project ordering, never UUID or timestamp ordering.
    pub seq: Sequence,
    /// Complete typed projection at this batch's captured history boundary.
    pub change: ViewerChangeKind,
}

/// Viewer-relevant changes; unrelated informational events are safely scanned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ViewerChangeKind {
    /// A newly retained channel, including the initialized default channel.
    ChannelCreated {
        /// Complete immutable channel metadata.
        channel: Channel,
    },
    /// A complete root or direct reply, with a coherent total root reply count.
    MessageCreated {
        /// Full body, attribution and root identity.
        message: ObservedMessage,
    },
}

/// Bounded owned replay data. Progress covers only applied items and explicitly
/// scanned irrelevant events; it cannot advance past an omitted matching item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChangeBatch {
    /// Requested exclusive start, retained for overlap validation.
    pub after: Checkpoint,
    /// Last safely scanned event; the next read resumes here.
    pub scan: Checkpoint,
    /// Captured catch-up head, fixed until its continuations drain.
    pub head: Checkpoint,
    /// Complete matching changes, possibly empty for an irrelevant span.
    pub items: Vec<ViewerChange>,
    /// More retained events remain before this captured head.
    pub has_more: bool,
}
