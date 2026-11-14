//! Official-SDK MCP tools over the same public operations used by the CLI.

mod conversations;
mod info;
mod lifetime;
mod params;
mod result;
mod routing;
mod scope;
mod state;
mod stdio;
mod timings;
mod tools;
mod transport;

use agentchat_protocol::operations::{Backend, Target};
use rmcp::{ServerHandler, handler::server::router::tool::ToolRouter, tool_handler};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Immutable application binding, or an explicitly composed transport gateway.
/// Neither mode infers identity from its process working directory.
#[derive(Clone)]
pub struct Server {
    backend: Arc<dyn Backend>,
    state: Arc<Mutex<state::State>>,
    activation_replacement: Arc<std::sync::atomic::AtomicBool>,
    activation_group: Option<tokio::sync::watch::Receiver<bool>>,
    operation: Arc<Mutex<()>>,
    tools: ToolRouter<Self>,
    lifetime: Option<tokio::sync::watch::Receiver<bool>>,
    required_context: Option<uuid::Uuid>,
    bootstrap: bool,
    initialization_identity: Option<agentchat_protocol::operations::InitializationIdentity>,
    routing: Option<routing::Routing>,
    native_host: Option<agentchat_core::model::HostKind>,
    native_namespace: Option<String>,
    timings: timings::Timings,
}
impl Server {
    /// Create an unbound server without storage discovery or runtime startup.
    pub fn new(backend: Arc<dyn Backend>) -> Self {
        Self {
            backend,
            state: Arc::new(Mutex::new(state::State::default())),
            activation_replacement: Arc::default(),
            activation_group: None,
            operation: Arc::default(),
            tools: Self::project_tools()
                + Self::identity_tools()
                + Self::message_tools()
                + Self::reservation_tools(),
            lifetime: None,
            required_context: None,
            bootstrap: false,
            initialization_identity: None,
            routing: None,
            native_host: None,
            native_namespace: None,
            timings: timings::Timings::default(),
        }
    }
    /// Create a fresh immutable application server owned by this host lifetime.
    /// Retirement cancels its calls and closes a transport serving this server;
    /// it never makes this server reusable by a different root conversation.
    pub fn with_lifetime(backend: Arc<dyn Backend>, lifetime: &ConnectionLifetime) -> Self {
        let mut server = Self::new(backend);
        server.lifetime = Some(lifetime.subscribe());
        server
    }
    /// Restore a retained joined context supplied explicitly by a host adapter.
    /// This never invites an unjoined conversation or guesses from process cwd.
    pub async fn restore(&self, target: Target) -> agentchat_core::Result<()> {
        if self.routing.is_some() || self.bootstrap {
            return Err(conversations::unavailable());
        }
        tokio::select! {
            biased;
            _ = self.retirement() => Err(conversations::unavailable()),
            result = self.restore_binding(target) => result,
        }
    }
}
#[tool_handler(router=self.tools)]
impl ServerHandler for Server {
    fn get_info(&self) -> rmcp::model::ServerConfig {
        self.information()
    }
    fn supported_protocol_versions(
        &self,
    ) -> std::borrow::Cow<'static, [rmcp::model::ProtocolVersion]> {
        use rmcp::model::ProtocolVersion as V;
        // Complete results require structuredContent, and binding requires the
        // session lifecycle. Future stateless revisions need separate review.
        std::borrow::Cow::Borrowed(&[V::V_2025_06_18, V::V_2025_11_25])
    }
    async fn call_tool(
        &self,
        request: rmcp::model::CallToolRequestParams,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::CallToolResponse, rmcp::ErrorData> {
        let cancellation = context.ct.clone();
        let activating = request.name == "agentchat_join";
        tokio::select! {
            biased;
            _ = self.call_retirement(activating) => Err(rmcp::ErrorData::internal_error("Host connection retired; use a fresh verified connection and the original mutation request_id.", None)),
            _ = cancellation.cancelled() => Err(rmcp::ErrorData::internal_error("Tool call cancelled; retry mutations using the same request_id.", None)),
            result = self.route_tool(request, context) => result,
        }
    }
}

pub use conversations::{Conversation, ConversationKey, Conversations};
pub use lifetime::ConnectionLifetime;
pub use routing::{CallEvidence, CallVerifier};
pub use scope::{BootstrapKey, CallScope};
pub use transport::{serve, stdio};
