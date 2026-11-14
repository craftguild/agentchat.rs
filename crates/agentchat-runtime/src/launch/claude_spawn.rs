//! Normal configured native startup, shared by product and acceptance callers.
use crate::project::{Project, ProjectManager};
use agentchat_core::{Error, ErrorCode, Result};
use agentchat_hosts::{
    launch::{NativeChild, NativeTerminal},
    setup::ClaudeLaunchPlan,
};
use std::{ffi::OsString, path::Path};
use uuid::Uuid;

/// Spawn the selected original Claude child with normal configuration discovery
/// and the pre-exec first-input gate. A plan and successful spawn do not admit a
/// profile or establish readiness/membership: the caller must independently
/// challenge this child and complete shared startup before releasing its input.
/// No settings/MCP, model, approval, trust or sandbox override is supplied.
pub async fn spawn_claude(
    manager: &ProjectManager,
    project: &Project,
    directory: &Path,
    plan: &ClaudeLaunchPlan,
    root: Uuid,
    resume: bool,
    terminal: NativeTerminal,
) -> Result<NativeChild> {
    manager.validate(project).await?;
    if root.is_nil() {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            "Native startup requires a non-nil selected root.",
        ));
    }
    let directory = directory.canonicalize().map_err(|_| {
        Error::new(
            ErrorCode::IoError,
            "Native launch directory is unavailable.",
        )
    })?;
    if manager
        .discover(&directory)
        .await?
        .is_none_or(|p| p.identity().id != project.identity().id)
    {
        return Err(Error::new(
            ErrorCode::ProjectBindingMismatch,
            "Native launch must retain its initialized project and selected root.",
        ));
    }
    let arguments = [
        OsString::from(if resume { "--resume" } else { "--session-id" }),
        root.to_string().into(),
        "--plugin-dir".into(),
        plan.plugin_directory().as_os_str().into(),
    ];
    let child = NativeChild::spawn_with_startup_gate_terminal(
        &plan.profile().executable,
        &directory,
        &arguments,
        &plan.configured_environment()?,
        root,
        terminal,
    )
    .await?;
    if plan.is_managed() {
        agentchat_hosts::launch::Registry::new(manager.home())
            .publish_claude_profile(&child, plan.profile())?;
    }
    Ok(child)
}
