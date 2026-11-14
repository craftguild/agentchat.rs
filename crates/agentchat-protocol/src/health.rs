//! Typed runtime health composed above application-owned read DTOs.

use agentchat_core::{
    ErrorCode,
    read::{HealthView, WhoamiView},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Final shared status composed for CLI and MCP, before presentation encoding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PublicWhoami {
    /// Runtime and application status with their existing flattened fields.
    #[serde(flatten)]
    pub whoami: RuntimeWhoami,
    /// Compiled best-effort host capabilities.
    pub capabilities: crate::integration::Capabilities,
    /// Read-only current integration ownership health.
    pub integrations: crate::integration::IntegrationHealth,
}

/// Application status enriched by runtime-owned health.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeWhoami {
    /// Application snapshot, with unchanged field names.
    #[serde(flatten)]
    pub application: WhoamiView,
    /// Runtime health sampled during this request.
    pub runtime: RuntimeHealth,
}

/// Private actor-free diagnostic read; it is not a public status operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InternalHealth {
    /// Application snapshot without an actor or native binding.
    #[serde(flatten)]
    pub application: HealthView,
    /// Bounded coordinator/log health sampled during the call.
    pub runtime: RuntimeHealth,
}

/// Live bounded coordinator health; no host liveness mutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeHealth {
    /// Current activity counters.
    #[serde(flatten)]
    pub activity: ActivityHealth,
    /// Actual daemon incarnation.
    pub instance_id: Uuid,
    /// Last bounded control error.
    pub last_control_error: Option<ErrorCode>,
    /// Live log health and limits.
    pub diagnostics: DiagnosticHealth,
    /// Native work occupancy and cumulative observations.
    pub native_delivery: NativeHealth,
    /// Attempted group metrics, independent of physical sync counts.
    pub mutation_groups: MutationHealth,
}

/// Current activity that participates in ordinary idle shutdown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityHealth {
    /// Active requests.
    pub requests: usize,
    /// Context-free observers.
    pub observers: usize,
    /// Joined integration connections.
    pub attached_connections: usize,
}

/// Safe diagnostic queue and rotation health.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticHealth {
    /// Running, closing, closed or failed.
    pub state: String,
    /// Records rejected by the bounded queue.
    pub dropped_records: u64,
    /// Fixed queue capacity.
    pub queue_records: usize,
    /// Maximum individual encoded record.
    pub record_bytes: usize,
    /// Maximum rotated file size.
    pub file_bytes: usize,
    /// Maximum retained file count.
    pub file_count: usize,
}

/// Bounded native jobs and advisory candidate observations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeHealth {
    /// Active native jobs.
    pub jobs: usize,
    /// Fixed job capacity.
    pub max_jobs: usize,
    /// Pending completion callbacks.
    pub queued_completions: usize,
    /// Fixed completion queue capacity.
    pub completion_capacity: usize,
    /// Attempted completion groups.
    pub completion_groups: u64,
    /// Largest observed completion group.
    pub largest_completion_group: usize,
    /// Fixed completion group limit.
    pub max_completion_group: usize,
    /// Advisory preparation scans.
    pub candidate_scans: u64,
    /// Contexts visited by advisory scans.
    pub candidate_contexts: u64,
    /// Empty or deferred preparation visits.
    pub candidate_pruned: u64,
    /// Advisory failures using full preparation.
    pub candidate_fallbacks: u64,
    /// Fixed per-scan context limit.
    pub max_candidate_contexts: usize,
}

/// Saturating attempted mutation and ready-commit group metrics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutationHealth {
    /// Attempted mutation groups.
    pub groups: u64,
    /// Groups containing multiple mutations.
    pub batched_groups: u64,
    /// Attempted mutation requests.
    pub requests: u64,
    /// Largest mutation group.
    pub largest: usize,
    /// Fixed mutation group limit.
    pub limit: usize,
    /// Groups including ready native completions.
    pub ready_commit_groups: u64,
    /// Groups containing mutations and completions.
    pub mixed_commit_groups: u64,
    /// Largest combined group.
    pub largest_commit_group: usize,
    /// Fixed combined group limit.
    pub max_commit_group: usize,
}
