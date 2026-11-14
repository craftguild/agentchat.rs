//! Explicit-root routing; current original kernel owners grant availability.
mod directory;
use super::required;
use crate::{
    claude::metadata::{self, Publisher},
    launch::{Binding, Endpoint, LaunchOwners, Registry},
};
use agentchat_core::{
    Error, ErrorCode, Result,
    model::{Context, HostKind, LaunchOwnerState},
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

/// One supervisor-published exact-root association. Version 2 uses the actual
/// designated process, not a native traffic relay or observer socket. Retained
/// version 1 data is readable only to refuse adoption or prove prior owner loss.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeRoute {
    version: u32,
    binding: Binding,
    revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    observer: Option<serde_json::Value>,
}
impl NativeRoute {
    /// Original complete native descriptor and physical launch association.
    pub fn binding(&self) -> &Binding {
        &self.binding
    }
    /// Monotonic metadata revision for immutable context replacement only.
    pub fn revision(&self) -> u64 {
        self.revision
    }
    fn validate(&self, root: Uuid) -> Result<()> {
        let b = &self.binding;
        if self.revision == 0
            || b.context.host != HostKind::Codex
            || b.context.host_session_id != root.to_string()
            || !matches!(b.endpoint, Endpoint::Codex { .. })
            || Binding::new(b.context.clone(), b.identity.clone(), b.endpoint.clone())? != *b
            || !matches!((self.version, &self.observer), (2, None) | (1, Some(_)))
        {
            return Err(required());
        }
        let Endpoint::Codex { executable, .. } = &b.endpoint else {
            return Err(required());
        };
        if LaunchOwners::native_owner(&b.identity)?.executable() != *executable {
            return Err(required());
        }
        Ok(())
    }
    fn direct(&self) -> Result<()> {
        if self.version != 2 {
            return Err(legacy(&self.binding));
        }
        Ok(())
    }
}

