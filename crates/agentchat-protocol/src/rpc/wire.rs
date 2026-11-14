//! Typed allowlisted methods; JSON-RPC identifiers are separate from retry keys.

use crate::envelope::{Envelope, Handshake};
use agentchat_core::{
    Error, Result,
    application::{ManagedActivationCause, Mutation},
    read::Query,
};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// Standard JSON-RPC correlation identifier; never a conversation identity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Id {
    /// String correlation.
    Text(String),
    /// JSON number correlation, preserved without conversion to a request UUID.
    Number(serde_json::Number),
    /// Explicit null is distinct from an omitted notification identifier.
    Null,
}

/// Exact local operation subject; a descriptor still requires registry/owner proof.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ActorReference {
    /// Private registry-issued native descriptor, never caller-supplied host facts.
    Native {
        /// Exact context UUID resolved by the runtime.
        context_id: Uuid,
    },
    /// One project-common nonreceiving user; closed fields prevent fake native facts.
    User {},
}

impl ActorReference {
    /// Exact native descriptor, absent for anonymous participation.
    pub fn context_id(self) -> Option<Uuid> {
        match self {
            Self::Native { context_id } => Some(context_id),
            Self::User {} => None,
        }
    }
}

/// Versioned local methods. No arbitrary command, SQL or shell execution exists.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", deny_unknown_fields)]
pub enum Call {
    /// Dedicated context-free typed retained-change observation.
    #[serde(rename = "viewer.subscribe")]
    SubscribeChanges {
        /// Explicit validated last-applied boundary for durable replay.
        after: agentchat_core::read::Checkpoint,
    },
    /// Hold ordinary runtime activity without a participant or event-body queue.
    #[serde(rename = "observer.attach")]
    Observe,
    /// Must precede all commands/subscription data on a connection.
    #[serde(rename = "handshake")]
    Handshake(Handshake),
    /// Same typed business operation used by both public transports.
    #[serde(rename = "application.mutate")]
    Mutate {
        /// Stable semantic retry key, independent of JSON-RPC correlation.
        request_id: Uuid,
        /// Selected actor; native references remain exact private descriptors.
        actor: ActorReference,
        /// Validated application mutation.
        command: Mutation,
    },
    /// Pure finite application retrieval.
    #[serde(rename = "application.query")]
    Query {
        /// Selected actor, or explicit context-free observation on an unbound socket.
        actor: Option<ActorReference>,
        /// Typed finite query.
        query: Query,
    },
    /// Private native recovery read, independent of the public whoami projection.
    #[serde(rename = "native.binding")]
    NativeBinding {
        /// Exact native descriptor; no anonymous or observer binding exists.
        context_id: Uuid,
    },
    /// Internal actor-free health used by runtime diagnostics.
    #[serde(rename = "internal.health")]
    InternalHealth,
    /// Native startup handshake after exact launch/root proof, outside the model.
    /// Parameters cannot supply or manufacture a process owner or generation.
    #[serde(rename = "launch.start")]
    StartLaunch {
        /// Exact immutable descriptor proved by the launch-aware host gate.
        context_id: Uuid,
        /// Optional immutable normalized participant handle.
        name: Option<String>,
    },
    /// Internal verified managed-launch activation intent. Host authority is
    /// resolved and rechecked by the runtime/Core; these fields grant none.
    #[serde(rename = "launch.activate_managed.v1")]
    ActivateManaged {
        /// Stable semantic retry key retained after an unknown RPC result.
        request_id: Uuid,
        /// Exact registry-issued target descriptor.
        context_id: Uuid,
        /// Initial readiness or one correlated distinct-root transition.
        cause: ManagedActivationCause,
        /// Optional launcher-selected immutable participant handle.
        name: Option<String>,
    },
    /// Reconcile positive original-owner loss through the shared application.
    #[serde(rename = "launch.reconcile")]
    ReconcileLaunch {
        /// Exact descriptor for routing and local queued-job cancellation.
        context_id: Uuid,
        /// Original retained generation; a successor cannot be retired by it.
        generation: Uuid,
    },
    /// Internal native-hook fence; never dispatches, binds or refreshes presence.
    #[serde(rename = "delivery.check.v2")]
    CheckDelivery {
        /// Exact current immutable descriptor.
        context_id: Uuid,
        /// Persisted wake batch whose current attempt is being accepted.
        batch_id: Uuid,
        /// Unique durable attempt token, not a transport receipt.
        token: Uuid,
        /// Exact bounded delivery references frozen into this native hint.
        delivery_ids: Vec<Uuid>,
    },
    /// Full-body final-hook fence; snapshot only, without native/application I/O.
    #[serde(rename = "message.check.v2")]
    CheckMessage {
        /// Exact immutable application connection.
        context_id: Uuid,
        /// Original native launch generation.
        generation: Uuid,
        /// Exact stable recipient message delivery.
        delivery_id: Uuid,
        /// Current committed head attempt token.
        token: Uuid,
    },
    /// Unified body/notice input fence; snapshot only and never a receipt.
    #[serde(rename = "native.check.v1")]
    CheckNative {
        /// Exact immutable application connection.
        context_id: Uuid,
        /// Original native launch generation.
        generation: Uuid,
        /// Exact stable recipient delivery of either native input kind.
        delivery_id: Uuid,
        /// Current committed head attempt token.
        token: Uuid,
    },
    /// Explicit integration connection restoration; new identities stay unjoined.
    #[serde(rename = "session.attach")]
    Attach {
        /// Exact fresh descriptor.
        context_id: Uuid,
    },
    /// Actual host liveness proof is verified by the application's host gate.
    #[serde(rename = "session.heartbeat")]
    Heartbeat {
        /// Exact current descriptor.
        context_id: Uuid,
    },
    /// Authoritative lifecycle detach, not participation leave.
    #[serde(rename = "session.detach")]
    Detach {
        /// Exact current descriptor.
        context_id: Uuid,
    },
}
impl Call {
    /// Methods that consume a dedicated observer connection after setup.
    pub fn dedicated(&self) -> bool {
        matches!(self, Self::SubscribeChanges { .. } | Self::Observe)
    }
    /// Whether a wire method is in this explicit versioned allowlist.
    pub fn known(method: &str) -> bool {
        matches!(
            method,
            "handshake"
                | "application.mutate"
                | "application.query"
                | "launch.start"
                | "launch.activate_managed.v1"
                | "launch.reconcile"
                | "delivery.check.v2"
                | "message.check.v2"
                | "native.check.v1"
                | "session.attach"
                | "session.heartbeat"
                | "session.detach"
                | "native.binding"
                | "internal.health"
                | "viewer.subscribe"
                | "observer.attach"
        )
    }
    /// Explicit descriptor supplied by this call, if any.
    pub fn context_id(&self) -> Option<Uuid> {
        match self {
            Self::StartLaunch { context_id, .. }
            | Self::ActivateManaged { context_id, .. }
            | Self::ReconcileLaunch { context_id, .. }
            | Self::CheckDelivery { context_id, .. }
            | Self::CheckMessage { context_id, .. }
            | Self::CheckNative { context_id, .. }
            | Self::Attach { context_id }
            | Self::Heartbeat { context_id }
            | Self::Detach { context_id }
            | Self::NativeBinding { context_id } => Some(*context_id),
            Self::Mutate { actor, .. } => actor.context_id(),
            Self::Query { actor, .. } => actor.and_then(ActorReference::context_id),
            _ => None,
        }
    }
    /// Anonymous calls must never inherit a socket's retained native binding.
    pub fn selects_user(&self) -> bool {
        matches!(
            self,
            Self::Mutate {
                actor: ActorReference::User {},
                ..
            } | Self::Query {
                actor: Some(ActorReference::User {}),
                ..
            }
        )
    }
    /// Only native mutations use the existing full-commit grouping lane.
    pub fn grouped_mutation(&self) -> bool {
        matches!(
            self,
            Self::Mutate {
                actor: ActorReference::Native { .. },
                ..
            }
        )
    }
    /// Successful mutation result correlation uses its semantic retry key.
    pub fn result_key(&self, fallback: Uuid) -> Uuid {
        match self {
            Self::Mutate { request_id, .. } | Self::ActivateManaged { request_id, .. } => {
                *request_id
            }
            _ => fallback,
        }
    }
}

