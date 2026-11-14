//! Bounded, no-follow snapshots and removals through the original parent FD.
use super::required as unavailable;
use crate::lock::ScopedLock;
use agentchat_core::Result;
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsStr,
    fs::{self, File, Metadata, OpenOptions},
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

/// Device remains in the legacy wire shape and checksum, but is not identity:
/// macOS/Linux may renumber it after reboot/remount. Original parent scope,
/// inode, private ownership and complete-record checks remain independent.
#[derive(Debug, Clone, Copy, Eq, Serialize, Deserialize)]
pub(crate) struct FileId(pub u64, pub u64);
impl PartialEq for FileId {
    fn eq(&self, other: &Self) -> bool {
        self.1 == other.1
    }
}
impl FileId {
    pub fn of(metadata: &Metadata) -> Self {
        Self(metadata.dev(), metadata.ino())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SocketId {
    id: FileId,
    changed: (i64, i64),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Snapshot {
    pub id: FileId,
    pub(crate) changed: (i64, i64),
    pub(crate) modified: (i64, i64),
    pub bytes: Vec<u8>,
}
pub(crate) struct Directory {
    pub path: PathBuf,
    pub id: FileId,
    fd: File,
    private: bool,
    exact: bool,
}
fn valid(metadata: &Metadata, directory: bool, exact: bool) -> bool {
    metadata.uid() == rustix::process::geteuid().as_raw()
        && if exact {
            metadata.mode() & 0o777 == if directory { 0o700 } else { 0o600 }
        } else {
            metadata.mode() & 0o077 == 0
        }
        && if directory {
            metadata.is_dir()
        } else {
            metadata.is_file() && metadata.nlink() == 1
        }
}
impl Directory {
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_inner(path, true, true)
    }
    pub fn owned(path: &Path) -> Result<Self> {
        if !path.is_absolute() {
            return Err(unavailable());
        }
        Self::open_inner(path, true, false)
    }
    /// Explicitly captured cwd/tmp parent; its children still require private
    /// original-file proof. This does not establish a conversation identity.
    pub(crate) fn shared(path: &Path) -> Result<Self> {
        if !path.is_absolute() {
            return Err(unavailable());
        }
        Self::open_inner(path, false, false)
    }
    fn open_inner(path: &Path, private: bool, exact: bool) -> Result<Self> {
        let fd = OpenOptions::new()
            .read(true)
            .custom_flags(
                (rustix::fs::OFlags::DIRECTORY | rustix::fs::OFlags::NOFOLLOW).bits() as i32,
            )
            .open(path)
            .map_err(|_| unavailable())?;
        let metadata = fd.metadata().map_err(|_| unavailable())?;
        if !metadata.is_dir() || private && !valid(&metadata, true, exact) {
            return Err(unavailable());
        }
        let value = Self {
            path: path.into(),
            id: FileId::of(&metadata),
            fd,
            private,
            exact,
        };
        value.current()?;
        Ok(value)
    }
    pub fn current(&self) -> Result<()> {
        let metadata = fs::symlink_metadata(&self.path).map_err(|_| unavailable())?;
        let held = self.fd.metadata().map_err(|_| unavailable())?;
        if !metadata.is_dir()
            || self.private && !valid(&metadata, true, self.exact)
            || FileId::of(&metadata) != self.id
            || FileId::of(&held) != self.id
        {
            return Err(unavailable());
        }
        Ok(())
    }
    pub fn sync(&self) -> Result<()> {
        self.current()?;
        #[cfg(test)]
        crate::staging::tests::durability::before_sync(&self.path)?;
        self.fd.sync_all().map_err(|_| unavailable())
    }
    pub fn create_record(&self, name: &OsStr) -> Result<File> {
        self.current()?;
        let fd = rustix::fs::openat(
            &self.fd,
            name,
            rustix::fs::OFlags::CREATE
                | rustix::fs::OFlags::EXCL
                // Genesis transfers this original locked description into its
                // verified reader/writer; reopening would split lock ownership.
                | rustix::fs::OFlags::RDWR
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NOFOLLOW,
            rustix::fs::Mode::from_raw_mode(0o600),
        )
        .map_err(|_| unavailable())?;
        self.current()?;
        Ok(File::from(fd))
    }
    pub fn publish_new(&self, staged: &OsStr, target: &OsStr) -> Result<()> {
        self.publish_new_io(staged, target)
            .map_err(|_| unavailable())
    }
    pub fn publish_new_io(&self, staged: &OsStr, target: &OsStr) -> std::io::Result<()> {
        self.current().map_err(std::io::Error::other)?;
        // Both names are relative to the already-held original parent, so an
        // ancestor substitution cannot publish into a foreign directory.
        rustix::fs::renameat_with(
            &self.fd,
            staged,
            &self.fd,
            target,
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .map_err(std::io::Error::from)?;
        self.current().map_err(std::io::Error::other)
    }
    pub fn parent_id(&self) -> Result<FileId> {
        Self::parent_of(&self.path)
    }
    pub fn parent_of(path: &Path) -> Result<FileId> {
        let path = path
            .parent()
            .ok_or_else(unavailable)?
            .canonicalize()
            .map_err(|_| unavailable())?;
        Ok(Self::open_inner(&path, false, false)?.id)
    }
    pub fn absent(path: &Path) -> Result<bool> {
        match fs::symlink_metadata(path) {
            Ok(_) => Ok(false),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
            Err(_) => Err(unavailable()),
        }
    }
    pub fn read(&self, name: &OsStr) -> Result<Option<Snapshot>> {
        self.read_bound(name, 65_536)
    }
    pub fn read_bound(&self, name: &OsStr, limit: usize) -> Result<Option<Snapshot>> {
        if limit == 0 || limit > 524_288 {
            return Err(unavailable());
        }
        self.current()?;
        let fd = match rustix::fs::openat(
            &self.fd,
            name,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NONBLOCK,
            rustix::fs::Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(_) => return Err(unavailable()),
        };
        let file = File::from(fd);
        let before = file.metadata().map_err(|_| unavailable())?;
        if !valid(&before, false, self.exact) || before.len() > limit as u64 {
            return Err(unavailable());
        }
        let mut bytes = Vec::new();
        (&file)
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| unavailable())?;
        let after = file.metadata().map_err(|_| unavailable())?;
        self.current()?;
        let path = fs::symlink_metadata(self.path.join(name)).map_err(|_| unavailable())?;
        let stamp = |m: &Metadata| {
            (
                FileId::of(m),
                m.len(),
                m.ctime(),
                m.ctime_nsec(),
                m.mtime(),
                m.mtime_nsec(),
            )
        };
        if !valid(&after, false, self.exact)
            || !valid(&path, false, self.exact)
            || stamp(&before) != stamp(&after)
            || stamp(&after) != stamp(&path)
            || bytes.len() as u64 != after.len()
        {
            return Err(unavailable());
        }
        Ok(Some(Snapshot {
            id: FileId::of(&after),
            changed: (after.ctime(), after.ctime_nsec()),
            modified: (after.mtime(), after.mtime_nsec()),
            bytes,
        }))
    }
    pub fn matches(&self, name: &OsStr, snapshot: &Snapshot) -> Result<bool> {
        Ok(self.read_bound(name, snapshot.bytes.len().max(1))?.as_ref() == Some(snapshot))
    }
    pub fn lock_record(&self, name: &OsStr, snapshot: &Snapshot) -> Result<ScopedLock> {
        self.current()?;
        let fd = rustix::fs::openat(
            &self.fd,
            name,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NONBLOCK,
            rustix::fs::Mode::empty(),
        )
        .map_err(|_| unavailable())?;
        let file = File::from(fd);
        let metadata = file.metadata().map_err(|_| unavailable())?;
        if !valid(&metadata, false, self.exact) || FileId::of(&metadata) != snapshot.id {
            return Err(unavailable());
        }
        // A waiter on a superseded inode must recheck the pathname after taking
        // this nonblocking lock. No old-inode lock can authorize a successor.
        fs2::FileExt::try_lock_exclusive(&file).map_err(|_| unavailable())?;
        let file = ScopedLock::acquired(file);
        if !self.matches(name, snapshot)? {
            return Err(unavailable());
        }
        Ok(file)
    }
    pub fn replace_record(
        &self,
        staged: &OsStr,
        target: &OsStr,
        original: &Snapshot,
    ) -> Result<()> {
        if !self.matches(target, original)? {
            return Err(unavailable());
        }
        self.current()?;
        rustix::fs::renameat(&self.fd, staged, &self.fd, target).map_err(|_| unavailable())?;
        self.sync()
    }
    pub fn remove_record(&self, name: &OsStr, snapshot: &Snapshot) -> Result<bool> {
        if !self.matches(name, snapshot)? {
            return Ok(false);
        }
        self.current()?;
        // The held FD prevents a directory substitution from redirecting unlink.
        rustix::fs::unlinkat(&self.fd, name, rustix::fs::AtFlags::empty())
            .map_err(|_| unavailable())?;
        self.sync()?;
        Ok(true)
    }
    // Stat field widths differ between Darwin/libc and Linux's raw syscall ABI.
    #[allow(clippy::unnecessary_cast)]
    pub fn socket(&self, name: &OsStr) -> Result<Option<SocketId>> {
        self.current()?;
        let stat = match rustix::fs::statat(&self.fd, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => stat,
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(_) => return Err(unavailable()),
        };
        if rustix::fs::FileType::from_raw_mode(stat.st_mode) != rustix::fs::FileType::Socket
            || stat.st_uid != rustix::process::geteuid().as_raw()
            || stat.st_mode & 0o777 != 0o600
            || stat.st_nlink != 1
        {
            return Err(unavailable());
        }
        self.current()?;
        Ok(Some(SocketId {
            id: FileId(stat.st_dev as u64, stat.st_ino as u64),
            changed: (stat.st_ctime as i64, stat.st_ctime_nsec as i64),
        }))
    }
    pub fn remove_socket(&self, name: &OsStr, original: &SocketId) -> Result<bool> {
        match self.socket(name)? {
            None => return Ok(false),
            Some(now) if &now == original => {}
            _ => return Err(unavailable()),
        }
        self.current()?;
        rustix::fs::unlinkat(&self.fd, name, rustix::fs::AtFlags::empty())
            .map_err(|_| unavailable())?;
        self.sync()?;
        Ok(true)
    }
    pub fn remove_empty(&self, expected_parent: FileId) -> Result<()> {
        self.current()?;
        let parent = self
            .path
            .parent()
            .ok_or_else(unavailable)?
            .canonicalize()
            .map_err(|_| unavailable())?;
        let parent = Self::open_inner(&parent, false, false)?;
        if parent.id != expected_parent {
            return Err(unavailable());
        }
        self.current()?;
        rustix::fs::unlinkat(
            &parent.fd,
            self.path.file_name().ok_or_else(unavailable)?,
            rustix::fs::AtFlags::REMOVEDIR,
        )
        .map_err(|_| unavailable())?;
        parent.sync()
    }
}
