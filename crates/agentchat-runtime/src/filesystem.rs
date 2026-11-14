//! Private same-user runtime metadata, separate from configuration merge helpers.
mod staged;

use agentchat_core::{Error, ErrorCode, Result};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    fs::{self, DirBuilder, File},
    io::Read,
    os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt},
    path::Path,
};

pub(crate) fn error(message: &str) -> Error {
    Error::new(ErrorCode::ConfigurationConflict, message)
}
pub(crate) fn io(_: std::io::Error) -> Error {
    Error::new(ErrorCode::IoError, "Runtime filesystem operation failed.")
}
pub(crate) fn private(path: &Path, directory: bool) -> Result<()> {
    let meta = fs::symlink_metadata(path).map_err(io)?;
    let mode = if directory { 0o700 } else { 0o600 };
    if meta.file_type().is_symlink()
        || meta.uid() != rustix::process::geteuid().as_raw()
        || meta.permissions().mode() & 0o777 != mode
        || if directory {
            !meta.is_dir()
        } else {
            !meta.is_file()
        }
    {
        return Err(error(
            "Runtime metadata is not private, owned and of the expected type.",
        ));
    }
    Ok(())
}
pub(crate) fn directory(path: &Path) -> Result<()> {
    match DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(io(error)),
    }
    private(path, true)?;
    File::open(path).map_err(io)?.sync_all().map_err(io)?;
    if let Some(parent) = path.parent() {
        File::open(parent).map_err(io)?.sync_all().map_err(io)?;
    }
    Ok(())
}
pub(crate) fn read<T: DeserializeOwned>(path: &Path, limit: usize) -> Result<T> {
    private(path, false)?;
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(io)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(io)?;
    if bytes.len() > limit {
        return Err(error("Runtime descriptor exceeds its bounded size."));
    }
    serde_json::from_slice(&bytes).map_err(|_| error("Runtime descriptor has an invalid format."))
}
pub(crate) fn write_new<T: Serialize>(
    home: &Path,
    path: &Path,
    value: &T,
    limit: usize,
) -> Result<()> {
    let bytes =
        serde_json::to_vec(value).map_err(|_| error("Runtime descriptor cannot be serialized."))?;
    if bytes.len() > limit {
        return Err(error("Runtime descriptor exceeds its bounded size."));
    }
    let parent = path
        .parent()
        .ok_or_else(|| error("Runtime descriptor has no parent."))?;
    let name = path
        .file_name()
        .ok_or_else(|| error("Runtime descriptor has no filename."))?;
    let mut stage = staged::Staged::prepare(home, parent, name, &bytes).map_err(io)?;
    #[cfg(test)]
    tests::cut("prepared", &stage.path(), path);
    stage.publish(name).map_err(io)?;
    #[cfg(test)]
    tests::cut("published", &stage.path(), path);
    stage.sync().map_err(io)
}

#[cfg(test)]
#[path = "../tests/unit/context_publication.rs"]
mod tests;
