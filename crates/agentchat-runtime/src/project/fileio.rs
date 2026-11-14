//! Bounded metadata reads, private creation, atomic replacement and OS ownership.

use agentchat_core::{Error, ErrorCode, Result};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    fs::{self, DirBuilder, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Component, Path, PathBuf},
    time::{Duration, Instant},
};
use uuid::Uuid;

pub(super) const METADATA_LIMIT: usize = 256 * 1024;

pub(super) fn io(error: std::io::Error) -> Error {
    Error::new(
        ErrorCode::IoError,
        format!("Project filesystem operation failed: {error}"),
    )
}
pub(super) fn conflict(message: &str) -> Error {
    Error::new(ErrorCode::ConfigurationConflict, message)
}
pub(super) fn database(path: &Path, required: bool) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(conflict("Project database is not a regular file.")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !required => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(Error::new(
            ErrorCode::IntegrityError,
            "Initialized project database is missing; refusing to recreate retained history. Use agentchat install --new-project, or agentchat uninstall followed by agentchat install.",
        )),
        Err(error) => Err(io(error)),
    }
}

pub(super) fn home(path: &Path) -> Result<PathBuf> {
    let absolute = std::path::absolute(path).map_err(io)?;
    let mut ancestor = absolute.as_path();
    while !ancestor.try_exists().map_err(io)? {
        ancestor = ancestor
            .parent()
            .ok_or_else(|| conflict("State root has no existing ancestor."))?;
    }
    let tail = absolute
        .strip_prefix(ancestor)
        .map_err(|_| conflict("State root cannot be resolved."))?;
    if tail.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(conflict(
            "State root traverses a parent of a nonexistent directory.",
        ));
    }
    // Extending components avoids appending an empty path (and thus a trailing
    // separator) once the state root exists. Configured MCP environment values
    // must stay byte-stable before/after the first initialization.
    let mut selected = ancestor.canonicalize().map_err(io)?;
    selected.extend(tail.components());
    Ok(selected)
}

pub(super) fn private_dir(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {
            // A prior process may have created this entry before its parent
            // sync. Recovery must anchor observed directories before publishing.
            sync_dir(path)?;
            if let Some(parent) = path.parent() {
                sync_dir(parent)?;
            }
            Ok(())
        }
        Ok(_) => Err(conflict(
            "Project metadata directory is not a regular directory.",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = path
                .parent()
                .ok_or_else(|| conflict("Metadata directory has no parent."))?;
            private_dir(parent)?;
            match DirBuilder::new().mode(0o700).create(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    private_dir(path)?
                }
                Err(error) => return Err(io(error)),
            }
            sync_dir(path)?;
            sync_dir(parent)
        }
        Err(error) => Err(io(error)),
    }
}

pub(super) fn bytes(path: &Path, limit: usize) -> Result<Option<Vec<u8>>> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io(error)),
    };
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err(conflict(
            "Project metadata must be a regular file, not a symbolic link.",
        ));
    }
    if meta.len() > limit as u64 {
        return Err(conflict(
            "Existing project metadata exceeds its bounded size.",
        ));
    }
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(io)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(io)?;
    if bytes.len() > limit {
        return Err(conflict("Project metadata grew beyond its bounded size."));
    }
    Ok(Some(bytes))
}
pub(super) fn read<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    bytes(path, METADATA_LIMIT)?
        .map(|data| {
            serde_json::from_slice(&data)
                .map_err(|_| conflict("Project metadata is not valid for its declared format."))
        })
        .transpose()
}
pub(super) fn write<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|_| conflict("Project metadata cannot be serialized."))?;
    bytes.push(b'\n');
    if bytes.len() > METADATA_LIMIT {
        return Err(conflict("Project metadata exceeds its bounded size."));
    }
    atomic(path, &bytes, 0o600)
}
pub(super) fn atomic(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| conflict("Metadata filename has no parent."))?;
    private_dir(parent)?;
    // A unique, exclusively created staging file never overwrites a foreign
    // collision. Recovery uses the durable journal, not an incomplete temp file.
    let staged = parent.join(format!(".agentchat-stage-{}", Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&staged)
        .map_err(io)?;
    let result = (|| {
        file.write_all(bytes).map_err(io)?;
        file.set_permissions(fs::Permissions::from_mode(mode))
            .map_err(io)?;
        file.sync_all().map_err(io)?;
        fs::rename(&staged, path).map_err(io)?;
        sync_dir(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&staged);
    }
    result
}
pub(super) fn sync_dir(path: &Path) -> Result<()> {
    File::open(path).map_err(io)?.sync_all().map_err(io)
}
pub(super) fn durable(path: &Path) -> Result<()> {
    File::open(path).map_err(io)?.sync_all().map_err(io)?;
    sync_dir(
        path.parent()
            .ok_or_else(|| conflict("Metadata filename has no parent."))?,
    )
}
pub(super) fn remove(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => sync_dir(
            path.parent()
                .ok_or_else(|| conflict("Metadata filename has no parent."))?,
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io(error)),
    }
}

pub(super) fn lock(home: &Path) -> Result<File> {
    private_dir(home)?;
    match fs::symlink_metadata(home.join("init.lock")) {
        Ok(meta) if !meta.is_file() || meta.file_type().is_symlink() => {
            return Err(conflict("Initialization lock is not a regular file."));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(io(error)),
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(home.join("init.lock"))
        .map_err(io)?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match fs2::FileExt::try_lock_exclusive(&file) {
            Ok(()) => return Ok(file),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    let mut error = Error::new(
                        ErrorCode::Busy,
                        "Another project initialization still owns the OS lock.",
                    );
                    error.retryable = true;
                    return Err(error);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(io(error)),
        }
    }
}
