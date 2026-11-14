//! Read only the private physical transport locator, never native root facts.
mod lease;
mod ownership;
mod recovery;
use super::{OwnerNamespace, storage, unavailable};
use agentchat_core::Result;
pub use lease::TransportLease;
pub use ownership::TransportOwnership;
pub(super) use recovery::{Removed, recover, recover_managed};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;
pub(super) fn allocation_publication(
    root: &crate::claude::metadata::directory::Directory,
    name: &std::ffi::OsStr,
    snapshot: &crate::claude::metadata::directory::Snapshot,
) -> Result<crate::staging::allocation::Published> {
    let value: TransportEndpoint = crate::claude::metadata::decode_proved(root, name, snapshot)?;
    let proof = value.ownership.as_ref().ok_or_else(unavailable)?;
    let slot = format!("gateway-{}.json", proof.scope);
    let lock = root
        .read(format!("gateway-{}.lock", proof.scope).as_ref())?
        .ok_or_else(unavailable)?;
    if proof.version != 1
        || proof.scope.is_nil()
        || value.nonce.is_nil()
        || name != std::ffi::OsStr::new(&slot)
        || !lock.bytes.is_empty()
        || lock.id != proof.lock
        || value.socket.file_name() != Some("control.sock".as_ref())
    {
        return Err(unavailable());
    }
    Ok(crate::staging::allocation::Published {
        path: value.socket.parent().ok_or_else(unavailable)?.into(),
        helper: proof.helper.clone(),
        native: proof.native.clone(),
        directory: proof.directory,
        parent: proof.parent,
        sockets: vec![("control.sock".into(), proof.socket.clone())],
    })
}

/// Private control endpoint publication. A nonce locates its incarnation; it
/// cannot assert a root, participant, native policy state or wake permission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransportEndpoint {
    /// Short private Unix control socket.
    pub socket: PathBuf,
    /// Unpredictable physical endpoint incarnation.
    pub nonce: Uuid,
    /// Optional original physical publication proof. Legacy locators remain
    /// readable, but cannot authorize external socket collection or replacement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ownership: Option<TransportOwnership>,
}
impl OwnerNamespace {
    /// Read the current endpoint with bounded private-file/link/inode checks.
    /// Absence permits only transport setup/cleanup, never generic-root fallback.
    pub async fn transport(&self) -> Result<Option<TransportEndpoint>> {
        Ok(self.transport_observation(false).await?.map(|(e, _)| e))
    }
    /// Capture a proved physical helper for the existing native observer. Its
    /// later positive birth loss permits only a host-controlled reconnect hint.
    /// Legacy/unproved endpoint payloads cannot supply this token.
    pub(crate) async fn watched_transport(
        &self,
    ) -> Result<Option<(TransportEndpoint, Option<super::ProcessIdentity>)>> {
        self.transport_observation(true).await
    }
    async fn transport_observation(
        &self,
        watch: bool,
    ) -> Result<Option<(TransportEndpoint, Option<super::ProcessIdentity>)>> {
        let ns = self.clone();
        tokio::task::spawn_blocking(move || {
            crate::claude::metadata::private_directory(&ns.directory)?;
            if !ns.owner.is_continuing() {
                return Err(unavailable());
            }
            let path = ns.directory.join(format!("gateway-{}.json", ns.id));
            match std::fs::symlink_metadata(&path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(_) => return Err(unavailable()),
                Ok(_) => {}
            }
            let endpoint: TransportEndpoint = storage::read_record(&path, 65_536)?;
            if endpoint.nonce.is_nil()
                || !endpoint.socket.is_absolute()
                || !ns.owner.is_continuing()
            {
                return Err(unavailable());
            }
            let helper = if watch {
                let parent = crate::claude::metadata::directory::Directory::owned(&ns.directory)?;
                let name = path.file_name().ok_or_else(unavailable)?;
                parent.read(name)?.and_then(|snapshot| {
                    let proved: TransportEndpoint =
                        crate::claude::metadata::decode_proved(&parent, name, &snapshot).ok()?;
                    let claim = allocation_publication(&parent, name, &snapshot).ok()?;
                    (proved == endpoint && claim.native == ns.owner).then_some(claim.helper)
                })
            } else {
                None
            };
            Ok(Some((endpoint, helper)))
        })
        .await
        .map_err(|_| unavailable())?
    }
}
