//! Per-call host verification selects a fixed attachment, never a new identity.

use crate::{CallScope, ConversationKey, Conversations, Server};
use agentchat_core::{Error, ErrorCode, Result};
use async_trait::async_trait;
use rmcp::{
    RoleServer,
    model::{CallToolRequestParams, CallToolResponse, RequestMetaObject},
    service::RequestContext,
};
use std::{
    sync::{Arc, Weak},
    time::Duration,
    time::Instant,
};

/// Transport facts available to the native identity verifier, without bodies.
pub struct CallEvidence<'a> {
    /// Exact MCP tool name; correlate it with the native hook's tool name.
    pub tool_name: &'a str,
    /// Untrusted native request metadata; it is not identity on its own.
    pub metadata: Option<&'a RequestMetaObject>,
}
/// Exact per-call host authority, supplied by an explicitly composed adapter.
///
/// Correlate fresh native tool-use evidence with a verified current lifecycle.
/// Reject missing, ambiguous, stale and replayed proof. Never infer identity
/// from tool arguments, process cwd, display name or the MCP spawn environment.
/// Success neither joins/restores an agent nor establishes wake eligibility.
#[async_trait]
pub trait CallVerifier: Send + Sync {
    /// Consume one call's identity proof and return its exact lifecycle key.
    async fn verify(&self, evidence: CallEvidence<'_>) -> Result<ConversationKey>;
    /// Consume fresh native proof for either a context or a bootstrap lifecycle.
    /// The default retains exact-context verification. A host overriding this
    /// method must independently prove the native root even before project init;
    /// absence of a context, observer or proof is never a bootstrap fallback.
    async fn verify_scope(&self, evidence: CallEvidence<'_>) -> Result<CallScope> {
        self.verify(evidence).await.map(CallScope::Conversation)
    }
}

#[derive(Clone)]
pub(crate) struct Routing {
    pub conversations: Weak<Conversations>,
    verifier: Arc<dyn CallVerifier>,
    calls: Arc<tokio::sync::Mutex<()>>,
}
impl Server {
    /// Compose a retained transport with distinct immutable root attachments.
    /// Discovery is inert; every tool call requires fresh native verification.
    /// EOF permanently closes the supplied Conversations control. This method
    /// holds only a weak reference: retain the lifecycle control in the host
    /// authority, not this transport. Losing that control retires attachments.
    /// It admits no host and is not enabled by generic `agentchat mcp` startup.
    pub fn gateway(conversations: Arc<Conversations>, verifier: Arc<dyn CallVerifier>) -> Self {
        let mut server = Self::new(conversations.backend.clone());
        server.routing = Some(Routing {
            conversations: Arc::downgrade(&conversations),
            verifier,
            calls: Arc::default(),
        });
        server
    }
    pub(super) async fn route_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> std::result::Result<CallToolResponse, rmcp::ErrorData> {
        let Some(routing) = &self.routing else {
            return self.dispatch_tool(request, context).await;
        };
        let proof = CallEvidence {
            tool_name: &request.name,
            // The pinned SDK moves wire _meta into RequestContext before
            // dispatch, leaving the params field empty. Verify the received
            // metadata there; an argument or empty params is no fallback.
            metadata: Some(&context.meta),
        };
        let started = Instant::now();
        let selected = match tokio::time::timeout(Duration::from_secs(5), async {
            // Retain this gate through dispatch: otherwise a second proof
            // can succeed while the first call still owns binding state,
            // merely moving the same avoidable Busy to another layer.
            // Queueing and fresh verification share the existing deadline;
            // the transport already bounds outstanding calls to 32.
            let guard = routing.calls.lock().await;
            let key = routing.verifier.verify_scope(proof).await?;
            let server = routing
                .conversations
                .upgrade()
                .ok_or_else(crate::conversations::unavailable)?
                .resolve_scope(key)?;
            Ok::<_, Error>((guard, server))
        })
        .await
        {
            Ok(result) => result,
            Err(_) => {
                let mut error = Error::new(
                    ErrorCode::Busy,
                    "Native call identity verification exceeded its bounded deadline.",
                );
                error.retryable = true;
                Err(error)
            }
        };
        let verification_us = crate::timings::elapsed(started);
        let verified = selected.is_ok();
        let executing = Instant::now();
        let activating = request.name == "agentchat_join";
        let _dispatch_guard;
        let mut result = match selected {
            Ok((call_guard, mut server)) => {
                _dispatch_guard = Some(call_guard);
                server.timings = self.timings.clone();
                server.dispatch_tool(request, context).await
            }
            Err(error) => {
                _dispatch_guard = None;
                let id = request
                    .arguments
                    .as_ref()
                    .and_then(|args| args.get("request_id"))
                    .and_then(|id| id.as_str())
                    .and_then(|id| id.parse().ok());
                Ok(CallToolResponse::Complete(self.invalid(id, error)))
            }
        };
        if activating
            && let Ok(CallToolResponse::Complete(response)) = &result
            && let Some(receipt) = response.structured_content.as_ref()
            && let Some(context) = receipt
                .get("result")
                .and_then(|v| v.get("context_id"))
                .and_then(|v| v.as_str())
                .and_then(|v| v.parse().ok())
            && let Some(conversations) = routing.conversations.upgrade()
            && let Err(mut error) = conversations.restore_committed_join(context).await
        {
            let id = receipt
                .get("request_id")
                .and_then(|v| v.as_str())
                .and_then(|v| v.parse().ok())
                .unwrap_or_else(uuid::Uuid::new_v4);
            let project = receipt
                .get("project_id")
                .and_then(|v| v.as_str())
                .and_then(|v| v.parse().ok());
            error.details["request_processed"] = true.into();
            error.details["request_id"] = id.to_string().into();
            result = Ok(CallToolResponse::Complete(crate::result::wrap(
                agentchat_protocol::envelope::Envelope::new(id, project, Err(error)),
            )));
        }
        // Only fixed-shape timings and the public correlation UUID are retained.
        // No arguments, bodies, native root IDs or arbitrary error text enter
        // diagnostics; dropping a sample cannot alter any call outcome.
        if let Ok(CallToolResponse::Complete(response)) = &result
            && let Some(id) = response
                .structured_content
                .as_ref()
                .and_then(|v| v.get("request_id"))
                .and_then(|v| v.as_str())
                .and_then(|id| id.parse().ok())
        {
            self.timings.record(
                id,
                verification_us,
                crate::timings::elapsed(executing),
                verified,
            );
        }
        result
    }
    async fn dispatch_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> std::result::Result<CallToolResponse, rmcp::ErrorData> {
        let cancellation = context.ct.clone();
        // Only a host-verified same-conversation replacement may preserve an
        // in-flight Join receipt. Root changes and physical control loss still
        // retire that wait; every other call retains ordinary cancellation.
        let activating = request.name == "agentchat_join";
        let call = rmcp::handler::server::tool::ToolCallContext::new(self, request, context);
        tokio::select! {
            biased;
            _ = self.call_retirement(activating) => Err(rmcp::ErrorData::internal_error("Conversation attachment retired; retain the original mutation request_id.", None)),
            _ = cancellation.cancelled() => Err(rmcp::ErrorData::internal_error("Tool call cancelled; retry mutations using the same request_id.", None)),
            result = self.tools.call(call) => result,
        }
    }
}
