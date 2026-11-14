//! Transport-neutral result envelopes and explicit project/protocol handshake.

use agentchat_core::{Error, ErrorCode, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Supported public and local RPC major version.
pub const VERSION: u32 = 1;
/// Maximum serialized page payload, leaving headroom within the wire frame.
pub const MAX_PAGE_BYTES: usize = 768 * 1_024;
/// Maximum wake context, including references, preview and pending count.
pub const MAX_WAKE_BYTES: usize = agentchat_core::delivery::MAX_WAKE_BYTES;
/// Maximum event references in a wake hint.
pub const MAX_WAKE_EVENTS: usize = agentchat_core::delivery::MAX_WAKE_EVENTS;

/// Result discriminant guarantees exactly one `result` or `error` field.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum Outcome<T> {
    /// Committed application success or a read-only result.
    Success {
        /// Typed application result.
        result: T,
    },
    /// Machine-actionable failure.
    Failure {
        /// Shared application error.
        error: Error,
    },
}

/// Same versioned JSON result for CLI and typed MCP tool results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope<T> {
    /// Public envelope major version.
    pub version: u32,
    /// Mutation retry key or operation correlation UUID.
    pub request_id: Uuid,
    /// Discovered project; absent only before successful discovery.
    pub project_id: Option<Uuid>,
    /// Successful result or unsuccessful error, never both.
    #[serde(flatten)]
    pub outcome: Outcome<T>,
}

impl<T> Envelope<T> {
    /// Adapt a typed successful payload while retaining every envelope guard.
    pub fn map<U>(self, map: impl FnOnce(T) -> U) -> Envelope<U> {
        Envelope {
            version: self.version,
            request_id: self.request_id,
            project_id: self.project_id,
            outcome: match self.outcome {
                Outcome::Success { result } => Outcome::Success {
                    result: map(result),
                },
                Outcome::Failure { error } => Outcome::Failure { error },
            },
        }
    }

    /// Adapt an application result without taking ownership of business policy.
    pub fn new(request_id: Uuid, project_id: Option<Uuid>, result: Result<T>) -> Self {
        Self {
            version: VERSION,
            request_id,
            project_id,
            outcome: match result {
                Ok(result) => Outcome::Success { result },
                Err(error) => Outcome::Failure { error },
            },
        }
    }
}

/// First local RPC exchange, before commands or subscription data are accepted.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Handshake {
    /// Supported protocol major.
    pub version: u32,
    /// Expected UUID, so a stale/wrong socket can never select another project.
    pub project_id: Uuid,
}

impl Handshake {
    /// Verify both independent identity and protocol constraints.
    pub fn validate(&self, project_id: Uuid) -> Result<()> {
        if self.version != VERSION {
            return Err(Error::new(
                ErrorCode::UnsupportedProtocol,
                "Unsupported local protocol major version.",
            ));
        }
        if self.project_id != project_id {
            return Err(Error::new(
                ErrorCode::ProjectBindingMismatch,
                "Socket belongs to a different project UUID.",
            ));
        }
        Ok(())
    }
}
