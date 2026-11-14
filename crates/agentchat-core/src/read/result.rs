//! Typed dispatch results. Variants are internal and never tag existing wires.

use serde::Serialize;

use super::*;
use crate::model::Event;

/// A shared read result retained through application and consumer adaptation.
///
/// Deliberately serialize-only: overlapping page shapes cannot identify their
/// own type. A wire client must select the concrete DTO from its request before
/// decoding. Runtime composition replaces only the status payload.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum QueryResult<S = WhoamiView> {
    /// Current restore-aware context-free snapshot boundary.
    ViewerHead(Checkpoint),
    /// Backward root-only window.
    RootWindow(MessageWindow),
    /// Pinned root independent of its reply window.
    PinnedRoot(PinnedRoot),
    /// Backward direct-reply window.
    ReplyWindow(MessageWindow),
    /// Complete retained changes with safe scanned progress.
    ViewerChanges(ChangeBatch),
    /// Exact conversation binding, including unjoined/left states.
    Whoami(S),
    /// Retained identities and presence.
    Agents(Page<AgentView>),
    /// Retained channels with derived archive metadata.
    Channels(ChannelPage),
    /// Current channel membership.
    ChannelMembers(Page<ActorView>),
    /// Complete message with independently paged canonical receipts.
    Message(MessageDetail),
    /// Ascending immutable history.
    History(Page<MessageView>),
    /// Ascending literal search matches.
    Search(Page<MessageView>),
    /// Complete recipient inbox entries.
    Inbox(Page<InboxView>),
    /// Extensible events within a typed page.
    Events(Page<Event>),
    /// Effective reservation states.
    Reservations(Page<ReservationView>),
}

impl<S> QueryResult<S> {
    /// Compose adapter-owned status metadata without touching other DTOs.
    pub fn map_whoami<T>(self, map: impl FnOnce(S) -> T) -> QueryResult<T> {
        match self {
            Self::ViewerHead(value) => QueryResult::ViewerHead(value),
            Self::RootWindow(value) => QueryResult::RootWindow(value),
            Self::PinnedRoot(value) => QueryResult::PinnedRoot(value),
            Self::ReplyWindow(value) => QueryResult::ReplyWindow(value),
            Self::ViewerChanges(value) => QueryResult::ViewerChanges(value),
            Self::Whoami(value) => QueryResult::Whoami(map(value)),
            Self::Agents(value) => QueryResult::Agents(value),
            Self::Channels(value) => QueryResult::Channels(value),
            Self::ChannelMembers(value) => QueryResult::ChannelMembers(value),
            Self::Message(value) => QueryResult::Message(value),
            Self::History(value) => QueryResult::History(value),
            Self::Search(value) => QueryResult::Search(value),
            Self::Inbox(value) => QueryResult::Inbox(value),
            Self::Events(value) => QueryResult::Events(value),
            Self::Reservations(value) => QueryResult::Reservations(value),
        }
    }
}
