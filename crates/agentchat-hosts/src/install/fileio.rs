//! Bounded regular-file access, durable atomic publication and an OS-held lock.
use super::{conflict, digest, io};
use agentchat_core::{Error, ErrorCode, Result};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Component, Path, PathBuf},
    time::{Duration, Instant},
};

pub(super) const FILE_LIMIT: usize = 1024 * 1024;
pub(super) const JOURNAL_LIMIT: usize = 32 * 1024 * 1024;
pub(super) fn native(path: &Path) -> Vec<u8> {
    path.as_os_str().as_bytes().to_vec()
}
pub(super) fn path(bytes: &[u8]) -> Result<PathBuf> {
    if bytes.len() > 16384 || bytes.contains(&0) {
        return Err(conflict("Invalid integration native path."));
    }
    let path = PathBuf::from(std::ffi::OsString::from_vec(bytes.to_vec()));
    checked(&path)?;
    Ok(path)
}
pub(super) fn checked(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(conflict(
            "Integration paths must be normalized and absolute.",
        ));
    }
    Ok(())
}
pub(super) fn absolute(path: &Path) -> Result<PathBuf> {
    checked(path)?;
    // Resolve only the explicitly selected root's existing ancestor. Symlinks
    // inside an allowed root are rejected separately, never followed on writes.
    let mut ancestor = path;
    let mut tail = Vec::new();
    while !ancestor.try_exists().map_err(io)? {
        tail.push(
            ancestor
                .file_name()
                .ok_or_else(|| conflict("Invalid integration root."))?
                .to_owned(),
        );
        ancestor = ancestor
            .parent()
            .ok_or_else(|| conflict("Missing integration root ancestor."))?;
    }
    let mut result = ancestor.canonicalize().map_err(io)?;
    for name in tail.into_iter().rev() {
        result.push(name);
    }
    Ok(result)
}
pub(super) fn regular(path: &Path) -> Result<Option<std::fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => Ok(Some(meta)),
        Ok(_) => Err(conflict(
            "Integration destination must be a regular file, not a symbolic link.",
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io(e)),
    }
}
pub(super) fn safe_parents(path: &Path) -> Result<()> {
    checked(path)?;
    let mut cursor = PathBuf::new();
    for c in path
        .parent()
        .ok_or_else(|| conflict("Integration destination has no parent."))?
        .components()
    {
        cursor.push(c);
        match fs::symlink_metadata(&cursor) {
            Ok(m) if m.is_dir() && !m.file_type().is_symlink() => {}
            Ok(_) => {
                return Err(conflict(
                    "Integration parent contains a symbolic link or foreign file.",
                ));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io(e)),
        }
    }
    Ok(())
}
pub(super) fn read(path: &Path, limit: usize) -> Result<Option<String>> {
    safe_parents(path)?;
    let Some(meta) = regular(path)? else {
        return Ok(None);
    };
    if meta.len() > limit as u64 {
        return Err(conflict("Integration file exceeds its bounded size."));
    }
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(io)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(io)?;
    if bytes.len() > limit {
        return Err(conflict("Integration file grew past its bounded size."));
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| conflict("Integration file is not valid UTF-8."))
}
pub(super) fn directory(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {
            sync_dir(path)?;
            if let Some(p) = path.parent() {
                sync_dir(p)?;
            }
            Ok(())
        }
        Ok(_) => Err(conflict(
            "Integration directory has a foreign type or symbolic link.",
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let parent = path
                .parent()
                .ok_or_else(|| conflict("Integration directory has no parent."))?;
            directory(parent)?;
            match fs::DirBuilder::new().mode(0o700).create(path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return directory(path),
                Err(e) => return Err(io(e)),
            }
            sync_dir(path)?;
            sync_dir(parent)
        }
        Err(e) => Err(io(e)),
    }
}
pub(super) fn sync_dir(path: &Path) -> Result<()> {
    File::open(path).map_err(io)?.sync_all().map_err(io)
}
pub(super) fn durable(path: &Path) -> Result<()> {
    File::open(path).map_err(io)?.sync_all().map_err(io)?;
    sync_dir(path.parent().ok_or_else(|| conflict("Missing parent."))?)
}
pub(super) fn atomic(path: &Path, text: &str, mode: u32) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| conflict("Missing integration parent."))?;
    let stage = parent.join(format!(".agentchat-state-{}", digest(&native(path))));
    publish(path, &stage, text, mode, None)
}
pub(super) fn publish(
    path: &Path,
    stage: &Path,
    text: &str,
    mode: u32,
    expected: Option<&Option<String>>,
) -> Result<()> {
    safe_parents(path)?;
    regular(path)?;
    let parent = path
        .parent()
        .ok_or_else(|| conflict("Missing integration parent."))?;
    directory(parent)?;
    remove(stage)?;
    let previous = read(path, JOURNAL_LIMIT)?;
    if let Some(expected) = expected {
        if &previous != expected && previous.as_deref() != Some(text) {
            return Err(conflict("Host configuration changed before publication."));
        }
        if previous.is_some() && self::mode(path)? != mode {
            return Err(conflict(
                "Host configuration permissions changed before publication.",
            ));
        }
    }
    if previous.as_deref() == Some(text) {
        return durable(path);
    }
    let result = (|| {
        let mut staged = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(stage)
            .map_err(io)?;
        staged.write_all(text.as_bytes()).map_err(io)?;
        staged
            .set_permissions(fs::Permissions::from_mode(mode & 0o777))
            .map_err(io)?;
        staged.sync_all().map_err(io)?;
        // A noncooperating host can edit between the whole-plan validation and
        // this write. Check again immediately before rename; the installation
        // lock only serializes agentchat writers, not arbitrary external applications.
        if expected.is_some()
            && (read(path, JOURNAL_LIMIT)? != previous || self::mode(path)? != mode)
        {
            return Err(conflict(
                "Host configuration changed while its replacement was staged.",
            ));
        }
        fs::rename(stage, path).map_err(io)?;
        sync_dir(parent)
    })();
    if result.is_err() {
        let _ = remove(stage);
    }
    result
}
pub(super) fn remove(path: &Path) -> Result<()> {
    regular(path)?;
    match fs::remove_file(path) {
        Ok(()) => sync_dir(
            path.parent()
                .ok_or_else(|| conflict("Missing integration parent."))?,
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io(e)),
    }
}
pub(super) fn mode(path: &Path) -> Result<u32> {
    Ok(regular(path)?.map_or(0o600, |m| m.permissions().mode() & 0o777))
}
pub(super) fn lock(state: &Path) -> Result<crate::lock::ScopedLock> {
    directory(state)?;
    let name = state.join("install.lock");
    regular(&name)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(name)
        .map_err(io)?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match fs2::FileExt::try_lock_exclusive(&file) {
            Ok(()) => return Ok(crate::lock::ScopedLock::acquired(file)),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    let mut e = Error::new(
                        ErrorCode::Busy,
                        "Another integration installation holds the OS lock.",
                    );
                    e.retryable = true;
                    return Err(e);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => return Err(io(e)),
        }
    }
}
