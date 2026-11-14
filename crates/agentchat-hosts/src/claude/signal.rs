//! Owned, inode-stable shared file signals; counters carry no wake authority.
mod counter;
mod genesis;
pub(crate) mod recovery;
use super::{metadata, namespace};
use crate::lock::ScopedLock;
use agentchat_core::{Error, ErrorCode, Result};
use fs2::FileExt;
use metadata::directory::FileId;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
const MAX: u64 = 9_007_199_254_740_991;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Anchor {
    version: u32,
    cwd: PathBuf,
    dev: u64,
    ino: u64,
    floor: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending: Option<counter::Pending>,
}
fn required() -> Error {
    Error::new(
        ErrorCode::ConfigurationConflict,
        "The native signal file or its ownership records changed or are malformed; they were preserved.",
    )
}
fn busy() -> Error {
    let mut e = Error::new(ErrorCode::Busy, "The native signal publication is busy.");
    e.retryable = true;
    e
}
fn exists(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(required()),
    }
}
fn anchor_path(home: &Path, cwd: &Path) -> Result<PathBuf> {
    metadata::private_directory(home)?;
    let mut root = home.to_owned();
    for part in ["hosts", "claude", "signals"] {
        root.push(part);
        namespace::private(&root)?;
    }
    Ok(recovery::location(home, cwd))
}
fn location(home: &Path, cwd: &Path) -> PathBuf {
    let digest = Sha256::digest(cwd.as_os_str().as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    home.join("hosts/claude/signals")
        .join(format!("{digest}.json"))
}
fn lock(file: &File) -> Result<()> {
    let started = Instant::now();
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(()),
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    && started.elapsed() < Duration::from_secs(2) =>
            {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Err(busy()),
            Err(_) => return Err(required()),
        }
    }
}
struct Locked {
    file: ScopedLock,
    birth: genesis::Birth,
    path: PathBuf,
    anchor_path: PathBuf,
    anchor: Anchor,
    count: u64,
}
impl Locked {
    fn open(home: &Path, cwd: &Path) -> Result<Self> {
        if !cwd.is_absolute() || std::fs::canonicalize(cwd).map_err(|_| required())? != cwd {
            return Err(required());
        }
        let anchor_path = anchor_path(home, cwd)?;
        let mut birth = genesis::Birth::open(home, &anchor_path, cwd)?;
        let path = cwd.join(".agentchat-wakeup");
        let file = birth.signal(&path, &anchor_path)?;
        // Keep the watched-inode lock as well, so an old v1 writer cannot race
        // its one-time upgrade. Lock order is always private lock, then signal.
        lock(&file)?;
        let mut file = ScopedLock::acquired(file);
        let anchor = birth.finish(&mut file, &path, &anchor_path)?;
        let mut locked = Self {
            file,
            birth,
            path,
            anchor_path,
            anchor,
            count: 0,
        };
        locked.recover()?;
        locked.check()?;
        Ok(locked)
    }
    fn check(&self) -> Result<()> {
        self.birth.check()?;
        let m = checked(&self.path, &self.file)?;
        if FileId::of(&m) != FileId(self.anchor.dev, self.anchor.ino)
            || metadata::read::<Anchor>(&self.anchor_path)? != self.anchor
        {
            return Err(required());
        }
        Ok(())
    }
    fn persist(&mut self, next: Anchor) -> Result<()> {
        self.check()?;
        self.birth.publisher().write(&self.anchor_path, &next)?;
        self.anchor = next;
        self.check()
    }
}
fn checked(path: &Path, file: &File) -> Result<std::fs::Metadata> {
    let m = std::fs::symlink_metadata(path).map_err(|_| required())?;
    let f = file.metadata().map_err(|_| required())?;
    if !m.is_file()
        || m.uid() != rustix::process::geteuid().as_raw()
        || m.mode() & 0o077 != 0
        || m.nlink() != 1
        || m.len() > 32
        || m.ino() != f.ino()
    {
        return Err(required());
    }
    Ok(m)
}
fn signal_file(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)
        .map_err(|_| required())?;
    checked(path, &file)?;
    Ok(file)
}
fn bytes(file: &mut File) -> Result<Vec<u8>> {
    file.seek(SeekFrom::Start(0)).map_err(|_| required())?;
    let mut bytes = Vec::new();
    Read::by_ref(file)
        .take(33)
        .read_to_end(&mut bytes)
        .map_err(|_| required())?;
    if bytes.len() > 32 {
        return Err(required());
    }
    Ok(bytes)
}
pub(super) fn prepare(home: &Path, cwd: &Path) -> Result<()> {
    Locked::open(home, cwd).map(|_| ())
}
pub(super) fn send<T>(
    home: &Path,
    cwd: &Path,
    publish: impl FnOnce(u64) -> Result<T>,
) -> Result<T> {
    let mut signal = Locked::open(home, cwd)?;
    let next = signal
        .count
        .checked_add(1)
        .filter(|n| *n <= MAX)
        .ok_or_else(required)?;
    // Reserve the ordering floor before a callback can publish a wake record.
    // Failed callbacks consume a generation; neither gaps nor repair are ACKs.
    signal.reserve(next)?;
    let result = publish(next)?;
    signal.complete()?;
    Ok(result)
}

#[cfg(test)]
#[path = "../../tests/unit/native_signal.rs"]
pub(crate) mod tests;
