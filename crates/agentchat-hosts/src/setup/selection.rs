//! Retain an explicit native selection; discovery never adopts foreign config.
use super::{CodexProfile, Roots};
use crate::{
    claude::observer::NativeProfile,
    install::{Artifact, Installed},
};
use agentchat_core::{Error, ErrorCode, Result, model::HostKind};
use std::path::{Path, PathBuf};

pub(super) enum ClaudeSelection {
    Fixed(NativeProfile),
    Managed,
}

pub(super) fn artifacts(
    roots: &Roots,
    hosts: &[HostKind],
    executable: &Path,
    state: &Path,
    installed: &Installed<'_>,
) -> Result<Vec<Artifact>> {
    artifacts_with_native(roots, hosts, executable, state, installed, None, None)
}

/// Release callers may differ from the saved dispatch image. Read the command
/// only through the locked ownership/hash view, then retain complete artifact
/// and state comparison. Explicit installation still uses the caller's path.
pub(super) fn inspection_artifacts(
    roots: &Roots,
    hosts: &[HostKind],
    executable: &Path,
    state: &Path,
    installed: &Installed<'_>,
) -> Result<Vec<Artifact>> {
    if cfg!(debug_assertions) {
        return artifacts(roots, hosts, executable, state, installed);
    }
    let mut desired = Vec::new();
    for host in hosts {
        let value = match host {
            HostKind::Codex => installed.toml(
                "codex.mcp",
                &roots.codex_config,
                &["mcp_servers", "agentchat"],
            )?,
            HostKind::Claude => installed.json(
                "claude.mcp",
                &roots.claude_registry,
                &["mcpServers", "agentchat"],
            )?,
        };
        let saved = value
            .as_ref()
            .map(|value| -> Result<PathBuf> {
                let command = value
                    .get("command")
                    .and_then(|v| v.as_str())
                    .ok_or_else(invalid)?;
                let path = PathBuf::from(command);
                super::encoded(&path)?;
                Ok(path)
            })
            .transpose()?;
        // A missing unrelated host contributes no owned entries. Launch's
        // required-host checks remain separate and cannot adopt a missing MCP.
        desired.extend(artifacts(
            roots,
            &[*host],
            saved.as_deref().unwrap_or(executable),
            state,
            installed,
        )?);
    }
    Ok(desired)
}

pub(super) fn artifacts_with_native(
    roots: &Roots,
    hosts: &[HostKind],
    executable: &Path,
    state: &Path,
    installed: &Installed<'_>,
    selected_codex: Option<&CodexProfile>,
    selected_claude: Option<&NativeProfile>,
) -> Result<Vec<Artifact>> {
    let mut artifacts = roots.tool_artifacts(hosts, executable, state)?;
    let installed_codex = codex_profile(roots, installed)?;
    if hosts.contains(&HostKind::Codex) {
        if selected_codex.is_some() || managed_codex(roots, installed)? {
            artifacts.retain(|a| !a.key.starts_with("codex."));
            artifacts.extend(roots.codex_managed_artifacts(executable, state)?);
        } else if let Some(profile) = installed_codex {
            artifacts.retain(|a| !a.key.starts_with("codex."));
            artifacts.extend(roots.codex_native_artifacts(executable, state, &profile)?);
        }
    }
    if !hosts.contains(&HostKind::Claude) {
        return Ok(artifacts);
    }
    // An existing owned Claude selector takes precedence over discovery. A
    // malformed selection must fail rather than being silently replaced.
    let installed_claude = installed
        .has_prefix("claude.native.")
        .then(|| claude_selection(roots, installed))
        .transpose()?;
    if selected_claude.is_some() || matches!(installed_claude, Some(ClaudeSelection::Managed)) {
        artifacts.retain(|a| !a.key.starts_with("claude."));
        artifacts.extend(roots.claude_managed_artifacts(executable, state)?);
    } else if let Some(ClaudeSelection::Fixed(profile)) = installed_claude {
        artifacts.retain(|a| !a.key.starts_with("claude."));
        artifacts.extend(roots.claude_native_artifacts(executable, state, &profile)?);
    }
    Ok(artifacts)
}

