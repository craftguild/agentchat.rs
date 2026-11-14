//! Complete stages publish/remove only through their originally held parent.
use super::{
    directory::{Directory, FileId, Snapshot},
    required,
};
use agentchat_core::Result;
use std::{
    ffi::{OsStr, OsString},
    fs::File,
};

pub(crate) struct Staged<'a> {
    parent: &'a Directory,
    name: OsString,
    id: FileId,
    pub file: File,
    complete: Option<Snapshot>,
    origin: Option<crate::staging::FileOrigin>,
    keep: bool,
}
/// Pending provenance plus the same locked open-file description. A durable
/// genesis transfers these together rather than reopening its own locked inode.
pub(crate) struct Retained {
    pub origin: crate::staging::FileOrigin,
    pub file: File,
}
impl<'a> Staged<'a> {
    pub fn new(parent: &'a Directory, prefix: &str) -> Result<Self> {
        let name = OsString::from(format!("{prefix}{}", uuid::Uuid::new_v4()));
        let file = parent.create_record(&name)?;
        let id = FileId::of(&file.metadata().map_err(|_| required())?);
        Ok(Self {
            parent,
            name,
            id,
            file,
            complete: None,
            origin: None,
            keep: false,
        })
    }
    /// Register intent before allocation. Low-level `new` remains untracked
    /// solely for the self-bound origin-index swap and legacy/test publishers.
    pub(crate) fn tracked(
        parent: &'a Directory,
        prefix: &str,
        home: &std::path::Path,
        target: &OsStr,
        purpose: crate::staging::StagePurpose,
        alternate_parent: Option<&std::path::Path>,
        guard: Option<&OsStr>,
    ) -> Result<Self> {
        let name = OsString::from(format!("{prefix}{}", uuid::Uuid::new_v4()));
        let origin =
            crate::staging::OriginRegistry::new(home).begin_file(crate::staging::FilePlan {
                parent: &parent.path,
                stage: &name,
                target,
                purpose,
                alternate_parent,
                guard,
            })?;
        let file = parent.create_record(&name)?;
        let id = FileId::of(&file.metadata().map_err(|_| required())?);
        Ok(Self {
            parent,
            name,
            id,
            file,
            complete: None,
            origin: Some(origin),
            keep: false,
        })
    }
    pub(crate) fn name(&self) -> &OsStr {
        &self.name
    }
    pub(crate) fn keep_on_failure(&mut self) {
        self.keep = true;
    }
    pub(crate) fn seal(&mut self, expected: &[u8]) -> std::io::Result<()> {
        self.file.sync_all()?;
        let snapshot = self
            .parent
            .read_bound(&self.name, expected.len().max(1))
            .map_err(std::io::Error::other)?
            .ok_or_else(|| std::io::Error::other("The original stage is missing."))?;
        if snapshot.id != self.id || snapshot.bytes != expected {
            return Err(std::io::Error::other("The original stage has changed."));
        }
        self.complete = Some(snapshot);
        if let Some(origin) = self.origin.as_mut() {
            origin
                .seal(&self.file, expected)
                .map_err(std::io::Error::other)?;
        }
        Ok(())
    }
    pub fn publish(
        mut self,
        target: &OsStr,
        expected: &[u8],
        prior: Option<&Snapshot>,
    ) -> std::io::Result<()> {
        self.commit(target, expected, prior, true)
    }
    pub(crate) fn pair_with(&mut self, other: &mut Staged<'_>) -> Result<()> {
        self.origin
            .as_mut()
            .ok_or_else(required)?
            .pair(other.origin.as_mut().ok_or_else(required)?)
    }
    /// Transfer a winning genesis stage without deleting it. Its canonical
    /// birth record now owns the pending name; ordinary retry must finish it.
    pub(crate) fn retain_pending(mut self) -> Result<Retained> {
        // The winning birth already names this pending stage. Even a failed FD
        // transfer must preserve its data and witness for exact recovery.
        self.complete = None;
        let file = self.file.try_clone().map_err(|_| required())?;
        let origin = self.origin.take().ok_or_else(required)?;
        Ok(Retained { origin, file })
    }
    pub(crate) fn publish_retained(
        mut self,
        target: &OsStr,
        expected: &[u8],
    ) -> std::io::Result<Retained> {
        let file = self.file.try_clone()?;
        self.commit(target, expected, None, false)?;
        let origin = self
            .origin
            .take()
            .ok_or_else(|| std::io::Error::other("Missing original publication proof"))?;
        Ok(Retained { origin, file })
    }
    fn commit(
        &mut self,
        target: &OsStr,
        expected: &[u8],
        prior: Option<&Snapshot>,
        release_writer: bool,
    ) -> std::io::Result<()> {
        self.seal(expected)?;
        #[cfg(test)]
        tests::retention::cut(&self.parent.path, &self.name, target);
        #[cfg(test)]
        tests::publishers::cut(&self.parent.path, &self.name, target);
        let published = if let Some(prior) = prior {
            self.parent
                .replace_record(&self.name, target, prior)
                .map_err(std::io::Error::other)
        } else {
            // Keep AlreadyExists typed for competing first creators. No path-
            // based tempfile destructor can later unlink a foreign successor.
            self.parent.publish_new_io(&self.name, target)?;
            self.parent.sync().map_err(std::io::Error::other)
        };
        published?;
        if release_writer && self.origin.is_some() {
            // seal() protects an unpublished writer through every FD copy.
            // After the exact rename AND parent sync succeed, this writer is
            // finished. A pre-exec child must not keep the canonical record
            // locked merely by retaining an incidental inherited descriptor.
            // Failed/uncertain publication and retained pending stages keep
            // their original last-FD retention lock unchanged.
            fs2::FileExt::unlock(&self.file)?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/native_metadata_staged.rs"]
mod tests;
impl Drop for Staged<'_> {
    fn drop(&mut self) {
        if self.keep {
            return;
        }
        if let Some(snapshot) = &self.complete {
            let _ = self.parent.remove_record(&self.name, snapshot);
        }
        // Incomplete/unproved stages survive for authenticated retention work;
        // neither their prefix nor the latest pathname is deletion authority.
    }
}
