//! Exact native scope and designated child precede shared startup.
use super::required;
use crate::{
    context::{ContextRegistry, ContextSeed},
    project::{Project, ProjectManager},
};
use agentchat_core::{
    Result,
    model::{HostKind, LaunchOwnerState},
};
use agentchat_hosts::{
    codex::{CodexReady, Daemon},
    launch::{Binding, Endpoint, NativeChild, NativeProof, NativeTerminal},
};
use std::{collections::BTreeMap, ffi::OsString, path::Path};
use uuid::Uuid;

/// One retained startup attempt. Retry this same prepared descriptor after an
/// ambiguous RPC; allocating another context is not a retry of physical readiness.
pub struct PreparedCodexLaunch {
    binding: Binding,
    ready: CodexReady,
}
impl PreparedCodexLaunch {
    /// Exact original native association; no ongoing authority is cached here.
    pub fn binding(&self) -> &Binding {
        &self.binding
    }
    /// Native startup facts paired with this actual child and exact context.
    pub fn ready(&self) -> &CodexReady {
        &self.ready
    }
}

/// Spawn only the real matching native CLI with its exact selected UUID and the
/// actual shared daemon through the owned transparent metadata proxy. No prompt,
/// `-c`, embedded server or policy override is supplied; membership follows separately.
#[allow(clippy::too_many_arguments)]
pub async fn spawn_codex(
    manager: &ProjectManager,
    project: &Project,
    directory: &Path,
    daemon: &Daemon,
    root: &str,
    proxy: &Path,
    environment: &BTreeMap<OsString, OsString>,
    terminal: NativeTerminal,
) -> Result<NativeChild> {
    manager.validate(project).await?;
    let id = Uuid::parse_str(root).map_err(|_| required())?;
    let directory = directory.canonicalize().map_err(|_| required())?;
    if id.is_nil()
        || root != id.to_string()
        || !daemon.socket.is_absolute()
        || !proxy.is_absolute()
        || manager
            .discover(&directory)
            .await?
            .is_none_or(|p| p.identity().id != project.identity().id)
    {
        return Err(required());
    }
    let mut remote = OsString::from("unix://");
    remote.push(proxy.as_os_str());
    let arguments = [
        OsString::from("--cd"),
        directory.as_os_str().into(),
        OsString::from("resume"),
        root.into(),
        OsString::from("--remote"),
        remote,
    ];
    NativeChild::spawn_with_terminal(
        &daemon.executable,
        &directory,
        &arguments,
        environment,
        terminal,
    )
    .await
}

/// Prepare once from the selected exact root, actual child image/birth and
/// scoped native MCP health. Retain the descriptor before publication; shared
/// startup retries this same immutable intent without intercepting CLI traffic.
pub async fn prepare_codex_start(
    manager: &ProjectManager,
    project: &Project,
    child: &NativeProof,
    daemon: &Daemon,
    root: &str,
    directory: &Path,
) -> Result<PreparedCodexLaunch> {
    manager.validate(project).await?;
    if child.state() != LaunchOwnerState::Current
        || child.owner().executable() != daemon.executable
        || !directory.is_absolute()
    {
        return Err(required());
    }
    let mut native =
        agentchat_hosts::codex::Client::connect(&daemon.socket, &daemon.version).await?;
    let ready = native
        .ready(
            root,
            directory,
            &agentchat_hosts::codex::integration_namespace(manager.home()),
        )
        .await?;
    if child.state() != LaunchOwnerState::Current {
        return Err(required());
    }
    let context = ContextRegistry::new(manager.clone())
        .prepare(ContextSeed {
            host: HostKind::Codex,
            host_session_id: ready.root().into(),
            host_version: ready.version().into(),
            os_profile: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            working_directory: ready.directory().into(),
        })
        .await?;
    if context.project_id != project.identity().id {
        return Err(required());
    }
    let binding = Binding::new(
        context,
        child.identity().clone(),
        Endpoint::Codex {
            socket: daemon.socket.clone(),
            executable: daemon.executable.clone(),
        },
    )?;
    Ok(PreparedCodexLaunch { binding, ready })
}
