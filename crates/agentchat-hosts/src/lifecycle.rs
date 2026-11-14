//! Stable filesystem coordination outside the state tree that removal retires.

use crate::lock::ScopedLock;
use agentchat_core::{Error, ErrorCode, Result};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Component, Path, PathBuf},
    time::{Duration, Instant},
};
mod activity;
pub use activity::Activity;

/// Canonical selected state root and its private surviving recovery directory.
/// The path hash locates coordination only; it never authorizes project data.
#[derive(Clone, Debug)]
pub struct Lifecycle {
    root: PathBuf,
    directory: PathBuf,
}

/// An OS-held fence. Never unlink its file or directory during removal.
pub struct Fence {
    file: ScopedLock,
    path: PathBuf,
    exclusive: bool,
}

impl Lifecycle {
    /// Resolve a stable sibling without creating state or recovery metadata.
    pub fn new(root: &Path) -> Result<Self> {
        if !root.is_absolute()
            || root.as_os_str().as_bytes().contains(&0)
            || root.components().any(|c| matches!(c, Component::ParentDir))
        {
            return Err(conflict(
                "Lifecycle state root must be an absolute normalized path.",
            ));
        }
        let mut ancestor = root;
        let mut tail = Vec::new();
        loop {
            match fs::symlink_metadata(ancestor) {
                Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => break,
                Ok(_) => {
                    return Err(conflict(
                        "Lifecycle state root has a foreign filesystem type.",
                    ));
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    tail.push(
                        ancestor
                            .file_name()
                            .ok_or_else(|| conflict("Invalid state root."))?
                            .to_owned(),
                    );
                    ancestor = ancestor
                        .parent()
                        .ok_or_else(|| conflict("Invalid state root."))?;
                }
                Err(e) => return Err(io(e)),
            }
        }
        let mut root = ancestor.canonicalize().map_err(io)?;
        root.extend(tail.into_iter().rev());
        let parent = root
            .parent()
            .ok_or_else(|| conflict("Filesystem root cannot hold AgentChat state."))?;
        let key = Sha256::digest(root.as_os_str().as_bytes());
        let key = key.iter().map(|b| format!("{b:02x}")).collect::<String>();
        let directory = parent.join(format!(".agentchat-lifecycle-{key}"));
        Ok(Self { root, directory })
    }

    /// Selected canonical root, never used to infer a conversation identity.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Private location for journals, retired request keys and configuration backups.
    /// It survives deleting/recreating `root`; it is deliberately not history backup.
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Fence an install or startup against removal. Existing finer-grained
    /// installer/project locks retain their ordering inside this shared fence.
    pub fn shared(&self) -> Result<Fence> {
        self.acquire(false, Duration::from_secs(5))
    }

    /// Fence removal against all installs/startups, with bounded contention.
    pub fn exclusive(&self) -> Result<Fence> {
        self.acquire(true, Duration::from_secs(5))
    }

