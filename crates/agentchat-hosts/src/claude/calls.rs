//! Consume exact native tool observations once; caller arguments cannot route.

use super::{NativeBootstrap, NativeLifecycle, NativeScope, replay::Replay};
use agentchat_core::{Error, ErrorCode, Result, model::Context};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use uuid::Uuid;

/// Descriptor routing selected by a native lifecycle, never by an MCP argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallScope {
    /// Fresh exact-context descriptor issued by the runtime registry.
    pub context_id: Uuid,
    /// Monotonic native lifecycle generation for this observer/transport owner.
    pub generation: u64,
}

/// Trusted lifecycle input supplied by the native hook boundary.
/// This record is local routing metadata, not cross-user authentication.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeRegistration {
    /// Opaque physical namespace locator, never current native root authority.
    /// Historical isolated probes used their initial root as this locator.
    pub owner_root: Uuid,
    /// Native observer instance, stable through clear and distinct between CLIs.
    pub observer: String,
    /// Positive monotonic generation captured before asynchronous hook work.
    pub generation: u64,
    /// Current exact native root from the lifecycle hook/live root getter.
    pub root: Uuid,
    /// Registry-issued descriptor agreeing with the native root and host kind.
    pub context: Context,
}
impl NativeRegistration {
    /// Exact immutable scope; does not claim presence, participation or admission.
    pub fn scope(&self) -> CallScope {
        CallScope {
            context_id: self.context.context_id,
            generation: self.generation,
        }
    }
}

/// One actual native tool-call observation, without tool arguments or bodies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeCall {
    /// Same native observer instance as the lifecycle registration.
    pub observer: String,
    /// Native generation captured with this tool observation.
    pub generation: u64,
    /// Exact root making the call; not inferred from a directory or process ID.
    pub root: Uuid,
    /// Increasing native call-observation sequence, including across clear.
    pub sequence: u64,
    /// Complete native MCP tool name, including the owned server prefix.
    pub tool_name: String,
    /// Exact native tool_use_id later carried as claudecode/toolUseId.
    pub tool_use_id: String,
}

