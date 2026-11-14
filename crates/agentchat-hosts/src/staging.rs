//! Physical provenance for incomplete metadata work; never host or wake authority.
pub(crate) mod allocation;
pub(crate) mod inactive;
mod reap;
mod record;
mod signal;
mod writer;
use crate::claude::metadata::directory::Directory;
use agentchat_core::{Error, ErrorCode, Result};
pub use allocation::{AllocationOrigin, AllocationPlan};
pub use reap::{OriginReaper, OriginReport};
use std::{
    ffi::OsStr,
    fs::DirBuilder,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
};
pub use writer::FileOrigin;

/// Pure immutable-descriptor validation supplied by the runtime layer. It must
/// perform only bounded decoding, without I/O, discovery or application work.
pub type ContextStageValidator = fn(&[u8], &OsStr) -> Result<()>;

/// Recovery semantics selected by a trusted metadata publisher, not tool input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StagePurpose {
    /// Unpublished mutable native metadata; it must never be replayed as live state.
    Metadata,
    /// Full native message input; regenerable from the committed AGENTCHAT stream.
    Input,
    /// Original owner header; preserve committed floors rather than replaying it.
    Header,
    /// Native lock/genesis publication, separate from established canonical state.
    Genesis,
    /// Garbage-collection certificate publication under its original collector lock.
    Certificate,
    /// Private signal genesis lock, paired with its original cwd signal stage.
    SignalLock,
    /// Cwd-local zero signal stage; original signal intent/floors must be respected.
    Signal,
    /// Immutable routing/retry identity; never discard its only full descriptor.
    Context,
    /// Unpublished relay endpoint, carrying its separate original socket provenance.
    Relay,
    /// Unpublished gateway endpoint, carrying its separate original socket provenance.
    Gateway,
}
impl StagePurpose {
    pub(crate) fn limit(self) -> usize {
        match self {
            Self::Input => 524_288,
            Self::Header | Self::Certificate => 262_144,
            Self::Context => 131_072,
            Self::Signal => 2,
            _ => 65_536,
        }
    }
}
/// Original entry names and parent selected before a publisher allocates data.
pub struct FilePlan<'a> {
    /// Original existing data parent, including public cwd parents for signals.
    pub parent: &'a Path,
    /// Unpredictable single-component temporary entry, recorded before creation.
    pub stage: &'a OsStr,
    /// Intended single-component canonical entry. It grants no live authority.
    pub target: &'a OsStr,
    /// Role-specific recovery contract; mutable payloads are never blindly replayed.
    pub purpose: StagePurpose,
    /// Authorized rename of the same original parent, such as native quarantine.
    /// Recovery must still prove the identical original directory inode.
    pub alternate_parent: Option<&'a Path>,
    /// Optional existing lifecycle lock in the original data parent. Its inode
    /// is captured before allocation; recovery must lock that same inode too.
    pub guard: Option<&'a OsStr>,
}
/// One private pending-work index below an explicit existing AGENTCHAT state home.
/// Construction is inert. Only a producer creates its index; diagnostics and
/// collectors must use read-only lookup and never initialize a project/home.
#[derive(Debug, Clone)]
pub struct OriginRegistry {
    home: PathBuf,
}
impl OriginRegistry {
    /// Record explicit physical allocation intent before creating its directory.
    /// This supplies retention provenance, never native root or wake authority.
    pub fn begin_allocation(&self, plan: AllocationPlan<'_>) -> Result<AllocationOrigin> {
        allocation::begin(self, plan)
    }
    /// Select the explicit state root without inspecting cwd, sessions or env vars.
    pub fn new(home: impl Into<PathBuf>) -> Self {
        Self { home: home.into() }
    }
    pub(crate) fn index(&self, create: bool) -> Result<Option<Directory>> {
        if !self.home.is_absolute()
            || self
                .home
                .components()
                .any(|p| p == std::path::Component::ParentDir)
        {
            return Err(unavailable());
        }
        if Directory::absent(&self.home)? {
            return if create { Err(unavailable()) } else { Ok(None) };
        }
        let parent = Directory::owned(&self.home)?;
        let path = self.home.join("staging");
        if create {
            match DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => {
                    parent.sync()?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err(unavailable()),
            }
        } else if Directory::absent(&path)? {
            return Ok(None);
        }
        parent.current()?;
        let directory = Directory::owned(&path)?;
        if directory.parent_id()? != parent.id {
            return Err(unavailable());
        }
        Ok(Some(directory))
    }
    /// Durably record original intent before data creation. No caller may infer
    /// ownership from a prefix if it dies before completing this intent/proof.
    pub fn begin_file(&self, plan: FilePlan<'_>) -> Result<FileOrigin> {
        writer::begin(self, plan)
    }
}
fn unavailable() -> Error {
    Error::new(
        ErrorCode::SessionRequired,
        "Original staging provenance is unavailable or changed.",
    )
}

#[cfg(test)]
#[path = "../tests/unit/stage_origins.rs"]
pub(crate) mod tests;
