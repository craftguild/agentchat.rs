//! Read-only ownership health; neither lock creation nor journal recovery occurs.

use super::{Artifact, Installed, Installer, conflict, fileio, io, merge};
use agentchat_core::{Error, ErrorCode, Result};
use std::{collections::HashSet, fs::File};

pub use agentchat_protocol::integration::{
    ComponentInspection, ComponentState, Inspection, InspectionState,
};
enum ReadLock {
    Missing,
    Busy,
    Held(crate::lock::ScopedLock),
}
impl Installer {
    /// Inspect owned values using the existing validators and a nonblocking
    /// shared lock on the existing lock file. This never installs, repairs,
    /// creates a lock/directory, executes hooks or admits a host.
    pub async fn inspect(&self, desired: Vec<Artifact>) -> Result<Inspection> {
        validate_desired(&desired)?;
        self.inspect_with(move |_| Ok(desired)).await
    }
    /// Select desired artifacts from intact owned values under the same read
    /// lock as inspection. Missing/busy/recovery paths invoke no planner.
    pub(crate) async fn inspect_with<F>(&self, plan: F) -> Result<Inspection>
    where
        F: FnOnce(&Installed<'_>) -> Result<Vec<Artifact>> + Send + 'static,
    {
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.inspect_current(plan))
            .await
            .map_err(|_| Error::new(ErrorCode::IoError, "Installation inspection worker failed."))?
    }
    fn inspect_current<F>(&self, plan: F) -> Result<Inspection>
    where
        F: FnOnce(&Installed<'_>) -> Result<Vec<Artifact>>,
    {
        let _lock = match self.read_lock()? {
            ReadLock::Missing => return Ok(empty(InspectionState::NotInstalled)),
            ReadLock::Busy => return Ok(empty(InspectionState::Busy)),
            ReadLock::Held(file) => file,
        };
        if let Some(meta) = fileio::regular(&self.state.join("pending.json"))? {
            if meta.len() > fileio::JOURNAL_LIMIT as u64 {
                return Err(conflict(
                    "Pending installation intent exceeds its bounded size.",
                ));
            }
            // A partly published journal cannot be described as healthy. Leave
            // validation/recovery to an explicit mutation; no preimages escape.
            return Ok(empty(InspectionState::RecoveryRequired));
        }
        let installed = Installed::read(self)?;
        let desired = plan(&installed)?;
        validate_desired(&desired)?;
        let ledger = &installed.ledger;
        let mut components = Vec::new();
        for (key, owned) in &ledger.entries {
            if !merge::valid_key(key) {
                return Err(conflict("Invalid recorded component key."));
            }
            let (state, error) = if let Some(artifact) = desired.iter().find(|a| &a.key == key) {
                // Preparation reads/validates but never publishes. Reuse exact
                // alias, selector, JSON/TOML and owned-value policy in memory.
                match merge::prepare(self, vec![artifact.clone()]) {
                    Ok(plan) if plan.ledger.entries[key].hash == owned.hash => {
                        (ComponentState::Owned, None)
                    }
                    Ok(_) => (ComponentState::UpdateAvailable, None),
                    Err(error) => (ComponentState::Conflict, Some(error)),
                }
            } else {
                (ComponentState::Unverified, None)
            };
            components.push(ComponentInspection {
                key: key.clone(),
                state,
                error,
            });
        }
        let state = if components.is_empty() {
            InspectionState::NotInstalled
        } else if components.iter().any(|c| {
            matches!(
                c.state,
                ComponentState::Conflict | ComponentState::Unverified
            )
        }) {
            InspectionState::AttentionRequired
        } else if components
            .iter()
            .any(|c| c.state == ComponentState::UpdateAvailable)
        {
            InspectionState::NeedsUpdate
        } else {
            InspectionState::Healthy
        };
        Ok(Inspection { state, components })
    }
    fn read_lock(&self) -> Result<ReadLock> {
        let path = self.state.join("install.lock");
        fileio::safe_parents(&path)?;
        if fileio::regular(&path)?.is_none() {
            if fileio::regular(&self.state.join("ownership.json"))?.is_some()
                || fileio::regular(&self.state.join("pending.json"))?.is_some()
            {
                return Err(conflict(
                    "Installation metadata exists without its lock file.",
                ));
            }
            return Ok(ReadLock::Missing);
        }
        let file = File::open(path).map_err(io)?;
        match fs2::FileExt::try_lock_shared(&file) {
            Ok(()) => Ok(ReadLock::Held(crate::lock::ScopedLock::acquired(file))),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(ReadLock::Busy),
            Err(e) => Err(io(e)),
        }
    }
}
fn validate_desired(desired: &[Artifact]) -> Result<()> {
    let mut keys = HashSet::new();
    if desired.len() > 64
        || desired
            .iter()
            .any(|a| !merge::valid_key(&a.key) || !keys.insert(a.key.clone()))
    {
        return Err(conflict(
            "Inspection requires at most sixty-four unique bounded component keys.",
        ));
    }
    Ok(())
}

fn empty(state: InspectionState) -> Inspection {
    Inspection {
        state,
        components: Vec::new(),
    }
}
