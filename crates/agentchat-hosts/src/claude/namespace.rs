//! A physical-owner IPC locator, deliberately separate from conversation identity.
mod allocation;
mod genesis;
mod guards;
mod pending_calls;
mod projections;
mod reap;
mod snapshot;
mod state;
mod storage;
mod transport;
use super::process::ProcessIdentity;
use agentchat_core::{Error, ErrorCode, Result};
pub use projections::{LifecycleOwner, remove_notices};
pub use reap::{OwnerReaper, ReapReport};
use sha2::{Digest, Sha256};
pub use state::{CommittedLifecycle, NativeSequence, Reservation};
use std::{
    os::unix::fs::{DirBuilderExt, MetadataExt},
    path::{Component, Path, PathBuf},
};
pub use transport::{TransportEndpoint, TransportLease, TransportOwnership};
pub(crate) fn allocation_publication(
    root: &super::metadata::directory::Directory,
    name: &std::ffi::OsStr,
    snapshot: &super::metadata::directory::Snapshot,
) -> Result<crate::staging::allocation::Published> {
    transport::allocation_publication(root, name, snapshot)
}
use uuid::Uuid;

/// One native CLI process's private transport directory. Its digest is a locator,
/// not a conversation ID, participant, heartbeat or automatic-wake permission.
#[derive(Debug, Clone)]
pub struct OwnerNamespace {
    home: PathBuf,
    directory: PathBuf,
    owner: ProcessIdentity,
    id: Uuid,
    observer: String,
}
impl OwnerNamespace {
    fn location(home: &Path, owner: ProcessIdentity) -> Result<Self> {
        if !owner.is_continuing() {
            return Err(unavailable());
        }
        Self::locator(home, owner)
    }
    // Pure locator construction is private. Dead-owner collection must separately
    // authenticate the original namespace/seal and positive birth loss; callers
    // cannot obtain a usable live-owner handle through this path.
    fn locator(home: &Path, owner: ProcessIdentity) -> Result<Self> {
        if !home.is_absolute() || home.components().any(|c| c == Component::ParentDir) {
            return Err(unavailable());
        }
        let digest = Sha256::digest(serde_json::to_vec(&owner).map_err(|_| unavailable())?);
        let id = Uuid::from_bytes(digest[..16].try_into().map_err(|_| unavailable())?);
        if id.is_nil() {
            return Err(unavailable());
        }
        let key = digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        Ok(Self {
            home: home.into(),
            directory: home.join("hosts/claude/owners").join(key),
            owner,
            id,
            observer: format!("native-{}", id.simple()),
        })
    }
    /// Resolve only an existing complete owner cursor. This never creates a
    /// directory, lock or header and cannot reset floors when metadata is missing.
    pub async fn existing(home: &Path, owner: ProcessIdentity) -> Result<Self> {
        let home = home.to_owned();
        tokio::task::spawn_blocking(move || {
            let ns = Self::location(&home, owner)?;
            let mut path = home;
            super::metadata::private_directory(&path)?;
            for part in ["hosts", "claude", "owners"] {
                path.push(part);
                super::metadata::private_directory(&path)?;
            }
            super::metadata::private_directory(&ns.directory)?;
            storage::inspect(&ns, |_| Ok(()))?;
            Ok(ns)
        })
        .await
        .map_err(|_| unavailable())?
    }
    /// Read the current committed scope without publication or lock creation.
    /// Its contents remain routing hints, not fresh native root/control evidence.
    pub async fn read_current(&self) -> Result<Option<CommittedLifecycle>> {
        let ns = self.clone();
        tokio::task::spawn_blocking(move || {
            storage::inspect(&ns, |h| {
                Ok(if !h.retired && h.pending.is_none() {
                    h.current.clone()
                } else {
                    None
                })
            })
        })
        .await
        .map_err(|_| unavailable())?
    }
    /// Publish the immutable exact executable profile under the owner lock.
    /// Concurrent native stream/MCP startup may discover the same missing file;
    /// neither callback may overwrite a foreign value or fail merely because
    /// the other matching callback won its exclusive publication.
    pub async fn publish_profile(&self, profile: super::observer::NativeProfile) -> Result<()> {
        let ns = self.clone();
        tokio::task::spawn_blocking(move || {
            storage::access(&ns, |_| {
                if profile.executable != ns.owner.executable()
                    || !super::module::candidate_version(&profile.version)
                {
                    return Err(unavailable());
                }
                let path = ns.directory.join("native-profile.json");
                match std::fs::symlink_metadata(&path) {
                    Ok(_) => {
                        let prior: super::observer::NativeProfile = super::metadata::read(&path)?;
                        if prior.executable != profile.executable
                            || prior.version != profile.version
                        {
                            return Err(unavailable());
                        }
                        Ok(())
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        ns.metadata_publisher()?.create(&path, &profile)
                    }
                    Err(_) => Err(unavailable()),
                }
            })
        })
        .await
        .map_err(|_| unavailable())?
    }
    /// Create only private native transport directories under explicit agentchat state.
    /// Full boot/start/image/UID kernel identity distinguishes concurrent owners,
    /// including same-directory/same-root sessions. Native root proof remains
    /// separately required; a surviving helper cannot open a dead owner's scope.
    /// An interrupted new-format pristine initialization may finish under its
    /// original lock. Missing established/ambiguous legacy state never resets.
    pub async fn open(home: &Path, owner: ProcessIdentity) -> Result<Self> {
        let home = home.to_owned();
        let namespace = tokio::task::spawn_blocking(move || {
            let namespace = Self::location(&home, owner)?;
            private(&home)?;
            let mut path = home;
            for part in ["hosts", "claude", "owners"] {
                path.push(part);
                private(&path)?;
            }
            private(&namespace.directory)?;
            storage::access(&namespace, |_| Ok(()))?;
            Ok(namespace)
        })
        .await
        .map_err(|_| unavailable())??;
        Ok(namespace)
    }
    /// Absolute private transport directory, never a project discovery result.
    pub fn directory(&self) -> &Path {
        &self.directory
    }
    /// Explicit physical state home; never project or conversation identity.
    pub(crate) fn state_home(&self) -> &Path {
        &self.home
    }
    pub(super) fn quarantine_path(&self) -> Result<PathBuf> {
        let name = self
            .directory
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(unavailable)?;
        let key = name.strip_prefix(".retired-").unwrap_or(name);
        Ok(self
            .directory
            .parent()
            .ok_or_else(unavailable)?
            .join(format!(".retired-{key}")))
    }
    /// Select physical provenance for this namespace's plain projections. This
    /// creates nothing and grants no current-root or native-control authority.
    /// Invoke mutable writes only inside the existing owner-cursor callback.
    pub fn metadata_publisher(&self) -> Result<super::metadata::Publisher> {
        Ok(super::metadata::Publisher::new(&self.home)
            .alternate_parent(self.quarantine_path()?)
            .guard("owner-state.lock"))
    }
    /// Opaque physical-owner locator, derived from the complete kernel token.
    /// This is never a native root, application context or participant identity.
    pub fn id(&self) -> Uuid {
        self.id
    }
    /// Stable physical observer namespace; individual activations remain fresh.
    pub fn observer(&self) -> &str {
        &self.observer
    }
    /// Exact kernel owner that must still pass native/root/freshness checks.
    pub fn owner(&self) -> &ProcessIdentity {
        &self.owner
    }
}
pub(super) fn private(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(m) => {
            if !m.is_dir()
                || m.uid() != rustix::process::geteuid().as_raw()
                || m.mode() & 0o077 != 0
            {
                return Err(unavailable());
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            match std::fs::DirBuilder::new().mode(0o700).create(path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err(unavailable()),
            }
            let m = std::fs::symlink_metadata(path).map_err(|_| unavailable())?;
            if !m.is_dir()
                || m.uid() != rustix::process::geteuid().as_raw()
                || m.mode() & 0o077 != 0
            {
                return Err(unavailable());
            }
        }
        Err(_) => return Err(unavailable()),
    }
    Ok(())
}
fn unavailable() -> Error {
    Error::new(
        ErrorCode::SessionRequired,
        "The private native process namespace is unavailable or changed.",
    )
}
