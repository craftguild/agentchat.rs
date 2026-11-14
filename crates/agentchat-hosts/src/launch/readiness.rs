//! Original launch readiness releases input without creating participation.
use super::{Binding, LaunchOwners, Registry};
use crate::claude::{metadata, process::ProcessIdentity};
use agentchat_core::{Error, ErrorCode, Result, validation};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Immutable native startup authorization, published by the actual supervisor.
/// This proves no membership. Every activation still checks original processes,
/// native scope and shared application policy; a retained file cannot renew it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadyAuthorization {
    version: u32,
    binding: Binding,
    control: PathBuf,
    preferred_name: Option<String>,
}
impl ReadyAuthorization {
    pub(super) fn matches_key(&self, key: &str) -> Result<bool> {
        self.valid()?;
        Ok(key == self.binding.identity.generation.to_string()
            || key == super::readiness_scope::key(&self.binding.context)?
            || key
                == owner_key(
                    &LaunchOwners::native_owner(&self.binding.identity)?,
                    &self.binding.context.host_session_id,
                )?)
    }
    /// Original exact native context and immutable physical launch identity.
    pub fn binding(&self) -> &Binding {
        &self.binding
    }
    /// Private activation endpoint; clients additionally verify kernel peers.
    pub fn control(&self) -> &Path {
        &self.control
    }
    /// Optional launcher-selected default for the first explicit Join.
    pub fn preferred_name(&self) -> Option<&str> {
        self.preferred_name.as_deref()
    }
    /// Compare a kernel-observed peer PID with the original live supervisor.
    /// A supplied request PID is never evidence for this check.
    pub fn supervisor_peer(&self, pid: u32) -> bool {
        LaunchOwners::native_owner(&self.binding.identity)
            .is_ok_and(|owner| LaunchOwners::supervisor_peer(&self.binding.identity, &owner, pid))
    }
    pub(super) fn valid(&self) -> Result<()> {
        self.binding.validate()?;
        let root =
            Uuid::parse_str(&self.binding.context.host_session_id).map_err(|_| required())?;
        if root.is_nil()
            || root.to_string() != self.binding.context.host_session_id
            || self.version != 1
            || !self.control.is_absolute()
            || self.control.as_os_str().as_encoded_bytes().len() >= 100
            || self
                .preferred_name
                .as_deref()
                .is_some_and(|s| validation::handle(s).is_err())
        {
            return Err(required());
        }
        LaunchOwners::native_owner(&self.binding.identity)?;
        Ok(())
    }
}

pub(super) fn owner_key(owner: &ProcessIdentity, root: &str) -> Result<String> {
    let bytes = serde_json::to_vec(&(owner, root)).map_err(|_| required())?;
    Ok(Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}
fn same_scope(a: &Binding, b: &Binding) -> bool {
    a.identity.owner == b.identity.owner
        && a.endpoint == b.endpoint
        && same_context_scope(&a.context, &b.context)
}
pub(super) fn same_context_scope(
    a_ctx: &agentchat_core::model::Context,
    b_ctx: &agentchat_core::model::Context,
) -> bool {
    a_ctx.project_id == b_ctx.project_id
        && a_ctx.host == b_ctx.host
        && a_ctx.host_session_id == b_ctx.host_session_id
        && a_ctx.host_version == b_ctx.host_version
        && a_ctx.os_profile == b_ctx.os_profile
        && a_ctx.working_directory == b_ctx.working_directory
}

impl Registry {
    /// Record actual startup readiness, independently of the membership writer.
    /// Both exact proved indexes must exist before readers release native input.
    /// Retries synchronize the same original files, never replace their intent.
    pub fn publish_ready(
        &self,
        binding: &Binding,
        control: PathBuf,
        preferred_name: Option<String>,
    ) -> Result<ReadyAuthorization> {
        let value = ReadyAuthorization {
            version: 1,
            binding: binding.clone(),
            control,
            preferred_name,
        };
        value.valid()?;
        if !LaunchOwners::is_supervisor(&binding.identity)
            || self.resolve(&binding.context)?.as_ref() != Some(binding)
        {
            return Err(required());
        }
        let directory = self.data_directory("ready", true)?.ok_or_else(required)?;
        let owner = LaunchOwners::native_owner(&binding.identity)?;
        // Commit the physical-generation index last. Native input readers use
        // it as the historical readiness barrier, independently of later scope
        // replacement after supervisor loss. No membership is implied.
        let owner_key = owner_key(&owner, &binding.context.host_session_id)?;
        match self.read_ready(&owner_key)? {
            Some(original)
                if original.binding.identity.owner == binding.identity.owner
                    && same_context_scope(&original.binding.context, &binding.context) => {}
            Some(_) => return Err(required()),
            None => self.write_ready(&directory, &owner_key, &value)?,
        }
        self.index_ready_scope(&value)?;
        self.write_ready(&directory, &binding.identity.generation.to_string(), &value)?;
        Ok(value)
    }

    fn write_ready(
        &self,
        directory: &crate::claude::metadata::directory::Directory,
        key: &str,
        value: &ReadyAuthorization,
    ) -> Result<()> {
        let path = directory.path.join(format!("{key}.json"));
        if let Some(snapshot) = directory.read(path.file_name().ok_or_else(required)?)? {
            let old: ReadyAuthorization = metadata::decode_proved(
                directory,
                path.file_name().ok_or_else(required)?,
                &snapshot,
            )?;
            if old != *value {
                return Err(required());
            }
            metadata::sync_input(&path, &old)
        } else {
            metadata::Publisher::new(&self.home).create(&path, value)
        }
    }

    /// Read the original readiness for a current or retired participation route.
    /// New participation contexts/generations may share only the same original
    /// owner proof and exact project/native scope. Reading grants no liveness.
    pub fn readiness(&self, binding: &Binding) -> Result<Option<ReadyAuthorization>> {
        binding.validate()?;
        let owner = LaunchOwners::native_owner(&binding.identity)?;
        let exact = self.read_ready(&binding.identity.generation.to_string())?;
        let value = match exact {
            Some(value) => value,
            None => {
                let Some(value) =
                    self.read_ready(&owner_key(&owner, &binding.context.host_session_id)?)?
                else {
                    return Ok(None);
                };
                value
            }
        };
        if !same_scope(binding, &value.binding) {
            return Err(required());
        }
        if self
            .read_ready(&value.binding.identity.generation.to_string())?
            .as_ref()
            != Some(&value)
        {
            return Err(required());
        }
        Ok(Some(value))
    }

    pub(super) fn readiness_for_scope(
        &self,
        generation: Uuid,
    ) -> Result<Option<ReadyAuthorization>> {
        let Some(value) = self.read_ready(&generation.to_string())? else {
            return Ok(None);
        };
        if value.binding.identity.generation != generation
            || self.readiness(&value.binding)?.as_ref() != Some(&value)
        {
            return Err(required());
        }
        Ok(Some(value))
    }
    pub(super) fn read_ready(&self, key: &str) -> Result<Option<ReadyAuthorization>> {
        let Some(directory) = self.data_directory("ready", false)? else {
            return Ok(None);
        };
        let name = format!("{key}.json");
        let Some(snapshot) = directory.read(name.as_ref())? else {
            return Ok(None);
        };
        let value: ReadyAuthorization =
            metadata::decode_proved(&directory, name.as_ref(), &snapshot)?;
        value.valid()?;
        Ok(Some(value))
    }
}
fn required() -> Error {
    Error::new(
        ErrorCode::SessionRequired,
        "The original managed launch readiness is unavailable or changed.",
    )
}
