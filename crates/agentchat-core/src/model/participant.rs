//! Durable common participation without fabricating native user metadata.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{ActorKind, Agent, HostKind};
use crate::{sequence::Sequence, time::Timestamp};

/// An actor's immutable identity; only agents have native host/chat metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ActorIdentity {
    /// Existing project/host/chat association with separately verified authority.
    Agent {
        /// Native host kind, independent of version admission.
        host: HostKind,
        /// Exact case-sensitive host chat identity.
        chat_id: String,
    },
    /// One anonymous project principal, with no host/session identity.
    User,
}

impl ActorIdentity {
    /// Public actor category, without guessing from its label or UUID.
    pub const fn kind(&self) -> ActorKind {
        match self {
            Self::Agent { .. } => ActorKind::Agent,
            Self::User => ActorKind::User,
        }
    }
}

/// Retained common author, owner, member and reader participation record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Actor {
    /// Durable identity; all pre-migration native UUIDs are retained exactly.
    pub actor_id: Uuid,
    /// Agent native identity or the project-common anonymous principal.
    #[serde(flatten)]
    pub identity: ActorIdentity,
    /// Immutable native handle or the common anonymous label `user`.
    pub handle: String,
    /// Explicit participation; a user's state has no host presence meaning.
    pub joined: bool,
    /// Original first Join time, retained across participation generations.
    pub first_join_at: Timestamp,
    /// Most recent forward participation boundary.
    pub join_seq: Sequence,
    /// Most recent explicit departure boundary, if any.
    pub leave_seq: Option<Sequence>,
}

impl Actor {
    /// Shared actor category; only agents may become delivery recipients.
    pub const fn kind(&self) -> ActorKind {
        self.identity.kind()
    }

    /// Extract the native-only projection without manufacturing user host facts.
    pub fn into_agent(self) -> Option<Agent> {
        let ActorIdentity::Agent { host, chat_id } = self.identity else {
            return None;
        };
        Some(Agent {
            id: self.actor_id,
            host,
            host_session_id: chat_id,
            handle: self.handle,
            joined: self.joined,
            first_join_at: self.first_join_at,
            join_seq: self.join_seq,
            leave_seq: self.leave_seq,
        })
    }
}

impl From<Agent> for Actor {
    fn from(agent: Agent) -> Self {
        Self {
            actor_id: agent.id,
            identity: ActorIdentity::Agent {
                host: agent.host,
                chat_id: agent.host_session_id,
            },
            handle: agent.handle,
            joined: agent.joined,
            first_join_at: agent.first_join_at,
            join_seq: agent.join_seq,
            leave_seq: agent.leave_seq,
        }
    }
}

/// Retained author/reader identity without unbounded or host-specific metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentityLabel {
    /// Exact retained author, owner or reader actor.
    pub actor_id: Uuid,
    /// Native agent or project-common anonymous user.
    pub kind: ActorKind,
    /// Immutable bounded handle.
    pub handle: String,
}

impl From<&Actor> for IdentityLabel {
    fn from(actor: &Actor) -> Self {
        Self {
            actor_id: actor.actor_id,
            kind: actor.kind(),
            handle: actor.handle.clone(),
        }
    }
}
