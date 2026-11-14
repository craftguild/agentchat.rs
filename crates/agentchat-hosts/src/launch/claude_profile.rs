//! One managed launch's immutable engine snapshot, never a PATH fallback.
use super::{LaunchOwners, NativeChild, Registry, StartupScope};
use crate::claude::{metadata, observer::NativeProfile, process::ProcessIdentity};
use agentchat_core::{Error, ErrorCode, Result, model::LaunchIdentity};
use serde::{Deserialize, Serialize};
use std::path::Path;
use uuid::Uuid;

/// Private dispatcher selector. It is not an executable path or native identity.
pub const MANAGED_CLAUDE: &str = "@managed";

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Selected {
    version: u32,
    root: Uuid,
    pub(super) identity: LaunchIdentity,
    profile: NativeProfile,
}

impl Selected {
    pub(super) fn owner(&self) -> Result<ProcessIdentity> {
        let owner = LaunchOwners::native_owner(&self.identity)?;
        if self.version != 1
            || self.root.is_nil()
            || self.profile.executable != owner.executable()
            || !crate::claude::module::candidate_version(&self.profile.version)
        {
            return Err(required());
        }
        Ok(owner)
    }
}

impl Registry {
    /// Pin the independently captured actual child after spawn and before native
    /// readiness. Only its original live supervisor may publish this snapshot.
    /// Publication does not release startup input or establish participation.
    pub fn publish_claude_profile(
        &self,
        child: &NativeChild,
        profile: &NativeProfile,
    ) -> Result<()> {
        let root = child.startup_root().ok_or_else(required)?;
        if !LaunchOwners::is_supervisor(child.identity())
            || !child.owner().is_current()
            || profile.executable != child.owner().executable()
            || !crate::claude::module::candidate_version(&profile.version)
        {
            return Err(required());
        }
        let value = Selected {
            version: 1,
            root,
            identity: child.identity().clone(),
            profile: profile.clone(),
        };
        let directory = self
            .data_directory("claude-profiles", true)?
            .ok_or_else(required)?;
        metadata::Publisher::new(&self.home).create(
            &directory
                .path
                .join(format!("{}.json", value.identity.generation)),
            &value,
        )
    }

    /// Read the exact launch snapshot. No discovery, replacement or repair is
    /// attempted when original ownership or metadata is missing/uncertain.
    pub fn selected_claude(
        &self,
        scope: &StartupScope,
    ) -> Result<(ProcessIdentity, NativeProfile)> {
        self.read_selected_claude(scope)?.ok_or_else(required)
    }

    fn read_selected_claude(
        &self,
        scope: &StartupScope,
    ) -> Result<Option<(ProcessIdentity, NativeProfile)>> {
        let Some(directory) = self.data_directory("claude-profiles", false)? else {
            return Ok(None);
        };
        let name = format!("{}.json", scope.generation);
        let Some(snapshot) = directory.read(name.as_ref())? else {
            return Ok(None);
        };
        let value: Selected = metadata::decode_proved(&directory, name.as_ref(), &snapshot)?;
        let owner = value.owner()?;
        if value.root != scope.root
            || value.identity.generation != scope.generation
            || !owner.is_continuing()
        {
            return Err(required());
        }
        // This locates the original native profile, not AgentChat authority.
        // Keep it after supervisor loss so the historical first-input barrier
        // can release ordinary native input. Core/readiness independently check
        // the original supervisor before participation or delivery is possible.
        Ok(Some((owner, value.profile)))
    }
}

/// Resolve the dispatcher only from this managed launch's original snapshot and
/// actual kernel ancestry. Inherited locators alone never grant authority.
pub async fn managed_claude(home: &Path, direct: bool) -> Result<(ProcessIdentity, NativeProfile)> {
    let scope = StartupScope::from_environment()?.ok_or_else(required)?;
    let registry = Registry::new(home);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    // A very fast child can start its helper before the supervisor captures
    // and publishes its image. Wait only for absence; malformed/foreign records
    // and uncertain ownership fail immediately rather than adopting PATH.
    let selected = loop {
        if let Some(selected) = registry.read_selected_claude(&scope)? {
            break selected;
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(required());
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    };
    if direct {
        selected.0.verify_parent()?;
    } else {
        selected.0.verify_ancestor()?;
    }
    Ok(selected)
}

fn required() -> Error {
    Error::new(
        ErrorCode::SessionRequired,
        "The original managed Claude engine snapshot is unavailable or changed.",
    )
}
