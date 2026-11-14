//! Original external objects and physical births, never native wake authority.
use crate::claude::{
    metadata::directory::{Directory, FileId, SocketId},
    process::ProcessIdentity,
};
use agentchat_core::Result;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Versioned local publication provenance for conservative transport recovery.
/// This opaque record is covered by the canonical metadata publication proof;
/// it is not authentication against another process with the same OS user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransportOwnership {
    pub(super) version: u32,
    pub(super) scope: Uuid,
    pub(super) lock: FileId,
    pub(super) directory: FileId,
    pub(super) parent: FileId,
    pub(super) socket: SocketId,
    pub(super) native: ProcessIdentity,
    pub(super) helper: ProcessIdentity,
}
impl TransportOwnership {
    pub(super) fn capture(
        scope: Uuid,
        lock: FileId,
        directory: &Directory,
        socket: SocketId,
        native: &ProcessIdentity,
    ) -> Result<Self> {
        Ok(Self {
            version: 1,
            scope,
            lock,
            directory: directory.id,
            parent: directory.parent_id()?,
            socket,
            native: native.clone(),
            helper: ProcessIdentity::capture(std::process::id())?,
        })
    }
}
