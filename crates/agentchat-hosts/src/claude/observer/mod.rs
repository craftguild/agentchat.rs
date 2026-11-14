//! Current native observer facts, composed separately from profile admission.
//!
//! Trusted lifecycle helpers publish immutable context indexes and replace one
//! current record per observer. An index is only a locator: both the initial
//! and post-response current record must agree with the exact caller context.
mod files;
pub mod relay;
pub(crate) fn allocation_publication(
    root: &crate::claude::metadata::directory::Directory,
    original_path: &std::path::Path,
    name: &std::ffi::OsStr,
    snapshot: &crate::claude::metadata::directory::Snapshot,
) -> agentchat_core::Result<crate::staging::allocation::Published> {
    relay::allocation_publication(root, original_path, name, snapshot)
}
mod wire;
pub use wire::exchange;

use super::{NativeRegistration, freshness::Freshness, process::ProcessIdentity};
use agentchat_core::{Error, ErrorCode, Result, model::Context, ports::HostState};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::Mutex;

/// Native lifecycle facts with their independently captured kernel owner.
/// Private local metadata is not a cross-user authentication protocol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegisteredOwner {
    /// Registry-issued context and current native lifecycle facts.
    pub registration: NativeRegistration,
    /// Exact native CLI owner, never the surviving relay/helper process.
    pub owner: ProcessIdentity,
}

/// Exact expected native engine, supplied by an independently selected profile.
/// Naming a profile here does not admit it to automatic-wake support.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeProfile {
    /// Canonical executable of the CLI that owns native callbacks.
    pub executable: PathBuf,
    /// Exact engine version expected from the live native getter.
    pub version: String,
}

/// Bounded current-state observer for one private native integration namespace.
/// Keeps at most 32 physical replay windows. No heartbeat, participant, runtime
/// launch, profile admission or prompt submission is created by observation.
pub struct NativeObserver {
    root: PathBuf,
    profile: NativeProfile,
    states: Mutex<HashMap<String, Physical>>,
}
struct Physical {
    owner: ProcessIdentity,
    proof: Arc<Mutex<Freshness>>,
}
impl NativeObserver {
    /// Open a verified existing owner's route after an engine update. The
    /// original captured image/profile stay immutable; this does not reopen the
    /// engine path. Current route checks and the live SDK challenge still apply.
    pub async fn open_owned(
        root: PathBuf,
        profile: NativeProfile,
        owner: &ProcessIdentity,
    ) -> Result<Self> {
        if !root.is_absolute()
            || profile.executable != owner.executable()
            || !owner.is_continuing()
            || !super::module::candidate_version(&profile.version)
        {
            return Err(unavailable());
        }
        files::directory(&root).await?;
        Ok(Self {
            root,
            profile,
            states: Mutex::default(),
        })
    }

    /// Open existing private metadata and pin an explicit expected engine.
    /// The caller still owns live API compatibility and lifecycle validation.
    pub async fn open(root: PathBuf, mut profile: NativeProfile) -> Result<Self> {
        if !root.is_absolute() {
            return Err(unavailable());
        }
        files::directory(&root).await?;
        if !super::module::candidate_version(&profile.version) {
            return Err(unavailable());
        }
        profile.executable = tokio::fs::canonicalize(&profile.executable)
            .await
            .map_err(|_| unavailable())?;
        Ok(Self {
            root,
            profile,
            states: Mutex::default(),
        })
    }

    /// Read fresh guard state for this exact immutable context, or refuse it.
    /// A historical index, live helper, phase cache or another root is never a
    /// fallback. Profile admission and automatic wake remain separate policies.
    pub async fn verify(&self, context: &Context) -> Result<HostState> {
        let index: RegisteredOwner = files::read(
            &self
                .root
                .join(format!("context-{}.json", context.context_id)),
        )
        .await?;
        let observer = &index.registration.observer;
        if !identifier(observer)
            || index.registration.context != *context
            || context.host_version != self.profile.version
            || index.owner.executable() != self.profile.executable
            || !index.owner.is_continuing()
        {
            return Err(unavailable());
        }
        let path = self.root.join(format!("native-{observer}.json"));
        let current: RegisteredOwner = files::read(&path).await?;
        if current != index {
            return Err(unavailable());
        }
        let state = {
            let mut states = self.states.lock().await;
            if !states.contains_key(observer) && states.len() >= 32 {
                // Only kernel-proven dead owners can release capacity. An old
                // in-flight check still owns its state and must fail the same
                // kernel recheck; removing this map entry cannot renew it.
                states.retain(|_, state| {
                    state.owner.continuity_state() != super::process::ContinuityState::Gone
                });
            }
            if !states.contains_key(observer) && states.len() >= 32 {
                let mut error = Error::new(ErrorCode::Busy, "Native observer capacity is full.");
                error.retryable = true;
                return Err(error);
            }
            states
                .entry(observer.clone())
                .or_insert_with(|| Physical {
                    owner: current.owner.clone(),
                    proof: Arc::default(),
                })
                .proof
                .clone()
        };
        // Each physical observer serializes its own requests, while independent
        // roots and native lifecycle retirement remain able to make progress.
        let mut proof = state.lock().await;
        let request = proof.begin(
            &current.registration,
            &current.owner,
            Duration::from_secs(5),
        )?;
        let answer = match exchange(&self.root, observer, current.owner.pid(), &request).await {
            Ok(answer) => answer,
            Err(error) => {
                proof.cancel(request.nonce);
                return Err(error);
            }
        };
        let after: RegisteredOwner = match files::read(&path).await {
            Ok(after) => after,
            Err(error) => {
                proof.cancel(request.nonce);
                return Err(error);
            }
        };
        proof.finish(&after.registration, &after.owner, answer)
    }
}
fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}
fn unavailable() -> Error {
    Error::new(
        ErrorCode::SessionRequired,
        "Fresh exact native observer state is unavailable.",
    )
}
