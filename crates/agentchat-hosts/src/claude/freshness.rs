//! One-use native challenge proof; process survival alone cannot renew a root.
//!
//! The lifecycle caller must supply the current independently validated
//! registration and immutable native process owner. This module does not admit
//! profiles, discover roots, restore participation or authorize a model prompt.

use super::{NativeLifecycle, NativeRegistration, process::ProcessIdentity};
use agentchat_core::{Error, ErrorCode, Result, model::Phase, ports::HostState};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use uuid::Uuid;

/// Bounded request sent to the native-owned observer, with no proposed root ID.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Challenge {
    /// New unpredictable value; cached observer output cannot answer it.
    pub nonce: Uuid,
}

/// Native API/root-guard observation made in response to one challenge.
/// The wire transport must cap the frame before deserializing (8 KiB).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    /// Exact challenge nonce, not an unsolicited heartbeat.
    pub nonce: Uuid,
    /// Relay ordering is diagnostic; it is not native root authority.
    pub relay_sequence: u64,
    /// Monotonic native observation counter, captured before async reads.
    pub native_sequence: u64,
    /// Native module incarnation associated with this physical owner.
    pub observer: String,
    /// Physical-owner namespace hint; cannot authorize a current root alone.
    pub owner_root: Uuid,
    /// Current root read from the native API, never copied from the request.
    pub root: Uuid,
    /// Native lifecycle generation captured with that root.
    pub generation: u64,
    /// Guard-state fence checked after native API reads.
    pub epoch: u64,
    /// Native lifecycle, getter and fence all agreed without a guard fault.
    pub available: bool,
    /// Native interruption latch; typed continuation clears it.
    pub paused: bool,
    /// Optional first cause of the latest pause episode, possibly historical.
    /// Legacy producers omit it. Identity, liveness and wake policy ignore it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_cause: Option<serde_json::Value>,
    /// Conservative native approval latch. It may remain set until an already
    /// approved tool dispatch completes; absence of this latch alone never
    /// authorizes a wake or overrides the host permission decision.
    pub approval: bool,
    /// Root model turns only; subagent activity cannot become root authority.
    pub active_turns: Vec<String>,
    /// Host registry still contains the exact owned observer marker.
    pub guard_present: bool,
    /// Surfaces observed by the native API; the CLI profile requires terminal.
    pub surfaces: Vec<String>,
    /// Actual engine version reported by the native API.
    pub version: String,
    /// Diagnostic wall time only; validity uses the caller's monotonic deadline.
    pub unix: u64,
}

struct Pending {
    nonce: Uuid,
    registration: NativeLifecycle,
    owner: ProcessIdentity,
    expires: Instant,
}

/// Bounded freshness state for one physical owner/observer, with one pending ask.
/// Never cache a successful response to renew later presence or leases. Native
/// lifecycle retirement still fences dispatch; a phase snapshot cannot make an
/// old Server reusable or undo a mutation already committed during a race.
#[derive(Default)]
pub struct Freshness {
    pending: Option<Pending>,
    physical: Option<(Uuid, String, ProcessIdentity)>,
    last_native_sequence: u64,
    closed: bool,
}
impl Freshness {
    /// Reserve a fresh request for current validated lifecycle facts and owner.
    /// A live pending request returns Busy; an expired/cancelled ask may be
    /// replaced. This never registers or switches a lifecycle itself.
    pub fn begin(
        &mut self,
        current: &NativeRegistration,
        owner: &ProcessIdentity,
        timeout: Duration,
    ) -> Result<Challenge> {
        self.begin_lifecycle(&NativeLifecycle::Project(current.clone()), owner, timeout)
    }

