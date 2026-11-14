//! Versioned, bounded transport data and framing for AgentChat.

pub mod envelope;
pub mod framing;
pub mod health;
pub mod integration;
pub mod operations;
pub mod removal;
pub mod rpc;
pub mod viewer;
pub mod viewer_write;

/// Native MCP capability epoch for physical readiness and explicit participation.
/// This is independent of the public CLI/MCP result envelope version. Cached
/// helpers implementing automatic startup membership cannot satisfy this epoch.
pub const NATIVE_ROUTING_PROTOCOL: u32 = 3;
