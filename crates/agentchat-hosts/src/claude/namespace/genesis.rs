//! A durable birth witness permits only interrupted pristine initialization.
use super::{OwnerNamespace, storage, unavailable};
use agentchat_core::Result;
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{Read, Write},
    os::unix::fs::FileExt,
};

const LIMIT: u64 = 65_536;
const LOCK: &str = "owner-state.lock";
const MARKER: &str = "owner-state.initialized.json";
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Record {
    version: u32,
    owner: crate::claude::process::ProcessIdentity,
    namespace_id: uuid::Uuid,
    observer: String,
    lock_identity: (u64, u64),
}
fn marker_exists(ns: &OwnerNamespace) -> Result<bool> {
    match std::fs::symlink_metadata(ns.directory.join(MARKER)) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(unavailable()),
    }
}
pub(super) fn open(ns: &OwnerNamespace, write: bool, create: bool) -> Result<File> {
    let path = ns.directory.join(LOCK);
    match std::fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && create => {
            if vacant(ns)? {
                publish(ns)?;
            }
        }
        Err(_) => return Err(unavailable()),
        Ok(_) => {}
    }
    storage::open(&path, LIMIT, write)
}
fn vacant(ns: &OwnerNamespace) -> Result<bool> {
    let lock = ns.directory.join(LOCK);
    let appeared = || lock.try_exists().map_err(|_| unavailable());
    if appeared()? {
        return Ok(false);
    }
    if marker_exists(ns)?
        || ns
            .directory
            .join("owner-state.json")
            .try_exists()
            .map_err(|_| unavailable())?
    {
        return if appeared()? {
            Ok(false)
        } else {
            Err(unavailable())
        };
    }
    // Only unpublished staging names may survive a first creator crash. Their
    // bytes are preserved and never treated as a committed lock or authority.
    for (index, entry) in std::fs::read_dir(&ns.directory)
        .map_err(|_| unavailable())?
        .enumerate()
    {
        let entry = entry.map_err(|_| unavailable())?;
        if index >= 64
            || !entry
                .file_name()
                .to_string_lossy()
                .starts_with(".owner-lock-")
        {
            return if appeared()? {
                Ok(false)
            } else {
                Err(unavailable())
            };
        }
    }
    Ok(!appeared()?)
}

fn publish(ns: &OwnerNamespace) -> Result<()> {
    let parent = crate::claude::metadata::directory::Directory::owned(&ns.directory)?;
    let mut file = crate::claude::metadata::staged::Staged::tracked(
        &parent,
        ".owner-lock-",
        &ns.home,
        LOCK.as_ref(),
        crate::staging::StagePurpose::Genesis,
        Some(&ns.quarantine_path()?),
        None,
    )?;
    let record = Record {
        version: 3,
        owner: ns.owner.clone(),
        namespace_id: ns.id,
        observer: ns.observer.clone(),
        lock_identity: storage::identity(&file.file.metadata().map_err(|_| unavailable())?),
    };
    let bytes = serde_json::to_vec(&record).map_err(|_| unavailable())?;
    if bytes.len() > LIMIT as usize - 2 {
        return Err(unavailable());
    }
    let data = [b"0\n".as_slice(), &bytes].concat();
    file.file.write_all(&data).map_err(|_| unavailable())?;
    // Native atomic no-replace rename avoids the hard-link publication gap:
    // a killed creator cannot leave the canonical lock with two link names.
    // Unsupported filesystems fail closed; there is no weaker link fallback.
    match file.publish(LOCK.as_ref(), &data, None) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(unavailable()),
    }
    parent.sync()
}
pub(super) fn record(ns: &OwnerNamespace, file: &File) -> Result<Option<(Record, bool)>> {
    let m = file.metadata().map_err(|_| unavailable())?;
    if m.len() == 0 {
        // Legacy empty locks retain their existing header contract. A missing
        // legacy header is ambiguous, and a truncated new lock cannot downgrade.
        if marker_exists(ns)? {
            return Err(unavailable());
        }
        return Ok(None);
    }
    let mut bytes = Vec::new();
    file.try_clone()
        .map_err(|_| unavailable())?
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| unavailable())?;
    if bytes.len() > LIMIT as usize {
        return Err(unavailable());
    }
    let sealed = match bytes.get(..2) {
        Some(b"0\n") => false,
        Some(b"1\n") => true,
        _ => return Err(unavailable()),
    };
    let record: Record = serde_json::from_slice(&bytes[2..]).map_err(|_| unavailable())?;
    if record.version != 3
        || record.owner != ns.owner
        || record.namespace_id != ns.id
        || record.observer != ns.observer
        || record.lock_identity != storage::identity(&m)
    {
        return Err(unavailable());
    }
    Ok(Some((record, sealed)))
}
pub(super) fn marked(ns: &OwnerNamespace, record: &Record) -> Result<bool> {
    if !marker_exists(ns)? {
        return Ok(false);
    }
    let marker: Record = crate::claude::metadata::read(&ns.directory.join(MARKER))?;
    if marker != *record {
        return Err(unavailable());
    }
    Ok(true)
}
/// Seal initialization in the original locked inode before any callback.
/// The byte only changes from zero to one, after header/witness synchronization.
/// Thus losing both external records cannot resemble a first-creator crash.
/// A torn/invalid seal refuses; it never grants pristine initialization.
pub(super) fn seal(file: &File) -> Result<()> {
    file.write_all_at(b"1", 0)
        .and_then(|_| file.sync_all())
        .map_err(|_| unavailable())
}
pub(super) fn mark(ns: &OwnerNamespace, record: &Record) -> Result<()> {
    ns.metadata_publisher()?
        .create(&ns.directory.join(MARKER), record)
}

#[cfg(test)]
#[path = "../../../tests/unit/native_genesis.rs"]
mod tests;
