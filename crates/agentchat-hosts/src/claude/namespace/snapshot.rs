//! Bounded snapshots and atomic certificates; no recursive deletion or aliases.
use super::{storage, unavailable};
use agentchat_core::{Error, ErrorCode, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
};

const LIMIT: usize = 262_144;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Stamp {
    parent: (u64, u64),
    identity: (u64, u64),
    length: u64,
    digest: [u8; 32],
}
pub(super) fn directory(path: &Path) -> Result<(u64, u64)> {
    crate::claude::metadata::private_directory(path)?;
    Ok(storage::identity(
        &std::fs::symlink_metadata(path).map_err(|_| unavailable())?,
    ))
}
pub(super) fn exists(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(unavailable()),
    }
}
pub(super) fn sync(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|_| unavailable())
}
fn bytes(path: &Path) -> Result<(Vec<u8>, Stamp)> {
    let parent_path = path.parent().ok_or_else(unavailable)?;
    let parent = directory(parent_path)?;
    let file = storage::open(path, LIMIT as u64, false)?;
    let before = file.metadata().map_err(|_| unavailable())?;
    let mut data = Vec::new();
    file.take(LIMIT as u64 + 1)
        .read_to_end(&mut data)
        .map_err(|_| unavailable())?;
    let after = storage::open(path, LIMIT as u64, false)?
        .metadata()
        .map_err(|_| unavailable())?;
    if directory(parent_path)? != parent
        || data.len() > LIMIT
        || before.len() != data.len() as u64
        || storage::identity(&before) != storage::identity(&after)
        || before.len() != after.len()
        || before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec()
        || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec()
    {
        return Err(unavailable());
    }
    let stamp = Stamp {
        parent,
        identity: storage::identity(&before),
        length: before.len(),
        digest: Sha256::digest(&data).into(),
    };
    Ok((data, stamp))
}
pub(super) fn record<T: DeserializeOwned>(path: &Path) -> Result<(T, Stamp)> {
    let (data, stamp) = bytes(path)?;
    Ok((
        crate::claude::metadata::decode(path, &data, stamp.parent, stamp.identity)?,
        stamp,
    ))
}
pub(super) fn small_record<T: DeserializeOwned>(path: &Path) -> Result<(T, Stamp)> {
    let (data, stamp) = bytes(path)?;
    if data.len() > 65_536 {
        return Err(unavailable());
    }
    Ok((
        crate::claude::metadata::decode(path, &data, stamp.parent, stamp.identity)?,
        stamp,
    ))
}
impl Stamp {
    pub fn in_directory(&self, expected: (u64, u64)) -> bool {
        self.parent == expected
    }
    pub fn capture(path: &Path) -> Result<Self> {
        Ok(bytes(path)?.1)
    }
    pub fn matches(&self, path: &Path) -> Result<bool> {
        Ok(Self::capture(path)? == *self)
    }
    pub fn remove(&self, path: &Path) -> Result<bool> {
        let parent = path.parent().ok_or_else(unavailable)?;
        directory(parent)?;
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(
                (rustix::fs::OFlags::DIRECTORY | rustix::fs::OFlags::NOFOLLOW).bits() as i32,
            )
            .open(parent)
            .map_err(|_| unavailable())?;
        if storage::identity(&directory.metadata().map_err(|_| unavailable())?) != self.parent {
            return Ok(false);
        }
        if !exists(path)? || !self.matches(path)? {
            return Ok(false);
        }
        // Pin the authenticated directory so a pathname replacement cannot
        // redirect unlink to a foreign successor directory.
        rustix::fs::unlinkat(
            &directory,
            path.file_name().ok_or_else(unavailable)?,
            rustix::fs::AtFlags::empty(),
        )
        .map_err(|_| unavailable())?;
        directory.sync_all().map_err(|_| unavailable())?;
        Ok(true)
    }
}
pub(super) fn publish(
    home: &Path,
    path: &Path,
    value: &impl Serialize,
    prior: Option<&Stamp>,
    guard: Option<&std::ffi::OsStr>,
) -> Result<()> {
    let parent = path.parent().ok_or_else(unavailable)?;
    let held = crate::claude::metadata::directory::Directory::owned(parent)?;
    let name = path.file_name().ok_or_else(unavailable)?;
    let before = held.read_bound(name, LIMIT)?;
    if let Some(prior) = prior {
        if !prior.in_directory((held.id.0, held.id.1)) || !prior.matches(path)? || before.is_none()
        {
            return Err(unavailable());
        }
    } else if before.is_some() {
        return Err(unavailable());
    }
    let data = serde_json::to_vec(value).map_err(|_| unavailable())?;
    if data.len() > LIMIT {
        return Err(unavailable());
    }
    let mut stage = crate::claude::metadata::staged::Staged::tracked(
        &held,
        ".gc-",
        home,
        name,
        crate::staging::StagePurpose::Certificate,
        None,
        guard,
    )?;
    stage.file.write_all(&data).map_err(|_| unavailable())?;
    stage
        .publish(
            name,
            &data,
            if prior.is_some() {
                before.as_ref()
            } else {
                None
            },
        )
        .map_err(|_| unavailable())
}
pub(super) fn locked(path: &Path, create: bool) -> Result<Option<crate::lock::ScopedLock>> {
    if !exists(path)? && !create {
        return Ok(None);
    }
    if create && !exists(path)? {
        match OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(path)
        {
            Ok(file) => {
                file.sync_all().map_err(|_| unavailable())?;
                sync(path.parent().ok_or_else(unavailable)?)?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(unavailable()),
        }
    }
    let file = storage::open(path, 65_536, true)?;
    if file.metadata().map_err(|_| unavailable())?.len() != 0 {
        return Err(unavailable());
    }
    exclusive(&file)?;
    let file = crate::lock::ScopedLock::acquired(file);
    if storage::identity(&file.metadata().map_err(|_| unavailable())?)
        != storage::identity(&std::fs::symlink_metadata(path).map_err(|_| unavailable())?)
    {
        return Err(unavailable());
    }
    Ok(Some(file))
}
pub(super) fn exclusive(file: &File) -> Result<()> {
    file.try_lock_exclusive().map_err(|error| {
        if error.kind() == std::io::ErrorKind::WouldBlock {
            let mut error = Error::new(ErrorCode::Busy, "Native metadata collection is busy.");
            error.retryable = true;
            error
        } else {
            unavailable()
        }
    })
}
