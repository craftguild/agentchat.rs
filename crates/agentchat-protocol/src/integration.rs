//! Typed host capability and ownership-health wire projections.

use agentchat_core::{Error, model::HostKind};
use serde::{Deserialize, Serialize};

/// Health of managed configuration, independent of native host admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InspectionState {
    /// No owned components have been recorded by this installer.
    NotInstalled,
    /// Every owned component matches both its ledger and the supplied plan.
    Healthy,
    /// Owned contents are intact, but a fresh init should update their values.
    NeedsUpdate,
    /// A component is changed, missing or unknown to the supplied plan.
    AttentionRequired,
    /// Pending intent exists; inspection does not validate or replay that intent.
    RecoveryRequired,
    /// An installer owns the exclusive lock; no partial snapshot is reported.
    Busy,
}
/// One bounded component result without configuration values or credentials.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComponentInspection {
    /// Exact agentchat ownership key, never a foreign configuration member.
    pub key: String,
    /// Current ownership/value comparison.
    pub state: ComponentState,
    /// Fixed safe installer error, when validation fails.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Error>,
}
/// Result of comparing one managed component with its recorded and desired value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentState {
    /// The recorded value and current desired value match.
    Owned,
    /// The recorded value is intact but differs from current desired setup.
    UpdateAvailable,
    /// The recorded value/destination cannot be validated.
    Conflict,
    /// The supplied plan does not describe this recorded component.
    Unverified,
}
/// Bounded current health of owned files, not historical init retry evidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Inspection {
    /// Summary over owned components, pending intent and lock availability.
    pub state: InspectionState,
    /// At most 128 safe ownership-key results; empty when busy or recovering.
    pub components: Vec<ComponentInspection>,
}
/// Required live host API surfaces; diagnostic, never an admission certificate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostSurface {
    /// Compiled host adapter.
    pub host: HostKind,
    /// Adapter label.
    pub adapter: String,
    /// Required live API names.
    pub required_api: Vec<String>,
}
/// Best-effort host capability report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Capabilities {
    /// Automatic delivery is compiled in.
    pub automatic_wake: bool,
    /// Best-effort API-surface policy.
    pub compatibility: String,
    /// Bounded compiled adapter set.
    pub host_surfaces: Vec<HostSurface>,
    /// Version is diagnostic and provenance evidence.
    pub version_policy: String,
}
/// Current native installation surface, without host execution or liveness proof.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum LifecycleHealth {
    /// At least one intact native installation.
    Available {
        /// Exact installed profiles, not G0 certificates.
        profiles: Vec<LifecycleProfile>,
    },
    /// No usable intact installation.
    Unavailable {
        /// Bounded explanation.
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}
/// One intact native installation's diagnostic provenance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LifecycleProfile {
    /// Installed host.
    pub host: HostKind,
    /// Exact diagnostic version.
    pub host_version: String,
    /// Best-effort API-surface policy.
    pub compatibility: String,
    /// Compiled OS/architecture label.
    pub os_profile: String,
}
/// Current ownership health. Optional members preserve existing wire omissions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IntegrationHealth {
    /// Observed ownership state, independent of native admission.
    pub state: String,
    /// Tool ownership snapshot when composed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Inspection>,
    /// Native installation surface, if inspection was attempted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<LifecycleHealth>,
    /// Inspection failure, preserving its open structured error details.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Error>,
    /// Missing-composition explanation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}
