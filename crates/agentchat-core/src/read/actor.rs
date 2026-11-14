//! Common actor presentation and consolidated identity/health reads.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{NativeDeliveryView, Participation, ProjectStatistics};
use crate::{
    model::{Actor, Phase},
    project::{ProjectChannelPolicy, ProjectReceiptSettings},
    sequence::Cursor,
};

/// Mixed participant projection; users have no fabricated native presence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActorView {
    /// Durable typed actor and participation, including retained departures.
    #[serde(flatten)]
    pub actor: Actor,
    /// Native phase for an agent; none for the anonymous user.
    pub presence: Option<Phase>,
    /// Exact native liveness for an agent; none for the anonymous user.
    pub connected: Option<bool>,
    /// Current native incarnation when one exists; never allocated for a user.
    pub incarnation: Option<Uuid>,
}

/// Internal actor-independent health composition, without a public status route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthView {
    /// Stable namespace.
    pub project_id: Uuid,
    /// Committed event head at this read snapshot.
    pub head: Cursor,
    /// Effective ordered project default channels.
    pub channel_policy: ProjectChannelPolicy,
    /// Effective configuration, without changing any original input snapshot.
    pub receipt_settings: ProjectReceiptSettings,
    /// Same-snapshot aggregates; agent counts exclude anonymous actors.
    pub statistics: ProjectStatistics,
}

/// One public identity/health result for the explicitly selected subject.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WhoamiView {
    /// Project policy and scalar health share the identity's read snapshot.
    #[serde(flatten)]
    pub health: HealthView,
    /// Selected actor when previously joined; lookup never registers one.
    pub actor: Option<ActorView>,
    /// Unjoined, joined or left independently from native liveness.
    pub participation: Participation,
    /// Exact native descriptor when applicable; none for a user.
    pub context_id: Option<Uuid>,
    /// Native ordered-input progress when applicable; none for a user.
    pub native_delivery: Option<NativeDeliveryView>,
}