pub(super) fn codex_profile(
    roots: &Roots,
    installed: &Installed<'_>,
) -> Result<Option<CodexProfile>> {
    let Some(value) = installed.toml(
        "codex.mcp",
        &roots.codex_config,
        &["mcp_servers", "agentchat"],
    )?
    else {
        return Ok(None);
    };
    let environment = value.get("env").ok_or_else(invalid)?;
    let native = environment.get("AGENTCHAT_CODEX_NATIVE_EXE");
    let version = environment.get("AGENTCHAT_CODEX_NATIVE_VERSION");
    if let Some(mode) = environment.get("AGENTCHAT_CODEX_MANAGED") {
        if mode.as_str() != Some("1") || native.is_some() || version.is_some() {
            return Err(invalid());
        }
        return Ok(None);
    }
    if native.is_none() && version.is_none() {
        return Ok(None);
    }
    let profile = CodexProfile {
        executable: PathBuf::from(native.and_then(|v| v.as_str()).ok_or_else(invalid)?),
        version: version.and_then(|v| v.as_str()).ok_or_else(invalid)?.into(),
    };
    profile.validate()?;
    Ok(Some(profile))
}

pub(super) fn managed_codex(roots: &Roots, installed: &Installed<'_>) -> Result<bool> {
    // Parse the complete selection first, including malformed mixed modes.
    codex_profile(roots, installed)?;
    Ok(installed
        .toml(
            "codex.mcp",
            &roots.codex_config,
            &["mcp_servers", "agentchat"],
        )?
        .is_some_and(|v| {
            v.get("env")
                .and_then(|v| v.get("AGENTCHAT_CODEX_MANAGED"))
                .and_then(|v| v.as_str())
                == Some("1")
        }))
}

pub(super) fn claude_selection(
    roots: &Roots,
    installed: &Installed<'_>,
) -> Result<ClaudeSelection> {
    for key in ["module", "manifest", "modules", "file-changed", "final"] {
        if !installed.has(&format!("claude.native.{key}")) {
            return Err(invalid());
        }
    }
    // Only an intact owned selector can choose a native bundle. Env, foreign
    // settings and executable discovery cannot grant this selection/admission.
    let value = installed
        .json(
            "claude.mcp",
            &roots.claude_registry,
            &["mcpServers", "agentchat"],
        )?
        .ok_or_else(invalid)?;
    let environment = value.get("env").ok_or_else(invalid)?;
    if let Some(mode) = environment.get("AGENTCHAT_CLAUDE_MANAGED") {
        if mode.as_str() != Some("1")
            || environment.get("AGENTCHAT_CLAUDE_NATIVE_EXE").is_some()
            || environment.get("AGENTCHAT_CLAUDE_NATIVE_VERSION").is_some()
            || ["input", "stop"]
                .into_iter()
                .any(|key| !installed.has(&format!("claude.native.{key}")))
        {
            return Err(invalid());
        }
        return Ok(ClaudeSelection::Managed);
    }
    let native = environment
        .get("AGENTCHAT_CLAUDE_NATIVE_EXE")
        .and_then(|v| v.as_str())
        .ok_or_else(invalid)?;
    let version = environment
        .get("AGENTCHAT_CLAUDE_NATIVE_VERSION")
        .and_then(|v| v.as_str())
        .ok_or_else(invalid)?;
    let profile = NativeProfile {
        executable: PathBuf::from(native),
        version: version.into(),
    };
    if crate::claude::module::has_startup_gate_source(&profile.version)
        && ["input", "stop"]
            .into_iter()
            .any(|key| !installed.has(&format!("claude.native.{key}")))
    {
        return Err(invalid());
    }
    Ok(ClaudeSelection::Fixed(profile))
}
fn invalid() -> Error {
    Error::new(
        ErrorCode::ConfigurationConflict,
        "The owned native selection requires an explicit supported update.",
    )
}
