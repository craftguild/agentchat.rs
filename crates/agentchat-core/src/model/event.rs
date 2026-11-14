//! Immutable occurrence records; dispatch creation is a separate policy.

use super::{
    EventKind,
    event_payload::{id, require, validate_scope},
};
use crate::{Result, sequence::Sequence, time::Timestamp};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// Immutable typed occurrence; publication alone creates no sending obligation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// Envelope major version.
    pub version: u32,
    /// Stable event UUID for deduplication.
    pub event_id: Uuid,
    /// Positive project ordering, serialized as a decimal string.
    pub seq: Sequence,
    /// Canonical exhaustive occurrence kind, separate from notification kinds.
    pub kind: EventKind,
    /// Canonical UTC event time.
    pub occurred_at: Timestamp,
    /// Originating identity, if applicable.
    pub actor_id: Option<Uuid>,
    /// Applicable channel, if any.
    pub channel_id: Option<Uuid>,
    /// Applicable immutable message, if any.
    pub message_id: Option<Uuid>,
    /// Kind-specific payload; full message bodies are not duplicated here.
    pub data: Value,
}

impl Event {
    /// Validate the kind-specific facts and their immutable envelope attribution.
    pub fn validate(&self) -> Result<()> {
        require(self.version == 1 && !self.event_id.is_nil())?;
        require(
            [self.actor_id, self.channel_id, self.message_id]
                .into_iter()
                .flatten()
                .all(|id| !id.is_nil()),
        )?;
        self.kind.validate_data(&self.data)?;
        validate_scope(self.kind, self.channel_id, &self.data)?;
        use EventKind::*;
        match self.kind {
            AgentJoined | AgentLeft | AgentLaunchRetired | ChannelJoined | ChannelLeft => require(
                self.actor_id == Some(id(&self.data, "agent_id")?) && self.message_id.is_none(),
            ),
            UserJoined | UserLeft => require(
                self.actor_id == Some(id(&self.data, "actor_id")?) && self.message_id.is_none(),
            ),
            ChannelCreated => require(self.message_id.is_none()),
            MessageCreated => require(self.actor_id.is_some() && self.message_id.is_some()),
            MessageDeliveryConfirmationTimeout => require(
                self.actor_id.is_none() && self.message_id == Some(id(&self.data, "message_id")?),
            ),
            ReservationAcquired | ReservationReleased => require(
                self.actor_id == Some(id(&self.data["owner"], "actor_id")?)
                    && self.message_id.is_none(),
            ),
            ReservationExpired | ProjectChannelPolicyChanged | ProjectChannelPolicyMaterialized => {
                require(self.actor_id.is_none() && self.message_id.is_none())
            }
        }
    }
}
