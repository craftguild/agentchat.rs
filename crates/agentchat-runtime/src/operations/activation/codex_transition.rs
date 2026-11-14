//! Delivered native success becomes a separately fenced or activated target.

use super::{CodexRelease, Control, Native, Operations, PendingTransition, required};
use crate::context::{ContextRegistry, ContextSeed};
use agentchat_core::{
    Error, ErrorCode, Result, application::ManagedActivationCause, model::LaunchIdentity,
};
use agentchat_hosts::{
    codex::{Client, ProxyDecision, ProxyEvent, integration_namespace},
    launch::{Binding, Endpoint, NativeProof, Registry},
};
use agentchat_protocol::rpc::{Call, success};
use tokio::sync::mpsc::error::TryRecvError;
use uuid::Uuid;

impl Control {
    pub(super) async fn advance_codex_transition(
        &mut self,
        operations: &Operations,
        route: &super::super::Route,
        proof: &NativeProof,
    ) -> Result<bool> {
        if !matches!(self.native, Native::Codex { .. }) {
            return Ok(false);
        }
        let registry = Registry::new(operations.manager.home());
        self.drain_proxy_events(&registry)?;
        if self.codex_release.is_none() {
            return Ok(self.codex_candidate.is_some());
        }
        if self.transition.is_none()
            && let Err(error) = self.prepare_codex_target(operations, route, proof).await
        {
            // A terminal MCP configuration fault cannot terminate an already
            // running native frontend. Drop only this activation decision;
            // the observer retains its durable fence and marks it unknown.
            if !error.retryable {
                self.codex_release = None;
            }
            return Err(error);
        }
        if self.transition.is_none() {
            return Ok(true);
        }
        self.activate_transition(operations, route).await?;
        let released = self.codex_release.take().ok_or_else(|| required(None))?;
        released
            .release
            .send(ProxyDecision::Activated)
            .map_err(|_| unknown())?;
        Ok(true)
    }

