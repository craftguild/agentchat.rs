//! Original-proved immutable routing facts. A record alone grants no authority.
use super::LaunchOwners;
use crate::claude::metadata::{self, Publisher, directory::Directory};
use agentchat_core::{
    Error, ErrorCode, Result,
    model::{Context, HostKind, LaunchIdentity},
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
};

/// Native route pinned by the supervisor after the host startup handshake.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "host", rename_all = "snake_case", deny_unknown_fields)]
pub enum Endpoint {
    /// Existing shared app-server and actual native binary, never another server.
    Codex {
        /// Exact existing local control socket.
        socket: PathBuf,
        /// Actual matching native CLI selected for the child.
        executable: PathBuf,
    },
    /// The existing exact-context native owner/observer route supplies freshness.
    Claude,
}
/// One immutable supervisor-published native launch/context association.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    version: u32,
    /// Complete immutable descriptor; no lookup infers it from a directory/PID.
    pub context: Context,
    /// Original kernel owner/supervisor proof and fresh generation.
    pub identity: LaunchIdentity,
    /// Exact native control transport.
    pub endpoint: Endpoint,
}
impl Binding {
    /// Assemble metadata after independently verifying the exact native startup
    /// handshake. This constructor supplies no root, trust, liveness or admission
    /// evidence; publication additionally requires the actual live supervisor.
    pub fn new(context: Context, identity: LaunchIdentity, endpoint: Endpoint) -> Result<Self> {
        let value = Self {
            version: 1,
            context,
            identity,
            endpoint,
        };
        value.validate()?;
        Ok(value)
    }
    pub(super) fn validate(&self) -> Result<()> {
        let c = &self.context;
        if self.version != 1
            || [
                c.project_id,
                c.context_id,
                c.incarnation,
                self.identity.generation,
            ]
            .into_iter()
            .any(|id| id.is_nil())
            || c.host_session_id.is_empty()
            || c.host_session_id.len() > 1024
            || c.host_version.is_empty()
            || c.host_version.len() > 128
            || c.os_profile.is_empty()
            || c.os_profile.len() > 128
            || !c.working_directory.is_absolute()
            || self.identity.owner.is_empty()
            || self.identity.owner.len() > 32_768
        {
            return Err(required());
        }
        match (&self.endpoint, c.host) {
            (Endpoint::Codex { socket, executable }, HostKind::Codex)
                if socket.is_absolute() && executable.is_absolute() =>
            {
                Ok(())
            }
            (Endpoint::Claude, HostKind::Claude) => Ok(()),
            _ => Err(required()),
        }
    }
}
/// O(1) immutable launch-binding lookup under the explicit private AGENTCHAT state root.
/// Construction and resolution create no files and perform no native operation.
#[derive(Clone)]
pub struct Registry {
    pub(super) home: PathBuf,
}
impl Registry {
    /// Capture the exact state root without probing or changing it.
    pub fn new(home: impl Into<PathBuf>) -> Self {
        Self { home: home.into() }
    }

    /// Publish once from the original supervisor after native startup proof.
    /// Canonical metadata carries original parent/inode/full-byte provenance.
    /// Exact retry synchronizes that same original file and parent; foreign,
    /// copied, aliased, legacy or edited records can never be adopted or replaced.
    pub fn publish(&self, binding: &Binding) -> Result<()> {
        binding.validate()?;
        if !LaunchOwners::is_supervisor(&binding.identity) {
            return Err(required());
        }
        let parent = self.directory(true)?.ok_or_else(required)?;
        let name = format!("{}.json", binding.context.context_id);
        if let Some(snapshot) = parent.read(name.as_ref())? {
            let old: Binding = metadata::decode_proved(&parent, name.as_ref(), &snapshot)?;
            if old != *binding {
                return Err(required());
            }
            let file = parent.lock_record(name.as_ref(), &snapshot)?;
            file.sync_all().map_err(|_| required())?;
            parent.sync()?;
            if !parent.matches(name.as_ref(), &snapshot)? {
                return Err(required());
            }
        } else {
            Publisher::new(&self.home).create(&parent.path.join(&name), binding)?;
        }
        Ok(())
    }

    /// Read only the original canonical binding for this exact descriptor.
    /// It remains readable after process exit so the application can reconcile
    /// positive loss; readers must independently recheck kernel/native facts.
    pub fn resolve(&self, context: &Context) -> Result<Option<Binding>> {
        let Some(parent) = self.directory(false)? else {
            return Ok(None);
        };
        let name = format!("{}.json", context.context_id);
        let Some(snapshot) = parent.read(name.as_ref())? else {
            return Ok(None);
        };
        let binding: Binding = metadata::decode_proved(&parent, name.as_ref(), &snapshot)?;
        binding.validate()?;
        if binding.context != *context {
            return Err(required());
        }
        Ok(Some(binding))
    }

    fn directory(&self, create: bool) -> Result<Option<Directory>> {
        self.data_directory("bindings", create)
    }
    pub(super) fn data_directory(&self, kind: &str, create: bool) -> Result<Option<Directory>> {
        if !matches!(
            kind,
            "bindings"
                | "root-changes"
                | "root-transitions"
                | "started"
                | "ready"
                | "claude-profiles"
        ) {
            return Err(required());
        }
        let mut path = self.home.clone();
        if !exists(&path)? {
            return if create { Err(required()) } else { Ok(None) };
        }
        metadata::private_directory(&path)?;
        for part in ["hosts", "launches", kind] {
            let original_parent = if create {
                Some(Directory::owned(&path)?)
            } else {
                None
            };
            path.push(part);
            if create {
                match fs::DirBuilder::new().mode(0o700).create(&path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(_) => return Err(required()),
                }
            }
            if !exists(&path)? {
                return Ok(None);
            }
            metadata::private_directory(&path)?;
            if let Some(parent) = original_parent {
                // Exact retries close a crash after mkdir but before its parent
                // sync. File publication cannot make an unsynced ancestor durable.
                Directory::owned(&path)?.sync()?;
                parent.sync()?;
            }
        }
        Directory::owned(&path).map(Some)
    }
}
fn exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(required()),
    }
}
fn required() -> Error {
    Error::new(
        ErrorCode::SessionRequired,
        "The original exact launch binding is unavailable or changed.",
    )
}
