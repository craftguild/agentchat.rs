//! Participation is independent of host presence and connection incarnation.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{sequence::Sequence, time::Timestamp};

/// Supported independent-conversation host kinds, without any version admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostKind {
    /// Codex CLI shared-server conversation.
    Codex,
    /// Claude Code CLI root conversation.
    Claude,
}

impl HostKind {
    /// Stable relational spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }
}

/// Exact routing descriptor produced by a validated host lifecycle adapter.
///
/// Directory and process metadata never substitute for the host/session pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Context {
    /// Opaque local routing descriptor UUID.
    pub context_id: Uuid,
    /// Independent persistent project identity.
    pub project_id: Uuid,
    /// Host integration kind.
    pub host: HostKind,
    /// Exact root host-conversation ID; forks have another value.
    pub host_session_id: String,
    /// Observed host version used for instance identity and diagnostics.
    pub host_version: String,
    /// Exact OS profile, not a host-kind-only capability guess.
    pub os_profile: String,
    /// Canonical working directory for routing and project binding validation.
    pub working_directory: PathBuf,
    /// New UUID for each independently verified connection lifetime.
    pub incarnation: Uuid,
}

/// Retained project-specific identity, including identities that explicitly left.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agent {
    /// Persistent agent UUID.
    pub id: Uuid,
    /// Exact host kind.
    pub host: HostKind,
    /// Exact root host-session identity.
    pub host_session_id: String,
    /// Immutable normalized handle.
    pub handle: String,
    /// Explicit participation; disconnection does not change this value.
    pub joined: bool,
    /// Original join time, retained across leave/rejoin.
    pub first_join_at: Timestamp,
    /// Latest explicit join boundary.
    pub join_seq: Sequence,
    /// Latest explicit leave boundary, if any.
    pub leave_seq: Option<Sequence>,
}

/// Current host phase; connected presence alone is not wake eligibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// No valid host-conversation attachment.
    Disconnected,
    /// Connected and waiting for input.
    Idle,
    /// An existing user/model turn is active.
    Active,
    /// Host is waiting for an approval decision.
    ApprovalBlocked,
    /// User interruption/pause forbids automatic continuation.
    Paused,
}

impl Phase {
    /// Stable relational spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Disconnected => "disconnected",
            Self::Idle => "idle",
            Self::Active => "active",
            Self::ApprovalBlocked => "approval_blocked",
            Self::Paused => "paused",
        }
    }
}

/// One incarnation of an exact host-conversation attachment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    /// Connection lifetime UUID.
    pub incarnation: Uuid,
    /// Exact retained agent identity.
    pub agent_id: Uuid,
    /// Host descriptor UUID for this incarnation.
    pub context_id: Uuid,
    /// Most recently verified host phase.
    pub phase: Phase,
    /// Last verified host liveness, never a helper-process heartbeat alone.
    pub heartbeat_at: Timestamp,
    /// Host turn ID required for safe active-turn steering.
    pub active_turn_id: Option<String>,
    /// User pause/interruption latch; only explicit host continuation clears it.
    pub wake_paused: bool,
}

impl Session {
    /// Connected snapshot used by `@here`, including paused/approval-blocked hosts.
    pub fn is_connected(&self, now: Timestamp) -> bool {
        let elapsed = now
            .datetime()
            .signed_duration_since(self.heartbeat_at.datetime());
        self.phase != Phase::Disconnected
            && elapsed >= chrono::Duration::zero()
            && elapsed < chrono::Duration::seconds(45)
    }
}

/// A work area; project defaults and explicit memberships select its agents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Channel {
    /// Stable channel UUID.
    pub id: Uuid,
    /// Normalized immutable slug.
    pub name: String,
    /// Creator, or none for a policy-created default channel.
    pub creator_id: Option<Uuid>,
    /// Committed creation ordering.
    pub creation_seq: Sequence,
}
