//! Executable composition: lazy owned runtimes without host-policy overrides.

use agentchat_core::{Error, ErrorCode, Result, time::SystemClock};
use agentchat_runtime::{
    daemon::{RuntimeOptions, Server},
    operations::{Launcher, Operations},
    project::{Project, ProjectManager},
};
use async_trait::async_trait;
use std::{
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
};
use tokio::process::Command;
use uuid::Uuid;

pub(crate) struct Executable(pub PathBuf);
pub(crate) fn current_executable() -> Result<PathBuf> {
    // macOS can retain ../ components from the invocation in current_exe().
    // Resolve the actual image before sharing its path with the installer,
    // native helpers and lazy daemon; installer validation remains strict.
    std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|_| Error::new(ErrorCode::IoError, "Current executable is unavailable."))
}
/// Compose the public operations with the absolute installed agentchat executable.
/// Construction starts/installs nothing; the supplied manager and captured
/// environment choose the state/config roots used by later explicit operations.
pub fn operations(manager: ProjectManager, executable: PathBuf) -> Operations {
    let mut setup = agentchat_hosts::setup::ToolSetup::new(
        manager.home().to_owned(),
        executable.clone(),
        agentchat_hosts::setup::Environment::capture(),
        std::env::var_os("PATH"),
    );
    // Real user-wide initialization may select a discovered running native
    // profile. Explicit isolated roots stay tool-only unless an acceptance
    // harness opts into native discovery itself.
    if std::env::var_os("AGENTCHAT_USER_CONFIG_ROOT").is_none() {
        setup = setup.with_native_discovery();
    }
    Operations::new(manager, Arc::new(Executable(executable))).with_host_tools(setup)
}
#[async_trait]
impl Launcher for Executable {
    async fn launch(&self, project: &Project, directory: &Path) -> Result<()> {
        let mut command = Command::new(&self.0);
        command
            .arg("--project")
            .arg(directory)
            .args(["internal", "daemon", "--project-id"])
            .arg(project.identity().id.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // A detached group avoids inheriting the foreground command's Ctrl-C.
        // No host sandbox/trust/approval options are changed.
        command.as_std_mut().process_group(0);
        let mut child = command.spawn().map_err(|_| {
            Error::new(
                ErrorCode::RuntimeUnavailable,
                "Could not start the project runtime executable.",
            )
        })?;
        tokio::spawn(async move {
            let _ = child.wait().await;
        });
        Ok(())
    }
}
pub(crate) async fn daemon(manager: ProjectManager, path: &Path, expected: Uuid) -> Result<()> {
    let project = manager.discover(path).await?.ok_or_else(|| {
        Error::new(
            ErrorCode::NotInitialized,
            "Daemon startup requires an initialized project.",
        )
    })?;
    if project.identity().id != expected {
        return Err(Error::new(
            ErrorCode::ProjectBindingMismatch,
            "Daemon startup reference changed from the expected UUID.",
        ));
    }
    let hosts = Arc::new(agentchat_hosts::AdmittedHosts::new(
        manager.home().to_owned(),
    ));
    let server = Server::bind(
        manager,
        project,
        hosts.clone(),
        Arc::new(SystemClock),
        RuntimeOptions::default(),
    )
    .await?
    .with_native_messages(hosts);
    let handle = server.handle();
    let serving = server.run();
    tokio::pin!(serving);
    let mut termination = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|_| {
            Error::new(
                ErrorCode::IoError,
                "Runtime termination signal could not be registered.",
            )
        })?;
    tokio::select! {
        result=&mut serving=>result,
        _=tokio::signal::ctrl_c()=>{handle.shutdown();serving.await},
        _=termination.recv()=>{handle.shutdown();serving.await},
    }
}
