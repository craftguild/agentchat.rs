//! Exact ready binding and durable delivery fence, never native RPC permission.
use crate::{
    codex::{NativeRegistry, required},
    launch::{Binding, Endpoint, LaunchOwners, NativeProof, Registry},
};
use agentchat_core::{
    Result,
    model::{HostKind, LaunchOwnerState},
};
use std::{
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
pub(super) struct Guard {
    home: PathBuf,
    backend: PathBuf,
    proof: Option<NativeProof>,
    tracked: Mutex<Tracked>,
    disabled: AtomicBool,
}
#[derive(Default)]
struct Tracked {
    binding: Option<Binding>,
    fenced: Option<(u64, u64)>,
}
impl Guard {
    pub(super) fn new(home: &Path, backend: &Path, proof: Option<NativeProof>) -> Self {
        Self {
            home: home.into(),
            backend: backend.into(),
            proof,
            tracked: Mutex::new(Tracked::default()),
            disabled: AtomicBool::new(false),
        }
    }
    pub(super) fn track(&self, binding: &Binding) -> Result<()> {
        let mut tracked = self.tracked.lock().expect("binding poisoned");
        let proof = self.proof.as_ref().ok_or_else(required)?;
        let registry = Registry::new(&self.home);
        let ready = registry.readiness(binding)?.ok_or_else(required)?;
        if !self.enabled()
            || proof.state() != LaunchOwnerState::Current
            || binding.context.host != HostKind::Codex
            || binding.identity.owner != proof.identity().owner
            || !LaunchOwners::is_supervisor(&binding.identity)
            || !matches!(&binding.endpoint,Endpoint::Codex{socket,..} if socket==&self.backend)
            || registry.root_departed(
                binding.context.project_id,
                binding.context.host,
                &binding.context.host_session_id,
                &binding.identity,
            )
            || registry.resolve(&binding.context)?.as_ref() != Some(binding)
            || registry.authorization(&binding.context)?.as_ref() != Some(&ready)
            || NativeRegistry::new(&self.home)
                .lookup(&binding.context.host_session_id)?
                .is_none_or(|route| route.binding() != binding)
        {
            return Err(required());
        }
        if let Some(current) = &tracked.binding {
            if current.context.host_session_id != binding.context.host_session_id {
                // Distinct-root activation follows the delivered native success.
                tracked.fenced = None;
            } else if current != binding && tracked.fenced.is_some() {
                // An in-flight switch cannot lend its fence to another descriptor.
                // Refuse delivery enablement; native forwarding remains independent.
                return Err(required());
            }
        }
        tracked.binding = Some(binding.clone());
        Ok(())
    }
    pub(super) fn release_initial(&self, fence: &mut super::ProxyFence) -> Result<()> {
        // Serialize this exception with first binding tracking. There is no old
        // authority to fence here; after tracking, only the durable path applies.
        let tracked = self.tracked.lock().expect("binding poisoned");
        if tracked.binding.is_some()
            || !self.enabled()
            || self
                .proof
                .as_ref()
                .is_none_or(|proof| proof.state() != LaunchOwnerState::Current)
        {
            return Err(required());
        }
        fence.release(())
    }
    pub(super) fn fence(&self, connection: u64, epoch: u64) -> Result<bool> {
        // Serialize retargeting and the durable write: delivery cannot enable a
        // replacement generation between choosing the binding and fencing it.
        let mut tracked = self.tracked.lock().expect("binding poisoned");
        let Some(binding) = &tracked.binding else {
            return Ok(false);
        };
        Registry::new(&self.home).record_managed_transition(binding, connection, epoch)?;
        tracked.fenced = Some((connection, epoch));
        Ok(true)
    }
    pub(super) fn release(&self, connection: u64, epoch: u64) {
        let mut tracked = self.tracked.lock().expect("binding poisoned");
        if tracked.fenced == Some((connection, epoch)) {
            tracked.fenced = None;
        }
    }
    pub(super) fn enabled(&self) -> bool {
        !self.disabled.load(Ordering::Acquire)
    }
    pub(super) fn disable(&self) {
        self.disabled.store(true, Ordering::Release);
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/codex_proxy_startup_guard.rs"]
mod tests;
