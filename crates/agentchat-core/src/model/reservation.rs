//! Retained cooperative lease groups and structured overlap conflicts.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::time::Timestamp;

/// Terminal states can never return to active under the same identity/generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseState {
    /// Exclusive logical-path ownership until the persisted deadline.
    Active,
    /// Owner explicitly released or left.
    Released,
    /// Deadline was materialized as an expiry event.
    Expired,
}

impl LeaseState {
    /// Stable relational spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Released => "released",
            Self::Expired => "expired",
        }
    }
}

/// Atomic ordered pattern set with one owner, generation and deadline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reservation {
    /// Stable group identity; a replacement gets another UUID.
    pub id: Uuid,
    /// Exclusive cooperating owner.
    pub owner_id: Uuid,
    /// Positive acquisition event sequence; immutable across lease renewal.
    /// A replacement has a new group UUID and another acquisition generation.
    pub generation: i64,
    /// Current retained lifecycle state.
    pub state: LeaseState,
    /// Original TTL, used by manual/connected renewal.
    pub ttl_seconds: u32,
    /// Acquisition time.
    pub acquired_at: Timestamp,
    /// Persisted deadline, preserved across runtime crashes.
    pub expires_at: Timestamp,
    /// Explicit release time; expiry is retained by the deadline.
    pub released_at: Option<Timestamp>,
    /// Optional bounded explanation, never an automatic negotiation command.
    pub reason: Option<String>,
    /// Validated grammar-v1 patterns in their acquisition order.
    pub patterns: Vec<String>,
}

/// Actionable conflict details, with a witness accepted by both compiled patterns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReservationConflict {
    /// Existing unexpired lease.
    pub reservation_id: Uuid,
    /// Typed owner, distinguishing same-text native and anonymous labels.
    pub owner: super::IdentityLabel,
    /// Requested pattern whose language overlaps.
    pub requested_pattern: String,
    /// Existing conflicting pattern.
    pub existing_pattern: String,
    /// Persisted lease deadline.
    pub expires_at: Timestamp,
    /// Normalized relative path in both languages, including future files.
    pub witness_path: String,
}
