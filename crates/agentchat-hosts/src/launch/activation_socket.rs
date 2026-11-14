//! A private supervisor endpoint, separate from native conversation traffic.
use super::{Binding, LaunchOwners, readiness::owner_key};
use crate::claude::metadata::{
    self,
    directory::{Directory, SocketId},
};
use agentchat_core::{Error, ErrorCode, Result};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, PermissionsExt},
    },
    path::{Path, PathBuf},
};
use tokio::net::{UnixListener, UnixStream};

/// Own exactly one original-supervisor activation socket. Publication of this
/// path supplies no membership or authority; clients check its kernel peer and
/// original launch proof. Drops remove only the same private directory/socket.
pub struct ActivationSocket {
    listener: UnixListener,
    directory: Directory,
    socket: SocketId,
    path: PathBuf,
}
impl ActivationSocket {
    /// Bind once from the actual live supervisor. Existing endpoints are
    /// preserved, never removed/adopted merely because their pathname matches.
    pub fn bind(home: &Path, binding: &Binding) -> Result<Self> {
        binding.validate()?;
        if !home.is_absolute() || !LaunchOwners::is_supervisor(&binding.identity) {
            return Err(required());
        }
        metadata::private_directory(home)?;
        let owner = LaunchOwners::native_owner(&binding.identity)?;
        let key = owner_key(&owner, &binding.context.host_session_id)?;
        let mut hash = Sha256::new();
        hash.update(home.as_os_str().as_bytes());
        hash.update(key.as_bytes());
        let digest = hash
            .finalize()
            .iter()
            .take(16)
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let path = PathBuf::from("/tmp").join(format!(
            "agentchat-launch-{}-{digest}",
            rustix::process::geteuid().as_raw()
        ));
        match fs::DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(required()),
        }
        let directory = Directory::open(&path)?;
        let path = path.join("join.sock");
        if path.as_os_str().as_bytes().len() >= 100 || fs::symlink_metadata(&path).is_ok() {
            return Err(required());
        }
        directory.current()?;
        let listener = UnixListener::bind(&path).map_err(|_| required())?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(|_| required())?;
        directory.current()?;
        // Capture the final private socket identity after chmod, including its
        // changed timestamp. Cleanup never adopts an existing/replaced endpoint.
        let socket = directory
            .socket("join.sock".as_ref())?
            .ok_or_else(required)?;
        Ok(Self {
            listener,
            directory,
            socket,
            path,
        })
    }

    /// Exact short endpoint to include in the proved readiness record.
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// Accept a local request without deriving a native caller or granting Join.
    /// The shared operation still validates request scope, current owner and Core.
    pub async fn accept(&self) -> Result<UnixStream> {
        self.directory.current()?;
        if self.directory.socket("join.sock".as_ref())?.as_ref() != Some(&self.socket) {
            return Err(required());
        }
        let (stream, _) = self.listener.accept().await.map_err(|_| required())?;
        if stream.peer_cred().map(|c| c.uid()).ok() != Some(rustix::process::geteuid().as_raw()) {
            return Err(required());
        }
        Ok(stream)
    }
}
impl Drop for ActivationSocket {
    fn drop(&mut self) {
        // A replacement, symlink or changed directory is preserved. No shared
        // native server/helper is ever signalled or unlinked during this cleanup.
        let _ = self
            .directory
            .remove_socket("join.sock".as_ref(), &self.socket);
        if let Ok(parent) = self.directory.parent_id() {
            let _ = self.directory.remove_empty(parent);
        }
    }
}
fn required() -> Error {
    Error::new(
        ErrorCode::SessionRequired,
        "The original managed activation endpoint is unavailable or changed.",
    )
}
