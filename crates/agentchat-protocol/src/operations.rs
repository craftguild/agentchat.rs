//! Shared public operation boundary; CLI and MCP only adapt typed inputs.

use crate::envelope::Envelope;
use agentchat_core::{
    Error, ErrorCode,
    application::Mutation,
    model::{ActorSelector, HostKind},
    project::Scope,
    read::Query,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use uuid::Uuid;

/// Explicit routing inputs. An omitted context never means cwd-based identity.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    /// Captured namespace guard, set by a bound transport rather than user JSON.
    #[serde(skip)]
    pub expected_project_id: Option<Uuid>,
    /// Verified native initializer before a project context exists. This is
    /// host-composition metadata, never accepted from tool/CLI/wire arguments.
    #[serde(skip)]
    pub initialization_identity: Option<InitializationIdentity>,
    /// Project discovery path, mandatory for unbound MCP project operations.
    pub project_path: Option<PathBuf>,
    /// Explicit public operation subject; omission is observer-only for reads.
    pub actor: Option<ActorSelector>,
    /// Private native descriptor supplied by a verified integration connection.
    pub context_id: Option<Uuid>,
}
/// Exact native conversation authorizing project initialization without joining.
/// A bounded value is not proof: the supplying host adapter must independently
/// verify its root/lifecycle on every call. This grants no application context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitializationIdentity {
    host: HostKind,
    host_session_id: String,
}
impl InitializationIdentity {
    /// Capture already-verified native identity, independent of cwd/PID/name.
    pub fn new(host: HostKind, host_session_id: String) -> agentchat_core::Result<Self> {
        if host_session_id.is_empty() || host_session_id.len() > 1024 {
            return Err(Error::new(
                ErrorCode::SessionRequired,
                "Initialization identity requires an exact bounded native conversation.",
            ));
        }
        Ok(Self {
            host,
            host_session_id,
        })
    }
    /// Native host kind supplied by the adapter's verified lifecycle.
    pub fn host(&self) -> HostKind {
        self.host
    }
    /// Exact retained root conversation ID, never an application context ID.
    pub fn host_session_id(&self) -> &str {
        &self.host_session_id
    }
}
/// Exact live identity restored through the shared application lifecycle path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    /// Captured namespace, fixed for the lifetime of the public connection.
    pub project_id: Uuid,
    /// Exact descriptor, never a process or directory guess.
    pub context_id: Uuid,
    /// Retained participant assigned to this exact host conversation.
    pub agent_id: Uuid,
}
/// Owned lifecycle attachment. Dropping it closes its integration connection.
#[async_trait]
pub trait Attachment: Send + Sync {
    /// Exact restored binding.
    fn binding(&self) -> &Binding;
    /// Read-only liveness check; this must not generate a host heartbeat.
    async fn check(&self) -> agentchat_core::Result<()>;
    /// Refresh this live incarnation through the shared verified heartbeat.
    /// Host control changes are hints to recheck, never supplied phase facts.
    /// This must not restore, join or revive an expired/retired attachment.
    async fn refresh(&self) -> agentchat_core::Result<()>;
}
/// Finite public operations shared by both transports.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    /// Recoverably initialize project storage and request host installation.
    Initialize {
        /// Explicit discovery scope, otherwise the existing/default scope.
        scope: Option<Scope>,
        /// Explicit ordered project defaults, or omission to retain/default them.
        ///
        /// Values remain raw at this transport boundary. Shared project policy
        /// performs normalization, validation and first-occurrence deduplication.
        default_channels: Option<Vec<String>>,
        /// Optional automatic message-read setting; false is an explicit update.
        auto_read: Option<bool>,
        /// Optional positive whole-second processing timeout; new default is 3600.
        delivery_confirmation_timeout_seconds: Option<u32>,
        /// Adopt a moved root only after its old canonical root disappeared.
        relocate: bool,
        /// Allocate a fresh namespace while retaining the old database.
        new_project: bool,
    },
    /// Existing shared application mutation, with no transport-specific policy.
    Mutate {
        /// Concrete typed mutation.
        command: Mutation,
    },
    /// Existing bounded read-only application query.
    Query {
        /// Concrete typed query.
        query: Query,
    },
    /// Storage/runtime/integration diagnostics without repair or trust approval.
    Doctor,
}
/// Runtime-composed application port. MCP does not depend on storage or runtime.
#[async_trait]
pub trait Backend: Send + Sync {
    /// Restore a retained joined conversation without creating participation.
    async fn attach(&self, target: Target) -> agentchat_core::Result<Box<dyn Attachment>>;
    /// Invoke one operation, always retaining its public correlation/retry key.
    async fn invoke(
        &self,
        request_id: Uuid,
        target: Target,
        operation: Operation,
    ) -> Envelope<Value>;
}
