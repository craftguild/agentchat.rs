//! Shared read dispatch; every request uses a single read-only WAL snapshot.

mod binding;
mod changes;
mod checkpoint;
mod health;
mod lists;
mod windows;
use binding::binding;
mod messages;

use uuid::Uuid;

use super::Application;
use crate::{
    Error, ErrorCode, Result,
    model::{Actor, ActorKind, Agent, Context, OperationActor, Phase},
    read::{
        ActorView, AgentView, BindingView, MAX_PAGE_BYTES, NativeDeliveryState, NativeDeliveryView,
        NativeHeadView, Participation, Query, QueryResult, Snapshot, WhoamiView,
    },
    sequence::Cursor,
    time::Timestamp,
};

impl Application {
    /// Capture a committed observer start boundary without reading past events.
    pub async fn event_head(&self) -> Result<Cursor> {
        Ok(self.storage.snapshot().await?.head())
    }

    /// Execute a bounded query without joining, acknowledging or marking read.
    /// Context-free history/events support human observation and diagnostics.
    /// A supplied context still must match this project and its exact binding.
    pub async fn query(&self, context: Option<&Context>, query: Query) -> Result<QueryResult> {
        let actor = context.cloned().map(OperationActor::Native);
        self.query_actor(actor.as_ref(), query).await
    }

    /// Read for a selected subject or an explicit context-free observer.
    /// A selected user remains contextual even without any native descriptor.
    pub async fn query_actor(
        &self,
        actor: Option<&OperationActor>,
        mut query: Query,
    ) -> Result<QueryResult> {
        let context = match actor {
            Some(OperationActor::Native(context)) => Some(context),
            _ => None,
        };

        if let Some(context) = context {
            self.validate_context(context)?;
        }
        query.normalize()?;
        if actor.is_some()
            && matches!(
                &query,
                Query::ViewerHead
                    | Query::RootWindow { .. }
                    | Query::ReplyWindow { .. }
                    | Query::PinnedRoot { .. }
                    | Query::ViewerChanges { .. }
            )
        {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "Viewer-only queries require the context-free observer path.",
            ));
        }
        if actor.is_none() && matches!(query, Query::Whoami | Query::Inbox { .. }) {
            return Err(Error::new(
                ErrorCode::SessionRequired,
                "This query requires an exact host descriptor.",
            ));
        }
        let mut snapshot = self.storage.snapshot().await?;
        let now = self.clock.now();
        if matches!(actor, Some(OperationActor::User)) && matches!(query, Query::Inbox { .. }) {
            return Err(super::actors::nonreceiving());
        }
        let viewer: Option<Actor> = match actor {
            Some(OperationActor::User) => snapshot.user().await?,
            Some(OperationActor::Native(_)) => {
                viewer(snapshot.as_mut(), context).await?.map(Into::into)
            }
            None => None,
        };
        let viewer_id = viewer.as_ref().map(|a| a.actor_id);
        let result = match query {
            Query::ViewerHead => QueryResult::ViewerHead(
                checkpoint::capture(snapshot.as_mut(), self.project_id()).await?,
            ),
            Query::RootWindow { channel, window } => QueryResult::RootWindow(
                windows::window(snapshot.as_mut(), self.project_id(), channel, None, window)
                    .await?,
            ),
            Query::ReplyWindow {
                channel,
                root_message_id,
                window,
            } => QueryResult::ReplyWindow(
                windows::window(
                    snapshot.as_mut(),
                    self.project_id(),
                    channel,
                    Some(root_message_id),
                    window,
                )
                .await?,
            ),
            Query::PinnedRoot {
                channel,
                root_message_id,
                snapshot: at,
            } => QueryResult::PinnedRoot(
                windows::pinned(
                    snapshot.as_mut(),
                    self.project_id(),
                    channel,
                    root_message_id,
                    at,
                )
                .await?,
            ),
            Query::ViewerChanges { changes: input } => QueryResult::ViewerChanges(
                changes::changes(snapshot.as_mut(), self.project_id(), input).await?,
            ),
            Query::Whoami => {
                let native = if context.is_some() {
                    Some(
                        binding(
                            snapshot.as_mut(),
                            context,
                            viewer.clone().and_then(Actor::into_agent),
                            now,
                        )
                        .await?,
                    )
                } else {
                    None
                };
                let participation = viewer.as_ref().map_or(Participation::Unjoined, |a| {
                    if a.joined {
                        Participation::Joined
                    } else {
                        Participation::Left
                    }
                });
                let selected = if let Some(actor) = viewer.clone() {
                    Some(actor_view(snapshot.as_mut(), actor, now).await?)
                } else {
                    None
                };
                QueryResult::Whoami(WhoamiView {
                    health: health::health_view(snapshot.as_mut(), self.project_id()).await?,
                    actor: selected,
                    participation,
                    context_id: context.map(|c| c.context_id),
                    native_delivery: native.and_then(|b| b.native_delivery),
                })
            }
            Query::Agents { page } => {
                QueryResult::Agents(lists::agents(snapshot.as_mut(), now, page, None).await?)
            }
            Query::Channels { condition, page } => QueryResult::Channels(
                lists::channels(snapshot.as_mut(), now, condition, page).await?,
            ),
            Query::ChannelMembers { channel, page } => {
                let channel = channel_id(snapshot.as_mut(), &channel).await?;
                QueryResult::ChannelMembers(
                    lists::actors(snapshot.as_mut(), now, page, channel).await?,
                )
            }
            Query::Message {
                message_id,
                receipts,
            } => {
                let channels =
                    content_channels(snapshot.as_mut(), actor.is_some(), &viewer).await?;
                QueryResult::Message(
                    messages::detail(
                        snapshot.as_mut(),
                        viewer_id,
                        channels.as_deref(),
                        message_id,
                        receipts,
                    )
                    .await?,
                )
            }
            Query::History {
                channel,
                thread,
                page,
            } => {
                let channels =
                    content_channels(snapshot.as_mut(), actor.is_some(), &viewer).await?;
                QueryResult::History(
                    messages::history(
                        snapshot.as_mut(),
                        viewer_id,
                        channels.as_deref(),
                        channel,
                        thread,
                        None,
                        page,
                    )
                    .await?,
                )
            }
            Query::Search {
                query,
                channel,
                thread,
                page,
            } => {
                let channels =
                    content_channels(snapshot.as_mut(), actor.is_some(), &viewer).await?;
                QueryResult::Search(
                    messages::history(
                        snapshot.as_mut(),
                        viewer_id,
                        channels.as_deref(),
                        channel,
                        thread,
                        Some(query),
                        page,
                    )
                    .await?,
                )
            }
            Query::Events { channel, page } => {
                let channels =
                    content_channels(snapshot.as_mut(), actor.is_some(), &viewer).await?;
                QueryResult::Events(
                    lists::events(snapshot.as_mut(), channels.as_deref(), channel, page).await?,
                )
            }
            Query::Inbox { page } => {
                let viewer = viewer.ok_or_else(|| {
                    Error::new(
                        ErrorCode::NotJoined,
                        "Inbox retrieval requires a retained exact conversation.",
                    )
                })?;
                QueryResult::Inbox(messages::inbox(snapshot.as_mut(), viewer.actor_id, page).await?)
            }
            Query::Reservations { owner, all, page } => QueryResult::Reservations(
                lists::reservations(snapshot.as_mut(), now, owner, all, page).await?,
            ),
        };
        // The common result must fit before either transport adds its envelope.
        // This also catches malformed oversized records in a damaged database.
        if serde_json::to_vec(&result)
            .map_err(|_| serialization())?
            .len()
            > MAX_PAGE_BYTES
        {
            return Err(Error::new(
                ErrorCode::IntegrityError,
                "Query result exceeds its finite serialized budget.",
            ));
        }
        Ok(result)
    }
}

