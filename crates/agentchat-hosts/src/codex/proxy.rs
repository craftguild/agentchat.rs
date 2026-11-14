//! Per-CLI transparent Codex transport and bounded root-transition correlation.

mod bridge;
mod frame_tap;
mod json_stream;
mod metadata;
mod observation;
mod observation_guard;
mod startup;
mod utf8;
use super::{required, thread_id, unavailable};
use crate::{
    claude::metadata::{
        self as filesystem_metadata,
        directory::{Directory, SocketId},
    },
    launch::{LaunchOwners, NativeProof},
};
use agentchat_core::{Result, model::LaunchOwnerState};
use bridge::bridge;
#[cfg(test)]
use metadata::{CandidateOutcome, candidate_request, candidate_response};
pub use startup::ProxyFence;
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};
use tokio::{
    net::{UnixListener, UnixStream},
    sync::{mpsc, oneshot},
    task::JoinSet,
};
use uuid::Uuid;

/// Application decision after the matching native success response is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyDecision {
    /// Same-project target is durably active.
    Activated,
    /// Old participation is fenced; a cross-project target remains unjoined.
    RetiredOnly,
}

/// Metadata-only lifecycle events. Native payload bodies are never retained.
pub enum ProxyEvent {
    /// A recognized transition has a durable fence before native server entry.
    Candidate {
        /// Exact native socket identity; arrival order grants no RPC privilege.
        connection_epoch: u64,
        /// Monotonic foreground transition within this owned proxy.
        transition_epoch: u64,
        /// Completes only after the old generation's delivery fence is durable.
        fence: ProxyFence,
    },
    /// A failed/cancelled request or exact-current-root success preserved mapping.
    RolledBack {
        /// Exact native socket identity; arrival order grants no RPC privilege.
        connection_epoch: u64,
        /// Matching foreground transition.
        transition_epoch: u64,
        /// Acknowledges removal of the exact fence; native delivery is independent.
        release: oneshot::Sender<()>,
    },
    /// Native outcome or lifecycle observation is unknown; AgentChat stays fenced.
    Unknown {
        /// Exact native socket identity; arrival order grants no RPC privilege.
        connection_epoch: u64,
        /// Matching foreground transition.
        transition_epoch: u64,
    },
    /// A distinct-root response reached the owned frontend transport.
    Delivered {
        /// Matching authoritative frontend connection.
        connection_epoch: u64,
        /// Matching foreground transition.
        transition_epoch: u64,
        /// Acknowledges target delivery enablement; native input is independent.
        release: oneshot::Sender<()>,
    },
    /// A distinct-root native success was written; activation remains gated.
    Succeeded {
        /// Exact native socket identity; arrival order grants no RPC privilege.
        connection_epoch: u64,
        /// Matching foreground transition.
        transition_epoch: u64,
        /// Exact root returned by the successful foreground response.
        root: String,
        /// One-use application acknowledgement after fencing/activation.
        release: oneshot::Sender<ProxyDecision>,
    },
}

/// Owned event stream; dropping it stops only this private proxy task.
pub struct ProxyEvents {
    receiver: mpsc::Receiver<ProxyEvent>,
    initial: Option<ProxyEvent>,
    initial_prepared: bool,
    task: tokio::task::JoinHandle<()>,
    observer: observation::Observer,
    observation_task: tokio::task::JoinHandle<()>,
}
impl ProxyEvents {
    /// Track the current published binding before restoration or delivery.
    /// Original owner/readiness remains fixed across rejoin generations. Stale
    /// descriptors and unresolved same-root fence retargeting are refused.
    /// This permits a durable pre-entry fence without an application round trip.
    pub fn track(&self, binding: &crate::launch::Binding) -> Result<()> {
        self.observer.track(binding)
    }
    /// Nonblocking ordered event read for the original supervisor loop.
    pub fn try_recv(&mut self) -> std::result::Result<ProxyEvent, mpsc::error::TryRecvError> {
        self.initial
            .take()
            .map_or_else(|| self.receiver.try_recv(), Ok)
    }
    /// Await the next bounded event. Used by focused transport acceptance; the
    /// production supervisor uses nonblocking reads in its existing select loop.
    pub async fn recv(&mut self) -> Option<ProxyEvent> {
        match self.initial.take() {
            Some(event) => Some(event),
            None => self.receiver.recv().await,
        }
    }
}
impl Drop for ProxyEvents {
    fn drop(&mut self) {
        self.task.abort();
        self.observation_task.abort();
    }
}