/// Bounded correlation state for one physical Claude MCP connection.
///
/// Lifecycle registration happens separately from ordinary MCP calls. Native
/// hooks record pending calls; dispatch consumes their exact native ID once.
/// Keep at most 32 pending observations and a 4,096-sequence replay window.
/// There is no message body, database, timer, helper heartbeat or host admission.
/// Native controls must revoke cancelled/finished calls and retire ended roots.
/// This ledger is identity evidence only, never a substitute for live HostGate.
pub struct CallProofs {
    owner_hint: Uuid,
    prefix: String,
    current: Option<NativeLifecycle>,
    retired: bool,
    closed: bool,
    pending: HashMap<[u8; 32], NativeCall>,
    replay: Replay,
}
impl CallProofs {
    /// Create inert state for an exact owned native MCP prefix, such as mcp__agentchat__.
    /// The physical locator cannot register a root or authorize any call alone.
    pub fn new(owner_hint: Uuid, native_tool_prefix: String) -> Result<Self> {
        if native_tool_prefix.len() > 256
            || !native_tool_prefix.starts_with("mcp__")
            || !native_tool_prefix.ends_with("__")
            || !identifier(&native_tool_prefix)
        {
            return Err(required());
        }
        Ok(Self {
            owner_hint,
            prefix: native_tool_prefix,
            current: None,
            retired: false,
            closed: false,
            pending: HashMap::new(),
            replay: Replay::default(),
        })
    }
    /// Register fresh lifecycle facts, preserving exact same-generation replay.
    /// A different observer, conflicting generation or reused descriptor fails.
    /// No MCP request metadata can invoke this transition implicitly.
    pub fn register(&mut self, registration: NativeRegistration) -> Result<bool> {
        self.register_lifecycle(NativeLifecycle::Project(registration))
    }
    /// Register initialization-only native facts without fabricating a context.
    /// Promotion uses register with a fresh descriptor and higher generation;
    /// it preserves this physical owner's call replay window.
    pub fn register_bootstrap(&mut self, registration: NativeBootstrap) -> Result<bool> {
        self.register_lifecycle(NativeLifecycle::Bootstrap(registration))
    }
    /// Apply an authoritative lifecycle callback, never an ordinary tool call.
    /// The caller must retire its previous application attachment separately.
    pub fn register_lifecycle(&mut self, registration: NativeLifecycle) -> Result<bool> {
        registration.validate()?;
        if self.closed || registration.owner_root() != self.owner_hint {
            return Err(required());
        }
        if let Some(current) = &self.current {
            if current == &registration && !self.retired {
                return Ok(false);
            }
            if current.observer() != registration.observer()
                || registration.generation() <= current.generation()
                || registration.reuses_identity(current)
            {
                return Err(required());
            }
        }
        self.current = Some(registration);
        self.pending.clear();
        self.retired = false;
        Ok(true)
    }
    /// Inspect current routing facts without inferring liveness or binding.
    pub fn current(&self) -> Option<&NativeRegistration> {
        match self.lifecycle()? {
            NativeLifecycle::Project(registration) => Some(registration),
            NativeLifecycle::Bootstrap(_) => None,
        }
    }
    /// Current native lifecycle, including initialization-only authority.
    /// Absence is never a reason to invent or infer a bootstrap registration.
    pub fn lifecycle(&self) -> Option<&NativeLifecycle> {
        (!self.closed && !self.retired)
            .then_some(self.current.as_ref())
            .flatten()
    }
    /// Record one fresh native call before host permission/dispatch.
    /// Identical pending retransmission is idempotent, without reopening a
    /// consumed/revoked observation. Parallel observations may arrive reordered.
    pub fn record(&mut self, call: NativeCall) -> Result<bool> {
        let current = self.lifecycle().ok_or_else(required)?;
        if call.observer != current.observer()
            || call.generation != current.generation()
            || call.root != current.root()
            || !nonce(&call.tool_use_id)
            || !call.tool_name.starts_with(&self.prefix)
            || call.tool_name.len() > self.prefix.len() + 128
            || call.tool_name.len() == self.prefix.len()
            || !identifier(&call.tool_name)
        {
            return Err(required());
        }
        let hash = hash(&call.tool_use_id);
        if let Some(old) = self.pending.get(&hash) {
            return if old == &call {
                Ok(false)
            } else {
                Err(required())
            };
        }
        if self.pending.len() >= 32 {
            let mut error =
                Error::new(ErrorCode::Busy, "Native call correlation capacity is full.");
            error.retryable = true;
            return Err(error);
        }
        self.replay.record(call.sequence, hash)?;
        self.pending.insert(hash, call);
        Ok(true)
    }
    /// Consume exact current-root proof for one MCP name and native metadata ID.
    /// A name mismatch consumes/refuses the proof rather than lending it later.
    pub fn consume(&mut self, tool_name: &str, tool_use_id: &str) -> Result<CallScope> {
        match self.consume_scope(tool_name, tool_use_id)? {
            NativeScope::Project(scope) => Ok(scope),
            NativeScope::Bootstrap { .. } => Err(required()),
        }
    }
    /// Consume exact one-use proof without treating a bootstrap nonce as context.
    /// Even a wrong-stage/name request consumes its proof, preventing fallback.
    pub fn consume_scope(&mut self, tool_name: &str, tool_use_id: &str) -> Result<NativeScope> {
        if !nonce(tool_use_id) || tool_name.len() > 128 || !identifier(tool_name) {
            return Err(required());
        }
        let scope = self.lifecycle().ok_or_else(required)?.scope();
        let call = self
            .pending
            .remove(&hash(tool_use_id))
            .ok_or_else(required)?;
        if call.tool_name != format!("{}{tool_name}", self.prefix) {
            return Err(required());
        }
        Ok(scope)
    }
    /// Revoke one exact native call after cancellation/completion; stale roots
    /// cannot revoke a replacement root's work. No reply/receipt is produced.
    pub fn revoke(
        &mut self,
        observer: &str,
        generation: u64,
        root: Uuid,
        tool_use_id: &str,
    ) -> bool {
        if !nonce(tool_use_id) || !self.matches(observer, generation, root) {
            return false;
        }
        self.pending.remove(&hash(tool_use_id)).is_some()
    }
    /// Retire one exact lifecycle while keeping the physical owner available
    /// for a later fresh generation. Old end callbacks are harmless no-ops.
    pub fn retire(&mut self, observer: &str, generation: u64, root: Uuid) -> Option<CallScope> {
        match self.retire_scope(observer, generation, root)? {
            NativeScope::Project(scope) => Some(scope),
            NativeScope::Bootstrap { .. } => None,
        }
    }
    /// Retire either exact native stage; late callbacks cannot retire promotion.
    pub fn retire_scope(
        &mut self,
        observer: &str,
        generation: u64,
        root: Uuid,
    ) -> Option<NativeScope> {
        if !self.matches(observer, generation, root) {
            return None;
        }
        let scope = self.current.as_ref()?.scope();
        self.retired = true;
        self.pending.clear();
        Some(scope)
    }
    /// Permanently close this physical owner; no later registration can revive it.
    pub fn close(&mut self) {
        self.closed = true;
        self.pending.clear();
    }
    fn matches(&self, observer: &str, generation: u64, root: Uuid) -> bool {
        self.lifecycle().is_some_and(|c| {
            c.observer() == observer && c.generation() == generation && c.root() == root
        })
    }
}
pub(super) fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
pub(super) fn nonce(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && identifier(value)
}
fn hash(value: &str) -> [u8; 32] {
    Sha256::digest(value.as_bytes()).into()
}
fn required() -> Error {
    Error::new(
        ErrorCode::SessionRequired,
        "Fresh exact native call/lifecycle evidence is required.",
    )
}
