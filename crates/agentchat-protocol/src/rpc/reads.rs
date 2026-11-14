//! Request-selected decoding; never infer a read type from overlapping JSON.

use super::{ActorReference, Call, Client};
use crate::{
    envelope::Envelope,
    health::{InternalHealth, RuntimeWhoami},
};
use agentchat_core::{
    Result,
    read::{BindingView, Query, QueryResult, ReadRequest, requests},
};
use uuid::Uuid;

/// Runtime-composed read result; internal variants never appear on the wire.
pub type ReadResult = QueryResult<RuntimeWhoami>;

impl Client {
    /// Execute a concrete request and decode directly into its associated DTO.
    pub async fn read<R: ReadRequest>(
        &mut self,
        id: Uuid,
        context_id: Option<Uuid>,
        request: R,
    ) -> Result<Envelope<R::Response<RuntimeWhoami>>>
    where
        R::Response<RuntimeWhoami>: serde::de::DeserializeOwned,
    {
        self.read_actor(
            id,
            context_id.map(|context_id| ActorReference::Native { context_id }),
            request,
        )
        .await
    }

    /// Read for an explicit native/user subject; `None` is observer selection.
    pub async fn read_actor<R: ReadRequest>(
        &mut self,
        id: Uuid,
        actor: Option<ActorReference>,
        request: R,
    ) -> Result<Envelope<R::Response<RuntimeWhoami>>>
    where
        R::Response<RuntimeWhoami>: serde::de::DeserializeOwned,
    {
        self.call_as(
            id,
            Call::Query {
                actor,
                query: request.into_query(),
            },
        )
        .await
    }

    /// Invoke a shared read, choosing its concrete response before wire decoding.
    pub async fn query(
        &mut self,
        id: Uuid,
        context_id: Option<Uuid>,
        query: Query,
    ) -> Result<Envelope<ReadResult>> {
        self.query_actor(
            id,
            context_id.map(|context_id| ActorReference::Native { context_id }),
            query,
        )
        .await
    }

    /// Invoke a canonical shared read for the selected actor without identity guesses.
    pub async fn query_actor(
        &mut self,
        id: Uuid,
        actor: Option<ActorReference>,
        query: Query,
    ) -> Result<Envelope<ReadResult>> {
        // Only dynamic callers receive the enum. Concrete requests retain their
        // associated DTO through read(), without this dispatch or re-decoding.
        match query {
            Query::ViewerHead => self
                .read_actor(id, actor, requests::ViewerHead)
                .await
                .map(|e| e.map(QueryResult::ViewerHead)),
            Query::RootWindow { channel, window } => self
                .read_actor(id, actor, requests::RootWindow { channel, window })
                .await
                .map(|e| e.map(QueryResult::RootWindow)),
            Query::PinnedRoot {
                channel,
                root_message_id,
                snapshot,
            } => self
                .read_actor(
                    id,
                    actor,
                    requests::PinnedRoot {
                        channel,
                        root_message_id,
                        snapshot,
                    },
                )
                .await
                .map(|e| e.map(QueryResult::PinnedRoot)),
            Query::ReplyWindow {
                channel,
                root_message_id,
                window,
            } => self
                .read_actor(
                    id,
                    actor,
                    requests::ReplyWindow {
                        channel,
                        root_message_id,
                        window,
                    },
                )
                .await
                .map(|e| e.map(QueryResult::ReplyWindow)),
            Query::ViewerChanges { changes } => self
                .read_actor(id, actor, requests::ViewerChanges { changes })
                .await
                .map(|e| e.map(QueryResult::ViewerChanges)),
            Query::Whoami => self
                .read_actor(id, actor, requests::Whoami)
                .await
                .map(|e| e.map(QueryResult::Whoami)),
            Query::Agents { page } => self
                .read_actor(id, actor, requests::Agents { page })
                .await
                .map(|e| e.map(QueryResult::Agents)),
            Query::Channels { condition, page } => self
                .read_actor(id, actor, requests::Channels { condition, page })
                .await
                .map(|e| e.map(QueryResult::Channels)),
            Query::ChannelMembers { channel, page } => self
                .read_actor(id, actor, requests::ChannelMembers { channel, page })
                .await
                .map(|e| e.map(QueryResult::ChannelMembers)),
            Query::Message {
                message_id,
                receipts,
            } => self
                .read_actor(
                    id,
                    actor,
                    requests::Message {
                        message_id,
                        receipts,
                    },
                )
                .await
                .map(|e| e.map(QueryResult::Message)),
            Query::History {
                channel,
                thread,
                page,
            } => self
                .read_actor(
                    id,
                    actor,
                    requests::History {
                        channel,
                        thread,
                        page,
                    },
                )
                .await
                .map(|e| e.map(QueryResult::History)),
            Query::Search {
                query,
                channel,
                thread,
                page,
            } => self
                .read_actor(
                    id,
                    actor,
                    requests::Search {
                        query,
                        channel,
                        thread,
                        page,
                    },
                )
                .await
                .map(|e| e.map(QueryResult::Search)),
            Query::Inbox { page } => self
                .read_actor(id, actor, requests::Inbox { page })
                .await
                .map(|e| e.map(QueryResult::Inbox)),
            Query::Events { channel, page } => self
                .read_actor(id, actor, requests::Events { channel, page })
                .await
                .map(|e| e.map(QueryResult::Events)),
            Query::Reservations { owner, all, page } => self
                .read_actor(id, actor, requests::Reservations { owner, all, page })
                .await
                .map(|e| e.map(QueryResult::Reservations)),
        }
    }

    /// Exact contextual binding decoded directly into its expected projection.
    pub async fn native_binding(&mut self, context_id: Uuid) -> Result<BindingView> {
        super::success(
            self.call_as(Uuid::new_v4(), Call::NativeBinding { context_id })
                .await?,
        )
    }

    /// Private actor-free runtime/application health decoded into its typed DTO.
    pub async fn health(&mut self) -> Result<InternalHealth> {
        super::success(self.call_as(Uuid::new_v4(), Call::InternalHealth).await?)
    }
}
