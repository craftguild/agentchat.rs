//! A completed first-input barrier is historical fact, never ongoing authority.
use super::{Binding, LaunchOwners, Registry};
use crate::claude::{metadata, process::ProcessIdentity};
use agentchat_core::{Error, ErrorCode, Result, model::HostKind, sequence::Sequence};
use serde::{Deserialize, Serialize};
use std::path::Path;
use uuid::Uuid;

pub(super) const ROOT_ENV: &str = "AGENTCHAT_LAUNCH_ROOT";
pub(super) const GENERATION_ENV: &str = "AGENTCHAT_LAUNCH_GENERATION";

/// Launcher-selected pending scope. These locators alone never identify or
/// authorize a conversation, establish membership, or release a first input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupScope {
    /// Requested root, independently compared with live native SDK facts.
    pub root: Uuid,
    /// Fresh UUID selected before spawning this actual native child.
    pub generation: Uuid,
}
impl StartupScope {
    /// Read only the two native startup locators. Partial/malformed inherited
    /// settings fail closed. An ordinary host with neither locator is inert.
    pub fn from_environment() -> Result<Option<Self>> {
        let root = std::env::var_os(ROOT_ENV);
        let generation = std::env::var_os(GENERATION_ENV);
        match (root, generation) {
            (None, None) => Ok(None),
            (Some(root), Some(generation)) => {
                let parse = |value: std::ffi::OsString| {
                    value
                        .to_str()
                        .and_then(|s| Uuid::parse_str(s).ok())
                        .filter(|v| !v.is_nil())
                        .ok_or_else(required)
                };
                Ok(Some(Self {
                    root: parse(root)?,
                    generation: parse(generation)?,
                }))
            }
            _ => Err(required()),
        }
    }
}
/// First-input progress only. None of these states authorizes an AGENTCHAT operation,
/// body, heartbeat, join, reconnection or automatic-wake capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupState {
    /// The original native launch readiness has not been durably recorded yet.
    Waiting,
    /// Original-child readiness recorded; native input may continue unjoined.
    Released,
    /// A new root remains ordinary/unjoined, without inheriting the barrier.
    OtherRoot,
    /// An ordinary different CLI inherited locators, not launch ownership.
    OtherOwner,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Started {
    version: u32,
    pub(super) binding: Binding,
    boundary: Sequence,
}
impl Started {
    pub(super) fn valid(&self) -> bool {
        self.version == 1
            && self.binding.validate().is_ok()
            && matches!(
                self.binding.context.host,
                HostKind::Claude | HostKind::Codex
            )
            && Uuid::parse_str(&self.binding.context.host_session_id).is_ok_and(|id| !id.is_nil())
            && LaunchOwners::native_owner(&self.binding.identity).is_ok()
    }
}
impl Registry {
    /// Whether a fresh project lifecycle belongs to the original managed owner
    /// after its exact initial generation has positively departed. This only
    /// keeps the native restoration path pending; it creates no descriptor,
    /// membership, readiness record or delivery authority.
    pub fn managed_transition_pending(
        &self,
        scope: &StartupScope,
        owner: &ProcessIdentity,
        target: &agentchat_core::model::Context,
    ) -> Result<bool> {
        if scope.root.is_nil()
            || scope.generation.is_nil()
            || !owner.is_continuing()
            || target.host != HostKind::Claude
            || !Uuid::parse_str(&target.host_session_id).is_ok_and(|root| !root.is_nil())
        {
            return Ok(false);
        }
        let Some(ready) = self.readiness_for_scope(scope.generation)? else {
            return Ok(false);
        };
        let original = ready.binding();
        Ok(LaunchOwners::native_owner(&original.identity)? == *owner
            && LaunchOwners::observe(&original.identity)
                == agentchat_core::model::LaunchOwnerState::Current
            && original.context.project_id == target.project_id
            && original.context.host == target.host
            && original.context.host_version == target.host_version
            && original.context.os_profile == target.os_profile
            && original.context.working_directory == target.working_directory
            && self.root_changed(
                original.context.project_id,
                original.context.host,
                &original.context.host_session_id,
                &original.identity,
            ))
    }

