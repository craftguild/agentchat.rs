//! External lifetime leases cover runtime loss and unjoined managed/viewer owners.

use super::{Fence, Lifecycle, conflict, io, private_directory, validate_private};
use crate::lock::ScopedLock;
use agentchat_core::{Error, ErrorCode, Result};
use std::{
    fs::{self, OpenOptions},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    time::{Duration, Instant},
};
use uuid::Uuid;

/// OS-held project activity, retained for the complete owning process/session
/// lifetime. Its existence alone is never activity; only a conflicting lock is.
pub struct Activity {
    _fence: Fence,
}

impl Lifecycle {
    /// Recorded exact UUIDs locate possibly stale endpoints after state loss.
    /// Record existence grants no project binding or live-session authority.
    pub fn recorded_projects(&self, root_fence: &Fence) -> Result<Vec<Uuid>> {
        root_fence.require_exclusive(&self.root)?;
        let directory = self.directory.join("active");
        match fs::symlink_metadata(&directory) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io(e)),
            Ok(meta) => validate_private(&meta, true)?,
        }
        if directory.canonicalize().map_err(io)? != directory {
            return Err(conflict("Project activity directory was redirected."));
        }
        let mut ids = Vec::new();
        for (index, entry) in fs::read_dir(&directory).map_err(io)?.enumerate() {
            if index >= 4096 {
                return Err(conflict("Global endpoint inventory exceeds its bound."));
            }
            let entry = entry.map_err(io)?;
            let id = entry
                .file_name()
                .to_str()
                .and_then(|s| s.strip_suffix(".lock"))
                .and_then(|s| Uuid::parse_str(s).ok())
                .filter(|id| !id.is_nil())
                .ok_or_else(|| conflict("Project activity has an unknown record name."))?;
            validate_private(&fs::symlink_metadata(entry.path()).map_err(io)?, false)?;
            ids.push(id);
        }
        ids.sort();
        Ok(ids)
    }
    /// Claim activity before validating/opening data or starting a managed host.
    /// A removal cannot pass between startup coordination and lifetime ownership.
    pub fn activity(&self, project: Uuid) -> Result<Activity> {
        if project.is_nil() {
            return Err(conflict("Project activity requires a UUID."));
        }
        let _startup = self.shared()?;
        let directory = self.directory.join("active");
        private_directory(&directory)?;
        validate_private(&fs::symlink_metadata(&directory).map_err(io)?, true)?;
        Ok(Activity {
            _fence: lock(&directory.join(format!("{project}.lock")), false, true)?,
        })
    }

    /// Require relevant activity to be quiescent while holding the exclusive
    /// root fence. Retain these returned locks until destructive work finishes.
    /// No native process enumeration or executable-name inference occurs here.
    pub fn quiescent(&self, root_fence: &Fence, project: Option<Uuid>) -> Result<Vec<Fence>> {
        root_fence.validate()?;
        if !root_fence.exclusive || root_fence.path != self.directory.join("fence.lock") {
            return Err(conflict(
                "Quiescence requires this root's exclusive lifecycle fence.",
            ));
        }
        let directory = self.directory.join("active");
        match fs::symlink_metadata(&directory) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io(e)),
            Ok(meta) => validate_private(&meta, true)?,
        }
        if directory.canonicalize().map_err(io)? != directory {
            return Err(conflict("Project activity directory was redirected."));
        }
        if let Some(project) = project {
            let path = directory.join(format!("{project}.lock"));
            return match fs::symlink_metadata(&path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
                Err(e) => Err(io(e)),
                Ok(_) => Ok(vec![lock(&path, true, false)?]),
            };
        }
        let mut held = Vec::new();
        for (index, entry) in fs::read_dir(&directory).map_err(io)?.enumerate() {
            if index >= 65_536 {
                return Err(conflict("Project activity inventory exceeds its bound."));
            }
            let entry = entry.map_err(io)?;
            let name = entry.file_name();
            let id = name
                .to_str()
                .and_then(|s| s.strip_suffix(".lock"))
                .and_then(|s| Uuid::parse_str(s).ok())
                .ok_or_else(|| conflict("Project activity has an unknown record name."))?;
            if id.is_nil() {
                return Err(conflict("Project activity has a nil record identity."));
            }
            held.push(lock(&entry.path(), true, false)?);
        }
        Ok(held)
    }
}

fn lock(path: &Path, exclusive: bool, create: bool) -> Result<Fence> {
    let file = OpenOptions::new()
        .create(create)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)
        .map_err(io)?;
    validate_private(&file.metadata().map_err(io)?, false)?;
    let deadline = Instant::now()
        + if exclusive {
            Duration::ZERO
        } else {
            Duration::from_secs(5)
        };
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
                        "A relevant AgentChat runtime, viewer or managed launch is active; close it normally and retry uninstall.",
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
        path: path.to_owned(),
        exclusive,
    };
    fence.validate()?;
    Ok(fence)
}
