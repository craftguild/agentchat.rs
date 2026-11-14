//! Bounded private native metadata; no record alone grants conversation authority.
pub(crate) mod directory;
mod publisher;
mod record;
pub use publisher::Publisher;
pub(crate) mod staged;
use agentchat_core::{Error, ErrorCode, Result};
use directory::{Directory, FileId};
use serde::{Serialize, de::DeserializeOwned};
use std::{io::Write, os::unix::fs::MetadataExt, path::Path};

/// Call-site-selected wire budget; input never widens ordinary metadata reads.
#[derive(Clone, Copy)]
pub(crate) enum Budget {
    Metadata,
    Input,
}
impl Budget {
    pub(crate) fn purpose(self) -> crate::staging::StagePurpose {
        match self {
            Self::Metadata => crate::staging::StagePurpose::Metadata,
            Self::Input => crate::staging::StagePurpose::Input,
        }
    }
    pub(crate) fn limit(self) -> usize {
        self.purpose().limit()
    }
}
fn required() -> Error {
    Error::new(
        ErrorCode::SessionRequired,
        "Private native routing metadata is unavailable or changed.",
    )
}
/// Check an existing owned private directory, without creating it.
pub fn private_directory(path: &Path) -> Result<()> {
    let m = std::fs::symlink_metadata(path).map_err(|_| required())?;
    if !path.is_absolute()
        || !m.is_dir()
        || m.uid() != rustix::process::geteuid().as_raw()
        || m.mode() & 0o077 != 0
    {
        return Err(required());
    }
    Ok(())
}
/// Read at most 64 KiB, rejecting aliases and replacements before/after the read.
/// Legacy payloads are observation inputs only. Proved records must also match
/// their original parent, entry name, inode and complete content checksum.
pub fn read<T: DeserializeOwned>(path: &Path) -> Result<T> {
    read_budget(path, Budget::Metadata)
}
/// Original-proved small records; native input locators/receipts never adopt
/// legacy observation-only JSON as permission to consume or complete a body.
pub(crate) fn read_proved<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let parent = Directory::owned(path.parent().ok_or_else(required)?)?;
    let name = path.file_name().ok_or_else(required)?;
    let snapshot = parent.read(name)?.ok_or_else(required)?;
    decode_proved(&parent, name, &snapshot)
}
pub(crate) fn read_budget<T: DeserializeOwned>(path: &Path, budget: Budget) -> Result<T> {
    let parent = Directory::owned(path.parent().ok_or_else(required)?)?;
    let name = path.file_name().ok_or_else(required)?;
    let snapshot = parent
        .read_bound(name, budget.limit())?
        .ok_or_else(required)?;
    decode_budget(
        path,
        &snapshot.bytes,
        parent.id,
        snapshot.id,
        budget,
        matches!(budget, Budget::Input),
    )
}
pub(crate) fn decode<T: DeserializeOwned>(
    path: &Path,
    bytes: &[u8],
    parent: (u64, u64),
    file: (u64, u64),
) -> Result<T> {
    decode_budget(
        path,
        bytes,
        FileId(parent.0, parent.1),
        FileId(file.0, file.1),
        Budget::Metadata,
        false,
    )
}
fn decode_budget<T: DeserializeOwned>(
    path: &Path,
    bytes: &[u8],
    parent: FileId,
    file: FileId,
    budget: Budget,
    proved: bool,
) -> Result<T> {
    let decoded = record::decode(
        parent,
        file,
        path.file_name().ok_or_else(required)?,
        bytes,
        proved,
        budget,
    )?;
    // Raw versioned headers/legacy DTOs keep serde's original typed duplicate-
    // field refusal; normalizing them through Value would weaken that parser.
    if decoded.proved {
        serde_json::from_value(decoded.value).map_err(|_| required())
    } else {
        serde_json::from_slice(bytes).map_err(|_| required())
    }
}
/// Require the original canonical publication, not merely readable legacy data.
pub(crate) fn decode_proved<T: DeserializeOwned>(
    parent: &Directory,
    name: &std::ffi::OsStr,
    snapshot: &directory::Snapshot,
) -> Result<T> {
    let value = record::decode(
        parent.id,
        snapshot.id,
        name,
        &snapshot.bytes,
        true,
        Budget::Metadata,
    )?;
    serde_json::from_value(value.value).map_err(|_| required())
}
fn encoded(value: &impl Serialize, budget: Budget) -> Result<Vec<u8>> {
    struct Limited(Vec<u8>, usize);
    impl Write for Limited {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.1 - self.0.len() {
                return Err(std::io::Error::other("Native record bound"));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut bytes = Limited(Vec::new(), budget.limit());
    serde_json::to_writer(&mut bytes, value).map_err(|_| required())?;
    Ok(bytes.0)
}
/// Atomically replace one bounded record. Serialize mutable ownership decisions
/// separately; this helper preserves an observed foreign/successor replacement.
/// An existing record must carry original publication provenance; legacy data
/// remains readable but is never silently adopted for mutation.
pub fn write(path: &Path, value: &impl Serialize) -> Result<()> {
    publish(path, value, false)
}
/// Publish a complete immutable record exclusively; an existing value is never adopted.
pub fn create(path: &Path, value: &impl Serialize) -> Result<()> {
    publish(path, value, true)
}
fn publish(path: &Path, value: &impl Serialize, exclusive: bool) -> Result<()> {
    let parent = Directory::owned(path.parent().ok_or_else(required)?)?;
    let name = path.file_name().ok_or_else(required)?;
    publish_in(&parent, name, value, exclusive)
}
/// Keep an already authenticated parent across a physical transport operation.
pub(in crate::claude) fn publish_in(
    parent: &Directory,
    name: &std::ffi::OsStr,
    value: &impl Serialize,
    exclusive: bool,
) -> Result<()> {
    publish_selected(parent, name, value, exclusive, None, Budget::Metadata)
}
fn publish_selected(
    parent: &Directory,
    name: &std::ffi::OsStr,
    value: &impl Serialize,
    exclusive: bool,
    publisher: Option<&Publisher>,
    budget: Budget,
) -> Result<()> {
    let previous = parent.read_bound(name, budget.limit())?;
    let _lock = if let Some(snapshot) = &previous {
        if exclusive {
            return Err(required());
        }
        let lock = parent.lock_record(name, snapshot)?;
        record::decode(parent.id, snapshot.id, name, &snapshot.bytes, true, budget)?;
        Some(lock)
    } else {
        None
    };
    let payload = record::payload(value, budget)?;
    record::check_size(parent, name, &payload, budget)?;
    if let Some(original) = &previous
        && !parent.matches(name, original)?
    {
        return Err(required());
    }
    let mut stage = match publisher {
        Some(publisher) => publisher.stage(parent, name)?,
        None => staged::Staged::new(parent, ".native-")?,
    };
    let id = FileId::of(&stage.file.metadata().map_err(|_| required())?);
    let data = record::encode(parent, id, name, payload, budget)?;
    stage.file.write_all(&data).map_err(|_| required())?;
    stage
        .publish(name, &data, previous.as_ref())
        .map_err(|_| required())
}
/// Remove only an exact still-owned record under its caller's lifecycle lock.
/// Missing or changed values do not authorize deleting a successor or registry history.
pub fn remove_if<T: DeserializeOwned + PartialEq>(path: &Path, expected: &T) -> Result<bool> {
    remove_budget(path, expected, Budget::Metadata)
}
pub(crate) fn remove_budget<T: DeserializeOwned + PartialEq>(
    path: &Path,
    expected: &T,
    budget: Budget,
) -> Result<bool> {
    let parent = Directory::owned(path.parent().ok_or_else(required)?)?;
    let name = path.file_name().ok_or_else(required)?;
    let Some(snapshot) = parent.read_bound(name, budget.limit())? else {
        return Ok(false);
    };
    let _lock = parent.lock_record(name, &snapshot)?;
    let actual: T = serde_json::from_value(
        record::decode(parent.id, snapshot.id, name, &snapshot.bytes, true, budget)?.value,
    )
    .map_err(|_| required())?;
    if actual != *expected {
        return Ok(false);
    }
    parent.remove_record(name, &snapshot)
}
/// An idempotent winner may have become visible before its publisher died at
/// parent sync. Prove and lock those exact bytes, then establish durability.
pub(crate) fn sync_input<T: DeserializeOwned + PartialEq>(path: &Path, expected: &T) -> Result<()> {
    let parent = Directory::owned(path.parent().ok_or_else(required)?)?;
    let name = path.file_name().ok_or_else(required)?;
    let snapshot = parent
        .read_bound(name, Budget::Input.limit())?
        .ok_or_else(required)?;
    let actual: T = decode_budget(
        path,
        &snapshot.bytes,
        parent.id,
        snapshot.id,
        Budget::Input,
        true,
    )?;
    if actual != *expected {
        return Err(required());
    }
    let _lock = parent.lock_record(name, &snapshot)?;
    parent.sync()?;
    if !parent.matches(name, &snapshot)? {
        return Err(required());
    }
    Ok(())
}
