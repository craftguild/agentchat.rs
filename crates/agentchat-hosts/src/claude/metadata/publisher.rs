//! Explicit physical origin context; canonical capsule encoding stays unchanged.
use super::{Budget, Directory, publish_selected, required, staged::Staged};
use agentchat_core::Result;
use serde::Serialize;
use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};

/// A plain native-metadata publisher rooted in explicit agentchat state. Construction
/// is inert and grants no host/root/control authority. Callers still serialize
/// mutable lifecycle policy under the original lock before invoking it.
#[derive(Clone)]
pub struct Publisher {
    home: PathBuf,
    alternate: Option<PathBuf>,
    guard: Option<OsString>,
    budget: Budget,
}
impl Publisher {
    /// Select explicit state without environment lookup or filesystem creation.
    pub fn new(home: impl Into<PathBuf>) -> Self {
        Self {
            home: home.into(),
            alternate: None,
            guard: None,
            budget: Budget::Metadata,
        }
    }
    /// Permit only the same original parent inode at this known quarantine path.
    /// The path itself is never original-ownership proof.
    pub fn alternate_parent(mut self, path: impl Into<PathBuf>) -> Self {
        self.alternate = Some(path.into());
        self
    }
    /// Bind an existing lifecycle-lock entry in the original data parent. A
    /// collector must nonblockingly lock that original inode before discarding
    /// an unpublished mutable stage; legitimate lock seal bytes may evolve.
    pub fn guard(mut self, name: impl Into<OsString>) -> Self {
        self.guard = Some(name.into());
        self
    }
    /// Selected only by the typed full-input codec; not a generic public escape
    /// hatch around ordinary metadata's 64-KiB publication/read budget.
    pub(crate) fn input(mut self) -> Self {
        self.budget = Budget::Input;
        self
    }
    /// Publish a new original-proved capsule exclusively, with pending origin.
    pub fn create(&self, path: &Path, value: &impl Serialize) -> Result<()> {
        self.publish(path, value, true)
    }
    /// Replace only an original-proved observed capsule under caller policy.
    /// Orphan collection never replays an unpublished future value.
    pub fn write(&self, path: &Path, value: &impl Serialize) -> Result<()> {
        self.publish(path, value, false)
    }
    fn publish(&self, path: &Path, value: &impl Serialize, exclusive: bool) -> Result<()> {
        let parent = Directory::owned(path.parent().ok_or_else(required)?)?;
        let name = path.file_name().ok_or_else(required)?;
        publish_selected(&parent, name, value, exclusive, Some(self), self.budget)
    }
    pub(super) fn stage<'a>(&self, parent: &'a Directory, name: &OsStr) -> Result<Staged<'a>> {
        Staged::tracked(
            parent,
            ".native-",
            &self.home,
            name,
            self.budget.purpose(),
            self.alternate.as_deref(),
            self.guard.as_deref(),
        )
    }
}
