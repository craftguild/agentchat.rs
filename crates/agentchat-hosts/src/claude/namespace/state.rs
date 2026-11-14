//! One durable current lifecycle and one supersedable native transition intent.
use super::{OwnerNamespace, storage, unavailable};
use crate::claude::NativeLifecycle;
use agentchat_core::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

// Native JavaScript must represent every counter exactly. Exhaustion refuses
// renewal rather than rounding, wrapping, or resetting an authority floor.
pub(super) const MAX_SEQUENCE: u64 = 9_007_199_254_740_991;

/// Native callback intent captured before asynchronous descriptor creation.
/// Superseded tickets cannot commit, including after clear or module reload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reservation {
    /// Unpredictable transition key; neither application identity nor authority.
    pub ticket: Uuid,
    /// Exact native root from a live callback, never the physical locator.
    pub root: Uuid,
    /// Positive generation allocated under the physical owner's file lock.
    pub generation: u64,
    /// Live native working directory, used only for project discovery.
    pub working_directory: PathBuf,
    /// Observed MCP endpoint incarnation, absent before its publication.
    pub transport_nonce: Option<Uuid>,
}
/// Committed routing facts. Fresh call, kernel, observer and control checks are
/// still mandatory; reading this record never restores participation or wakes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommittedLifecycle {
    /// Original native intent, retained for exact interrupted-commit retry.
    pub reservation: Reservation,
    /// Immutable bootstrap or registry-issued project lifecycle.
    pub lifecycle: NativeLifecycle,
}
/// Persistent ordering plus exact-current eligibility for a native observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeSequence {
    /// Increasing physical-owner order, preserved across module/transport reload.
    pub sequence: u64,
    /// False during a transition or for an obsolete callback. Observation replies
    /// must then say unavailable, rather than claiming facts for the new root.
    pub current: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Header {
    pub version: u32,
    pub owner: crate::claude::process::ProcessIdentity,
    pub namespace_id: Uuid,
    pub observer: String,
    pub lock_identity: (u64, u64),
    pub generation: u64,
    pub call_sequence: u64,
    pub observation_sequence: u64,
    pub current: Option<CommittedLifecycle>,
    pub retired: bool,
    pub pending: Option<Reservation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allocation: Option<super::allocation::Allocation>,
    #[serde(default)]
    pub calls: Vec<crate::claude::NativeCall>,
}
impl Header {
    pub fn new(namespace: &OwnerNamespace, lock_identity: (u64, u64)) -> Self {
        Self {
            version: 1,
            owner: namespace.owner.clone(),
            namespace_id: namespace.id,
            observer: namespace.observer.clone(),
            lock_identity,
            generation: 0,
            call_sequence: 0,
            observation_sequence: 0,
            current: None,
            retired: false,
            pending: None,
            allocation: None,
            calls: Vec::new(),
        }
    }
    pub fn pristine(&self) -> bool {
        self.generation == 0
            && self.call_sequence == 0
            && self.observation_sequence == 0
            && self.current.is_none()
            && !self.retired
            && self.pending.is_none()
            && self.allocation.is_none()
            && self.calls.is_empty()
    }
    pub fn validate(&self, ns: &OwnerNamespace, lock: (u64, u64)) -> Result<()> {
        if !matches!(self.version, 1 | 2)
            || self.owner != ns.owner
            || self.namespace_id != ns.id
            || self.observer != ns.observer
            || self.lock_identity != lock
            || [
                self.generation,
                self.call_sequence,
                self.observation_sequence,
            ]
            .into_iter()
            .any(|n| n > MAX_SEQUENCE)
        {
            return Err(unavailable());
        }
        super::pending_calls::validate(self)?;
        super::allocation::validate(self)?;
        if !self.retired
            && (self.pending.is_some()
                || self.current.as_ref().map_or(self.generation != 0, |c| {
                    c.reservation.generation != self.generation
                }))
        {
            return Err(unavailable());
        }
        if let Some(current) = &self.current {
            self.validate_commit(&current.reservation, &current.lifecycle)?;
            if current.reservation.generation > self.generation {
                return Err(unavailable());
            }
        }
        if let Some(pending) = &self.pending {
            validate_reservation(pending)?;
            if pending.generation != self.generation
                || self.current.as_ref().is_some_and(|c| {
                    c.reservation.generation >= pending.generation
                        || c.reservation.ticket == pending.ticket
                })
            {
                return Err(unavailable());
            }
        }
        Ok(())
    }
    pub(super) fn validate_commit(
        &self,
        intent: &Reservation,
        lifecycle: &NativeLifecycle,
    ) -> Result<()> {
        validate_reservation(intent)?;
        lifecycle.validate()?;
        if lifecycle.owner_root() != self.namespace_id
            || lifecycle.observer() != self.observer
            || lifecycle.root() != intent.root
            || lifecycle.generation() != intent.generation
            || lifecycle.working_directory() != intent.working_directory
        {
            return Err(unavailable());
        }
        super::allocation::check_commit(self, intent, lifecycle)
    }
    pub(super) fn matches(&self, root: Uuid, generation: u64) -> bool {
        !self.retired
            && self.pending.is_none()
            && self.current.as_ref().is_some_and(|c| {
                c.lifecycle.root() == root && c.lifecycle.generation() == generation
            })
    }
}
fn validate_reservation(r: &Reservation) -> Result<()> {
    if r.ticket.is_nil()
        || r.root.is_nil()
        || r.generation == 0
        || r.generation > MAX_SEQUENCE
        || !r.working_directory.is_absolute()
        || r.working_directory.as_os_str().len() > 16_384
        || r.transport_nonce.is_some_and(|id| id.is_nil())
    {
        return Err(unavailable());
    }
    Ok(())
}
pub(super) fn advance(value: &mut u64) -> Result<u64> {
    if *value >= MAX_SEQUENCE {
        return Err(unavailable());
    }
    *value += 1;
    Ok(*value)
}
impl OwnerNamespace {
    pub(super) async fn update<T: Send + 'static>(
        &self,
        change: impl FnOnce(&mut Header) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let namespace = self.clone();
        tokio::task::spawn_blocking(move || storage::access(&namespace, change))
            .await
            .map_err(|_| unavailable())?
    }
    /// Reserve before asynchronous registry work. New intent supersedes the one
    /// pending ticket and makes previous native observation eligibility false.
    /// No lock is held across descriptor creation, IPC, or a native callback.
    pub async fn reserve(
        &self,
        root: Uuid,
        working_directory: PathBuf,
        transport_nonce: Option<Uuid>,
    ) -> Result<Reservation> {
        self.reserve_checked(None, root, working_directory, transport_nonce)
            .await
            .map(|(reservation, _)| reservation)
    }
    /// Reserve only after the exact last-observed floor. Native callbacks use
    /// this comparison after asynchronous input/cwd work so a delayed older
    /// helper cannot supersede a newer native callback's registration. The
    /// producer must also recheck its own live root/epoch/marker before retry.
    pub async fn reserve_after(
        &self,
        after_generation: u64,
        root: Uuid,
        working_directory: PathBuf,
        transport_nonce: Option<Uuid>,
    ) -> Result<Reservation> {
        self.reserve_checked(
            Some(after_generation),
            root,
            working_directory,
            transport_nonce,
        )
        .await
        .map(|(reservation, _)| reservation)
    }
    /// Reserve with the same floor fence and retain the prior committed facts,
    /// including a retired predecessor. Native composition can durably record
    /// a positive root change before registering the new root. The prior value
    /// grants no current authority and no I/O runs under the cursor lock.
    pub async fn reserve_after_with_prior(
        &self,
        after_generation: u64,
        root: Uuid,
        working_directory: PathBuf,
        transport_nonce: Option<Uuid>,
    ) -> Result<(Reservation, Option<CommittedLifecycle>)> {
        self.reserve_checked(
            Some(after_generation),
            root,
            working_directory,
            transport_nonce,
        )
        .await
    }
    /// Read only ordering metadata for a conditional native reservation. This
    /// floor cannot identify a conversation or authorize any operation alone.
    pub async fn generation_floor(&self) -> Result<u64> {
        self.update(|h| Ok(h.generation)).await
    }
    async fn reserve_checked(
        &self,
        expected: Option<u64>,
        root: Uuid,
        working_directory: PathBuf,
        transport_nonce: Option<Uuid>,
    ) -> Result<(Reservation, Option<CommittedLifecycle>)> {
        self.update(move |h| {
            if expected.is_some_and(|floor| floor != h.generation) {
                return Err(unavailable());
            }
            let r = Reservation {
                ticket: Uuid::new_v4(),
                root,
                generation: advance(&mut h.generation)?,
                working_directory,
                transport_nonce,
            };
            validate_reservation(&r)?;
            h.calls.clear();
            h.retired = true;
            h.pending = Some(r.clone());
            h.version = 2;
            h.allocation = Some(super::allocation::Allocation::Unassigned);
            Ok((r, h.current.clone()))
        })
        .await
    }
    /// Commit only the still-current intent after descriptor creation. Exact
    /// retries return the same facts; stale callbacks cannot publish a successor.
    pub async fn commit(&self, intent: Reservation, lifecycle: NativeLifecycle) -> Result<bool> {
        self.update(move |h| {
            h.validate_commit(&intent, &lifecycle)?;
            let committed = CommittedLifecycle {
                reservation: intent.clone(),
                lifecycle,
            };
            if !h.retired && h.pending.is_none() && h.current.as_ref() == Some(&committed) {
                return Ok(false);
            }
            if h.pending.as_ref() != Some(&intent) {
                return Err(unavailable());
            }
            if h.current
                .as_ref()
                .is_some_and(|c| committed.lifecycle.reuses_identity(&c.lifecycle))
            {
                return Err(unavailable());
            }
            h.current = Some(committed);
            h.retired = false;
            h.pending = None;
            h.allocation = None;
            Ok(true)
        })
        .await
    }
    /// Read current routing facts, refusing to lend an old lifecycle during a
    /// pending transition. This does not assert observer health or native policy.
    pub async fn current(&self) -> Result<Option<CommittedLifecycle>> {
        self.update(|h| {
            Ok(if !h.retired && h.pending.is_none() {
                h.current.clone()
            } else {
                None
            })
        })
        .await
    }
    /// Allocate native observation ordering, including unavailable transition
    /// replies. The actual root/control snapshot still comes from native getters.
    pub async fn observation_sequence(
        &self,
        root: Uuid,
        generation: u64,
    ) -> Result<NativeSequence> {
        self.update(move |h| {
            Ok(NativeSequence {
                sequence: advance(&mut h.observation_sequence)?,
                current: h.matches(root, generation),
            })
        })
        .await
    }
    /// Allocate a call order only for the exact current native lifecycle. Callers
    /// retain the resulting NativeCall for retries; this is not per-call proof.
    pub async fn call_sequence(&self, root: Uuid, generation: u64) -> Result<u64> {
        self.update(move |h| {
            if !h.matches(root, generation) {
                return Err(unavailable());
            }
            advance(&mut h.call_sequence)
        })
        .await
    }
    /// Retire only the exact named callback, retaining all monotonic floors.
    /// Late end callbacks cannot clear a different pending or current lifecycle.
    pub async fn retire(&self, root: Uuid, generation: u64) -> Result<bool> {
        self.retire_with(root, generation, |_| Ok(())).await
    }
}
