//! Ownership-preserving, journaled installation of host configuration artifacts.
//!
//! This library does not admit a host or bypass its review/trust controls.
mod current;
mod fileio;
mod inspection;
mod journal;
mod merge;
mod removal;
mod requests;
pub use removal::{RemovalPlan, Remover};
mod strict_json;
mod toml;

pub(crate) use current::Installed;
pub use inspection::{ComponentInspection, ComponentState, Inspection, InspectionState};

use agentchat_core::{Error, ErrorCode, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

/// One separately owned artifact within a host's existing configuration.
#[derive(Debug, Clone)]
pub struct Artifact {
    /// Stable agentchat component identity, independent from file formatting.
    pub key: String,
    /// Absolute destination within an explicitly selected configuration root.
    pub destination: PathBuf,
    /// Exact owned value, array member, or complete file.
    pub content: Content,
}
/// The installer only owns the selected value; sibling data belongs to the host.
#[derive(Debug, Clone)]
pub enum Content {
    /// Complete UTF-8 skill/resource file. A fresh exclusive directory must not
    /// exist already, even if it contains no SKILL.md.
    File {
        /// Desired complete bytes.
        text: String,
        /// Refuse a preexisting unowned skill directory.
        exclusive_directory: bool,
    },
    /// One JSON object member selected by its nonempty key path.
    Json {
        /// Object key path, never a JSON pointer expression.
        path: Vec<String>,
        /// Desired member value.
        value: Value,
    },
    /// One exactly identified JSON array member, preserving all foreign members.
    JsonArray {
        /// Object key path of the array.
        path: Vec<String>,
        /// Desired complete matcher group or member.
        value: Value,
    },
    /// One TOML value/table, retaining formatting and unrelated configuration.
    Toml {
        /// Table/key path.
        path: Vec<String>,
        /// JSON-compatible desired table/value.
        value: Value,
    },
}
/// Durable installation boundaries, usable for instrumentation/fault injection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallStage {
    /// Every destination parsed and all ownership/collision checks passed.
    Validated,
    /// Desired writes and complete previous contents are durably journaled.
    Journaled,
    /// One atomic file replacement and its parent directory were synchronized.
    Published(usize),
    /// The complete new ownership ledger is durable.
    OwnershipPublished,
    /// Journal removed; the most recent bounded backup remains available.
    Complete,
}
/// Optional bounded callback. Failures after Journaled retain recovery intent
/// until the Complete boundary; earlier failures have no host writes.
pub trait InstallProgress: Send + Sync {
    /// Observe a boundary. Do not modify integration destinations from this sink.
    fn reached(&self, stage: InstallStage) -> Result<()>;
}
struct Silent;
impl InstallProgress for Silent {
    fn reached(&self, _: InstallStage) -> Result<()> {
        Ok(())
    }
}
/// Result separates changed files from already-owned, unchanged artifacts.
#[derive(Debug, Clone, Serialize)]
pub struct InstallReport {
    /// Number of atomically replaced host files in this request.
    pub changed_files: usize,
    /// Whether interrupted earlier intent was completed first.
    pub recovered: bool,
}
/// Installer roots are explicit; construction has no host/configuration writes.
#[derive(Clone)]
pub struct Installer {
    state: PathBuf,
    roots: Vec<PathBuf>,
    aliases: Vec<(PathBuf, PathBuf)>,
    progress: Arc<dyn InstallProgress>,
}
impl Installer {
    /// Select the agentchat state root and allowed host configuration roots. Native
    /// paths are retained without deriving identity from display strings.
    pub fn new(state: impl AsRef<Path>, roots: Vec<PathBuf>) -> Result<Self> {
        if roots.is_empty() || roots.len() > 8 {
            return Err(conflict("Select one through eight configuration roots."));
        }
        let state = fileio::absolute(state.as_ref())?.join("integrations");
        let aliases = roots
            .iter()
            .map(|p| {
                fileio::checked(p)?;
                // A nested allowed root is still inside its explicitly selected
                // outer root. Canonicalizing it independently could turn an
                // interior symlink into permission to write a foreign tree.
                let outer = roots
                    .iter()
                    .filter(|root| p.starts_with(root))
                    .min_by_key(|root| root.components().count())
                    .expect("each root contains itself");
                Ok((
                    p.clone(),
                    fileio::absolute(outer)?.join(p.strip_prefix(outer).expect("contained root")),
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        let mut roots = aliases.iter().map(|(_, p)| p.clone()).collect::<Vec<_>>();
        roots.sort();
        roots.dedup();
        Ok(Self {
            state,
            roots,
            aliases,
            progress: Arc::new(Silent),
        })
    }
    /// Observe durable boundaries without exposing an alternate write path.
    pub fn with_progress(mut self, progress: Arc<dyn InstallProgress>) -> Self {
        self.progress = progress;
        self
    }
    /// Validate the whole plan, then publish each file atomically under one
    /// user-wide agentchat installation lock. Recovery runs before the new request.
    pub async fn install(&self, artifacts: Vec<Artifact>) -> Result<InstallReport> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || {
            let _fence = this.lifecycle_fence()?;
            let _lock = fileio::lock(&this.state)?;
            this.install_locked(artifacts)
        })
        .await
        .map_err(|_| Error::new(ErrorCode::IoError, "Installation worker failed."))?
    }
    // Both entry points own the same OS lock before using this implementation.
    // Successful request replays skip it, so they cannot reapply an old plan.
    fn install_locked(&self, artifacts: Vec<Artifact>) -> Result<InstallReport> {
        let recovered = journal::recover(self)?;
        let plan = merge::prepare(self, artifacts)?;
        self.progress.reached(InstallStage::Validated)?;
        let changed_files = plan.writes.len();
        if changed_files > 0 || plan.ownership_changed {
            journal::begin(self, &plan)?;
            self.progress.reached(InstallStage::Journaled)?;
            journal::apply(self, plan)?;
        }
        Ok(InstallReport {
            changed_files,
            recovered,
        })
    }
    /// Explicit private state location for diagnostics/recovery tooling.
    pub fn state_directory(&self) -> &Path {
        &self.state
    }
    fn lifecycle_fence(&self) -> Result<crate::lifecycle::Fence> {
        crate::lifecycle::Lifecycle::new(self.state.parent().expect("integration state parent"))?
            .shared()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Selector {
    File,
    Json { path: Vec<String> },
    JsonArray { path: Vec<String> },
    Toml { path: Vec<String> },
}
fn conflict(message: &str) -> Error {
    Error::new(ErrorCode::ConfigurationConflict, message)
}
fn io(_: std::io::Error) -> Error {
    Error::new(
        ErrorCode::IoError,
        "Integration filesystem operation failed.",
    )
}
fn digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn value_digest(value: &Value) -> Result<String> {
    Ok(digest(&serde_json::to_vec(value).map_err(|_| {
        conflict("Cannot encode owned configuration.")
    })?))
}