    /// Whether this original generation has a proved post-commit startup record.
    /// A renewed context may differ in descriptor/incarnation but must retain
    /// the same launch, project, exact native root and selected scope. This is
    /// historical ordering only; current owners and application leave/recovery
    /// checks still independently authorize every operation.
    pub fn was_started(&self, binding: &Binding) -> Result<bool> {
        let Some(directory) = self.data_directory("started", false)? else {
            return Ok(false);
        };
        let name = format!("{}.json", binding.identity.generation);
        let Some(snapshot) = directory.read(name.as_ref())? else {
            return Ok(false);
        };
        let value: Started = metadata::decode_proved(&directory, name.as_ref(), &snapshot)?;
        let old = &value.binding;
        if !value.valid()
            || old.identity != binding.identity
            || old.endpoint != binding.endpoint
            || old.context.project_id != binding.context.project_id
            || old.context.host != binding.context.host
            || old.context.host_session_id != binding.context.host_session_id
            || old.context.host_version != binding.context.host_version
            || old.context.os_profile != binding.context.os_profile
            || old.context.working_directory != binding.context.working_directory
        {
            return Err(required());
        }
        Ok(true)
    }

    /// Publish only after shared `start_launch` has committed this binding's
    /// exact generation and returned its join boundary. This is a first-input
    /// release record, not a replacement for current application authorization.
    /// Only the original live supervisor may publish or retry the same marker.
    pub fn mark_started(&self, binding: &Binding, boundary: Sequence) -> Result<()> {
        let value = Started {
            version: 1,
            binding: binding.clone(),
            boundary,
        };
        if !value.valid()
            || !LaunchOwners::is_supervisor(&binding.identity)
            || self.resolve(&binding.context)?.as_ref() != Some(binding)
            || self.root_changed(
                binding.context.project_id,
                binding.context.host,
                &binding.context.host_session_id,
                &binding.identity,
            )
        {
            return Err(required());
        }
        let directory = self.data_directory("started", true)?.ok_or_else(required)?;
        let path = directory
            .path
            .join(format!("{}.json", binding.identity.generation));
        if std::fs::symlink_metadata(&path).is_ok() {
            let old: Started = metadata::read_proved(&path)?;
            if old != value {
                return Err(required());
            }
            return metadata::sync_input(&path, &old);
        }
        metadata::Publisher::new(&self.home).create(&path, &value)
    }
    /// Compare live native facts and the actual direct parent's full kernel
    /// identity with the original readiness record. Missing means waiting;
    /// copied/edited/foreign metadata is never a successful release. Supervisor
    /// loss, explicit leave or a later root switch cannot undo a completed
    /// barrier or block ordinary native chat; AGENTCHAT authority is checked elsewhere.
    pub fn startup_state(
        &self,
        scope: &StartupScope,
        owner: &ProcessIdentity,
        root: Uuid,
        cwd: &Path,
        version: &str,
    ) -> Result<StartupState> {
        if scope.root.is_nil()
            || scope.generation.is_nil()
            || root.is_nil()
            || !owner.is_continuing()
        {
            return Err(required());
        }
        if root != scope.root {
            return Ok(StartupState::OtherRoot);
        }
        let Some(value) = self.readiness_for_scope(scope.generation)? else {
            return Ok(StartupState::Waiting);
        };
        let binding = value.binding();
        if binding.context.host_session_id != scope.root.to_string() {
            return Err(required());
        }
        if LaunchOwners::native_owner(&binding.identity)? != *owner {
            return Ok(StartupState::OtherOwner);
        }
        if binding.context.working_directory != cwd || binding.context.host_version != version {
            return Err(required());
        }
        if !self.was_started(binding)? {
            return Ok(StartupState::Waiting);
        }
        Ok(StartupState::Released)
    }
}
fn required() -> Error {
    Error::new(
        ErrorCode::SessionRequired,
        "The original native startup barrier is unavailable or changed.",
    )
}
