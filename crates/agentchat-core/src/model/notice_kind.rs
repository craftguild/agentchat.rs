//! Explicit notification content kinds; an audit record is not a notification.
use super::EventKind;
use crate::{Error, ErrorCode, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// Content selected by policy for a Notice Dispatch, never conversation text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NoticeKind {
    /// Notification: a native participant joined the project.
    #[serde(rename = "agent.joined")]
    AgentJoined,
    /// Notification: a native participant explicitly left the project.
    #[serde(rename = "agent.left")]
    AgentLeft,
    /// Notification: the original managed native launch retired.
    #[serde(rename = "agent.launch_retired")]
    AgentLaunchRetired,
    /// Notification: the project-common anonymous user joined.
    #[serde(rename = "user.joined")]
    UserJoined,
    /// Notification: the project-common anonymous user explicitly left.
    #[serde(rename = "user.left")]
    UserLeft,
    /// Notification: a named channel was created.
    #[serde(rename = "channel.created")]
    ChannelCreated,
    /// Notification: a participant joined one channel.
    #[serde(rename = "channel.joined")]
    ChannelJoined,
    /// Notification: a participant left one channel.
    #[serde(rename = "channel.left")]
    ChannelLeft,
    /// Notification: the original Message has expired recipient deliveries.
    #[serde(rename = "message.delivery_confirmation_timeout")]
    MessageDeliveryConfirmationTimeout,
    /// Notification: an owner acquired one frozen reservation group.
    #[serde(rename = "reservation.acquired")]
    ReservationAcquired,
    /// Notification: the owner explicitly released its reservation group.
    #[serde(rename = "reservation.released")]
    ReservationReleased,
    /// Notification: a retained reservation group reached its expiry.
    #[serde(rename = "reservation.expired")]
    ReservationExpired,
    /// Notification: the initial project channel policy was materialized.
    #[serde(rename = "project.channel_policy_materialized")]
    ProjectChannelPolicyMaterialized,
    /// Notification: the project channel policy was explicitly changed.
    #[serde(rename = "project.channel_policy_changed")]
    ProjectChannelPolicyChanged,
}

impl NoticeKind {
    /// Preserve the established notification spelling without aliasing its type.
    pub const fn as_str(self) -> &'static str {
        self.event_kind().as_str()
    }

    /// Validate complete stored content; bounded native omission is separate.
    pub fn validate_data(self, data: &Value) -> Result<()> {
        self.event_kind().validate_data(data)
    }

    /// The payload's channel facts must match its immutable Dispatch scope.
    pub fn validate_dispatch(self, channel: Option<Uuid>, data: &Value) -> Result<()> {
        self.validate_data(data)?;
        super::event_payload::validate_scope(self.event_kind(), channel, data)
    }

    /// Omission retains typed scope shape even though full data stays in storage.
    pub(crate) fn validate_channel_presence(self, channel: Option<Uuid>) -> Result<()> {
        let scoped = matches!(
            self,
            Self::ChannelCreated
                | Self::ChannelJoined
                | Self::ChannelLeft
                | Self::MessageDeliveryConfirmationTimeout
        );
        super::event_payload::require(
            channel.is_some() == scoped && channel.is_none_or(|id| !id.is_nil()),
        )
    }

    const fn event_kind(self) -> EventKind {
        match self {
            Self::AgentJoined => EventKind::AgentJoined,
            Self::AgentLeft => EventKind::AgentLeft,
            Self::AgentLaunchRetired => EventKind::AgentLaunchRetired,
            Self::UserJoined => EventKind::UserJoined,
            Self::UserLeft => EventKind::UserLeft,
            Self::ChannelCreated => EventKind::ChannelCreated,
            Self::ChannelJoined => EventKind::ChannelJoined,
            Self::ChannelLeft => EventKind::ChannelLeft,
            Self::MessageDeliveryConfirmationTimeout => {
                EventKind::MessageDeliveryConfirmationTimeout
            }
            Self::ReservationAcquired => EventKind::ReservationAcquired,
            Self::ReservationReleased => EventKind::ReservationReleased,
            Self::ReservationExpired => EventKind::ReservationExpired,
            Self::ProjectChannelPolicyMaterialized => EventKind::ProjectChannelPolicyMaterialized,
            Self::ProjectChannelPolicyChanged => EventKind::ProjectChannelPolicyChanged,
        }
    }
}

impl TryFrom<EventKind> for NoticeKind {
    type Error = Error;

    fn try_from(value: EventKind) -> Result<Self> {
        match value {
            EventKind::AgentJoined => Ok(Self::AgentJoined),
            EventKind::AgentLeft => Ok(Self::AgentLeft),
            EventKind::AgentLaunchRetired => Ok(Self::AgentLaunchRetired),
            EventKind::UserJoined => Ok(Self::UserJoined),
            EventKind::UserLeft => Ok(Self::UserLeft),
            EventKind::ChannelCreated => Ok(Self::ChannelCreated),
            EventKind::ChannelJoined => Ok(Self::ChannelJoined),
            EventKind::ChannelLeft => Ok(Self::ChannelLeft),
            EventKind::MessageDeliveryConfirmationTimeout => {
                Ok(Self::MessageDeliveryConfirmationTimeout)
            }
            EventKind::ReservationAcquired => Ok(Self::ReservationAcquired),
            EventKind::ReservationReleased => Ok(Self::ReservationReleased),
            EventKind::ReservationExpired => Ok(Self::ReservationExpired),
            EventKind::ProjectChannelPolicyMaterialized => {
                Ok(Self::ProjectChannelPolicyMaterialized)
            }
            EventKind::ProjectChannelPolicyChanged => Ok(Self::ProjectChannelPolicyChanged),
            EventKind::MessageCreated => Err(Error::new(
                ErrorCode::IntegrityError,
                "Message content cannot be a Notice.",
            )),
        }
    }
}

impl std::str::FromStr for NoticeKind {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self> {
        value.parse::<EventKind>()?.try_into()
    }
}

impl std::fmt::Display for NoticeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
