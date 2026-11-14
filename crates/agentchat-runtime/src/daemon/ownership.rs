//! OS-held runtime ownership and private short endpoints, with inode-safe cleanup.

use crate::{
    filesystem as private,
    project::{Project, blocking},
};
use agentchat_core::Result;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    os::unix::{
        ffi::OsStrExt,
        fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::PathBuf,
};
use tokio::{
    net::{UnixListener, UnixStream},
    time::{Duration, timeout},
};

/// Stable short paths; hashes locate endpoints but never define project identity.
#[derive(Debug, Clone)]
pub struct RuntimePaths {
    /// Private directory owned by the current effective OS user.
    pub directory: PathBuf,
    /// Private Unix socket, kept below the macOS pathname limit.
    pub socket: PathBuf,
    /// OS-held lifetime lock inside the project's data namespace.
    pub lock: PathBuf,
}
impl RuntimePaths {
    /// Compute paths without creating files or trusting a PID/stale marker.
    pub fn for_project(project: &Project) -> Self {
        Self::for_data_directory(project.data_dir())
    }
    /// Locate an endpoint for an exact selected namespace directory. This is
    /// only a path calculation, never authority to bind a conversation/project.
    pub fn for_data_directory(data: &std::path::Path) -> Self {
        let hash = Sha256::digest(data.as_os_str().as_bytes())
            .iter()
            .take(16)
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let directory = PathBuf::from("/tmp").join(format!(
            "agentchat-{}-{hash}",
            rustix::process::geteuid().as_raw()
        ));
        Self {
            socket: directory.join("rpc.sock"),
            directory,
            lock: data.join("runtime.lock"),
        }
    }
}
pub(super) struct Ownership {
    _lock: File,
    _activity: agentchat_hosts::lifecycle::Activity,
    paths: RuntimePaths,
    socket_identity: Option<(u64, u64)>,
}
impl Ownership {
    pub async fn claim(paths: RuntimePaths, project: &Project) -> Result<Self> {
        let data = project.data_dir().to_owned();
        let value = paths.clone();
        let project_id = project.identity().id;
        let (lock, activity) = blocking(move || {
            let activity = agentchat_hosts::lifecycle::Lifecycle::new(
                data.parent().expect("project state parent"),
            )?
            .activity(project_id)?;
            private::private(&data, true)?;
            if let Ok(meta) = fs::symlink_metadata(&value.lock)
                && (!meta.is_file() || meta.file_type().is_symlink())
            {
                return Err(private::error(
                    "Runtime lock has a foreign filesystem type.",
                ));
            }
            let file = OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .mode(0o600)
                .open(&value.lock)
                .map_err(private::io)?;
            private::private(&value.lock, false)?;
            fs2::FileExt::try_lock_exclusive(&file).map_err(|error| {
                if error.kind() == std::io::ErrorKind::WouldBlock {
                    super::busy("A live runtime already owns this project.")
                } else {
                    private::io(error)
                }
            })?;
            private::directory(&value.directory)?;
            Ok((file, activity))
        })
        .await?;
        Ok(Self {
            _lock: lock,
            _activity: activity,
            paths,
            socket_identity: None,
        })
    }
    pub async fn listen(&mut self) -> Result<UnixListener> {
        let path = self.paths.socket.clone();
        let existing = blocking(move || match fs::symlink_metadata(path) {
            Ok(meta) => Ok(Some(meta)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(private::io(error)),
        })
        .await?;
        if let Some(meta) = existing {
            if !meta.file_type().is_socket() || meta.uid() != rustix::process::geteuid().as_raw() {
                return Err(private::error(
                    "Stale runtime endpoint is not an owned socket.",
                ));
            }
            match timeout(
                Duration::from_secs(1),
                UnixStream::connect(&self.paths.socket),
            )
            .await
            {
                Ok(Err(error))
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
                    ) => {}
                _ => {
                    return Err(super::busy(
                        "An active endpoint exists without matching lifetime ownership; it was preserved.",
                    ));
                }
            }
            let path = self.paths.socket.clone();
            blocking(move || match fs::symlink_metadata(&path) {
                Ok(current)
                    if current.file_type().is_socket()
                        && current.dev() == meta.dev()
                        && current.ino() == meta.ino() =>
                {
                    fs::remove_file(path).map_err(private::io)
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                _ => Err(private::error(
                    "Stale runtime socket changed before removal.",
                )),
            })
            .await?;
        }
        if self.paths.socket.as_os_str().as_bytes().len() >= 100 {
            return Err(private::error(
                "Runtime socket path exceeds its portable limit.",
            ));
        }
        let listener = UnixListener::bind(&self.paths.socket).map_err(private::io)?;
        let meta = fs::symlink_metadata(&self.paths.socket).map_err(private::io)?;
        self.socket_identity = Some((meta.dev(), meta.ino()));
        fs::set_permissions(&self.paths.socket, fs::Permissions::from_mode(0o600))
            .map_err(private::io)?;
        Ok(listener)
    }
}
impl Drop for Ownership {
    fn drop(&mut self) {
        // Remove only our inode while the lifetime lock is still held. A late
        // destructor must never unlink a successor or a foreign replacement.
        if let Some((dev, ino)) = self.socket_identity
            && let Ok(meta) = fs::symlink_metadata(&self.paths.socket)
            && meta.file_type().is_socket()
            && meta.dev() == dev
            && meta.ino() == ino
        {
            let _ = fs::remove_file(&self.paths.socket);
        }
        let _ = fs::remove_dir(&self.paths.directory);
    }
}
