//! Original-supervisor activation serializes publication intent and Core Join.
mod binding;
mod fresh;
mod initial;
pub(in crate::operations) use fresh::Native;
mod codex_transition;
mod wire;
use super::{Operations, Route};
use crate::launch::StartedLaunch;
use agentchat_core::{
    Error, ErrorCode, Result,
    application::{ManagedActivationCause, ManagedActivationResult, Mutation},
    model::{Context, HostKind, LaunchOwnerState},
};
use agentchat_hosts::launch::{
    ActivationSocket, Binding, ClaudeReady, NativeProof, ReadyAuthorization, Registry,
};
use agentchat_protocol::{
    operations::Attachment,
    rpc::{Call, success},
};
use serde_json::Value;
use tokio::net::UnixStream;
use uuid::Uuid;

/// Retained only by the original live supervisor. Physical readiness remains
/// fixed across explicit Leave; each new activation retains one publication
/// descriptor until its result is known, including caller cancellation/loss.
pub(in crate::operations) struct Control {
    pub socket: ActivationSocket,
    ready: ReadyAuthorization,
    pub binding: Binding,
    pending: Option<Binding>,
    pending_request: Option<(Uuid, Mutation)>,
    attachment: Option<Box<dyn Attachment>>,
    pub started: Option<StartedLaunch>,
    pub native: Native,
    initial_request: Uuid,
    initial_waiting: bool,
    // A known receipt ends mutation replay. Attachment recovery must not offer
    // the same key with a replacement descriptor or allocate another Join.
    initial_committed: Option<ManagedActivationResult>,
    transition: Option<PendingTransition>,
    codex_candidate: Option<(u64, u64, bool)>,
    codex_release: Option<CodexRelease>,
}
#[derive(Clone)]
struct PendingTransition {
    request_id: Uuid,
    binding: Binding,
    authorization: ReadyAuthorization,
    observed: Option<ClaudeReady>,
    cause: ManagedActivationCause,
}
struct CodexRelease {
    connection_epoch: u64,
    transition_epoch: u64,
    root: String,
    release: tokio::sync::oneshot::Sender<agentchat_hosts::codex::ProxyDecision>,
}
impl Control {
    pub fn authorization(&self) -> &ReadyAuthorization {
        &self.ready
    }
    pub fn new(socket: ActivationSocket, ready: ReadyAuthorization, native: Native) -> Self {
        Self {
            binding: ready.binding().clone(),
            socket,
            ready,
            pending: None,
            pending_request: None,
            attachment: None,
            started: None,
            native,
            initial_request: Uuid::new_v4(),
            initial_waiting: false,
            initial_committed: None,
            transition: None,
            codex_candidate: None,
            codex_release: None,
        }
    }
    pub async fn accept(&self) -> Result<UnixStream> {
        self.socket.accept().await
    }
    /// Once accepted, the supervisor owns this operation, independently of the
    /// requesting MCP socket. A lost reply cannot create another generation.
    pub async fn serve(
        &mut self,
        operations: &Operations,
        route: &Route,
        proof: &NativeProof,
        mut stream: UnixStream,
    ) {
        let mut request = match wire::receive(&mut stream).await {
            Ok(value) => value,
            Err(_) => return,
        };
        if matches!(request.command, Mutation::Join { .. }) {
            match request.command.clone().normalize() {
                Ok(normalized) => request.command = normalized,
                Err(error) => {
                    wire::respond(&mut stream, request.id, Err(error)).await;
                    return;
                }
            }
        }
        let result = self.activate(operations, route, proof, &request).await;
        wire::respond(&mut stream, request.id, result).await;
    }
    async fn activate(
        &mut self,
        operations: &Operations,
        route: &Route,
        proof: &NativeProof,
        request: &wire::Request,
    ) -> Result<Value> {
        let registry = Registry::new(operations.manager.home());
        let context = crate::context::ContextRegistry::new(operations.manager.clone())
            .resolve(request.context, None)
            .await?;
        if request.version != 1
            || request.id.is_nil()
            || !matches!(request.command, Mutation::Join { .. })
            || registry.authorization(&context)?.as_ref() != Some(&self.ready)
            || proof.identity().owner != self.ready.binding().identity.owner
            || proof.state() != LaunchOwnerState::Current
            || !wire::same_scope(&context, &self.binding.context)
            || registry.root_changed(
                context.project_id,
                context.host,
                &context.host_session_id,
                &self.binding.identity,
            )
        {
            return Err(required(Some(&context)));
        }
        if self.initial_waiting {
            let mut error = Error::new(
                ErrorCode::Busy,
                "The original initial managed activation is still unresolved.",
            );
            error.retryable = true;
            return Err(error);
        }
        if self.pending_request.is_none() && self.started.is_some() {
            // Repair only an already-active native attachment before reporting
            // a duplicate Join. Native/MCP recovery creates no participation or
            // boundary; an unjoined/left launch still reaches explicit Core Join.
            self.tick(operations, route, proof).await?;
        }
        if self
            .pending_request
            .as_ref()
            .is_some_and(|(id, command)| *id != request.id || command != &request.command)
        {
            let mut error = Error::new(
                ErrorCode::Busy,
                "Retry the unresolved Join with its original request_id and arguments.",
            );
            error.retryable = true;
            return Err(error);
        }
        // Always offer the original request to Core first. Its durable receipt
        // wins before host checks; an old successful Join replay after Leave
        // cannot allocate/reopen a membership or replace a pending publication.
        let first = self
            .commit(
                operations,
                route,
                request.id,
                request.command.clone(),
                &self.binding,
            )
            .await;
        let result = match first {
            Ok(value) => value,
            Err(error) => {
                let mut read = operations.connect(route).await?;
                let state = read.native_binding(self.binding.context.context_id).await?;
                if error.code != ErrorCode::SessionRequired
                    || !matches!(
                        state.participation,
                        agentchat_core::read::Participation::Left
                            | agentchat_core::read::Participation::Unjoined
                    )
                {
                    return Err(error);
                }
                if self.pending.is_none() {
                    self.pending = Some(self.fresh(operations, proof, true).await?);
                    self.pending_request = Some((request.id, request.command.clone()));
                }
                let pending = self.pending.as_ref().expect("retained publication intent");
                self.publish(operations, pending).await?;
                let committed = self
                    .commit(
                        operations,
                        route,
                        request.id,
                        request.command.clone(),
                        pending,
                    )
                    .await;
                if let Err(error) = &committed
                    && !error.retryable
                    && !matches!(
                        error.code,
                        ErrorCode::IoError
                            | ErrorCode::StorageError
                            | ErrorCode::RuntimeUnavailable
                    )
                {
                    // A known rejection committed nothing. The published native
                    // scope is harmless; a corrected explicit request may prepare
                    // another intent. Ambiguous writes retain the exact original.
                    self.pending = None;
                    self.pending_request = None;
                }
                committed?
            }
        };
        let activated: ManagedActivationResult =
            serde_json::from_value(result.clone()).map_err(|_| required(Some(&context)))?;
        self.finish_activation(operations, route, &activated)
            .await?;
        self.mark_delivery_if_started(&registry)?;
        Ok(result)
    }
    async fn activate_transition(&mut self, operations: &Operations, route: &Route) -> Result<()> {
        let pending = self
            .transition
            .clone()
            .ok_or_else(|| required(Some(&self.binding.context)))?;
        self.pending = Some(pending.binding.clone());
        let mut client = operations.connect(route).await?;
        let value = success(
            client
                .call(
                    pending.request_id,
                    Call::ActivateManaged {
                        request_id: pending.request_id,
                        context_id: pending.binding.context.context_id,
                        cause: pending.cause.clone(),
                        name: None,
                    },
                )
                .await?,
        )?;
        let activated: ManagedActivationResult =
            serde_json::from_value(value).map_err(|_| required(Some(&pending.binding.context)))?;
        self.ready = pending.authorization.clone();
        if let (Native::Claude { ready, .. }, Some(observed)) =
            (&mut self.native, pending.observed.clone())
        {
            **ready = observed;
        }
        self.finish_activation(operations, route, &activated)
            .await?;
        if self.started.as_ref().is_none_or(|started| {
            started.identity.generation != activated.generation
                || started.context.context_id != activated.context_id
        }) {
            return Err(required(Some(&pending.binding.context)));
        }
        if self.codex_release.is_none() {
            self.mark_delivery_started(&Registry::new(operations.manager.home()))?;
        }
        self.transition = None;
        Ok(())
    }
    async fn commit(
        &self,
        operations: &Operations,
        route: &Route,
        id: Uuid,
        command: Mutation,
        binding: &Binding,
    ) -> Result<Value> {
        let mut client = operations.connect(route).await?;
        success(
            client
                .call(
                    id,
                    Call::Mutate {
                        request_id: id,
                        actor: agentchat_protocol::rpc::ActorReference::Native {
                            context_id: binding.context.context_id,
                        },
                        command,
                    },
                )
                .await?,
        )
    }
    pub async fn reconcile(&mut self, operations: &Operations, route: &Route) {
        if let Some(started) = &self.started
            && let Ok(mut client) = operations.connect(route).await
        {
            let _ = client
                .call(
                    Uuid::new_v4(),
                    Call::ReconcileLaunch {
                        context_id: started.context.context_id,
                        generation: started.identity.generation,
                    },
                )
                .await;
        }
    }
}
fn required(context: Option<&Context>) -> Error {
    let command = match context.map(|c| c.host) {
        Some(HostKind::Claude) => "agentchat launch claude",
        _ => "agentchat launch codex",
    };
    let mut error = Error::new(
        ErrorCode::SessionRequired,
        format!(
            "A current verified managed launch is required; start this conversation with {command}."
        ),
    );
    error.details =
        serde_json::json!({"reason":"managed_launch_required","required_command":command});
    error
}

pub(in crate::operations) fn signal_error() -> Error {
    Error::new(
        ErrorCode::IoError,
        "Native supervisor signal handling is unavailable.",
    )
}
