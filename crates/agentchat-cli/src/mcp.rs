//! Stdio composition bypasses finite CLI output and implicit cwd routing.

use crate::{process, terminal};
use agentchat_core::{Error, ErrorCode, Result};
use agentchat_runtime::project::ProjectManager;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;

pub(crate) async fn run() -> i32 {
    let result = serve().await;
    match result {
        Ok(()) => 0,
        Err(error) => failure(error).await,
    }
}
pub(crate) async fn failure(error: Error) -> i32 {
    // A CLI result envelope on stdout would corrupt the SDK stream.
    let message = terminal::escape(&error.message) + "\n";
    let mut stderr = tokio::io::stderr();
    let _ = stderr.write_all(message.as_bytes()).await;
    let _ = stderr.flush().await;
    error.code.exit_code()
}
async fn serve() -> Result<()> {
    let manager = ProjectManager::from_environment()?;
    let executable = process::current_executable()?;
    let codex = std::env::var_os("AGENTCHAT_CODEX_NATIVE_EXE");
    let codex_version = std::env::var("AGENTCHAT_CODEX_NATIVE_VERSION").ok();
    if let Some(mode) = std::env::var_os("AGENTCHAT_CODEX_MANAGED") {
        if mode != "1"
            || codex.is_some()
            || codex_version.is_some()
            || std::env::var_os("AGENTCHAT_CLAUDE_MANAGED").is_some()
            || std::env::var_os("AGENTCHAT_CLAUDE_NATIVE_EXE").is_some()
            || std::env::var_os("AGENTCHAT_CLAUDE_NATIVE_VERSION").is_some()
        {
            return Err(Error::new(
                ErrorCode::ConfigurationConflict,
                "Native MCP requires one owned host dispatch mode.",
            ));
        }
        let backend = Arc::new(process::operations(manager.clone(), executable));
        return crate::codex_gateway::serve_managed(manager, backend).await;
    }
    if codex.is_some() || codex_version.is_some() {
        if std::env::var_os("AGENTCHAT_CLAUDE_NATIVE_EXE").is_some()
            || std::env::var_os("AGENTCHAT_CLAUDE_MANAGED").is_some()
            || std::env::var_os("AGENTCHAT_CLAUDE_NATIVE_VERSION").is_some()
        {
            return Err(Error::new(
                ErrorCode::SessionRequired,
                "Native MCP requires one exact host selection without a borrowed context.",
            ));
        }
        let native = codex.ok_or_else(|| {
            Error::new(
                ErrorCode::SessionRequired,
                "The native Codex executable selection is missing.",
            )
        })?;
        let version = codex_version.ok_or_else(|| {
            Error::new(
                ErrorCode::SessionRequired,
                "The native Codex version selection is missing.",
            )
        })?;
        let backend = Arc::new(process::operations(manager.clone(), executable));
        return crate::codex_gateway::serve(
            manager,
            backend,
            std::path::Path::new(&native),
            &version,
        )
        .await;
    }
    let managed = std::env::var_os("AGENTCHAT_CLAUDE_MANAGED");
    let native = std::env::var_os("AGENTCHAT_CLAUDE_NATIVE_EXE");
    if managed.is_some()
        && (managed.as_deref() != Some(std::ffi::OsStr::new("1"))
            || native.is_some()
            || std::env::var_os("AGENTCHAT_CLAUDE_NATIVE_VERSION").is_some())
    {
        return Err(Error::new(
            ErrorCode::ConfigurationConflict,
            "Native Claude MCP requires one owned dispatch mode.",
        ));
    }
    if managed.is_some() && agentchat_hosts::launch::StartupScope::from_environment()?.is_none() {
        // Ordinary Claude still exposes initialization/unbound tools. Absence
        // of a managed launch is not permission to invent a native owner/root;
        // malformed or missing snapshots inside an actual launch never fall
        // through to this bootstrap-only composition.
        let backend = Arc::new(process::operations(manager, executable));
        return agentchat_mcp::stdio(agentchat_mcp::Server::new(backend)).await;
    }
    if let Some(native) =
        native.or_else(|| managed.map(|_| agentchat_hosts::launch::MANAGED_CLAUDE.into()))
    {
        let config = crate::claude_gateway::configured::configuration(
            &manager,
            std::path::Path::new(&native),
        )
        .await?;
        let backend = Arc::new(process::operations(manager.clone(), executable));
        return crate::claude_gateway::serve(config, backend, manager, None)
            .await
            .map_err(|e| {
                e.downcast_ref::<Error>().cloned().unwrap_or_else(|| {
                    Error::new(
                        ErrorCode::SessionRequired,
                        "The exact native MCP gateway is unavailable.",
                    )
                })
            });
    }
    let backend = Arc::new(process::operations(manager, executable));
    let server = agentchat_mcp::Server::new(backend);
    agentchat_mcp::stdio(server).await
}
