//! Guard small metadata projections without holding a lock across registry/IPC.
use super::{CommittedLifecycle, OwnerNamespace, Reservation, unavailable};
use crate::claude::NativeLifecycle;
use agentchat_core::Result;
use uuid::Uuid;

impl OwnerNamespace {
    /// Remove exact predecessor projections only while this reserved successor
    /// is still pending and the retained original predecessor is unchanged.
    /// The finite cleanup runs under the native cursor lock; it cannot issue
    /// descriptors, call a host/application or authorize a body/participation.
    pub async fn clean_predecessor(
        &self,
        intent: Reservation,
        prior: CommittedLifecycle,
        cleanup: impl FnOnce(CommittedLifecycle) -> Result<()> + Send + 'static,
    ) -> Result<()> {
        self.update(move |h| {
            if h.pending.as_ref() != Some(&intent) || h.current.as_ref() != Some(&prior) {
                return Err(unavailable());
            }
            cleanup(prior)
        })
        .await
    }
    /// Check an exact pending intent before asynchronous descriptor work. An
    /// already committed retry returns the original immutable lifecycle; an
    /// obsolete/retired ticket cannot issue or repair a successor's projection.
    pub async fn registration(&self, intent: Reservation) -> Result<Option<NativeLifecycle>> {
        self.update(move |h| {
            if h.pending.as_ref() == Some(&intent) {
                return Ok(None);
            }
            h.current
                .as_ref()
                .filter(|c| !h.retired && h.pending.is_none() && c.reservation == intent)
                .map(|c| Some(c.lifecycle.clone()))
                .ok_or_else(unavailable)
        })
        .await
    }

    /// Publish or repair bounded private metadata only while the exact committed
    /// ticket remains current. This callback runs on a blocking worker under the
    /// owner lock: perform only finite synchronous file work, never registry,
    /// network, native getters, application operations or asynchronous waits.
    /// A partial projection is repairable from the same committed facts.
    pub async fn publish_current<T: Send + 'static>(
        &self,
        intent: Reservation,
        publish: impl FnOnce(CommittedLifecycle) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        self.update(move |h| {
            let current = h
                .current
                .as_ref()
                .filter(|c| !h.retired && h.pending.is_none() && c.reservation == intent)
                .ok_or_else(unavailable)?;
            publish(current.clone())
        })
        .await
    }

    /// Retire exact native facts and clean only their own projections under the
    /// same lock. Repeated calls may finish interrupted cleanup without reviving
    /// authority. An older end callback cannot remove a successor's metadata.
    /// The callback has the same finite synchronous-I/O restriction as publish.
    pub async fn retire_with(
        &self,
        root: Uuid,
        generation: u64,
        cleanup: impl FnOnce(CommittedLifecycle) -> Result<()> + Send + 'static,
    ) -> Result<bool> {
        self.update(move |h| {
            let matching = h
                .current
                .as_ref()
                .filter(|c| c.lifecycle.root() == root && c.lifecycle.generation() == generation);
            let current = matching.is_some() && !h.retired;
            if let Some(matching) = matching {
                cleanup(matching.clone())?;
            }
            let pending = h
                .pending
                .as_ref()
                .is_some_and(|p| p.root == root && p.generation == generation);
            if current {
                h.calls.clear();
                h.retired = true;
            }
            if pending {
                h.pending = None;
                h.allocation = None;
            }
            Ok(current || pending)
        })
        .await
    }
}
