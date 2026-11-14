//! Durable launch authority is distinct from reconnectable transport presence.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{sequence::Sequence, time::Timestamp};

/// Immutable authority supplied only by the launch-aware host adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchIdentity {
    /// Fresh UUID for one AGENTCHAT-managed native CLI lifetime, never a reconnect ID.
    pub generation: Uuid,
    /// Bounded host-owned encoding of original kernel owner/supervisor tokens.
    /// The adapter validates boot/start/image/user provenance; core never derives
    /// a conversation from this opaque record or treats a heartbeat as renewal.
    pub owner: String,
}

/// Positive kernel evidence, without inferring death from an observation error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchOwnerState {
    /// All original kernel provenance still matches. Root/control checks remain.
    Current,
    /// Original birth is positively absent/replaced or exited.
    Gone,
    /// Unreadable, denied, changed image/user or otherwise inconclusive.
    Unknown,
}

/// Read-only eligibility for cancelling local native futures, never proof of death.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchState {
    /// Exact current attachment, active generation and original process proof.
    Current,
    /// Active authority is temporarily unreadable/unconfirmed. Preserve its work.
    Unconfirmed,
    /// Unjoined, retired or obsolete attachment. No local output is permitted.
    Inactive,
}

/// Retained authority and its exact initial application delivery boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Launch {
    /// Immutable generation and physical owner proof.
    pub identity: LaunchIdentity,
    /// Retained project participant, never reassigned to another root.
    pub agent_id: Uuid,
    /// Exact descriptor that completed startup membership.
    pub context_id: Uuid,
    /// Startup connection, distinct from later same-generation reconnects.
    pub incarnation: Uuid,
    /// Committed join event; earlier deliveries cannot enter this generation.
    pub start_seq: Sequence,
    /// Original successful startup time.
    pub started_at: Timestamp,
    /// Terminal time. A retired generation can never become active again.
    pub retired_at: Option<Timestamp>,
    /// Stable terminal cause, retained separately from read-deadline records.
    pub retirement_reason: Option<String>,
}
