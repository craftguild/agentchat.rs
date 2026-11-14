//! Typed finite read requests; storage receives only validated filter identities.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{ChangeRequest, ChannelListCondition, Checkpoint, PageRequest, WindowRequest};

/// Shared queries used by CLI, MCP, observers and host inbox retrieval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Query {
    /// Current restore-aware project boundary for context-free snapshots.
    ViewerHead,
    /// Latest/backward root-only retained history with batched reply counts.
    RootWindow {
        /// Selected channel slug.
        channel: String,
        /// Independent backward page and captured logical snapshot.
        #[serde(default)]
        window: WindowRequest,
    },
    /// One pinned root, independent of reply pagination and byte budgets.
    PinnedRoot {
        /// Selected channel, checked against the root.
        channel: String,
        /// Exact retained root UUID.
        root_message_id: Uuid,
        /// Optional captured logical snapshot.
        snapshot: Option<Checkpoint>,
    },
    /// Latest/backward direct replies, excluding the separately pinned root.
    ReplyWindow {
        /// Selected channel, checked against the root.
        channel: String,
        /// Exact retained root UUID.
        root_message_id: Uuid,
        /// Independent reply cursor and captured logical snapshot.
        #[serde(default)]
        window: WindowRequest,
    },
    /// Complete retained project changes for context-free observation.
    ViewerChanges {
        /// Explicit safely-applied start and optional fixed catch-up head.
        changes: ChangeRequest,
    },
    /// Exact bound identity; never infer participation from the current directory.
    Whoami,
    /// Retained identities and current verified presence.
    Agents {
        /// Finite ordering and item limit.
        #[serde(default)]
        page: PageRequest,
    },
    /// Retained channels in creation order.
    Channels {
        /// Presentation-only derived archive filter.
        #[serde(default)]
        condition: ChannelListCondition,
        /// Finite ordering and item limit.
        #[serde(default)]
        page: PageRequest,
    },
    /// Current joined membership of one channel.
    ChannelMembers {
        /// Channel slug.
        channel: String,
        /// Finite first-join ordering.
        #[serde(default)]
        page: PageRequest,
    },
    /// Complete message and a separately paged reader collection.
    Message {
        /// Exact message UUID.
        message_id: Uuid,
        /// Receipt pagination; the message body itself is returned completely.
        #[serde(default)]
        receipts: PageRequest,
    },
    /// Immutable root/reply history.
    History {
        /// Optional channel slug.
        channel: Option<String>,
        /// Optional direct root UUID.
        thread: Option<Uuid>,
        /// Ascending message creation order.
        #[serde(default)]
        page: PageRequest,
    },
    /// Case-sensitive literal substring search, with no wildcard/query language.
    Search {
        /// At most 1024 UTF-8 bytes; an empty substring matches every body.
        query: String,
        /// Optional channel slug.
        channel: Option<String>,
        /// Optional root UUID, consistent with the channel filter.
        thread: Option<Uuid>,
        /// Ascending message creation order.
        #[serde(default)]
        page: PageRequest,
    },
    /// Explicit recipient retrieval without acknowledging or reading.
    Inbox {
        /// Ascending referenced event order.
        #[serde(default)]
        page: PageRequest,
    },
    /// Finite event retrieval, also used to catch up an observer.
    Events {
        /// Optional channel slug.
        channel: Option<String>,
        /// Ascending project event order.
        #[serde(default)]
        page: PageRequest,
    },
    /// Reservation groups, including logical expiry at the snapshot's clock.
    Reservations {
        /// Optional retained owner handle.
        owner: Option<String>,
        /// Include released and expired groups.
        #[serde(default)]
        all: bool,
        /// Ascending acquisition generation/event order.
        #[serde(default)]
        page: PageRequest,
    },
}

/// Validated relational message predicates, never raw SQL or wildcard syntax.
#[derive(Debug, Clone, Default)]
pub struct MessageFilter {
    /// Applicable retained channel.
    pub channel: Option<Uuid>,
    /// Allowed channel set for a contextual participant; none is observer-wide.
    pub channels: Option<Vec<Uuid>>,
    /// Root message identity; includes that root and its direct replies.
    pub thread: Option<Uuid>,
    /// Literal case-sensitive substring.
    pub query: Option<String>,
}

impl Query {
    pub(crate) fn normalize(&mut self) -> crate::Result<()> {
        use crate::{Error, ErrorCode, validation};
        match self {
            Self::RootWindow { channel, window }
            | Self::ReplyWindow {
                channel, window, ..
            } => {
                *channel = validation::channel(channel)?;
                window.limit()?;
            }
            Self::PinnedRoot { channel, .. } => {
                *channel = validation::channel(channel)?;
            }
            Self::ViewerChanges { changes } => {
                changes.limit()?;
            }
            Self::Agents { page } | Self::Channels { page, .. } | Self::Inbox { page } => {
                page.limit()?;
            }
            Self::Message { receipts, .. } => {
                receipts.limit()?;
            }
            Self::ChannelMembers { channel, page } => {
                *channel = validation::channel(channel)?;
                page.limit()?;
            }
            Self::History { channel, page, .. } | Self::Events { channel, page } => {
                *channel = channel.as_deref().map(validation::channel).transpose()?;
                page.limit()?;
            }
            Self::Search {
                query,
                channel,
                page,
                ..
            } => {
                if query.len() > validation::MAX_QUERY_BYTES {
                    return Err(Error::new(
                        ErrorCode::InvalidArgument,
                        "Search query exceeds 1024 UTF-8 bytes.",
                    ));
                }
                *channel = channel.as_deref().map(validation::channel).transpose()?;
                page.limit()?;
            }
            Self::Reservations { owner, page, .. } => {
                *owner = owner.as_deref().map(validation::handle).transpose()?;
                page.limit()?;
            }
            Self::Whoami | Self::ViewerHead => {}
        }
        Ok(())
    }
}
