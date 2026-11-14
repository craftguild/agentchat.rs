//! One exclusive publication; local cleanup retains original content and parents.
use super::{
    identity::{Directory, FileId, Snapshot, SocketId},
    ownership::Ownership,
    protocol::{self, Endpoint},
    recovery, unavailable,
};
use crate::claude::process::ProcessIdentity;
use agentchat_core::Result;
use std::{
    ffi::OsString,
    fs::{self, DirBuilder},
    io::Write,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::net::UnixListener;
use uuid::Uuid;

pub(super) struct Publication {
    // Listeners close before Owned runs, so normal release cannot unlink a
    // still-listening successor through an inherited pathname.
    pub requests: UnixListener,
    pub replies: UnixListener,
    pub reply_socket: PathBuf,
    pub token: Uuid,
    _owned: Owned,
}
struct Owned {
    allocation: Option<crate::staging::AllocationOrigin>,
    root: Directory,
    directory: Directory,
    parent: FileId,
    sockets: Vec<(&'static str, SocketId)>,
    records: Vec<(OsString, Snapshot)>,
}
impl Owned {
    fn release(&mut self) -> Result<()> {
        if let Some(allocation) = &mut self.allocation {
            allocation.begin_release()?;
        }
        if !Directory::absent(&self.directory.path)? {
            self.directory.current()?;
            // Validate the whole original set before the first removal. Unknown
            // children make rmdir fail and retain the endpoint witness.
            for (name, id) in &self.sockets {
                if self
                    .directory
                    .socket(name.as_ref())?
                    .is_some_and(|now| now != *id)
                {
                    return Err(unavailable());
                }
            }
            for (name, id) in &self.sockets {
                self.directory.remove_socket(name.as_ref(), id)?;
            }
            self.directory.remove_empty(self.parent)?;
        }
        // Keep provenance until socket cleanup finishes; late drops cannot
        // remove a successor or a record changed in place.
        for (name, snapshot) in self.records.iter().rev() {
            self.root.remove_record(name, snapshot)?;
        }
        if let Some(allocation) = &mut self.allocation {
            allocation.finish()?;
        }
        Ok(())
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

pub(super) async fn bind(
    root: &Path,
    observer: &str,
    owner: &ProcessIdentity,
) -> Result<Publication> {
    bind_inner(root, observer, owner, None).await
}
pub(super) async fn bind_managed(
    namespace: &crate::claude::namespace::OwnerNamespace,
) -> Result<Publication> {
    bind_inner(
        namespace.directory(),
        namespace.observer(),
        namespace.owner(),
        Some(namespace),
    )
    .await
}
async fn bind_inner(
    root: &Path,
    observer: &str,
    owner: &ProcessIdentity,
    namespace: Option<&crate::claude::namespace::OwnerNamespace>,
) -> Result<Publication> {
    let root = Directory::open(root)?;
    if !super::super::identifier(observer) || !owner.is_continuing() {
        return Err(unavailable());
    }
    let endpoint = root.path.join(format!("fresh-{observer}.json"));
    if let Some(namespace) = namespace
        && let Some(snapshot) = root.read(endpoint.file_name().ok_or_else(unavailable)?)?
    {
        let proof = super::allocation_publication(
            &root,
            &root.path,
            endpoint.file_name().ok_or_else(unavailable)?,
            &snapshot,
        )?;
        if proof.helper.birth_state() == crate::claude::process::BirthState::Gone {
            let sockets = proof
                .sockets
                .iter()
                .map(|(n, s)| (n.as_str(), s))
                .collect::<Vec<_>>();
            let _ = crate::staging::allocation::published_cleanup(
                namespace.state_home(),
                &proof.path,
                &root,
                (endpoint.file_name().ok_or_else(unavailable)?, &snapshot),
                proof.directory,
                &sockets,
                &proof.helper,
            )?;
        }
    }
    tokio::time::timeout(
        Duration::from_secs(6),
        recovery::retire(&endpoint, observer, owner),
    )
    .await
    .map_err(|_| unavailable())??;
    root.current()?;
    if !owner.is_continuing() {
        return Err(unavailable());
    }
    let path = PathBuf::from("/tmp").join(format!(
        "agentchat-observer-{}-{}",
        rustix::process::geteuid().as_raw(),
        Uuid::new_v4()
    ));
    let allocation = if let Some(namespace) = namespace {
        let alternate = namespace.quarantine_path()?;
        Some(
            crate::staging::OriginRegistry::new(namespace.state_home()).begin_allocation(
                crate::staging::AllocationPlan {
                    path: &path,
                    root: &root.path,
                    alternate_root: Some(&alternate),
                    endpoint: endpoint.file_name().ok_or_else(unavailable)?,
                    sockets: &["request.sock", "reply.sock"],
                    guard: None,
                    purpose: crate::staging::StagePurpose::Relay,
                    native: Some(owner),
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
    let mut owned = Owned {
        allocation,
        root,
        directory,
        parent,
        sockets: Vec::new(),
        records: Vec::new(),
    };
    if let Some(allocation) = &mut owned.allocation {
        allocation.directory_created()?;
    }
    let request_socket = path.join("request.sock");
    let reply_socket = path.join("reply.sock");
    let requests = UnixListener::bind(&request_socket).map_err(|_| unavailable())?;
    fs::set_permissions(&request_socket, fs::Permissions::from_mode(0o600))
        .map_err(|_| unavailable())?;
    owned.sockets.push((
        "request.sock",
        owned
            .directory
            .socket("request.sock".as_ref())?
            .ok_or_else(unavailable)?,
    ));
    if let Some(allocation) = &mut owned.allocation {
        allocation.socket_created("request.sock")?;
    }
    let replies = UnixListener::bind(&reply_socket).map_err(|_| unavailable())?;
    fs::set_permissions(&reply_socket, fs::Permissions::from_mode(0o600))
        .map_err(|_| unavailable())?;
    owned.sockets.push((
        "reply.sock",
        owned
            .directory
            .socket("reply.sock".as_ref())?
            .ok_or_else(unavailable)?,
    ));
    if let Some(allocation) = &mut owned.allocation {
        allocation.socket_created("reply.sock")?;
    }
    let token = Uuid::new_v4();
    let stage_name = OsString::from(format!(".fresh-{}.tmp", Uuid::new_v4()));
    let mut stage_origin = if let Some(namespace) = namespace {
        let alternate = namespace.quarantine_path()?;
        Some(
            crate::staging::OriginRegistry::new(namespace.state_home()).begin_file(
                crate::staging::FilePlan {
                    parent: &owned.root.path,
                    stage: &stage_name,
                    target: endpoint.file_name().ok_or_else(unavailable)?,
                    purpose: crate::staging::StagePurpose::Metadata,
                    alternate_parent: Some(&alternate),
                    guard: None,
                },
            )?,
        )
    } else {
        None
    };
    let mut file = owned.root.create_record(&stage_name)?;
    let ownership = Ownership::prepare(
        &owned.root,
        FileId::of(&file.metadata().map_err(|_| unavailable())?),
        &owned.directory,
    )?;
    let mut value = Endpoint {
        observer: observer.into(),
        socket: request_socket,
        pid: std::process::id(),
        owner: Some(owner.clone()),
        retire_token: Some(token),
        ownership: Some(ownership),
    };
    Ownership::seal(&mut value)?;
    file.write_all(&protocol::encode(&value)?)
        .map_err(|_| unavailable())?;
    file.sync_all().map_err(|_| unavailable())?;
    if let Some(origin) = &mut stage_origin {
        origin.seal(&file, &protocol::encode(&value)?)?;
    }
    let snapshot = owned.root.read(&stage_name)?.ok_or_else(unavailable)?;
    owned.records.push((stage_name.clone(), snapshot));
    // The complete original inode is published once, without a link/unlink
    // crash window or replacement of a concurrent successor.
    let name = endpoint.file_name().ok_or_else(unavailable)?.to_owned();
    if !owner.is_continuing() {
        return Err(unavailable());
    }
    owned.root.publish_new(&stage_name, &name)?;
    let snapshot = owned.root.read(&name)?.ok_or_else(unavailable)?;
    owned.records.push((name, snapshot));
    owned.root.sync()?;
    #[cfg(test)]
    if namespace.is_some() {
        crate::staging::allocation::tests::cut(
            "endpoint-written",
            &path.canonicalize().map_err(|_| unavailable())?,
        );
    }
    if let Some(allocation) = &mut owned.allocation {
        allocation.published()?;
    }
    Ok(Publication {
        requests,
        replies,
        reply_socket,
        token,
        _owned: owned,
    })
}
