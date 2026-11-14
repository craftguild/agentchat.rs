//! Concrete read requests retain their response type through every consumer.

mod viewer;
pub use viewer::*;

use super::{
    ActorView, AgentView, ChannelListCondition, ChannelPage, InboxView, MessageDetail, MessageView,
    Page, PageRequest, Query, QueryResult, ReservationView,
};
use crate::{Result, model::Event, read::WhoamiView};
use uuid::Uuid;

mod private {
    pub trait Sealed {}
}

/// A supported read and its response, associated before execution or decoding.
///
/// Only status varies by composition layer: core supplies `WhoamiView`, while
/// the runtime supplies its typed enriched status. Every other DTO stays the
/// same. Sealing prevents a caller from pairing a request with an unrelated DTO.
pub trait ReadRequest: private::Sealed + Send {
    /// Concrete response for the layer's status representation.
    type Response<S>;
    /// Adapt to internal dynamic dispatch without changing request policy.
    fn into_query(self) -> Query;
    /// Extract the statically associated result from internal typed dispatch.
    fn from_result<S>(result: QueryResult<S>) -> Self::Response<S>;
}

/// Exact contextual identity without membership or liveness effects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Whoami;
impl private::Sealed for Whoami {}
impl ReadRequest for Whoami {
    type Response<S> = S;
    fn into_query(self) -> Query {
        Query::Whoami
    }
    fn from_result<S>(result: QueryResult<S>) -> Self::Response<S> {
        let QueryResult::Whoami(value) = result else {
            unreachable!("sealed read/result association")
        };
        value
    }
}

/// Retained identities and effective presence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agents {
    /// Ascending first-join page.
    pub page: PageRequest,
}
impl private::Sealed for Agents {}
impl ReadRequest for Agents {
    type Response<S> = Page<AgentView>;
    fn into_query(self) -> Query {
        Query::Agents { page: self.page }
    }
    fn from_result<S>(result: QueryResult<S>) -> Self::Response<S> {
        let QueryResult::Agents(value) = result else {
            unreachable!("sealed read/result association")
        };
        value
    }
}

/// Retained channels in creation order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Channels {
    /// Presentation-only archive filter; omission/default is active.
    pub condition: ChannelListCondition,
    /// Ascending creation page.
    pub page: PageRequest,
}
impl private::Sealed for Channels {}
impl ReadRequest for Channels {
    type Response<S> = ChannelPage;
    fn into_query(self) -> Query {
        Query::Channels {
            condition: self.condition,
            page: self.page,
        }
    }
    fn from_result<S>(result: QueryResult<S>) -> Self::Response<S> {
        let QueryResult::Channels(value) = result else {
            unreachable!("sealed read/result association")
        };
        value
    }
}

/// Current channel membership.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelMembers {
    /// Normalized channel slug.
    pub channel: String,
    /// Ascending first-join page.
    pub page: PageRequest,
}
impl private::Sealed for ChannelMembers {}
impl ReadRequest for ChannelMembers {
    type Response<S> = Page<ActorView>;
    fn into_query(self) -> Query {
        Query::ChannelMembers {
            channel: self.channel,
            page: self.page,
        }
    }
    fn from_result<S>(result: QueryResult<S>) -> Self::Response<S> {
        let QueryResult::ChannelMembers(value) = result else {
            unreachable!("sealed read/result association")
        };
        value
    }
}

/// Complete message and bounded explicit receipts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// Exact message UUID.
    pub message_id: Uuid,
    /// Independent reader page.
    pub receipts: PageRequest,
}
impl private::Sealed for Message {}
impl ReadRequest for Message {
    type Response<S> = MessageDetail;
    fn into_query(self) -> Query {
        Query::Message {
            message_id: self.message_id,
            receipts: self.receipts,
        }
    }
    fn from_result<S>(result: QueryResult<S>) -> Self::Response<S> {
        let QueryResult::Message(value) = result else {
            unreachable!("sealed read/result association")
        };
        value
    }
}

/// Ascending immutable message history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct History {
    /// Optional channel slug.
    pub channel: Option<String>,
    /// Optional direct root UUID.
    pub thread: Option<Uuid>,
    /// Ascending creation page.
    pub page: PageRequest,
}
impl private::Sealed for History {}
impl ReadRequest for History {
    type Response<S> = Page<MessageView>;
    fn into_query(self) -> Query {
        Query::History {
            channel: self.channel,
            thread: self.thread,
            page: self.page,
        }
    }
    fn from_result<S>(result: QueryResult<S>) -> Self::Response<S> {
        let QueryResult::History(value) = result else {
            unreachable!("sealed read/result association")
        };
        value
    }
}

/// Case-sensitive literal substring matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Search {
    /// Bounded literal substring.
    pub query: String,
    /// Optional channel slug.
    pub channel: Option<String>,
    /// Optional root UUID.
    pub thread: Option<Uuid>,
    /// Ascending creation page.
    pub page: PageRequest,
}
impl private::Sealed for Search {}
impl ReadRequest for Search {
    type Response<S> = Page<MessageView>;
    fn into_query(self) -> Query {
        Query::Search {
            query: self.query,
            channel: self.channel,
            thread: self.thread,
            page: self.page,
        }
    }
    fn from_result<S>(result: QueryResult<S>) -> Self::Response<S> {
        let QueryResult::Search(value) = result else {
            unreachable!("sealed read/result association")
        };
        value
    }
}

/// Complete recipient retrieval without ACK/read effects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inbox {
    /// Ascending referenced-event page.
    pub page: PageRequest,
}
impl private::Sealed for Inbox {}
impl ReadRequest for Inbox {
    type Response<S> = Page<InboxView>;
    fn into_query(self) -> Query {
        Query::Inbox { page: self.page }
    }
    fn from_result<S>(result: QueryResult<S>) -> Self::Response<S> {
        let QueryResult::Inbox(value) = result else {
            unreachable!("sealed read/result association")
        };
        value
    }
}

/// Retained event envelopes with intentionally open event data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Events {
    /// Optional channel slug.
    pub channel: Option<String>,
    /// Ascending project-event page.
    pub page: PageRequest,
}
impl private::Sealed for Events {}
impl ReadRequest for Events {
    type Response<S> = Page<Event>;
    fn into_query(self) -> Query {
        Query::Events {
            channel: self.channel,
            page: self.page,
        }
    }
    fn from_result<S>(result: QueryResult<S>) -> Self::Response<S> {
        let QueryResult::Events(value) = result else {
            unreachable!("sealed read/result association")
        };
        value
    }
}

/// Effective retained lease groups.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reservations {
    /// Optional retained owner handle.
    pub owner: Option<String>,
    /// Include released and expired groups.
    pub all: bool,
    /// Ascending acquisition page.
    pub page: PageRequest,
}
impl private::Sealed for Reservations {}
impl ReadRequest for Reservations {
    type Response<S> = Page<ReservationView>;
    fn into_query(self) -> Query {
        Query::Reservations {
            owner: self.owner,
            all: self.all,
            page: self.page,
        }
    }
    fn from_result<S>(result: QueryResult<S>) -> Self::Response<S> {
        let QueryResult::Reservations(value) = result else {
            unreachable!("sealed read/result association")
        };
        value
    }
}

impl crate::application::Application {
    /// Execute a concrete request and retain its specific DTO through the caller.
    pub async fn read<R: ReadRequest>(
        &self,
        context: Option<&crate::model::Context>,
        request: R,
    ) -> Result<R::Response<WhoamiView>> {
        self.query(context, request.into_query())
            .await
            .map(R::from_result)
    }
}
