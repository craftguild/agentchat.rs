//! A positive native-root departure permanently fences one exact AGENTCHAT launch.
use super::{Binding, LaunchOwners, Registry};
use crate::claude::{metadata, process::ProcessIdentity};
use agentchat_core::{
    Error, ErrorCode, Result,
    model::{HostKind, LaunchIdentity, LaunchOwnerState},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Changed {
    version: u32,
    pub(super) binding: Binding,
    new_root: Uuid,
}
impl Changed {
    pub(super) fn valid(&self) -> bool {
        self.version == 1
            && self.binding.validate().is_ok()
            && matches!(
                self.binding.context.host,
                HostKind::Claude | HostKind::Codex
            )
            && Uuid::parse_str(&self.binding.context.host_session_id).is_ok_and(|id| !id.is_nil())
            && !self.new_root.is_nil()
            && self.binding.context.host_session_id != self.new_root.to_string()
            && LaunchOwners::native_owner(&self.binding.identity).is_ok()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    version: u32,
    binding: Binding,
    connection_epoch: u64,
    transition_epoch: u64,
}
impl Pending {
    fn valid(&self) -> bool {
        self.version == 1
            && self.binding.validate().is_ok()
            && self.binding.context.host == HostKind::Codex
            && self.connection_epoch > 0
            && self.transition_epoch > 0
            && LaunchOwners::native_owner(&self.binding.identity).is_ok()
    }
}
impl Registry {
    /// Fence delivery before the private Codex relay completes one observed
    /// transition request. A definite rollback may remove only this exact fact;
    /// transport uncertainty intentionally leaves it in place.
    pub fn record_managed_transition(
        &self,
        binding: &Binding,
        connection_epoch: u64,
        transition_epoch: u64,
    ) -> Result<()> {
        let value = Pending {
            version: 1,
            binding: binding.clone(),
            connection_epoch,
            transition_epoch,
        };
        if !value.valid() {
            return Err(required());
        }
        let path = self.pending_path(binding, true)?.ok_or_else(required)?;
        if std::fs::symlink_metadata(&path).is_ok() {
            let old: Pending = metadata::read_proved(&path)?;
            if old != value || !old.valid() {
                return Err(required());
            }
            return metadata::sync_input(&path, &old);
        }
        if self.readiness(binding)?.is_none()
            || !LaunchOwners::is_supervisor(&binding.identity)
            || LaunchOwners::observe(&binding.identity) != LaunchOwnerState::Current
        {
            return Err(required());
        }
        metadata::Publisher::new(&self.home).create(&path, &value)
    }

    /// Release only a definitely rejected/same-root transition. A missing or
    /// changed record never clears a newer or uncertain delivery fence.
    pub fn rollback_managed_transition(
        &self,
        binding: &Binding,
        connection_epoch: u64,
        transition_epoch: u64,
    ) -> Result<()> {
        let value = Pending {
            version: 1,
            binding: binding.clone(),
            connection_epoch,
            transition_epoch,
        };
        let path = self.pending_path(binding, false)?.ok_or_else(required)?;
        if !value.valid() || !metadata::remove_if(&path, &value)? {
            return Err(required());
        }
        Ok(())
    }

    /// Record a correlated Codex departure from the original live supervisor.
    /// The transparent relay supplies the correlated successful response after
    /// its native delivery. The pre-entry fence stays durable until activation.
    /// Generic callers cannot satisfy
    /// the exact supervisor/readiness checks or publish a replacement binding.
    pub fn record_managed_root_change(
        &self,
        binding: &Binding,
        new_root: Uuid,
        connection_epoch: u64,
        transition_epoch: u64,
    ) -> Result<()> {
        let value = Changed {
            version: 1,
            binding: binding.clone(),
            new_root,
        };
        let pending = Pending {
            version: 1,
            binding: binding.clone(),
            connection_epoch,
            transition_epoch,
        };
        if binding.context.host != HostKind::Codex || !value.valid() || !pending.valid() {
            return Err(required());
        }
        let pending_path = self.pending_path(binding, false)?;
        if let Some(directory) = self.data_directory("root-changes", false)? {
            let path = directory
                .path
                .join(format!("{}.json", binding.identity.generation));
            if std::fs::symlink_metadata(&path).is_ok() {
                let old: Changed = metadata::read_proved(&path)?;
                if !old.valid() || old != value {
                    return Err(required());
                }
                if let Some(pending_path) = pending_path.as_ref()
                    && std::fs::symlink_metadata(pending_path).is_ok()
                    && (!pending.valid()
                        || metadata::read_proved::<Pending>(pending_path)? != pending
                        || !metadata::remove_if(pending_path, &pending)?)
                {
                    return Err(required());
                }
                return metadata::sync_input(&path, &old);
            }
        }
        let pending_path = pending_path.ok_or_else(required)?;
        if self.readiness(binding)?.is_none()
            || metadata::read_proved::<Pending>(&pending_path)? != pending
            || !LaunchOwners::is_supervisor(&binding.identity)
            || LaunchOwners::observe(&binding.identity) != LaunchOwnerState::Current
        {
            return Err(required());
        }
        self.persist_change(&value)?;
        if !metadata::remove_if(&pending_path, &pending)? {
            return Err(required());
        }
        Ok(())
    }

    /// Record a positive departure only from the original native CLI's direct
    /// callback child. The trusted producer already obtained the new root from
    /// the live SDK and passed its native reservation floor. No generic tool,
    /// supervisor observation timeout or PID/cwd guess can produce this fact.
    /// The first proved departure wins for this participation generation. A
    /// later correlated return uses a fresh generation, so it cannot revive the
    /// departed launch. An exact retry resynchronizes original file/parent bytes.
    pub fn record_root_change(&self, binding: &Binding, new_root: Uuid) -> Result<()> {
        let value = Changed {
            version: 1,
            binding: binding.clone(),
            new_root,
        };
        if binding.context.host != HostKind::Claude
            || self.readiness(binding)?.is_none()
            || !value.valid()
        {
            return Err(required());
        }
        let owner = LaunchOwners::native_owner(&binding.identity)?;
        if ProcessIdentity::parent(&owner.executable())? != owner
            || LaunchOwners::observe(&binding.identity) != LaunchOwnerState::Current
        {
            return Err(required());
        }
        self.persist_change(&value)
    }
    fn persist_change(&self, value: &Changed) -> Result<()> {
        let binding = &value.binding;
        let directory = self
            .data_directory("root-changes", true)?
            .ok_or_else(required)?;
        let path = directory
            .path
            .join(format!("{}.json", binding.identity.generation));
        if std::fs::symlink_metadata(&path).is_ok() {
            let old: Changed = metadata::read_proved(&path)?;
            if !old.valid() || old.binding != *binding {
                return Err(required());
            }
            return metadata::sync_input(&path, &old);
        }
        // This immutable proof is small ordinary metadata. It changes no AGENTCHAT
        // database row or native authority, and never resubmits an input.
        metadata::Publisher::new(&self.home).create(&path, value)
    }
    /// Bounded local proof read, safe inside application reconciliation. This
    /// takes no owner-cursor lock, calls no native API, renews nothing and treats
    /// every absent/foreign/malformed record as unconfirmed rather than changed.
    pub fn root_changed(
        &self,
        project: Uuid,
        host: HostKind,
        root: &str,
        identity: &LaunchIdentity,
    ) -> bool {
        let work = || -> Result<bool> {
            if !matches!(host, HostKind::Claude | HostKind::Codex) {
                return Ok(false);
            }
            LaunchOwners::native_owner(identity)?;
            if let Some(path) = self.pending_path_for(identity.generation, false)?
                && let Ok(value) = metadata::read_proved::<Pending>(&path)
                && value.valid()
                && value.binding.identity == *identity
                && value.binding.context.project_id == project
                && value.binding.context.host == host
                && value.binding.context.host_session_id == root
            {
                return Ok(true);
            }
            Ok(self.root_departed(project, host, root, identity))
        };
        work().unwrap_or(false)
    }

    /// Read only the immutable positive departure record. Unlike
    /// [`Self::root_changed`], a provisional Codex delivery fence is false so
    /// callers cannot use a transition that may still roll back to authorize
    /// permanent retirement or a distinct-root activation.
    pub fn root_departed(
        &self,
        project: Uuid,
        host: HostKind,
        root: &str,
        identity: &LaunchIdentity,
    ) -> bool {
        let work = || -> Result<bool> {
            if !matches!(host, HostKind::Claude | HostKind::Codex) {
                return Ok(false);
            }
            LaunchOwners::native_owner(identity)?;
            let Some(directory) = self.data_directory("root-changes", false)? else {
                return Ok(false);
            };
            let path = directory.path.join(format!("{}.json", identity.generation));
            let value: Changed = metadata::read_proved(&path)?;
            Ok(value.valid()
                && value.binding.identity == *identity
                && value.binding.context.project_id == project
                && value.binding.context.host == host
                && value.binding.context.host_session_id == root)
        };
        work().unwrap_or(false)
    }

    fn pending_path(&self, binding: &Binding, create: bool) -> Result<Option<std::path::PathBuf>> {
        self.pending_path_for(binding.identity.generation, create)
    }

    fn pending_path_for(
        &self,
        generation: Uuid,
        create: bool,
    ) -> Result<Option<std::path::PathBuf>> {
        Ok(self
            .data_directory("root-transitions", create)?
            .map(|directory| directory.path.join(format!("{generation}.json"))))
    }
}
fn required() -> Error {
    Error::new(
        ErrorCode::SessionRequired,
        "The original native root-change proof is unavailable or changed.",
    )
}
