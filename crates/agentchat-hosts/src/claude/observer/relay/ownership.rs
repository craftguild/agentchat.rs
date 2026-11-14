//! Self-contained publication provenance, not authentication against the OS user.
use super::{
    identity::{Directory, FileId, SocketId},
    protocol::Endpoint,
    unavailable,
};
use crate::claude::process::ProcessIdentity;
use agentchat_core::Result;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::os::unix::ffi::OsStrExt;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::claude::observer) struct Ownership {
    version: u32,
    root_path: Vec<u8>,
    pub(super) root: FileId,
    pub(super) endpoint: FileId,
    pub(super) directory: FileId,
    pub(super) parent: FileId,
    pub(super) request: SocketId,
    pub(super) reply: SocketId,
    pub(super) relay: ProcessIdentity,
    checksum: [u8; 32],
}
impl Ownership {
    pub(super) fn prepare(
        root: &Directory,
        endpoint: FileId,
        directory: &Directory,
    ) -> Result<Self> {
        Ok(Self {
            version: 1,
            root_path: root.path.as_os_str().as_bytes().into(),
            root: root.id,
            endpoint,
            directory: directory.id,
            parent: directory.parent_id()?,
            request: directory
                .socket("request.sock".as_ref())?
                .ok_or_else(unavailable)?,
            reply: directory
                .socket("reply.sock".as_ref())?
                .ok_or_else(unavailable)?,
            relay: ProcessIdentity::capture(std::process::id())?,
            checksum: [0; 32],
        })
    }
    fn checksum(endpoint: &Endpoint) -> Result<[u8; 32]> {
        let mut value = endpoint.clone();
        value.ownership.as_mut().ok_or_else(unavailable)?.checksum = [0; 32];
        let data = super::protocol::encode(&value)?;
        Ok(Sha256::new()
            .chain_update(b"agentchat/relay-publication/v1\0")
            .chain_update(data)
            .finalize()
            .into())
    }
    pub(super) fn seal(endpoint: &mut Endpoint) -> Result<()> {
        let checksum = Self::checksum(endpoint)?;
        endpoint
            .ownership
            .as_mut()
            .ok_or_else(unavailable)?
            .checksum = checksum;
        Ok(())
    }
    pub(super) fn validate(&self, endpoint: &Endpoint, root: &Directory, id: FileId) -> Result<()> {
        self.validate_original(endpoint, root.id, id, &root.path)
    }
    pub(super) fn validate_original(
        &self,
        endpoint: &Endpoint,
        root: FileId,
        id: FileId,
        original_path: &std::path::Path,
    ) -> Result<()> {
        if self.version != 1
            || self.root_path != original_path.as_os_str().as_bytes()
            || self.root != root
            || self.endpoint != id
            || self.relay.pid() != endpoint.pid
            || endpoint.retire_token.is_none_or(|t| t.is_nil())
            || endpoint.socket.file_name() != Some("request.sock".as_ref())
            || self.checksum != Self::checksum(endpoint)?
        {
            return Err(unavailable());
        }
        Ok(())
    }
}