/// Bound private endpoint. It is inert until paired with the actual child proof.
pub struct Proxy {
    listener: UnixListener,
    directory: Directory,
    socket: SocketId,
    path: PathBuf,
    backend: PathBuf,
    active_root: String,
    home: PathBuf,
}
impl Proxy {
    /// Bind a fresh same-user Unix socket without contacting or replacing the
    /// existing shared server. The exact child is checked later on every accept.
    pub fn bind(home: &Path, backend: &Path, active_root: &str) -> Result<Self> {
        if !home.is_absolute()
            || !backend.is_absolute()
            || !thread_id(active_root)
            || !backend_socket(backend)?
        {
            return Err(required());
        }
        filesystem_metadata::private_directory(home)?;
        let directory_path = PathBuf::from("/tmp").join(format!(
            "agentchat-codex-{}-{}",
            rustix::process::geteuid().as_raw(),
            &Uuid::new_v4().simple().to_string()[..16]
        ));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory_path)
            .map_err(|_| required())?;
        let directory = Directory::open(&directory_path)?;
        let path = directory_path.join("proxy.sock");
        if path.as_os_str().as_encoded_bytes().len() >= 100 {
            return Err(required());
        }
        let listener = UnixListener::bind(&path).map_err(|_| required())?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(|_| required())?;
        let socket = directory
            .socket("proxy.sock".as_ref())?
            .ok_or_else(required)?;
        Ok(Self {
            listener,
            directory,
            socket,
            path,
            backend: backend.into(),
            active_root: active_root.into(),
            home: home.into(),
        })
    }

    /// Exact endpoint passed to the selected native CLI as `unix://...`.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Start forwarding only after the actual designated child is captured.
    pub fn serve(mut self, proof: NativeProof) -> ProxyEvents {
        let (sender, receiver) = mpsc::channel(16);
        let (observer, observation_task) = observation::start(
            &self.home,
            &self.backend,
            &self.active_root,
            proof.clone(),
            sender,
        );
        let observing = observer.clone();
        let task = tokio::spawn(async move {
            let _ = self.run(proof, observing).await;
        });
        ProxyEvents {
            receiver,
            initial: None,
            initial_prepared: false,
            task,
            observer,
            observation_task,
        }
    }

    async fn run(&mut self, proof: NativeProof, observer: observation::Observer) -> Result<()> {
        let mut connection_epoch = 0u64;
        let mut connections = JoinSet::new();
        loop {
            self.current()?;
            let (stream, _) = tokio::select! {
                accepted = self.listener.accept() => accepted.map_err(|_| unavailable())?,
                _ = connections.join_next(), if !connections.is_empty() => continue,
            };
            if !owned(&stream, &proof)? {
                continue;
            }
            connection_epoch = connection_epoch.checked_add(1).ok_or_else(required)?;
            let backend = self.backend.clone();
            let observer = observer.clone();
            // Order grants no RPC privileges. One socket ending cannot revoke
            // another exact-owner socket or promote it to a special role.
            connections.spawn(async move {
                let result = bridge(stream, &backend, connection_epoch, &observer).await;
                observer.closed(connection_epoch).await;
                result
            });
        }
    }

    fn current(&self) -> Result<()> {
        self.directory.current()?;
        if self.directory.socket("proxy.sock".as_ref())?.as_ref() != Some(&self.socket) {
            return Err(required());
        }
        Ok(())
    }
}

fn owned(stream: &UnixStream, proof: &NativeProof) -> Result<bool> {
    let credentials = stream.peer_cred().map_err(|_| required())?;
    Ok(credentials.uid() == rustix::process::geteuid().as_raw()
        && credentials.pid().and_then(|pid| u32::try_from(pid).ok()) == Some(proof.owner().pid())
        && proof.state() == LaunchOwnerState::Current
        && LaunchOwners::is_supervisor(proof.identity()))
}
impl Drop for Proxy {
    fn drop(&mut self) {
        let _ = self
            .directory
            .remove_socket("proxy.sock".as_ref(), &self.socket);
        if let Ok(parent) = self.directory.parent_id() {
            let _ = self.directory.remove_empty(parent);
        }
    }
}

fn backend_socket(path: &Path) -> Result<bool> {
    let metadata = fs::symlink_metadata(path).map_err(|_| unavailable())?;
    Ok(metadata.file_type().is_socket() && metadata.uid() == rustix::process::geteuid().as_raw())
}

#[cfg(test)]
#[path = "../../tests/unit/codex_proxy.rs"]
mod tests;

#[cfg(test)]
#[path = "../../tests/unit/codex_proxy_connections.rs"]
mod connection_tests;

#[cfg(test)]
#[path = "../../tests/unit/codex_proxy_metadata.rs"]
mod metadata_tests;

#[cfg(test)]
#[path = "../../tests/unit/codex_proxy_transport.rs"]
mod transport_tests;

#[cfg(test)]
#[path = "../../tests/unit/codex_proxy_startup.rs"]
mod startup_tests;
