//! Typed administrative removal outcomes, independent of CLI presentation.

use serde::{Deserialize, Serialize};
use std::path::Path;
use uuid::Uuid;

/// Explicit data-loss boundary selected by the administrative caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemovalScope {
    /// Only the effective project reference and its authorized UUID state.
    Project,
    /// AgentChat integrations and every history in the selected state root.
    Global,
}

/// Completed removal state; missing targets are successful no-ops.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemovalState {
    /// At least one selected component was removed.
    Removed,
    /// No selected target existed at the original request's planning boundary.
    AlreadyAbsent,
}

/// Display plus lossless native location, including non-UTF-8 Unix paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemovalPath {
    /// Human-readable location; never used as deletion authority.
    pub display: String,
    /// Original OS path bytes.
    pub native_bytes: Vec<u8>,
}
impl RemovalPath {
    /// Capture a selected path without inferring identity from its display.
    pub fn new(path: &Path) -> Self {
        Self {
            display: path.to_string_lossy().into_owned(),
            native_bytes: path.as_os_str().as_encoded_bytes().to_vec(),
        }
    }
}

/// A durable result. Replaying its exact request cannot delete later installs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemovalOutcome {
    /// Explicit project or all-project boundary.
    pub scope: RemovalScope,
    /// Removed or already absent at the original operation.
    pub state: RemovalState,
    /// Authorized selected UUID for project removal, if a reference existed.
    pub project_id: Option<Uuid>,
    /// Human-readable data loss, distinct from configuration recovery backups.
    pub data_loss: String,
    /// Bounded component identifiers completed by this operation.
    pub completed: Vec<String>,
    /// Private configuration preimages, when host files were modified.
    pub backup_directory: Option<RemovalPath>,
    /// Surviving private journal and immutable request-result location.
    pub recovery_directory: RemovalPath,
    /// Global reset deliberately retains repository/directory references.
    pub project_references_retained: bool,
    /// Whether this result completed a previously durable partial operation.
    pub recovered: bool,
}
