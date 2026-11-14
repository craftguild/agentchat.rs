//! Bounded immutable root attachments beneath one retained physical transport.

use crate::{BootstrapKey, CallScope, ConnectionLifetime, Server};
use agentchat_core::{Error, ErrorCode, Result};
use agentchat_protocol::operations::{Backend, InitializationIdentity, Target};
use std::sync::Arc;
use tokio::{sync::Mutex, task::JoinHandle};
use uuid::Uuid;

/// Exact descriptor and monotonically increasing lifecycle generation.
/// The host authority assigns a generation before asynchronous lifecycle work;
/// neither tool arguments nor ordinary calls may manufacture or advance it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConversationKey {
    /// Fresh opaque descriptor issued for this exact native lifecycle.
    pub context_id: Uuid,
    /// Positive generation scoped to this physical connection's native owner.
    pub generation: u64,
}

/// One immutable conversation attachment; clones cannot keep its authority live.
#[derive(Clone)]
pub struct Conversation {
    pub(crate) server: Server,
    key: ConversationKey,
}
impl Conversation {
    /// Inspect routing facts without joining or refreshing host presence.
    pub fn key(&self) -> ConversationKey {
        self.key
    }
    /// Restore only this descriptor through the ordinary shared lifecycle path.
    /// An explicit Join may already have attached this immutable slot since a
    /// previous NotJoined observation; recheck that attachment without replacing
    /// it. NotJoined remains an error and restoration never invites participation.
    pub async fn restore(&self) -> Result<()> {
        if self.server.check_bound_attachment().await? {
            return Ok(());
        }
        self.server
            .restore(Target {
                context_id: Some(self.key.context_id),
                ..Target::default()
            })
            .await
    }
}

struct Slot {
    key: CallScope,
    server: Server,
    owner: ConnectionLifetime,
    cleanup: Option<JoinHandle<()>>,
}
impl Slot {
    fn new(
        backend: Arc<dyn Backend>,
        key: CallScope,
        identity: Option<InitializationIdentity>,
        group: &ConnectionLifetime,
    ) -> Self {
        let owner = ConnectionLifetime::default();
        let mut server = Server::with_lifetime(backend, &owner);
        match key {
            CallScope::Conversation(key) => server.required_context = Some(key.context_id),
            CallScope::Bootstrap(_) => server.bootstrap = true,
        }
        server.initialization_identity = identity;
        server.activation_group = Some(group.subscribe());
        let retained = server.clone();
        // Retirement cleanup must survive cancellation of a lifecycle caller.
        // There is at most one such task/slot; replacement waits for cleanup.
        // Losing the last owner also closes it, even if Conversation clones live.
        let cleanup = tokio::spawn(async move {
            retained.retirement().await;
            retained.clear_binding().await;
        });
        Self {
            key,
            server,
            owner,
            cleanup: Some(cleanup),
        }
    }
    async fn retire(&mut self, continuation: bool) {
        self.server
            .activation_replacement
            .store(continuation, std::sync::atomic::Ordering::Release);
        self.owner.retire();
        if let Some(cleanup) = &mut self.cleanup {
            let _ = cleanup.await;
            self.cleanup = None;
        }
        // Also handles an unexpectedly failed cleanup task. Keep the retired
        // slot in State across every await so cancellation cannot lose cleanup.
        self.server.clear_binding().await;
    }
}

#[derive(Default)]
struct State {
    current: Option<Slot>,
    pending: Option<CallScope>,
    pending_initialization_identity: Option<InitializationIdentity>,
    pending_continuation: bool,
    group: ConnectionLifetime,
    closed: bool,
}

