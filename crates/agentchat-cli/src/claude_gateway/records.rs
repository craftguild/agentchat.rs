//! Private native-control records. None of these values establishes a root alone.
use agentchat_hosts::claude::NativeCall;
pub use agentchat_hosts::claude::metadata::{private_directory, read, remove_if, write};
pub use agentchat_hosts::claude::namespace::LifecycleOwner;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Composition/transport failures; application operation envelopes remain typed.
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
/// Private locator for one physical transport's control channel.
pub use agentchat_hosts::claude::namespace::TransportEndpoint as Endpoint;
/// Native-only lifecycle controls, unavailable as MCP application tools.
#[derive(Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Control {
    /// Activate a fresh immutable native lifecycle through its callback.
    Register {
        /// Exact callback/root/process facts.
        registration: Box<LifecycleOwner>,
    },
    /// Record one exact native tool-use proof after validating its lifecycle.
    Record {
        /// Current callback/root/process facts.
        registration: Box<LifecycleOwner>,
        /// One-use native call correlation.
        call: NativeCall,
    },
    /// Retire the proof of a completed or cancelled native call.
    Revoke {
        /// Exact observer incarnation.
        observer: String,
        /// Shared monotonic lifecycle generation.
        generation: u64,
        /// Exact native root.
        root: Uuid,
        /// Exact native tool-use ID.
        tool_use_id: String,
    },
    /// Retire only the named immutable lifecycle.
    Retire {
        /// Exact observer incarnation.
        observer: String,
        /// Shared monotonic lifecycle generation.
        generation: u64,
        /// Exact native root.
        root: Uuid,
    },
    /// Hint that a fresh shared-operation availability check is needed.
    Refresh {
        /// Exact observer incarnation.
        observer: String,
        /// Shared monotonic lifecycle generation.
        generation: u64,
        /// Exact native root.
        root: Uuid,
    },
}
/// One private control frame; the token cannot substitute for native proof.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// Exact control endpoint incarnation.
    pub nonce: Uuid,
    /// Native-only action.
    pub control: Control,
}
/// Control transport result, never delivery acknowledgement or reading.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    /// Whether this control was accepted.
    pub accepted: bool,
    /// Stable native/application classification when refused.
    pub error: Option<agentchat_core::ErrorCode>,
}
/// Locate the physical owner's control publication, without asserting identity.
pub fn endpoint(root: &Path, owner: Uuid) -> PathBuf {
    root.join(format!("gateway-{owner}.json"))
}
/// Locate only the pending native callback; it must be independently verified.
pub fn current_owner(root: &Path, owner: Uuid) -> PathBuf {
    root.join(format!("current-owner-{owner}.json"))
}
