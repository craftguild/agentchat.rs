//! Exhaustive immutable occurrence kinds, independent of notification policy.
use crate::{Error, ErrorCode, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A persisted audit kind; inserting one never implies native delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventKind {
    /// A native participant joined the project.
    #[serde(rename = "agent.joined")]
    AgentJoined,
    /// A native participant explicitly left the project.
    #[serde(rename = "agent.left")]
    AgentLeft,
    /// The original managed native launch retired.
    #[serde(rename = "agent.launch_retired")]
    AgentLaunchRetired,
    /// The project-common anonymous user joined.
    #[serde(rename = "user.joined")]
    UserJoined,
    /// The project-common anonymous user explicitly left.
    #[serde(rename = "user.left")]
    UserLeft,
    /// A named channel was created.
    #[serde(rename = "channel.created")]
    ChannelCreated,
    /// A participant joined one channel.
    #[serde(rename = "channel.joined")]
    ChannelJoined,
    /// A participant left one channel.
    #[serde(rename = "channel.left")]
    ChannelLeft,
    /// One immutable public Message was committed.
    #[serde(rename = "message.created")]
    MessageCreated,
    /// The original Message has expired recipient deliveries.
    #[serde(rename = "message.delivery_confirmation_timeout")]
    MessageDeliveryConfirmationTimeout,
    /// An owner acquired one frozen reservation group.
    #[serde(rename = "reservation.acquired")]
    ReservationAcquired,
    /// The owner explicitly released its reservation group.
    #[serde(rename = "reservation.released")]
    ReservationReleased,
    /// A retained reservation group reached its expiry.
    #[serde(rename = "reservation.expired")]
    ReservationExpired,
    /// The initial project channel policy was materialized.
    #[serde(rename = "project.channel_policy_materialized")]
    ProjectChannelPolicyMaterialized,
    /// The project channel policy was explicitly changed.
    #[serde(rename = "project.channel_policy_changed")]
    ProjectChannelPolicyChanged,
}

impl EventKind {
    /// Canonical wire/relational token, never a host version or free-form label.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AgentJoined => "agent.joined",
            Self::AgentLeft => "agent.left",
            Self::AgentLaunchRetired => "agent.launch_retired",
            Self::UserJoined => "user.joined",
            Self::UserLeft => "user.left",
            Self::ChannelCreated => "channel.created",
            Self::ChannelJoined => "channel.joined",
            Self::ChannelLeft => "channel.left",
            Self::MessageCreated => "message.created",
            Self::MessageDeliveryConfirmationTimeout => "message.delivery_confirmation_timeout",
            Self::ReservationAcquired => "reservation.acquired",
            Self::ReservationReleased => "reservation.released",
            Self::ReservationExpired => "reservation.expired",
            Self::ProjectChannelPolicyMaterialized => "project.channel_policy_materialized",
            Self::ProjectChannelPolicyChanged => "project.channel_policy_changed",
        }
    }

    /// Check the exact kind-specific data contract before any publication.
    pub fn validate_data(self, data: &Value) -> Result<()> {
        super::event_payload::validate(self, data)
    }
}

impl std::str::FromStr for EventKind {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "agent.joined" => Ok(Self::AgentJoined),
            "agent.left" => Ok(Self::AgentLeft),
            "agent.launch_retired" => Ok(Self::AgentLaunchRetired),
            "user.joined" => Ok(Self::UserJoined),
            "user.left" => Ok(Self::UserLeft),
            "channel.created" => Ok(Self::ChannelCreated),
            "channel.joined" => Ok(Self::ChannelJoined),
            "channel.left" => Ok(Self::ChannelLeft),
            "message.created" => Ok(Self::MessageCreated),
            "message.delivery_confirmation_timeout" => Ok(Self::MessageDeliveryConfirmationTimeout),
            "reservation.acquired" => Ok(Self::ReservationAcquired),
            "reservation.released" => Ok(Self::ReservationReleased),
            "reservation.expired" => Ok(Self::ReservationExpired),
            "project.channel_policy_materialized" => Ok(Self::ProjectChannelPolicyMaterialized),
            "project.channel_policy_changed" => Ok(Self::ProjectChannelPolicyChanged),
            _ => Err(Error::new(
                ErrorCode::IntegrityError,
                "Unknown audit event kind.",
            )),
        }
    }
}

impl std::fmt::Display for EventKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
