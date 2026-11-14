//! Bounded summary derived from expired original recipient delivery rows.
use uuid::Uuid;

/// No separate recipient state: all facts remain on the original delivery row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpiredRecipients {
    /// Complete number whose processing deadline expired.
    pub count: u64,
    /// At most twenty exact original delivery UUIDs, ordered deterministically.
    pub delivery_ids: Vec<Uuid>,
}
