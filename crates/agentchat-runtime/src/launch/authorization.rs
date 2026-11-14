//! Native startup publishes physical readiness without a membership operation.
use super::{PreparedCodexLaunch, required};
use crate::{
    context::ContextRegistry,
    project::{Project, ProjectManager},
};
use agentchat_core::{Result, model::LaunchOwnerState};
use agentchat_hosts::{
    codex::{NativeAdapter, NativeRegistry},
    launch::{
        ActivationSocket, Binding, ClaudeReady, Endpoint, NativeProof, ReadyAuthorization, Registry,
    },
};

/// Publish the original Claude scope and release native input while unjoined.
/// The returned owned endpoint must live with this original supervisor. No Core
/// writer, participant, boundary, heartbeat or synthetic Join is invoked here.
pub async fn authorize_claude_start(
    manager: &ProjectManager,
    project: &Project,
    child: &NativeProof,
    ready: &ClaudeReady,
    name: Option<String>,
) -> Result<(ActivationSocket, ReadyAuthorization)> {
    manager.validate(project).await?;
    if ready.context().project_id != project.identity().id
        || child.state() != LaunchOwnerState::Current
        || child
            .startup_root()
            .is_none_or(|root| root.to_string() != ready.context().host_session_id)
        || !agentchat_hosts::claude::module::has_startup_gate_source(&ready.context().host_version)
    {
        return Err(required());
    }
    let binding = Binding::new(
        ready.context().clone(),
        child.identity().clone(),
        Endpoint::Claude,
    )?;
    let socket = ActivationSocket::bind(manager.home(), &binding)?;
    let registry = Registry::new(manager.home());
    registry.publish(&binding)?;
    let authorization = registry.publish_ready(&binding, socket.path().into(), name)?;
    Ok((socket, authorization))
}
/// Publish the exact prepared direct Codex association, independently of Join.
/// The real native CLI continues talking directly to its ordinary shared daemon.
pub async fn authorize_codex_start(
    manager: &ProjectManager,
    project: &Project,
    child: &NativeProof,
    prepared: &PreparedCodexLaunch,
    name: Option<String>,
) -> Result<(ActivationSocket, ReadyAuthorization)> {
    manager.validate(project).await?;
    let binding = prepared.binding();
    if binding.context.project_id != project.identity().id
        || child.identity() != &binding.identity
        || child.state() != LaunchOwnerState::Current
    {
        return Err(required());
    }
    let socket = ActivationSocket::bind(manager.home(), binding)?;
    ContextRegistry::new(manager.clone())
        .publish(binding.context.clone())
        .await?;
    NativeRegistry::new(manager.home()).publish(binding)?;
    NativeAdapter::new(manager.home().into())
        .observe(&binding.context)
        .await?;
    let authorization =
        Registry::new(manager.home()).publish_ready(binding, socket.path().into(), name)?;
    Ok((socket, authorization))
}
