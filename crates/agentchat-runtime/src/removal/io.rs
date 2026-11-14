//! No-follow parent handles and bounded private removal journal publication.

use super::{conflict, io};
use agentchat_core::Result;
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Component, Path},
};
use uuid::Uuid;

pub(super) const LIMIT: usize = 32 * 1024 * 1024;

pub(super) struct Directory {
    path: std::path::PathBuf,
    file: File,
    id: (u64, u64),
}
impl Directory {
    pub fn open(path: &Path) -> Result<Self> {
        checked(path)?;
        if path.canonicalize().map_err(io)? != path {
            return Err(conflict("Removal parent was redirected."));
        }
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(
                (rustix::fs::OFlags::DIRECTORY | rustix::fs::OFlags::NOFOLLOW).bits() as i32,
            )
            .open(path)
            .map_err(io)?;
        let meta = file.metadata().map_err(io)?;
        let value = Self {
            path: path.into(),
            file,
            id: (meta.dev(), meta.ino()),
        };
        value.current()?;
        Ok(value)
    }
    pub fn current(&self) -> Result<()> {
        let meta = fs::symlink_metadata(&self.path).map_err(io)?;
        if !meta.is_dir()
            || meta.file_type().is_symlink()
            || (meta.dev(), meta.ino()) != self.id
            || self.path.canonicalize().map_err(io)? != self.path
        {
            return Err(conflict(
                "Removal parent identity changed; remaining data was preserved.",
            ));
        }
        Ok(())
    }
    pub fn sync(&self) -> Result<()> {
        self.current()?;
        self.file.sync_all().map_err(io)
    }
    pub fn unlink(&self, name: &std::ffi::OsStr, directory: bool) -> Result<()> {
        self.current()?;
        let flags = if directory {
            rustix::fs::AtFlags::REMOVEDIR
        } else {
            rustix::fs::AtFlags::empty()
        };
        match rustix::fs::unlinkat(&self.file, name, flags) {
            Ok(()) => self.sync(),
            Err(rustix::io::Errno::NOENT) => Ok(()),
            Err(e) => Err(io(e.into())),
        }
    }
}

pub(super) fn checked(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path.as_os_str().as_encoded_bytes().contains(&0)
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(conflict(
            "Removal paths must be normalized absolute native paths.",
        ));
    }
    Ok(())
}
pub(super) fn private_dir(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| conflict("Missing recovery parent."))?;
    let held = Directory::open(parent)?;
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => (),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(e) => return Err(io(e)),
    }
    let meta = fs::symlink_metadata(path).map_err(io)?;
    if !meta.is_dir()
        || meta.file_type().is_symlink()
        || meta.uid() != rustix::process::geteuid().as_raw()
        || meta.mode() & 0o077 != 0
    {
        return Err(conflict(
            "Recovery metadata directory is not private and owned.",
        ));
    }
    Directory::open(path)?.sync()?;
    held.sync()
}

pub(super) fn read(path: &Path) -> Result<Option<Vec<u8>>> {
    let parent = path
        .parent()
        .ok_or_else(|| conflict("Missing journal parent."))?;
    if !parent.try_exists().map_err(io)? {
        return Ok(None);
    }
    let held = Directory::open(parent)?;
    let parent_meta = fs::symlink_metadata(parent).map_err(io)?;
    if parent_meta.uid() != rustix::process::geteuid().as_raw() || parent_meta.mode() & 0o077 != 0 {
        return Err(conflict("Removal journal parent is not private and owned."));
    }
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io(e)),
    };
    let meta = file.metadata().map_err(io)?;
    if !meta.is_file()
        || meta.nlink() != 1
        || meta.uid() != rustix::process::geteuid().as_raw()
        || meta.mode() & 0o077 != 0
        || meta.len() > LIMIT as u64
    {
        return Err(conflict(
            "Removal journal must be a bounded private owned regular file.",
        ));
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(LIMIT as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(io)?;
    let after = file.metadata().map_err(io)?;
    let current = fs::symlink_metadata(path).map_err(io)?;
    if after.nlink() != 1
        || current.nlink() != 1
        || after.mode() != meta.mode()
        || current.mode() != meta.mode()
        || after.uid() != meta.uid()
        || current.uid() != meta.uid()
    {
        return Err(conflict(
            "Removal journal ownership or permissions changed during its read.",
        ));
    }
    if bytes.len() > LIMIT
        || (
            after.dev(),
            after.ino(),
            after.len(),
            after.mtime(),
            after.mtime_nsec(),
        ) != (
            meta.dev(),
            meta.ino(),
            meta.len(),
            meta.mtime(),
            meta.mtime_nsec(),
        )
        || (current.dev(), current.ino()) != (meta.dev(), meta.ino())
    {
        return Err(conflict("Removal journal changed during its bounded read."));
    }
    held.current()?;
    Ok(Some(bytes))
}

pub(super) fn write(path: &Path, bytes: &[u8], expected: Option<&[u8]>) -> Result<()> {
    if bytes.len() > LIMIT {
        return Err(conflict("Removal journal exceeds 32 MiB."));
    }
    let parent = Directory::open(
        path.parent()
            .ok_or_else(|| conflict("Missing journal parent."))?,
    )?;
    if read(path)?.as_deref() != expected {
        return Err(conflict("Removal recovery metadata changed externally."));
    }
    let stage = format!(".agentchat-removal-{}", Uuid::new_v4());
    let fd = rustix::fs::openat(
        &parent.file,
        stage.as_str(),
        rustix::fs::OFlags::WRONLY
            | rustix::fs::OFlags::CREATE
            | rustix::fs::OFlags::EXCL
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NOFOLLOW,
        rustix::fs::Mode::from_raw_mode(0o600),
    )
    .map_err(|e| io(e.into()))?;
    let mut file = File::from(fd);
    let result = (|| {
        file.write_all(bytes).map_err(io)?;
        file.sync_all().map_err(io)?;
        if read(path)?.as_deref() != expected {
            return Err(conflict(
                "Removal recovery metadata changed before publication.",
            ));
        }
        parent.current()?;
        rustix::fs::renameat(
            &parent.file,
            stage.as_str(),
            &parent.file,
            path.file_name()
                .ok_or_else(|| conflict("Missing journal filename."))?,
        )
        .map_err(|e| io(e.into()))?;
        parent.sync()
    })();
    if result.is_err() {
        let _ = parent.unlink(stage.as_ref(), false);
    }
    result
}

pub(super) fn clear(path: &Path, expected: &[u8]) -> Result<()> {
    if read(path)?.as_deref() != Some(expected) {
        return Err(conflict("Removal intent changed before completion."));
    }
    Directory::open(
        path.parent()
            .ok_or_else(|| conflict("Missing journal parent."))?,
    )?
    .unlink(
        path.file_name()
            .ok_or_else(|| conflict("Missing journal filename."))?,
        false,
    )
}
