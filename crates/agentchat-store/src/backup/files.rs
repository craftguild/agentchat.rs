//! Private staging and create-new publication of a closed, validated database.

use super::{BackupProgress, BackupStage};
use crate::error::io;
use agentchat_core::{Error, ErrorCode, Result};
use std::{
    fs::{self, DirBuilder, File},
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
};
use uuid::Uuid;

pub(super) struct Stage {
    pub directory: PathBuf,
    pub destination: PathBuf,
}
impl Stage {
    pub fn create(destination: &Path) -> Result<Self> {
        if fs::symlink_metadata(destination).is_ok() {
            return Err(Error::new(
                ErrorCode::ConfigurationConflict,
                "Backup destination already exists; it was preserved.",
            ));
        }
        let parent = destination
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let parent = parent.canonicalize().map_err(io)?;
        let name = destination.file_name().ok_or_else(|| {
            Error::new(
                ErrorCode::InvalidArgument,
                "Backup destination requires a filename.",
            )
        })?;
        let directory = parent.join(format!(".agentchat-backup-{}", Uuid::new_v4()));
        DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .map_err(io)?;
        Ok(Self {
            directory,
            destination: parent.join(name),
        })
    }
    pub fn database(&self) -> PathBuf {
        self.directory.join("agentchat.sqlite3")
    }
    pub fn publish(&self, progress: &dyn BackupProgress) -> Result<()> {
        File::open(self.database())
            .map_err(io)?
            .sync_all()
            .map_err(io)?;
        progress.reached(BackupStage::FileSynced);
        fs::hard_link(self.database(), &self.destination).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                Error::new(
                    ErrorCode::ConfigurationConflict,
                    "Backup destination was concurrently created; it was preserved.",
                )
            } else {
                io(error)
            }
        })?;
        progress.reached(BackupStage::Linked);
        File::open(self.destination.parent().expect("canonical parent"))
            .map_err(io)?
            .sync_all()
            .map_err(io)?;
        progress.reached(BackupStage::DirectorySynced);
        Ok(())
    }
}
impl Drop for Stage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}
