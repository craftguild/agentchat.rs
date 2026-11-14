//! Version-coupled native I/O entrypoints, separate from finite CLI envelopes.
mod control;
mod guard;
mod input;
mod startup;
use crate::args::Internal;
use agentchat_core::{Error, ErrorCode, Result};
use agentchat_hosts::claude::{
    namespace::OwnerNamespace, observer::relay, process::ProcessIdentity,
};

/// Dispatch only after resolving this original managed launch; never rediscover
/// an engine for a continuing helper or infer a native root from an ancestor.
async fn owner(
    home: &std::path::Path,
    executable: &std::path::Path,
    direct: bool,
) -> Result<ProcessIdentity> {
    if executable == std::path::Path::new(agentchat_hosts::launch::MANAGED_CLAUDE) {
        Ok(agentchat_hosts::launch::managed_claude(home, direct)
            .await?
            .0)
    } else if direct {
        ProcessIdentity::parent(executable)
    } else {
        ProcessIdentity::ancestor(executable)
    }
}

pub(crate) async fn run(command: &Internal) -> i32 {
    if let Internal::ClaudeInput {
        native_executable,
        state_directory,
        mode,
    } = command
    {
        return input::run(native_executable, state_directory, *mode).await;
    }
    if let Internal::ClaudeGuard {
        native_executable,
        state_directory,
        mode,
    } = command
    {
        return guard::run(native_executable, state_directory, *mode).await;
    }
    match serve(command).await {
        Ok(()) => 0,
        Err(error) => crate::mcp::failure(error).await,
    }
}
async fn serve(command: &Internal) -> Result<()> {
    match command {
        Internal::ClaudeStartup { native_executable } => {
            output(&startup::run(native_executable).await?).await
        }
        Internal::ClaudeNamespace {
            native_executable,
            native_version,
        } => {
            let manager = agentchat_runtime::project::ProjectManager::from_environment()?;
            let namespace = crate::claude_gateway::configured::namespace(
                &manager,
                native_executable,
                native_version,
            )
            .await?;
            output(&namespace).await
        }
        Internal::ClaudeControl { native_executable } => {
            output(&control::run(native_executable).await?).await
        }
        Internal::ClaudeStream {
            native_executable,
            state_directory,
            restart_backoff,
        } => {
            let manager = match state_directory {
                Some(path) => agentchat_runtime::project::ProjectManager::new(path.clone())?,
                None => agentchat_runtime::project::ProjectManager::from_environment()?,
            };
            let owner = owner(manager.home(), native_executable, true).await?;
            // The native SDK supplies argv-only spawn. Keep retry pacing in
            // Rust rather than assuming a JavaScript timer or spawn-env API.
            if *restart_backoff {
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                if !owner.is_continuing() {
                    return Err(Error::new(
                        ErrorCode::SessionRequired,
                        "The native owner ended.",
                    ));
                }
            }
            let ns = OwnerNamespace::open(manager.home(), owner.clone()).await?;
            relay_run(ns.directory(), ns.observer(), owner, Some(ns.clone())).await
        }
        Internal::ClaudeRelay {
            directory,
            observer,
            native_executable,
        } => {
            let manager = agentchat_runtime::project::ProjectManager::from_environment()?;
            let owner = owner(manager.home(), native_executable, true).await?;
            relay_run(directory, observer, owner, None).await
        }
        Internal::ClaudeAnswer { socket } => {
            let answer = relay::read_answer().await?;
            relay::respond(socket, &answer).await
        }
        Internal::ClaudeGuard { .. } | Internal::ClaudeInput { .. } => {
            unreachable!("native guard has hook-specific exit semantics")
        }
        Internal::Daemon { .. } => {
            unreachable!("project daemon uses the shared runtime entrypoint")
        }
    }
}

async fn output(value: &impl serde::Serialize) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    let mut output = relay::native_output()?;
    let mut bytes = serde_json::to_vec(value)
        .map_err(|_| Error::new(ErrorCode::IoError, "Native callback output is unavailable."))?;
    if bytes.len() > 65_536 {
        return Err(Error::new(
            ErrorCode::IoError,
            "Native callback output exceeds its bound.",
        ));
    }
    bytes.push(b'\n');
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        output.write_all(&bytes).await?;
        output.flush().await
    })
    .await
    .map_err(|_| Error::new(ErrorCode::IoError, "Native callback output timed out."))?
    .map_err(|_| Error::new(ErrorCode::IoError, "Native callback output ended."))
}

async fn relay_run(
    directory: &std::path::Path,
    observer: &str,
    owner: ProcessIdentity,
    managed: Option<OwnerNamespace>,
) -> Result<()> {
    let mut termination = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|_| {
            Error::new(
                ErrorCode::IoError,
                "Native transport termination signal is unavailable.",
            )
        })?;
    let output = relay::native_output()?;
    let serving = async {
        match managed {
            Some(namespace) => relay::serve_managed(namespace, output).await,
            None => relay::serve(directory, observer, owner, output).await,
        }
    };
    tokio::select! {result=serving=>result,_=termination.recv()=>Ok(()),_=tokio::signal::ctrl_c()=>Ok(())}
}