    pub(super) fn drain_proxy_events(&mut self, registry: &Registry) -> Result<bool> {
        let mut rolled_back = false;
        while let Native::Codex { proxy } = &mut self.native {
            let event = match proxy.try_recv() {
                Ok(event) => event,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if let Some((connection, epoch, _)) = self.codex_candidate {
                        registry.record_managed_transition(&self.binding, connection, epoch)?;
                        self.codex_candidate = Some((connection, epoch, true));
                        self.attachment = None;
                        self.started = None;
                    }
                    break;
                }
            };
            match event {
                ProxyEvent::Candidate {
                    connection_epoch,
                    transition_epoch,
                    fence,
                } => {
                    // A repeated exact fence quarantines overlapping native
                    // candidates. It may fence a binding just activated by an
                    // earlier observation; discard its uncommitted follow-up.
                    self.codex_release = None;
                    self.transition = None;
                    self.pending = None;
                    registry.record_managed_transition(
                        &self.binding,
                        connection_epoch,
                        transition_epoch,
                    )?;
                    self.attachment = None;
                    self.codex_candidate = Some((connection_epoch, transition_epoch, false));
                    if fence.send(()).is_err() {
                        self.codex_candidate = Some((connection_epoch, transition_epoch, true));
                        return Err(unknown());
                    }
                }
                ProxyEvent::RolledBack {
                    connection_epoch,
                    transition_epoch,
                    release,
                } => {
                    if self.codex_candidate == Some((connection_epoch, transition_epoch, false)) {
                        registry.rollback_managed_transition(
                            &self.binding,
                            connection_epoch,
                            transition_epoch,
                        )?;
                        self.codex_candidate = None;
                        if release.send(()).is_err() {
                            registry.record_managed_transition(
                                &self.binding,
                                connection_epoch,
                                transition_epoch,
                            )?;
                            self.codex_candidate = Some((connection_epoch, transition_epoch, true));
                            self.attachment = None;
                            self.started = None;
                            return Err(unknown());
                        }
                        rolled_back = true;
                    }
                }
                ProxyEvent::Unknown {
                    connection_epoch,
                    transition_epoch,
                } => {
                    if self.codex_candidate.is_some_and(|value| {
                        value.0 == connection_epoch && value.1 == transition_epoch
                    }) {
                        self.codex_release = None;
                        self.transition = None;
                        self.pending = None;
                        // Before success this is an idempotent old-root fence.
                        // After activation it fences the target whose application
                        // acknowledgement became uncertain. Native traffic runs.
                        registry.record_managed_transition(
                            &self.binding,
                            connection_epoch,
                            transition_epoch,
                        )?;
                        self.codex_candidate = Some((connection_epoch, transition_epoch, true));
                        self.attachment = None;
                        self.started = None;
                    }
                }
                ProxyEvent::Delivered {
                    connection_epoch,
                    transition_epoch,
                    release,
                } => {
                    if self.codex_candidate == Some((connection_epoch, transition_epoch, false))
                        && self.codex_release.is_none()
                        && self.transition.is_none()
                    {
                        let activated = self.started.is_some();
                        if activated {
                            self.mark_delivery_started(registry)?;
                        }
                        if release.send(()).is_err() {
                            if activated {
                                registry.record_managed_transition(
                                    &self.binding,
                                    connection_epoch,
                                    transition_epoch,
                                )?;
                            }
                            self.codex_candidate = Some((connection_epoch, transition_epoch, true));
                            self.attachment = None;
                            self.started = None;
                            return Err(unknown());
                        }
                        self.codex_candidate = None;
                    }
                }
                ProxyEvent::Succeeded {
                    connection_epoch,
                    transition_epoch,
                    root,
                    release,
                } => {
                    if self.codex_candidate != Some((connection_epoch, transition_epoch, false)) {
                        return Err(required(Some(&self.binding.context)));
                    }
                    self.codex_release = Some(CodexRelease {
                        connection_epoch,
                        transition_epoch,
                        root,
                        release,
                    });
                }
            }
        }
        Ok(rolled_back)
    }

    async fn prepare_codex_target(
        &mut self,
        operations: &Operations,
        route: &super::super::Route,
        proof: &NativeProof,
    ) -> Result<()> {
        let outcome = self.codex_release.as_ref().ok_or_else(|| required(None))?;
        if self.codex_candidate != Some((outcome.connection_epoch, outcome.transition_epoch, false))
        {
            return Err(required(Some(&self.binding.context)));
        }
        let root = Uuid::parse_str(&outcome.root)
            .ok()
            .filter(|root| !root.is_nil() && root.to_string() == outcome.root)
            .ok_or_else(|| required(Some(&self.binding.context)))?;
        let registry = Registry::new(operations.manager.home());
        registry.record_managed_root_change(
            &self.binding,
            root,
            outcome.connection_epoch,
            outcome.transition_epoch,
        )?;
        let Endpoint::Codex { socket, .. } = &self.binding.endpoint else {
            return Err(required(Some(&self.binding.context)));
        };
        let mut client = Client::connect(socket, &self.binding.context.host_version).await?;
        let observed = client.observe_target(&outcome.root).await?;
        let directory = observed
            .directory()
            .canonicalize()
            .map_err(|_| required(Some(&self.binding.context)))?;
        if directory != observed.directory() {
            return Err(required(Some(&self.binding.context)));
        }
        let discovered = operations.manager.discover(&directory).await?;
        if discovered
            .as_ref()
            .is_none_or(|project| project.identity().id != route.project.identity().id)
        {
            let mut runtime = operations.connect(route).await?;
            success(
                runtime
                    .call(
                        Uuid::new_v4(),
                        Call::ReconcileLaunch {
                            context_id: self.binding.context.context_id,
                            generation: self.binding.identity.generation,
                        },
                    )
                    .await?,
            )?;
            let released = self.codex_release.take().ok_or_else(|| required(None))?;
            released
                .release
                .send(ProxyDecision::RetiredOnly)
                .map_err(|_| unknown())?;
            self.attachment = None;
            self.started = None;
            return Ok(());
        }
        if !client
            .integration_ready_for(
                &outcome.root,
                &integration_namespace(operations.manager.home()),
            )
            .await?
        {
            return Err(waiting());
        }
        let context = ContextRegistry::new(operations.manager.clone())
            .prepare(ContextSeed {
                host: self.binding.context.host,
                host_session_id: outcome.root.clone(),
                host_version: observed.version().into(),
                os_profile: self.binding.context.os_profile.clone(),
                working_directory: directory,
            })
            .await?;
        let binding = Binding::new(
            context,
            LaunchIdentity {
                generation: Uuid::new_v4(),
                owner: proof.identity().owner.clone(),
            },
            self.binding.endpoint.clone(),
        )?;
        self.publish(operations, &binding).await?;
        let authorization = registry.publish_ready(&binding, self.socket.path().into(), None)?;
        self.transition = Some(PendingTransition {
            request_id: Uuid::new_v4(),
            binding,
            authorization,
            observed: None,
            cause: ManagedActivationCause::DistinctRoot {
                predecessor_context: self.binding.context.context_id,
                predecessor_generation: self.binding.identity.generation,
                transition_epoch: outcome.transition_epoch,
            },
        });
        Ok(())
    }
}

fn waiting() -> Error {
    let mut error = Error::new(
        ErrorCode::Busy,
        "The switched Codex root is waiting for exact AgentChat MCP readiness.",
    );
    error.retryable = true;
    error
}

fn unknown() -> Error {
    let mut error = Error::new(
        ErrorCode::RuntimeUnavailable,
        "The native Codex transition response release is unknown.",
    );
    error.retryable = true;
    error
}

#[cfg(test)]
#[path = "../../../tests/unit/codex_transition.rs"]
mod tests;