fn present_id<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<Option<Id>, D::Error> {
    Id::deserialize(d).map(Some)
}

/// Strict JSON-RPC request. Missing id is a notification and receives no reply.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// Exact JSON-RPC version string.
    pub jsonrpc: String,
    /// Correlation, or absent for a notification.
    #[serde(
        default,
        deserialize_with = "present_id",
        skip_serializing_if = "Option::is_none"
    )]
    pub id: Option<Id>,
    /// Explicit allowlisted method name.
    pub method: String,
    /// Bounded typed-method parameters, decoded before application invocation.
    #[serde(default)]
    pub params: Value,
}
impl Request {
    /// Adapt one typed operation to a UUID-correlated request.
    pub fn new(id: Uuid, call: &Call) -> Result<Self> {
        let value =
            serde_json::to_value(call).map_err(|_| super::invalid("Call serialization failed."))?;
        Ok(Self {
            jsonrpc: "2.0".into(),
            id: Some(Id::Text(id.to_string())),
            method: value["method"]
                .as_str()
                .ok_or_else(|| super::invalid("Missing typed method."))?
                .into(),
            params: value["params"].clone(),
        })
    }
    /// Decode known parameters without accepting arbitrary dispatch targets.
    pub fn call(&self) -> Result<Call> {
        if self.jsonrpc != "2.0" {
            return Err(super::invalid("Unsupported JSON-RPC version."));
        }
        if let Some(Id::Text(id)) = &self.id
            && id.len() > 128
        {
            return Err(super::invalid("RPC correlation identifier is too long."));
        }
        serde_json::from_value(serde_json::json!({"method":self.method,"params":self.params}))
            .map_err(|_| {
                agentchat_core::Error::new(
                    agentchat_core::ErrorCode::InvalidArgument,
                    "Invalid typed RPC method parameters.",
                )
            })
    }
    /// UUID envelope correlation, preserving an explicit mutation's retry key.
    pub fn result_key(&self, call: &Call) -> Uuid {
        let key = match &self.id {
            Some(Id::Text(id)) => Uuid::parse_str(id).unwrap_or_else(|_| Uuid::new_v4()),
            _ => Uuid::new_v4(),
        };
        call.result_key(key)
    }
}

