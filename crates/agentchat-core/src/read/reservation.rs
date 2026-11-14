//! Public reservation projection keeps the established identifier spelling.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    model::{LeaseState, Reservation},
    time::Timestamp,
};

/// Complete finite lease-group projection, distinct from its storage model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReservationView {
    /// Stable public group identity.
    pub reservation_id: Uuid,
    /// Retained owner.
    pub owner: super::IdentityLabel,
    /// Immutable acquisition generation.
    pub generation: i64,
    /// Effective lifecycle state at snapshot time.
    pub state: LeaseState,
    /// Original bounded TTL.
    pub ttl_seconds: u32,
    /// Original acquisition time.
    pub acquired_at: Timestamp,
    /// Persisted deadline.
    pub expires_at: Timestamp,
    /// Explicit release time.
    pub released_at: Option<Timestamp>,
    /// Optional bounded explanation.
    pub reason: Option<String>,
    /// Complete acquisition-ordered patterns.
    pub patterns: Vec<String>,
}

impl ReservationView {
    /// Compose a lease with its retained typed owner from the same snapshot.
    pub fn with_owner(group: Reservation, owner: super::IdentityLabel) -> Self {
        Self {
            reservation_id: group.id,
            owner,
            generation: group.generation,
            state: group.state,
            ttl_seconds: group.ttl_seconds,
            acquired_at: group.acquired_at,
            expires_at: group.expires_at,
            released_at: group.released_at,
            reason: group.reason,
            patterns: group.patterns,
        }
    }
}