    /// Select a shorter bounded wait, useful to callers with an existing deadline.
    pub fn acquire(&self, exclusive: bool, wait: Duration) -> Result<Fence> {
        let current = Self::new(&self.root)?;
        if current.root != self.root || current.directory != self.directory {
            return Err(conflict("Captured lifecycle root was redirected."));
        }
        if wait > Duration::from_secs(5) {
            return Err(conflict("Lifecycle wait exceeds five seconds."));
        }
        private_directory(&self.directory)?;
        let path = self.directory.join("fence.lock");
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(&path)
            .map_err(io)?;
        validate_private(&file.metadata().map_err(io)?, false)?;
        let deadline = Instant::now() + wait;
        loop {
            let result = if exclusive {
                fs2::FileExt::try_lock_exclusive(&file)
            } else {
                fs2::FileExt::try_lock_shared(&file)
            };
            match result {
                Ok(()) => break,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        let mut error = Error::new(
                            ErrorCode::Busy,
                            "Another AgentChat install, removal or startup owns the lifecycle fence; finish it and retry.",
                        );
                        error.retryable = true;
                        return Err(error);
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => return Err(io(e)),
            }
        }
        let fence = Fence {
            file: ScopedLock::acquired(file),
            path,
            exclusive,
        };
        fence.validate()?;
        if !exclusive {
            match fs::symlink_metadata(self.directory.join("pending-removal.json")) {
                Ok(_) => {
                    let mut error = conflict(
                        "An unfinished uninstall requires recovery. Retry its original scope and request ID before install or startup.",
                    );
                    error.details = serde_json::json!({"reason":"removal_recovery_required", "recovery_directory":agentchat_protocol::removal::RemovalPath::new(&self.directory)});
                    return Err(error);
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(e) => return Err(io(e)),
            }
        }
        File::open(&self.directory)
            .map_err(io)?
            .sync_all()
            .map_err(io)?;
        File::open(self.directory.parent().expect("sibling parent"))
            .map_err(io)?
            .sync_all()
            .map_err(io)?;
        Ok(fence)
    }
}

impl Fence {
    /// Validate that a host cleanup is inside the selected root's exclusive
    /// removal critical section. A shared or another root's fence is refused.
    pub fn require_exclusive(&self, root: &Path) -> Result<()> {
        let lifecycle = Lifecycle::new(root)?;
        if lifecycle.root() != root
            || !self.exclusive
            || self.path != lifecycle.directory().join("fence.lock")
        {
            return Err(conflict(
                "Host removal requires the selected root's exclusive lifecycle fence.",
            ));
        }
        self.validate()
    }
    /// Recheck that publication still holds the originally acquired inode.
    pub fn validate(&self) -> Result<()> {
        let current = fs::symlink_metadata(&self.path).map_err(io)?;
        validate_private(&current, false)?;
        let held = self.file.metadata().map_err(io)?;
        if (held.dev(), held.ino()) != (current.dev(), current.ino()) {
            return Err(conflict(
                "Lifecycle fence identity changed; no removal is authorized.",
            ));
        }
        let directory = self.path.parent().expect("fence parent");
        validate_private(&fs::symlink_metadata(directory).map_err(io)?, true)?;
        if directory.canonicalize().map_err(io)? != directory {
            return Err(conflict("Lifecycle recovery directory was redirected."));
        }
        Ok(())
    }
}

fn private_directory(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            if !meta.is_dir() || meta.file_type().is_symlink() {
                return Err(conflict("Lifecycle parent is not a regular directory."));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            private_directory(
                path.parent()
                    .ok_or_else(|| conflict("Missing lifecycle parent."))?,
            )?;
            match fs::DirBuilder::new().mode(0o700).create(path) {
                Ok(()) => (),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
                Err(e) => return Err(io(e)),
            }
        }
        Err(e) => return Err(io(e)),
    }
    // Only the dedicated recovery directory requires exclusive user ownership;
    // ancestor directories such as /private/var/tmp may be shared.
    if path
        .file_name()
        .is_some_and(|n| n.as_bytes().starts_with(b".agentchat-lifecycle-"))
    {
        validate_private(&fs::symlink_metadata(path).map_err(io)?, true)?;
    }
    if path.canonicalize().map_err(io)? != path {
        return Err(conflict("Lifecycle directory contains a symbolic alias."));
    }
    Ok(())
}

fn validate_private(meta: &fs::Metadata, directory: bool) -> Result<()> {
    if meta.file_type().is_symlink()
        || meta.is_dir() != directory
        || (!directory && !meta.is_file())
        || meta.uid() != rustix::process::geteuid().as_raw()
        || meta.permissions().mode() & 0o077 != 0
    {
        return Err(conflict(
            "Lifecycle metadata must be private and owned by this OS user.",
        ));
    }
    Ok(())
}
fn conflict(message: &str) -> Error {
    Error::new(ErrorCode::ConfigurationConflict, message)
}
fn io(_: std::io::Error) -> Error {
    Error::new(ErrorCode::IoError, "Lifecycle filesystem operation failed.")
}
