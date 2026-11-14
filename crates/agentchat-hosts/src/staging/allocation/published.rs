//! Typed canonical publications are physical claims, never native wake proof.
use super::*;
use crate::claude::{metadata::directory::SocketId, process::BirthState};

pub(crate) struct Published {
    pub path: PathBuf,
    pub helper: ProcessIdentity,
    pub native: ProcessIdentity,
    pub directory: FileId,
    pub parent: FileId,
    pub sockets: Vec<(String, SocketId)>,
}
pub(super) fn read(root: &Directory, value: &Record, snapshot: &Snapshot) -> Result<Published> {
    let name = record::component(&value.intent.target)?;
    match value.intent.purpose {
        StagePurpose::Gateway => {
            crate::claude::namespace::allocation_publication(root, name, snapshot)
        }
        StagePurpose::Relay => crate::claude::observer::allocation_publication(
            root,
            &record::path(
                &value
                    .intent
                    .allocation
                    .as_ref()
                    .ok_or_else(unavailable)?
                    .root
                    .path,
            ),
            name,
            snapshot,
        ),
        _ => Err(unavailable()),
    }
}
pub(super) fn belongs(value: &Record, claim: &Published) -> Result<bool> {
    let facts = value.intent.allocation.as_ref().ok_or_else(unavailable)?;
    if facts
        .native
        .as_ref()
        .is_some_and(|native| native != &claim.native)
    {
        return Err(unavailable());
    }
    let path =
        record::path(&value.intent.parent.path).join(record::component(&value.intent.stage)?);
    let same_name = claim.path.file_name() == path.file_name()
        && Directory::parent_of(&claim.path)? == value.intent.parent.id;
    if !same_name {
        return Ok(false);
    }
    if claim.helper != value.creator
        || Some(claim.directory) != facts.directory
        || claim.parent != value.intent.parent.id
        || claim.sockets != facts.sockets
    {
        return Err(unavailable());
    }
    Ok(true)
}
/// Called by already-proved canonical recovery before its first external unlink.
/// No missing journal is created, and legacy publications keep their own proof.
pub(crate) fn published_cleanup(
    home: &Path,
    path: &Path,
    root: &Directory,
    endpoint: (&OsStr, &Snapshot),
    directory: FileId,
    sockets: &[(&str, &SocketId)],
    creator: &ProcessIdentity,
) -> Result<Option<AllocationOrigin>> {
    let registry = OriginRegistry::new(home);
    let Some(index) = registry.index(false)? else {
        return Ok(None);
    };
    let name = format!("{}.json", key(path)?);
    let Some(snapshot) = index.read(name.as_ref())? else {
        return Ok(None);
    };
    let value = Record::decode(&index, name.as_ref(), &snapshot)?;
    if !belongs(&value, &read(root, &value, endpoint.1)?)? {
        return Err(unavailable());
    }
    let facts = value.intent.allocation.as_ref().ok_or_else(unavailable)?;
    if !root.matches(endpoint.0, endpoint.1)?
        || value.creator != *creator
        || creator.birth_state() != BirthState::Gone
        || facts.root.id != root.id
        || facts.directory != Some(directory)
        || facts.sockets
            != sockets
                .iter()
                .map(|(n, s)| ((*n).into(), (*s).clone()))
                .collect::<Vec<_>>()
        || value.intent.target != endpoint.0.as_bytes()
    {
        return Err(unavailable());
    }
    if facts
        .endpoint
        .as_ref()
        .is_some_and(|p| *p != FileProof::snapshot(endpoint.1))
    {
        return Err(unavailable());
    }
    let parent = Directory::shared(&record::path(&value.intent.parent.path))?;
    if parent.id != value.intent.parent.id
        || Directory::parent_of(path)? != parent.id
        || path.file_name() != Some(record::component(&value.intent.stage)?)
    {
        return Err(unavailable());
    }
    let mut journal = AllocationOrigin {
        index,
        parent,
        value,
        snapshot,
    };
    journal.facts_mut().endpoint = Some(FileProof::snapshot(endpoint.1));
    journal.value.retiring = true;
    journal.save()?;
    Ok(Some(journal))
}