    /// Challenge current native facts before or after project initialization.
    /// Bootstrap still needs a real kernel owner, exact observer/root and live
    /// native answer. It grants no context, presence or wake authorization.
    pub fn begin_lifecycle(
        &mut self,
        current: &NativeLifecycle,
        owner: &ProcessIdentity,
        timeout: Duration,
    ) -> Result<Challenge> {
        current.validate()?;
        if self.closed
            || timeout.is_zero()
            || timeout > Duration::from_secs(5)
            || !owner.is_continuing()
            || self
                .physical
                .as_ref()
                .is_some_and(|(hint, observer, process)| {
                    *hint != current.owner_root()
                        || observer != current.observer()
                        || process != owner
                })
        {
            return Err(unavailable());
        }
        let now = Instant::now();
        if self.pending.as_ref().is_some_and(|p| now < p.expires) {
            let mut error = Error::new(ErrorCode::Busy, "A native freshness check is pending.");
            error.retryable = true;
            return Err(error);
        }
        let nonce = Uuid::new_v4();
        self.physical.get_or_insert_with(|| {
            (
                current.owner_root(),
                current.observer().into(),
                owner.clone(),
            )
        });
        self.pending = Some(Pending {
            nonce,
            registration: current.clone(),
            owner: owner.clone(),
            expires: now + timeout,
        });
        Ok(Challenge { nonce })
    }

    /// Consume one matching native answer against the *current* lifecycle and
    /// owner. A delayed old nonce cannot consume a newer pending request. A
    /// matching invalid answer is consumed/refused rather than lent to a retry.
    pub fn finish(
        &mut self,
        current: &NativeRegistration,
        owner: &ProcessIdentity,
        answer: Snapshot,
    ) -> Result<HostState> {
        self.finish_lifecycle(&NativeLifecycle::Project(current.clone()), owner, answer)
    }

    /// Consume a one-use answer for the exact current native stage. A matching
    /// old answer is refused after promotion even if the native root is the same.
    /// The physical observation counter and owner pin survive that promotion.
    pub fn finish_lifecycle(
        &mut self,
        current: &NativeLifecycle,
        owner: &ProcessIdentity,
        answer: Snapshot,
    ) -> Result<HostState> {
        if self.closed
            || self
                .pending
                .as_ref()
                .is_none_or(|p| p.nonce != answer.nonce)
        {
            return Err(unavailable());
        }
        let pending = self.pending.take().ok_or_else(unavailable)?;
        if Instant::now() >= pending.expires
            || pending.registration != *current
            || pending.owner != *owner
            || !owner.is_continuing()
            || answer.root != current.root()
            || answer.generation != current.generation()
            || answer.observer != current.observer()
            || answer.owner_root != current.owner_root()
            || answer.version != current.host_version()
            || !answer.available
            || !answer.guard_present
            || answer.native_sequence <= self.last_native_sequence
            || answer.relay_sequence == 0
            || answer.epoch == 0
            || answer.active_turns.len() > 1
            || answer.active_turns.iter().any(|id| !identifier(id, 256))
            || answer.surfaces.len() > 8
            || answer.surfaces.iter().any(|id| !identifier(id, 32))
            || !answer.surfaces.iter().any(|surface| surface == "terminal")
        {
            return Err(unavailable());
        }
        self.last_native_sequence = answer.native_sequence;
        let phase = if answer.approval {
            Phase::ApprovalBlocked
        } else if answer.paused {
            Phase::Paused
        } else if answer.active_turns.is_empty() {
            Phase::Idle
        } else {
            Phase::Active
        };
        Ok(HostState {
            phase,
            active_turn_id: answer.active_turns.into_iter().next(),
            wake_paused: answer.paused,
        })
    }

    /// Cancel only this exact request; a stale cancellation cannot affect its
    /// replacement. Dropping a caller need not keep its request alive forever.
    pub fn cancel(&mut self, nonce: Uuid) -> bool {
        if self.pending.as_ref().is_some_and(|p| p.nonce == nonce) {
            self.pending = None;
            return true;
        }
        false
    }

    /// Permanently retire this physical observer's freshness authority.
    pub fn close(&mut self) {
        self.closed = true;
        self.pending = None;
    }
}
fn identifier(value: &str, limit: usize) -> bool {
    !value.is_empty()
        && value.len() <= limit
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}
fn unavailable() -> Error {
    Error::new(
        ErrorCode::SessionRequired,
        "Fresh exact native state is unavailable.",
    )
}
