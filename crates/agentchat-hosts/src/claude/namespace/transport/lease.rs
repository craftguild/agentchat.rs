//! One original physical publication; no recursive or unproved socket cleanup.
use super::{TransportEndpoint, TransportOwnership, recover};
use crate::claude::metadata::{
    self,
    directory::{Directory, FileId, Snapshot, SocketId},
};
use crate::claude::process::ProcessIdentity;
use crate::lock::ScopedLock;
use agentchat_core::{Error, ErrorCode, Result};
use std::{
    ffi::OsString,
    fs::{self, DirBuilder},
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::net::UnixListener;
use uuid::Uuid;

/// An owner-scoped physical gateway lease, separate from conversation identity.
/// The caller must independently pin its native owner/namespace. Neither this
/// locator, a nonce nor a live listener grants native root or wake authority.
pub struct TransportLease {
    /// Original control listener. Independent users must respect lease lifetime.
    pub listener: Arc<UnixListener>,
    /// Unpredictable physical endpoint incarnation, never a conversation ID.
    pub nonce: Uuid,
    // Declared after the listener so its owned handle closes before cleanup.
    _publication: Publication,
    // Keep the original gateway lock through every attempted publication unlink.
    _lock: ScopedLock,
}
struct Publication {
    allocation: Option<crate::staging::AllocationOrigin>,
    root: Directory,
    directory: Directory,
    parent: FileId,
    socket: Option<SocketId>,
    record: Option<(OsString, Snapshot)>,
}
fn unavailable() -> Error {
    Error::new(
        ErrorCode::SessionRequired,
        "The native gateway publication is unavailable or changed.",
    )
}
impl Publication {
    fn release(&mut self) -> Result<()> {
        if let Some(allocation) = &mut self.allocation {
            allocation.begin_release()?;
        }
        // A missing/replaced parent is not evidence that we removed it. Keep
        // the endpoint witness and never follow it to a foreign replacement.
        self.directory.current()?;
        if let Some(socket) = &self.socket {
            self.directory
                .remove_socket("control.sock".as_ref(), socket)?;
        }
        // Unknown children block rmdir and retain the original endpoint witness.
        // No TempDir destructor can recursively remove another component's data.
        self.directory.remove_empty(self.parent)?;
        if let Some((name, snapshot)) = &self.record {
            self.root.remove_record(name, snapshot)?;
        }
        if let Some(allocation) = &mut self.allocation {
            allocation.finish()?;
        }
        Ok(())
    }
}
impl Drop for Publication {
    fn drop(&mut self) {
        let _ = self.release();
    }
}
fn lock(root: &Directory, name: &std::ffi::OsStr) -> Result<(ScopedLock, Snapshot)> {
    let original = match root.read(name)? {
        Some(value) => value,
        None => {
            // A competing first creator may win the exclusive empty lock name.
            // It is adopted only as a complete private empty inode, never erased.
            let created = root.create_record(name);
            let snapshot = root.read(name)?.ok_or_else(unavailable)?;
            if let Ok(file) = created {
                if snapshot.id != FileId::of(&file.metadata().map_err(|_| unavailable())?) {
                    return Err(unavailable());
                }
                file.sync_all().map_err(|_| unavailable())?;
                root.sync()?;
            }
            snapshot
        }
    };
    if !original.bytes.is_empty() {
        return Err(unavailable());
    }
    let file = root.lock_record(name, &original)?;
    Ok((file, original))
}
impl TransportLease {
    /// Bind the ordinary managed gateway with explicit pending allocation proof.
    /// Namespace identity remains the independently verified full native token.
    pub async fn bind_managed(namespace: &super::super::OwnerNamespace) -> Result<Self> {
        Self::bind_inner(
            namespace.directory(),
            namespace.id(),
            namespace.owner(),
            Some(namespace),
        )
        .await
    }
    /// Bind a private control endpoint under its original nonblocking lock.
    /// Active/unproved endpoints are preserved. A positively dead helper's
    /// complete original publication is recovered before publishing a successor.
    /// The native token is physical retention input, never root/wake authority.
    /// Local drop removes only its original socket and empty original directory;
    /// changed parents, records or unknown children retain their data/witness.
    pub async fn bind(root: &Path, owner: Uuid, native: &ProcessIdentity) -> Result<Self> {
        Self::bind_inner(root, owner, native, None).await
    }
    async fn bind_inner(
        root: &Path,
        owner: Uuid,
        native: &ProcessIdentity,
        namespace: Option<&super::super::OwnerNamespace>,
    ) -> Result<Self> {
        if owner.is_nil() || !native.is_continuing() {
            return Err(unavailable());
        }
        let root = Directory::owned(root)?;
        let lock_name = OsString::from(format!("gateway-{owner}.lock"));
        let (lock, lock_snapshot) = lock(&root, &lock_name)?;
        let name = OsString::from(format!("gateway-{owner}.json"));
        if let Some(namespace) = namespace {
            super::recovery::recover_managed(
                &namespace.home,
                &root,
                owner,
                native,
                &lock_snapshot,
                &mut |_| {},
            )?;
        } else {
            recover(&root, owner, native, &lock_snapshot, &mut |_| {})?;
        }
        let previous = root.read(&name)?;
        if previous.is_some() {
            return Err(unavailable());
        }
        root.current()?;
        if !root.matches(&lock_name, &lock_snapshot)? || root.read(&name)? != previous {
            return Err(unavailable());
        }
        let path =
            PathBuf::from("/tmp").join(format!("agentchat-native-gateway-{}", Uuid::new_v4()));
        let allocation = if let Some(namespace) = namespace {
            let alternate = namespace.quarantine_path()?;
            Some(
                crate::staging::OriginRegistry::new(&namespace.home).begin_allocation(
                    crate::staging::AllocationPlan {
                        path: &path,
                        root: &root.path,
                        alternate_root: Some(&alternate),
                        endpoint: &name,
                        sockets: &["control.sock"],
                        guard: Some(&lock_name),
                        purpose: crate::staging::StagePurpose::Gateway,
                        native: Some(native),
                    },
                )?,
            )
        } else {
            None
        };
        DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(|_| unavailable())?;
        let directory = Directory::open(&path)?;
        let parent = directory.parent_id()?;
        let mut publication = Publication {
            allocation,
            root,
            directory,
            parent,
            socket: None,
            record: None,
        };
        if let Some(allocation) = &mut publication.allocation {
            allocation.directory_created()?;
        }
        let socket = path.join("control.sock");
        let listener = UnixListener::bind(&socket).map_err(|_| unavailable())?;
        publication.directory.current()?;
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))
            .map_err(|_| unavailable())?;
        publication.socket = Some(
            publication
                .directory
                .socket("control.sock".as_ref())?
                .ok_or_else(unavailable)?,
        );
        if let Some(allocation) = &mut publication.allocation {
            allocation.socket_created("control.sock")?;
        }
        let nonce = Uuid::new_v4();
        let ownership = TransportOwnership::capture(
            owner,
            lock_snapshot.id,
            &publication.directory,
            publication.socket.clone().ok_or_else(unavailable)?,
            native,
        )?;
        let endpoint = TransportEndpoint {
            socket,
            nonce,
            ownership: Some(ownership),
        };
        if !publication.root.matches(&lock_name, &lock_snapshot)?
            || publication.root.read(&name)? != previous
            || !native.is_continuing()
        {
            return Err(unavailable());
        }
        if let Some(namespace) = namespace {
            metadata::Publisher::new(&namespace.home)
                .alternate_parent(namespace.quarantine_path()?)
                .guard(lock_name.clone())
                .write(&publication.root.path.join(&name), &endpoint)?;
        } else {
            metadata::publish_in(&publication.root, &name, &endpoint, false)?;
        }
        let snapshot = publication.root.read(&name)?.ok_or_else(unavailable)?;
        let actual: TransportEndpoint = metadata::decode(
            &publication.root.path.join(&name),
            &snapshot.bytes,
            (publication.root.id.0, publication.root.id.1),
            (snapshot.id.0, snapshot.id.1),
        )?;
        if actual != endpoint {
            return Err(unavailable());
        }
        publication.record = Some((name, snapshot));
        #[cfg(test)]
        if namespace.is_some() {
            crate::staging::allocation::tests::cut(
                "endpoint-written",
                &path.canonicalize().map_err(|_| unavailable())?,
            );
        }
        if let Some(allocation) = &mut publication.allocation {
            allocation.published()?;
        }
        Ok(Self {
            listener: Arc::new(listener),
            nonce,
            _publication: publication,
            _lock: lock,
        })
    }
}
