//! Conversation UUID selects the original managed owner, never the caller PID.
use super::{LaunchOwners, ReadyAuthorization, Registry, readiness::same_context_scope};
use crate::claude::metadata::{
    self,
    directory::{Directory, Snapshot},
};
use agentchat_core::{
    Error, ErrorCode, Result,
    model::{Context, LaunchOwnerState},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub(super) fn key(context: &Context) -> Result<String> {
    let encoded = serde_json::to_vec(&(context.project_id, context.host, &context.host_session_id))
        .map_err(|_| required())?;
    Ok(format!(
        "scope-{}",
        Sha256::digest(encoded)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    ))
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Lock {
    version: u32,
    key: String,
    nonce: Uuid,
}
impl Registry {
    /// Read the original managed authorization by exact project/host/native UUID.
    /// A refreshed native MCP descriptor need not already have a participation
    /// binding. This read neither publishes its descriptor nor joins/restores it.
    /// Both original physical indexes still prove the selected immutable record;
    /// callers must independently check current original owner/supervisor state.
    pub fn authorization(&self, context: &Context) -> Result<Option<ReadyAuthorization>> {
        let Some(value) = self.read_ready(&key(context)?)? else {
            return Ok(None);
        };
        if !same_context_scope(context, &value.binding().context) {
            return Err(required());
        }
        let Some(committed) = self.readiness(value.binding())? else {
            return Ok(None);
        };
        if committed != value {
            return Err(required());
        }
        Ok(Some(value))
    }
    pub(super) fn index_ready_scope(&self, value: &ReadyAuthorization) -> Result<()> {
        let binding = value.binding();
        let key = key(&binding.context)?;
        let directory = self.data_directory("ready", true)?.ok_or_else(required)?;
        let lock_name = format!("{key}.lock");
        let snapshot = self.scope_lock(&directory, &key, &lock_name)?;
        let _held = directory.lock_record(lock_name.as_ref(), &snapshot)?;
        let name = format!("{key}.json");
        if let Some(old) = self.read_ready(&key)? {
            if old == *value {
                return metadata::sync_input(&directory.path.join(&name), value);
            }
            // A second live/unconfirmed managed owner cannot acquire this UUID.
            // The original supervisor may replace only its own exactly departed
            // participation generation; a fresh generation is what makes a
            // correlated `/new` -> `/resume` return distinct from revival.
            let old_binding = old.binding();
            let same_physical_scope = old_binding.identity.owner == binding.identity.owner
                && old_binding.endpoint == binding.endpoint
                && same_context_scope(&old_binding.context, &binding.context);
            let departed = self.root_departed(
                old_binding.context.project_id,
                old_binding.context.host,
                &old_binding.context.host_session_id,
                &old_binding.identity,
            );
            if LaunchOwners::observe(&old_binding.identity) != LaunchOwnerState::Gone
                && !(same_physical_scope && departed)
            {
                return Err(required());
            }
        }
        if !LaunchOwners::is_supervisor(&binding.identity)
            || !directory.matches(lock_name.as_ref(), &snapshot)?
        {
            return Err(required());
        }
        let publisher = metadata::Publisher::new(&self.home).guard(lock_name);
        let path = directory.path.join(&name);
        if directory.read(name.as_ref())?.is_some() {
            publisher.write(&path, value)
        } else {
            publisher.create(&path, value)
        }
    }
    fn scope_lock(&self, directory: &Directory, key: &str, name: &str) -> Result<Snapshot> {
        if directory.read(name.as_ref())?.is_none() {
            let value = Lock {
                version: 1,
                key: key.into(),
                nonce: Uuid::new_v4(),
            };
            if let Err(error) =
                metadata::Publisher::new(&self.home).create(&directory.path.join(name), &value)
                && directory.read(name.as_ref())?.is_none()
            {
                return Err(error);
            }
        }
        let snapshot = directory.read(name.as_ref())?.ok_or_else(required)?;
        let value: Lock = metadata::decode_proved(directory, name.as_ref(), &snapshot)?;
        if value.version != 1 || value.key != key || value.nonce.is_nil() {
            return Err(required());
        }
        Ok(snapshot)
    }
}
fn required() -> Error {
    Error::new(
        ErrorCode::SessionRequired,
        "The original managed conversation authorization is unavailable or changed.",
    )
}
