//! Concrete context-free viewer requests and their associated response DTOs.

use super::{ReadRequest, private};
use crate::read::{
    ChangeBatch, ChangeRequest, Checkpoint, MessageWindow, Query, QueryResult, WindowRequest,
};
use uuid::Uuid;

/// Capture the current project boundary without participant or host metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewerHead;
impl private::Sealed for ViewerHead {}
impl ReadRequest for ViewerHead {
    type Response<S> = Checkpoint;
    fn into_query(self) -> Query {
        Query::ViewerHead
    }
    fn from_result<S>(result: QueryResult<S>) -> Self::Response<S> {
        let QueryResult::ViewerHead(value) = result else {
            unreachable!("sealed read/result association")
        };
        value
    }
}

/// Latest/backward retained roots with coherent batched reply counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootWindow {
    /// Selected channel slug.
    pub channel: String,
    /// Independent backward root page.
    pub window: WindowRequest,
}
impl private::Sealed for RootWindow {}
impl ReadRequest for RootWindow {
    type Response<S> = MessageWindow;
    fn into_query(self) -> Query {
        Query::RootWindow {
            channel: self.channel,
            window: self.window,
        }
    }
    fn from_result<S>(result: QueryResult<S>) -> Self::Response<S> {
        let QueryResult::RootWindow(value) = result else {
            unreachable!("sealed read/result association")
        };
        value
    }
}

/// Independently pinned root, excluding its reply page from the same byte budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedRoot {
    /// Selected channel, checked against the root.
    pub channel: String,
    /// Exact retained root.
    pub root_message_id: Uuid,
    /// Optional fixed logical snapshot.
    pub snapshot: Option<Checkpoint>,
}
impl private::Sealed for PinnedRoot {}
impl ReadRequest for PinnedRoot {
    type Response<S> = crate::read::PinnedRoot;
    fn into_query(self) -> Query {
        Query::PinnedRoot {
            channel: self.channel,
            root_message_id: self.root_message_id,
            snapshot: self.snapshot,
        }
    }
    fn from_result<S>(result: QueryResult<S>) -> Self::Response<S> {
        let QueryResult::PinnedRoot(value) = result else {
            unreachable!("sealed read/result association")
        };
        value
    }
}

/// Latest/backward direct replies, with a cursor independent from root history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyWindow {
    /// Selected channel, checked against the root.
    pub channel: String,
    /// Exact retained root.
    pub root_message_id: Uuid,
    /// Independent backward reply page.
    pub window: WindowRequest,
}
impl private::Sealed for ReplyWindow {}
impl ReadRequest for ReplyWindow {
    type Response<S> = MessageWindow;
    fn into_query(self) -> Query {
        Query::ReplyWindow {
            channel: self.channel,
            root_message_id: self.root_message_id,
            window: self.window,
        }
    }
    fn from_result<S>(result: QueryResult<S>) -> Self::Response<S> {
        let QueryResult::ReplyWindow(value) = result else {
            unreachable!("sealed read/result association")
        };
        value
    }
}

/// Bounded retained change replay from an explicitly applied history boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewerChanges {
    /// Exclusive start, fixed optional head and finite matching-item limit.
    pub changes: ChangeRequest,
}
impl private::Sealed for ViewerChanges {}
impl ReadRequest for ViewerChanges {
    type Response<S> = ChangeBatch;
    fn into_query(self) -> Query {
        Query::ViewerChanges {
            changes: self.changes,
        }
    }
    fn from_result<S>(result: QueryResult<S>) -> Self::Response<S> {
        let QueryResult::ViewerChanges(value) = result else {
            unreachable!("sealed read/result association")
        };
        value
    }
}