/// Numeric JSON-RPC error with the unchanged domain error in data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    /// Standard protocol code or -32000 for application failure.
    pub code: i32,
    /// Bounded human explanation.
    pub message: String,
    /// Stable domain classification, retry policy and structured details.
    pub data: Error,
}
/// Exactly one successful envelope or numeric JSON-RPC error.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum ReplyBody<T = Value> {
    /// Successful application envelope.
    Success {
        /// Shared CLI/MCP result envelope.
        result: Envelope<T>,
    },
    /// Transport or application failure.
    Failure {
        /// JSON-RPC error.
        error: RpcError,
    },
}
/// Dynamic JSON reply for existing transport presentation and raw-wire callers.
pub type Reply = TypedReply<Value>;
/// JSON-RPC response; an unidentifiable malformed request uses null id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypedReply<T> {
    /// Exact JSON-RPC version.
    pub jsonrpc: String,
    /// Echoed correlation, or null for an invalid request.
    pub id: Option<Id>,
    /// Result/error discriminant.
    #[serde(flatten)]
    pub body: ReplyBody<T>,
}
impl<T> TypedReply<T> {
    /// Adapt one successfully executed call.
    pub fn success(id: Option<Id>, key: Uuid, project: Uuid, result: T) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            body: ReplyBody::Success {
                result: Envelope::new(key, Some(project), Ok(result)),
            },
        }
    }
    /// Preserve the machine-actionable domain failure.
    pub fn failure(id: Option<Id>, code: i32, error: Error) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            body: ReplyBody::Failure {
                error: RpcError {
                    code,
                    message: error.message.clone(),
                    data: error,
                },
            },
        }
    }
}
