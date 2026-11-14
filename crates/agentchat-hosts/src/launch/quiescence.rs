//! Bounded read-only removal checks against original managed process provenance.

use super::{
    Binding, LaunchOwners, ReadyAuthorization, Registry, root_change::Changed, startup::Started,
};
use crate::claude::{metadata, process::BirthState};
use agentchat_core::{Error, ErrorCode, Result};
use serde::Deserialize;
use std::time::{Duration, Instant};
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopeLock {
    version: u32,
    key: String,
    nonce: Uuid,
}

impl Registry {
    /// Refuse live or uncertain relevant original managed owners without
    /// starting, signalling or inferring ownership of ordinary native processes.
    /// Missing records are absence of evidence, not a busy managed session.
    pub fn require_quiescent(&self, project: Option<Uuid>) -> Result<()> {
        match std::fs::symlink_metadata(self.home.join("hosts/launches")) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(_) => return Err(busy("launches")),
            Ok(_) => (),
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut visited = 0usize;
        for (index, kind) in ["bindings", "root-changes", "started", "ready"]
            .into_iter()
            .enumerate()
        {
            let Some(directory) = self.data_directory(kind, false).map_err(|_| busy(kind))? else {
                continue;
            };
            for entry in std::fs::read_dir(&directory.path).map_err(|_| busy(kind))? {
                visited += 1;
                if visited > 65_536 || Instant::now() >= deadline {
                    return Err(busy(kind));
                }
                let name = entry.map_err(|_| busy(kind))?.file_name();
                let snapshot = directory
                    .read(&name)
                    .map_err(|_| busy(kind))?
                    .ok_or_else(|| busy(kind))?;
                let name_text = name.to_str().ok_or_else(|| busy(kind))?;
                if index == 3
                    && let Some(key) = name_text.strip_suffix(".lock")
                {
                    let lock: ScopeLock = metadata::decode_proved(&directory, &name, &snapshot)
                        .map_err(|_| busy(kind))?;
                    if lock.version != 1
                        || lock.key != key
                        || lock.nonce.is_nil()
                        || !key.strip_prefix("scope-").is_some_and(|s| {
                            s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
                        })
                    {
                        return Err(busy(kind));
                    }
                    // A proved scope record supplies the UUID hidden by its
                    // hashed lock key. Another project's ready publication is
                    // irrelevant to local removal; unknown scope stays guarded.
                    let scoped = format!("{key}.json");
                    if let Some(project) = project
                        && let Some(snapshot) =
                            directory.read(scoped.as_ref()).map_err(|_| busy(kind))?
                    {
                        let ready: ReadyAuthorization =
                            metadata::decode_proved(&directory, scoped.as_ref(), &snapshot)
                                .map_err(|_| busy(kind))?;
                        if !ready.matches_key(key).map_err(|_| busy(kind))? {
                            return Err(busy(kind));
                        }
                        if ready.binding().context.project_id != project {
                            continue;
                        }
                    }
                    let _held = directory
                        .lock_record(&name, &snapshot)
                        .map_err(|_| busy(kind))?;
                    continue;
                }
                let key = name_text.strip_suffix(".json").ok_or_else(|| busy(kind))?;
                let binding = match index {
                    0 => {
                        let value: Binding = metadata::decode_proved(&directory, &name, &snapshot)
                            .map_err(|_| busy(kind))?;
                        value.validate().map_err(|_| busy(kind))?;
                        if key != value.context.context_id.to_string() {
                            return Err(busy(kind));
                        }
                        value
                    }
                    1 => {
                        let value: Changed = metadata::decode_proved(&directory, &name, &snapshot)
                            .map_err(|_| busy(kind))?;
                        if !value.valid() || key != value.binding.identity.generation.to_string() {
                            return Err(busy(kind));
                        }
                        value.binding
                    }
                    2 => {
                        let value: Started = metadata::decode_proved(&directory, &name, &snapshot)
                            .map_err(|_| busy(kind))?;
                        if !value.valid() || key != value.binding.identity.generation.to_string() {
                            return Err(busy(kind));
                        }
                        value.binding
                    }
                    _ => {
                        let value: ReadyAuthorization =
                            metadata::decode_proved(&directory, &name, &snapshot)
                                .map_err(|_| busy(kind))?;
                        if !value.matches_key(key).map_err(|_| busy(kind))? {
                            return Err(busy(kind));
                        }
                        value.binding().clone()
                    }
                };
                if project.is_some_and(|id| id != binding.context.project_id) {
                    continue;
                }
                // A surviving original native child still consumes its loaded
                // readiness/input module even after supervisor loss. Keep that
                // exact managed record until positive original native birth loss.
                if LaunchOwners::native_owner(&binding.identity)
                    .map_err(|_| busy(kind))?
                    .birth_state()
                    != BirthState::Gone
                {
                    return Err(busy(kind));
                }
                if !directory
                    .matches(&name, &snapshot)
                    .map_err(|_| busy(kind))?
                {
                    return Err(busy(kind));
                }
            }
        }
        Ok(())
    }
}

fn busy(component: &str) -> Error {
    let mut error = Error::new(
        ErrorCode::Busy,
        "A relevant managed launch is active or its original ownership is uncertain. Exit that managed terminal normally, then retry uninstall; no native process was stopped.",
    );
    error.retryable = true;
    error.details =
        serde_json::json!({"reason":"managed_owner_not_quiescent","component":component});
    error
}