/// Host-owned lifecycle control for one physical MCP gateway.
///
/// Retains at most one current/retiring attachment and one pending key, not a
/// map of historical roots. This is not a native identity, liveness or admission
/// authority. The host adapter must supply verified fresh descriptors and call
/// activate only for authoritative lifecycle events, never for ordinary calls.
pub struct Conversations {
    pub(crate) backend: Arc<dyn Backend>,
    state: Mutex<State>,
}
impl Conversations {
    /// Construct inert routing control without project discovery or attachment.
    /// Use a separate instance for each physical MCP connection/native owner.
    pub fn new(backend: Arc<dyn Backend>) -> Self {
        Self {
            backend,
            state: Mutex::default(),
        }
    }
    /// Retire the preceding lifecycle before activating a fresh immutable one.
    /// Same-key replay is idempotent; older or conflicting generations fail.
    /// A cancelled transition retains its pending generation and retired slot,
    /// so neither stale callbacks nor retries can revive the preceding root.
    pub async fn activate(&self, key: ConversationKey) -> Result<Conversation> {
        let server = self
            .activate_scope(CallScope::Conversation(key), None, false)
            .await?;
        Ok(Conversation { server, key })
    }
    /// Replace a descriptor only after the host independently verifies the
    /// same native conversation and original physical owner. This preserves an
    /// already-dispatched Join receipt through its own registration callback;
    /// it cannot keep ordinary calls or waits alive across a root/end/close.
    /// Tool arguments cannot assert this continuation and establish no authority.
    pub async fn activate_same_conversation(&self, key: ConversationKey) -> Result<Conversation> {
        let server = self
            .activate_scope(CallScope::Conversation(key), None, true)
            .await?;
        Ok(Conversation { server, key })
    }
    /// Activate a verified native lifecycle without project discovery or an
    /// application context. Explicit-path reads/initialization remain available;
    /// no argument can lend it a context, invitation, restoration or heartbeat.
    /// Promotion requires another authoritative lifecycle callback, a fresh
    /// context and a strictly later shared generation, never an ordinary call.
    /// The captured native initializer is immutable for this key, including
    /// across a cancelled transition; it cannot be changed by a replay.
    pub async fn activate_bootstrap(
        &self,
        key: BootstrapKey,
        identity: InitializationIdentity,
    ) -> Result<()> {
        self.activate_scope(CallScope::Bootstrap(key), Some(identity), false)
            .await?;
        Ok(())
    }
    async fn activate_scope(
        &self,
        key: CallScope,
        identity: Option<InitializationIdentity>,
        continuation: bool,
    ) -> Result<Server> {
        let mut state = self.state.try_lock().map_err(|_| busy())?;
        if state.closed || key.generation() == 0 {
            return Err(unavailable());
        }
        if state.pending.is_none()
            && let Some(slot) = &state.current
            && slot.key == key
        {
            return if slot.server.is_retired() || slot.server.initialization_identity != identity {
                Err(unavailable())
            } else {
                Ok(slot.server.clone())
            };
        }
        // The first transition owns its continuation classification through
        // cancellation. A repeated native callback may already observe the new
        // root; it cannot reinterpret the pending transition as another group.
        let continuation = if state.pending == Some(key) {
            state.pending_continuation
        } else {
            continuation
        };
        if continuation && state.group.is_retired() {
            return Err(unavailable());
        }
        if state.pending == Some(key) && state.pending_initialization_identity != identity {
            return Err(unavailable());
        }
        if state
            .current
            .as_ref()
            .is_some_and(|slot| slot.key.token_id() == key.token_id())
        {
            // A new lifecycle needs a fresh descriptor/incarnation, not just
            // a larger counter that could reset an existing wake budget.
            return Err(unavailable());
        }
        let latest = state
            .pending
            .or_else(|| state.current.as_ref().map(|slot| slot.key));
        if state.pending != Some(key)
            && latest.is_some_and(|latest| key.generation() <= latest.generation())
        {
            return Err(unavailable());
        }
        if state.pending.is_none() {
            state.pending_continuation = continuation;
            if !continuation {
                state.group.retire();
            }
        }
        state.pending = Some(key);
        state.pending_initialization_identity = identity.clone();
        if let Some(current) = &mut state.current {
            current.retire(continuation).await;
        }
        if !continuation {
            state.group = ConnectionLifetime::default();
        }
        let slot = Slot::new(self.backend.clone(), key, identity, &state.group);
        let server = slot.server.clone();
        state.current = Some(slot);
        state.pending = None;
        state.pending_initialization_identity = None;
        Ok(server)
    }
    /// Permanently close this gateway's attachments, including retained clones.
    /// Safe to repeat or cancel; it never leaves the gateway reusable.
    pub async fn close(&self) {
        let mut state = self.state.lock().await;
        state.closed = true;
        state.group.retire();
        state.pending = None;
        state.pending_initialization_identity = None;
        if let Some(current) = &mut state.current {
            current.retire(false).await;
        }
        state.current = None;
    }
    /// Retire one exact current root without closing the physical gateway.
    /// A delayed end callback cannot retire a newer generation. The tombstone
    /// remains until replacement, so the same lifecycle cannot be reactivated.
    pub async fn retire(&self, key: ConversationKey) -> Result<bool> {
        self.retire_scope(CallScope::Conversation(key)).await
    }
    /// Retire only this exact bootstrap lifecycle, preserving the transport.
    /// Its tombstone shares the same fence as context-bearing attachments.
    pub async fn retire_bootstrap(&self, key: BootstrapKey) -> Result<bool> {
        self.retire_scope(CallScope::Bootstrap(key)).await
    }
    async fn retire_scope(&self, key: CallScope) -> Result<bool> {
        let mut state = self.state.try_lock().map_err(|_| busy())?;
        let Some(current) = &mut state.current else {
            return Ok(false);
        };
        if current.key != key {
            return Ok(false);
        }
        state.group.retire();
        state
            .current
            .as_mut()
            .expect("checked slot")
            .retire(false)
            .await;
        Ok(true)
    }
    pub(crate) fn resolve(&self, key: ConversationKey) -> Result<Conversation> {
        let server = self.resolve_scope(CallScope::Conversation(key))?;
        Ok(Conversation { server, key })
    }
    pub(crate) fn resolve_scope(&self, key: CallScope) -> Result<Server> {
        let state = self.state.try_lock().map_err(|_| busy())?;
        if !state.closed
            && state.pending.is_none()
            && let Some(slot) = &state.current
            && slot.key == key
            && !slot.server.is_retired()
        {
            return Ok(slot.server.clone());
        }
        Err(unavailable())
    }
    /// Finish binding only the current native slot matching an actual committed
    /// Join receipt. A retired/historical receipt cannot select another slot;
    /// shared Attach still refuses left/unjoined state and creates no membership.
    pub(crate) async fn restore_committed_join(&self, context: Uuid) -> Result<()> {
        let server = {
            let state = self.state.lock().await;
            let Some(slot) = &state.current else {
                return Ok(());
            };
            if state.closed
                || state.pending.is_some()
                || slot.server.is_retired()
                || !matches!(slot.key,CallScope::Conversation(key) if key.context_id==context)
            {
                return Ok(());
            }
            slot.server.clone()
        };
        if server.has_binding().await {
            return Ok(());
        }
        match server
            .restore(Target {
                context_id: Some(context),
                ..Default::default()
            })
            .await
        {
            // A concurrent Leave does not roll back the preceding Join receipt.
            Err(error) if error.code == ErrorCode::NotJoined => Ok(()),
            result => result,
        }
    }
    /// Recheck native availability for one exact already-attached lifecycle.
    /// Returns false while unbound; never discovers, joins or restores it.
    /// The host supplies only a change hint: the backend obtains fresh evidence
    /// through its ordinary heartbeat. Retirement cancels a pending refresh.
    pub async fn refresh(&self, key: ConversationKey) -> Result<bool> {
        self.resolve(key)?.server.refresh_binding().await
    }
}
pub(crate) fn unavailable() -> Error {
    Error::new(
        ErrorCode::SessionRequired,
        "This exact conversation lifecycle is unavailable or retired.",
    )
}
fn busy() -> Error {
    let mut error = Error::new(
        ErrorCode::Busy,
        "A conversation lifecycle transition is in progress.",
    );
    error.retryable = true;
    error
}
