//! Native shared-server MCP metadata selects an independently live launch root.
use agentchat_core::{
    Error, ErrorCode, Result,
    model::{HostKind, Phase},
};
use agentchat_hosts::{
    claude::process::ProcessIdentity,
    codex::{Client, Daemon, NativeAdapter, NativeRegistry, NativeRoute},
    launch::{LaunchOwners, Registry},
};
use agentchat_mcp::{
    BootstrapKey, CallEvidence, CallScope, CallVerifier, ConversationKey, Conversations,
};
use agentchat_protocol::operations::{Backend, InitializationIdentity};
use agentchat_runtime::project::ProjectManager;
use async_trait::async_trait;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::sync::Mutex;
use uuid::Uuid;

struct Verifier {
    parent: ProcessIdentity,
    daemon: Daemon,
    registry: NativeRegistry,
    launches: Registry,
    adapter: NativeAdapter,
    roots: Arc<Conversations>,
    bootstrap: Uuid,
    state: Mutex<State>,
}
#[derive(Default)]
struct State {
    root: Option<Uuid>,
    key: Option<ConversationKey>,
    owner: Option<String>,
    restored: bool,
    bootstrapped: bool,
}

impl Verifier {
    async fn startup_route(&self, root: Uuid) -> Result<Option<NativeRoute>> {
        // Direct native startup cannot hold a CLI resume response. A first AGENTCHAT
        // call may race the supervisor's readiness publication. Wait
        // within the existing five-second verification budget, never perform
        // a join here, and never use cached native rows as launch authority.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
        loop {
            if !self.parent.is_continuing() {
                return Err(required());
            }
            let route = match self.registry.lookup(&root.to_string()) {
                Ok(route) => route,
                Err(error)
                    if error.details["reason"] == "legacy_proxy_launch"
                        && error.details["owner_state"] == "gone"
                        && tokio::time::Instant::now() < deadline =>
                {
                    // A fresh managed resume may be replacing a positively
                    // retired legacy locator. Never adopt its authority while
                    // waiting for the independently proved v2 publication.
                    tokio::time::sleep(Duration::from_millis(25)).await;
                    continue;
                }
                Err(error) => return Err(error),
            };
            let started = route
                .as_ref()
                .map(|r| {
                    self.launches.authorization(&r.binding().context).map(|v| {
                        v.is_some_and(|ready| {
                            ready.binding().identity.owner == r.binding().identity.owner
                        })
                    })
                })
                .transpose()?
                .unwrap_or(false);
            if route.as_ref().is_some_and(|r| {
                LaunchOwners::observe(&r.binding().identity)
                    == agentchat_core::model::LaunchOwnerState::Current
            }) && started
            {
                return Ok(route);
            }
            if tokio::time::Instant::now() >= deadline {
                if route.as_ref().is_some_and(|r| {
                    LaunchOwners::observe(&r.binding().identity)
                        == agentchat_core::model::LaunchOwnerState::Current
                }) && !started
                {
                    let mut error = Error::new(
                        ErrorCode::Busy,
                        "The exact managed Codex launch readiness is not available yet.",
                    );
                    error.retryable = true;
                    return Err(error);
                }
                return Ok(route);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}

#[async_trait]
impl CallVerifier for Verifier {
    async fn verify(&self, evidence: CallEvidence<'_>) -> Result<ConversationKey> {
        match self.verify_scope(evidence).await? {
            CallScope::Conversation(key) => Ok(key),
            CallScope::Bootstrap(_) => Err(required()),
        }
    }
    async fn verify_scope(&self, evidence: CallEvidence<'_>) -> Result<CallScope> {
        if !self.parent.is_continuing() {
            return Err(required());
        }
        // The pinned rmcp SDK moves wire _meta to RequestContext. Arguments,
        // working directory, session names and the spawn environment are never
        // native root evidence. The configured real shared-server parent pins
        // the source; its process is not the designated CLI owner.
        let root = evidence
            .metadata
            .and_then(|m| m.get("threadId"))
            .and_then(serde_json::Value::as_str)
            .and_then(|s| Uuid::parse_str(s).ok())
            .filter(|id| !id.is_nil())
            .ok_or_else(required)?;
        // The MCP routing layer already bounds verification to five seconds
        // and the transport to 32 outstanding calls. Serialize within that
        // existing budget instead of rejecting ordinary parallel reads.
        let mut state = self.state.lock().await;
        if !self.parent.is_continuing() {
            return Err(required());
        }
        // The selected 0.163 adapter pins one root per native MCP instance.
        // A mismatching retained call cannot replace or borrow its attachment.
        if state.root.is_some_and(|old| old != root) {
            return Err(required());
        }
        state.root = Some(root);
        let Some(route) = self.startup_route(root).await? else {
            if state.key.is_some() {
                return Err(required());
            }
            let mut client = Client::connect(&self.daemon.socket, &self.daemon.version).await?;
            if client.observe(&root.to_string()).await?.phase == Phase::Disconnected
                || !self.parent.is_continuing()
            {
                return Err(required());
            }
            let key = BootstrapKey {
                lifecycle_id: self.bootstrap,
                generation: 1,
            };
            if !state.bootstrapped {
                self.roots
                    .activate_bootstrap(
                        key,
                        InitializationIdentity::new(HostKind::Codex, root.to_string())?,
                    )
                    .await?;
                state.bootstrapped = true;
            }
            // Explicit-path initialization/reads only. No missing launch is
            // promoted into participation, a context or an automatic join.
            return Ok(CallScope::Bootstrap(key));
        };
        let (current, _) = self.adapter.observe_route(&route.binding().context).await?;
        if current != route || !self.parent.is_continuing() {
            return Err(required());
        }
        let key = ConversationKey {
            context_id: route.binding().context.context_id,
            generation: route.revision().checked_add(1).ok_or_else(required)?,
        };
        if state.key != Some(key) {
            state.restored = false;
        }
        let continuation = state.key.is_some()
            && state.owner.as_deref() == Some(route.binding().identity.owner.as_str());
        let conversation = if continuation {
            self.roots.activate_same_conversation(key).await?
        } else {
            self.roots.activate(key).await?
        };
        state.key = Some(key);
        state.owner = Some(route.binding().identity.owner.clone());
        if !state.restored {
            match conversation.restore().await {
                Ok(()) => state.restored = true,
                // Readiness releases native input without membership. Explicit
                // Join is the only operation that may activate this root.
                Err(error) if error.code == ErrorCode::NotJoined => {}
                Err(error) => return Err(error),
            }
        }
        if self.registry.lookup(&root.to_string())?.as_ref() != Some(&route)
            || !self.parent.is_continuing()
        {
            return Err(required());
        }
        Ok(CallScope::Conversation(key))
    }
}

/// Serve an explicitly selected native Codex MCP entry. Generic agentchat mcp remains
/// unbound. This grants no profile admission or startup membership and never
/// derives a frontend PID from this shared-server parent or native cache.
pub async fn serve(
    manager: ProjectManager,
    backend: Arc<dyn Backend>,
    executable: &Path,
    version: &str,
) -> Result<()> {
    let parent = ProcessIdentity::parent(executable)?;
    let daemon = Daemon::inspect(executable).await?;
    let native = executable.canonicalize().map_err(|_| required())?;
    if daemon.version != version
        || daemon.executable != native
        || parent.executable() != native
        || !parent.is_current()
    {
        return Err(required());
    }
    serve_observed(manager, backend, parent, daemon).await
}

/// Resolve the current user PATH entry for a new managed MCP connection. The
/// observed shared-server parent remains independent of frontend/root authority.
pub async fn serve_managed(manager: ProjectManager, backend: Arc<dyn Backend>) -> Result<()> {
    let cli = agentchat_hosts::setup::codex_cli(std::env::var_os("PATH"))?;
    let daemon = Daemon::inspect(&cli).await?;
    let parent = ProcessIdentity::parent(&daemon.executable)?;
    if !parent.is_current() {
        return Err(required());
    }
    serve_observed(manager, backend, parent, daemon).await
}

async fn serve_observed(
    manager: ProjectManager,
    backend: Arc<dyn Backend>,
    parent: ProcessIdentity,
    daemon: Daemon,
) -> Result<()> {
    let roots = Arc::new(Conversations::new(backend));
    let namespace = agentchat_hosts::codex::integration_namespace(manager.home());
    let verifier = Arc::new(Verifier {
        parent,
        daemon,
        registry: NativeRegistry::new(manager.home()),
        launches: Registry::new(manager.home()),
        adapter: NativeAdapter::new(PathBuf::from(manager.home())),
        roots: roots.clone(),
        bootstrap: Uuid::new_v4(),
        state: Mutex::default(),
    });
    let server = agentchat_mcp::Server::gateway(roots.clone(), verifier)
        .with_native_routing(HostKind::Codex, namespace);
    let result = agentchat_mcp::stdio(server).await.map_err(|_| required());
    roots.close().await;
    result
}
fn required() -> Error {
    Error::new(
        ErrorCode::SessionRequired,
        "The exact native Codex call or launch binding is unavailable or changed.",
    )
}

#[cfg(test)]
#[path = "../tests/unit/codex_gateway.rs"]
mod tests;
