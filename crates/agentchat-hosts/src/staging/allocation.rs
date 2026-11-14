//! A pending allocation survives directory/socket/publication crash boundaries.
pub(super) mod model;
mod published;
mod recover;
pub(crate) use published::Published;
pub(crate) use published::published_cleanup;
pub(super) use recover::recover;
#[cfg(test)]
#[path = "../../tests/unit/allocation_origins.rs"]
pub(crate) mod tests;
use super::{
    OriginRegistry, StagePurpose,
    record::{self, FileProof, Guard, Intent, Parent, Record},
    unavailable, writer,
};
use crate::claude::{
    metadata::directory::{Directory, FileId, Snapshot},
    process::ProcessIdentity,
};
use agentchat_core::Result;
use model::Allocation;
use std::{
    ffi::OsStr,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
};
use uuid::Uuid;

/// Physical socket-allocation plan. All paths are explicit; no environment or
/// conversation discovery occurs. Only Gateway and Relay purposes are accepted.
pub struct AllocationPlan<'a> {
    /// Planned unpredictable external directory, before creation.
    pub path: &'a Path,
    /// Original private endpoint parent.
    pub root: &'a Path,
    /// Same original endpoint parent at its known quarantine path, if any.
    pub alternate_root: Option<&'a Path>,
    /// Intended endpoint entry in that private parent.
    pub endpoint: &'a OsStr,
    /// Expected socket names, in creation order; at most two.
    pub sockets: &'a [&'a str],
    /// Existing original endpoint-serialization lock, if applicable.
    pub guard: Option<&'a OsStr>,
    /// Role-specific recovery contract.
    pub purpose: StagePurpose,
    /// Optional full original native process token; never conversation authority.
    pub native: Option<&'a ProcessIdentity>,
}
/// Original physical journal retained until explicit safe release. Local drop
/// never removes external objects. Its durable facts cannot create host state.
pub struct AllocationOrigin {
    index: Directory,
    parent: Directory,
    value: Record,
    snapshot: Snapshot,
}
pub(super) fn key(path: &Path) -> Result<Uuid> {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(unavailable)?;
    let raw = name
        .get(name.len().checked_sub(36).ok_or_else(unavailable)?..)
        .ok_or_else(unavailable)?;
    let id = Uuid::parse_str(raw).map_err(|_| unavailable())?;
    if id.is_nil() || id.to_string() != raw {
        return Err(unavailable());
    }
    Ok(id)
}
pub(super) fn begin(
    registry: &OriginRegistry,
    plan: AllocationPlan<'_>,
) -> Result<AllocationOrigin> {
    if !matches!(plan.purpose, StagePurpose::Gateway | StagePurpose::Relay) {
        return Err(unavailable());
    }
    record::absolute(plan.path)?;
    let parent_path = plan
        .path
        .parent()
        .ok_or_else(unavailable)?
        .canonicalize()
        .map_err(|_| unavailable())?;
    let parent = Directory::shared(&parent_path)?;
    let stage = plan.path.file_name().ok_or_else(unavailable)?;
    record::component(stage.as_bytes())?;
    record::component(plan.endpoint.as_bytes())?;
    if !Directory::absent(&parent.path.join(stage))? {
        return Err(unavailable());
    }
    let root = Directory::owned(plan.root)?;
    let guard = plan
        .guard
        .map(|name| {
            let snapshot = root.read(name)?.ok_or_else(unavailable)?;
            Ok(Guard {
                name: name.as_bytes().into(),
                id: snapshot.id,
            })
        })
        .transpose()?;
    let allocation = Allocation {
        root: Parent {
            path: record::absolute(plan.root)?,
            id: root.id,
            alternate: plan.alternate_root.map(record::absolute).transpose()?,
        },
        guard,
        expected: plan.sockets.iter().map(|s| (*s).into()).collect(),
        directory: None,
        sockets: Vec::new(),
        endpoint: None,
        native: plan.native.cloned(),
    };
    allocation.validate()?;
    let index = registry.index(true)?.ok_or_else(unavailable)?;
    let name = format!("{}.json", key(plan.path)?);
    let value = Record {
        version: 1,
        index: index.id,
        file: FileId(0, 0),
        name: name.as_bytes().into(),
        temporary: Vec::new(),
        creator: ProcessIdentity::capture(std::process::id())?,
        retiring: false,
        intent: Intent {
            parent: Parent {
                path: record::absolute(&parent.path)?,
                id: parent.id,
                alternate: None,
            },
            stage: stage.as_bytes().into(),
            target: plan.endpoint.as_bytes().into(),
            purpose: plan.purpose,
            proof: None,
            guard: None,
            related: None,
            allocation: Some(allocation),
        },
        checksum: [0; 32],
    };
    let (value, snapshot) = writer::publish(&index, OsStr::new(&name), None, value)?;
    #[cfg(test)]
    tests::cut("intent", &parent.path.join(stage));
    Ok(AllocationOrigin {
        index,
        parent,
        value,
        snapshot,
    })
}
impl AllocationOrigin {
    fn facts(&self) -> &Allocation {
        self.value
            .intent
            .allocation
            .as_ref()
            .expect("allocation journal")
    }
    fn facts_mut(&mut self) -> &mut Allocation {
        self.value
            .intent
            .allocation
            .as_mut()
            .expect("allocation journal")
    }
    fn path(&self) -> Result<PathBuf> {
        Ok(self
            .parent
            .path
            .join(record::component(&self.value.intent.stage)?))
    }
    fn current(&self) -> Result<()> {
        if !self.value.creator.is_current()
            || !self
                .index
                .matches(record::component(&self.value.name)?, &self.snapshot)?
        {
            return Err(unavailable());
        }
        self.parent.current()
    }
    fn save(&mut self) -> Result<()> {
        let name = record::component(&self.value.name)?;
        let (value, snapshot) =
            writer::publish(&self.index, name, Some(&self.snapshot), self.value.clone())?;
        self.value = value;
        self.snapshot = snapshot;
        Ok(())
    }
    /// Capture the directory just created by this original physical writer.
    /// An orphan collector never fills this fact from a current-looking path.
    pub fn directory_created(&mut self) -> Result<()> {
        self.current()?;
        let directory = Directory::open(&self.path()?)?;
        if directory.parent_id()? != self.parent.id
            || self.facts().directory.is_some_and(|id| id != directory.id)
        {
            return Err(unavailable());
        }
        directory.sync()?;
        self.parent.sync()?;
        self.facts_mut().directory = Some(directory.id);
        self.save()?;
        #[cfg(test)]
        tests::cut("directory-captured", &self.path()?);
        Ok(())
    }
    /// Capture one newly bound original socket before publishing an endpoint.
    pub fn socket_created(&mut self, name: &str) -> Result<()> {
        self.current()?;
        let directory = Directory::open(&self.path()?)?;
        let facts = self.facts();
        if facts.directory != Some(directory.id)
            || facts.expected.get(facts.sockets.len()).map(String::as_str) != Some(name)
        {
            return Err(unavailable());
        }
        let socket = directory.socket(name.as_ref())?.ok_or_else(unavailable)?;
        directory.sync()?;
        self.facts_mut().sockets.push((name.into(), socket));
        self.save()?;
        #[cfg(test)]
        tests::cut("socket-captured", &self.path()?);
        Ok(())
    }
    /// Remember this writer's completed canonical endpoint without changing it.
    pub fn published(&mut self) -> Result<()> {
        self.current()?;
        let root = root(self.facts())?;
        let endpoint = root
            .read(record::component(&self.value.intent.target)?)?
            .ok_or_else(unavailable)?;
        self.facts_mut().endpoint = Some(FileProof::snapshot(&endpoint));
        self.save()?;
        #[cfg(test)]
        tests::cut("published", &self.path()?);
        Ok(())
    }
    /// Durably declare explicit local release before the caller unlinks objects.
    /// It remains the caller's duty to prove and remove only its original set.
    pub fn begin_release(&mut self) -> Result<()> {
        self.current()?;
        self.value.retiring = true;
        self.save()
    }
    /// Retire only after the original directory is gone and its parent syncs.
    pub fn finish(&mut self) -> Result<()> {
        self.current()?;
        if !Directory::absent(&self.path()?)?
            || !self.value.retiring && self.facts().directory.is_some()
        {
            return Err(unavailable());
        }
        self.parent.sync()?;
        let name = record::component(&self.value.name)?;
        let _lock = self.index.lock_record(name, &self.snapshot)?;
        self.index.remove_record(name, &self.snapshot)?;
        Ok(())
    }
}
impl Drop for AllocationOrigin {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}
pub(super) fn root(facts: &Allocation) -> Result<Directory> {
    for path in std::iter::once(&facts.root.path).chain(facts.root.alternate.iter()) {
        if let Ok(directory) = Directory::owned(&record::path(path))
            && directory.id == facts.root.id
        {
            return Ok(directory);
        }
    }
    Err(unavailable())
}