async fn content_channels(
    db: &mut dyn Snapshot,
    contextual: bool,
    viewer: &Option<Actor>,
) -> Result<Option<Vec<Uuid>>> {
    if !contextual {
        return Ok(None);
    }
    let viewer = viewer
        .as_ref()
        .filter(|agent| agent.joined)
        .ok_or_else(|| {
            Error::new(
                ErrorCode::NotJoined,
                "Agent content access requires current project participation.",
            )
        })?;
    Ok(Some(db.member_channels(viewer.actor_id).await?))
}

async fn viewer(db: &mut dyn Snapshot, context: Option<&Context>) -> Result<Option<Agent>> {
    let Some(context) = context else {
        return Ok(None);
    };
    let Some(agent) = db
        .conversation(context.host, &context.host_session_id)
        .await?
    else {
        return Ok(None);
    };
    let session = db.session(agent.id).await?.ok_or_else(|| {
        Error::new(
            ErrorCode::SessionRequired,
            "Retained identity has no current host binding.",
        )
    })?;
    if session.incarnation != context.incarnation || session.context_id != context.context_id {
        return Err(Error::new(
            ErrorCode::IdentityConflict,
            "Context belongs to an obsolete connection incarnation.",
        ));
    }
    Ok(Some(agent))
}

pub(super) async fn agent_view(
    db: &mut dyn Snapshot,
    agent: Agent,
    now: Timestamp,
) -> Result<AgentView> {
    let session = db.session(agent.id).await?;
    let connected = session.as_ref().is_some_and(|s| s.is_connected(now));
    let presence = if connected {
        session.as_ref().map_or(Phase::Disconnected, |s| s.phase)
    } else {
        Phase::Disconnected
    };
    Ok(AgentView {
        unread_count: db.unread_count(agent.id).await?,
        incarnation: session.as_ref().map(|s| s.incarnation),
        agent,
        presence,
        connected,
    })
}

pub(super) async fn channel_id(db: &mut dyn Snapshot, name: &str) -> Result<Uuid> {
    db.channel(name)
        .await?
        .map(|c| c.id)
        .ok_or_else(|| Error::new(ErrorCode::NotFound, "Channel does not exist."))
}
fn serialization() -> Error {
    Error::new(
        ErrorCode::IntegrityError,
        "Query result serialization failed.",
    )
}

pub(super) async fn actor_view(
    db: &mut dyn Snapshot,
    actor: Actor,
    now: Timestamp,
) -> Result<ActorView> {
    let (presence, connected, incarnation) = if actor.kind() == ActorKind::Agent {
        let session = db.session(actor.actor_id).await?;
        let connected = session.as_ref().is_some_and(|s| s.is_connected(now));
        (
            Some(if connected {
                session.as_ref().map_or(Phase::Disconnected, |s| s.phase)
            } else {
                Phase::Disconnected
            }),
            Some(connected),
            session.map(|s| s.incarnation),
        )
    } else {
        (None, None, None)
    };
    Ok(ActorView {
        actor,
        presence,
        connected,
        incarnation,
    })
}