/// O(1) explicit-root lookup. Reads create no directory, runtime or participant.
/// Mutable publication is serialized under the original root lock. Metadata,
/// successful native I/O and shared-server/helper liveness never renew an owner.
#[derive(Clone)]
pub struct NativeRegistry {
    home: PathBuf,
}
impl NativeRegistry {
    /// Capture only the explicit AGENTCHAT state home.
    pub fn new(home: impl Into<PathBuf>) -> Self {
        Self { home: home.into() }
    }
    /// Read a current-format locator, independently of process availability.
    /// Old proxy-bound records require complete exit and a fresh managed launch;
    /// a still-live old PID cannot silently acquire the direct authority model.
    pub fn lookup(&self, root: &str) -> Result<Option<NativeRoute>> {
        let route = self.read(root_id(root)?)?;
        if let Some(route) = &route {
            route.direct()?;
        }
        Ok(route)
    }
    /// Refuse a new managed owner while a prior association is current or
    /// unconfirmed. This is a read-only early guard; publication repeats the
    /// decision under the root lock to fence concurrent launchers.
    pub fn check_available(&self, root: &str) -> Result<()> {
        if let Some(old) = self.read(root_id(root)?)?
            && LaunchOwners::observe(&old.binding.identity) != LaunchOwnerState::Gone
        {
            return Err(if old.version == 1 {
                legacy(&old.binding)
            } else {
                required()
            });
        }
        Ok(())
    }
    fn read(&self, root: Uuid) -> Result<Option<NativeRoute>> {
        let Some(directory) = directory::open(&self.home, false)? else {
            return Ok(None);
        };
        let name = format!("{root}.json");
        let Some(snapshot) = directory.read(name.as_ref())? else {
            return Ok(None);
        };
        let route: NativeRoute = metadata::decode_proved(&directory, name.as_ref(), &snapshot)?;
        route.validate(root)?;
        Ok(Some(route))
    }
    /// Recheck the immutable exact context and both original kernel owners.
    /// No displayed-root observation or Esc latch exists: switching is unsupported
    /// and cannot move this binding. Native phase/approval facts remain separate.
    pub fn observe(&self, context: &Context) -> Result<NativeRoute> {
        let route = self
            .lookup(&context.host_session_id)?
            .ok_or_else(required)?;
        if route.binding.context != *context
            || Registry::new(&self.home).resolve(context)?.as_ref() != Some(&route.binding)
            || LaunchOwners::observe(&route.binding.identity) != LaunchOwnerState::Current
            || Registry::new(&self.home).root_changed(
                context.project_id,
                HostKind::Codex,
                &context.host_session_id,
                &route.binding.identity,
            )
        {
            return Err(required());
        }
        Ok(route)
    }
    /// Publish/retry only from the actual live supervisor after exact native
    /// readiness. One retained context is the publication intent across uncertain
    /// writes. A same-original-owner replacement preserves project/root/endpoint;
    /// another launch may replace a locator only after positive prior owner loss.
    pub fn publish(&self, binding: &Binding) -> Result<NativeRoute> {
        let root = root_id(&binding.context.host_session_id)?;
        if !LaunchOwners::is_supervisor(&binding.identity) {
            return Err(required());
        }
        let mut route = NativeRoute {
            version: 2,
            binding: binding.clone(),
            revision: 1,
            observer: None,
        };
        route.validate(root)?;
        let directory = directory::open(&self.home, true)?.ok_or_else(required)?;
        let lock_name = format!("{root}.lock");
        let lock = directory::root_lock(&self.home, &directory, root, &lock_name)?;
        let _held = directory.lock_record(lock_name.as_ref(), &lock)?;
        if let Some(old) = self.read(root)? {
            if old.binding.identity.owner == binding.identity.owner {
                old.direct()?;
                if !same_scope(&old.binding.context, &binding.context)
                    || old.binding.endpoint != binding.endpoint
                {
                    return Err(required());
                }
                if old.binding == *binding {
                    metadata::sync_input(&directory.path.join(format!("{root}.json")), &old)?;
                    return Ok(old);
                }
            } else if LaunchOwners::observe(&old.binding.identity) != LaunchOwnerState::Gone {
                return Err(if old.version == 1 {
                    legacy(&old.binding)
                } else {
                    required()
                });
            }
            route.revision = old.revision.checked_add(1).ok_or_else(required)?;
        }
        Registry::new(&self.home).publish(binding)?;
        if !LaunchOwners::is_supervisor(&binding.identity)
            || !directory.matches(lock_name.as_ref(), &lock)?
        {
            return Err(required());
        }
        let path = directory.path.join(format!("{root}.json"));
        let publisher = Publisher::new(&self.home).guard(lock_name);
        if directory
            .read(path.file_name().ok_or_else(required)?)?
            .is_some()
        {
            publisher.write(&path, &route)?;
        } else {
            publisher.create(&path, &route)?;
        }
        Ok(route)
    }
}
fn same_scope(a: &Context, b: &Context) -> bool {
    a.project_id == b.project_id
        && a.host == b.host
        && a.host_session_id == b.host_session_id
        && a.host_version == b.host_version
        && a.os_profile == b.os_profile
        && a.working_directory == b.working_directory
}
fn root_id(root: &str) -> Result<Uuid> {
    let id = Uuid::parse_str(root).map_err(|_| required())?;
    if id.is_nil() || root != id.to_string() {
        return Err(required());
    }
    Ok(id)
}
fn legacy(binding: &Binding) -> Error {
    let mut error = Error::new(
        ErrorCode::UnsupportedProtocol,
        "This Codex launch uses a retained proxy-bound route. Exit its managed CLI completely, then use a fresh agentchat launch codex --resume EXACT_ID.",
    );
    error.details = serde_json::json!({
        "host":"codex", "reason":"legacy_proxy_launch",
        "required_action":"exit_managed_cli_then_fresh_managed_resume",
        "owner_state": match LaunchOwners::observe(&binding.identity) {
            LaunchOwnerState::Current => "current",
            LaunchOwnerState::Gone => "gone",
            LaunchOwnerState::Unknown => "unknown",
        },
    });
    error
}

#[cfg(test)]
#[path = "../../tests/unit/codex_direct_registry.rs"]
mod tests;
