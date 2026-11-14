//! Read-only Codex ownership selection; shared-server discovery follows later.
use super::{CodexProfile, Environment, encoded, selection};
use crate::install::{InspectionState, Installer};
use agentchat_core::{Error, ErrorCode, Result, model::HostKind};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

/// Intact owned Codex integration and captured CLI selector. Selection executes
/// no host, starts no server and proves neither native trust nor readiness.
/// In particular an isolated installation is not the real CLI's configuration.
pub struct CodexLaunchPlan {
    profile: Option<CodexProfile>,
    cli: PathBuf,
    configuration: PathBuf,
    skill: PathBuf,
    state: PathBuf,
    environment: Environment,
    managed: bool,
}
impl CodexLaunchPlan {
    /// Whether the owned MCP entry resolves the current native CLI at use.
    pub fn is_managed(&self) -> bool {
        self.managed
    }

    pub(super) fn select_current(&mut self, profile: CodexProfile) {
        self.profile = Some(profile);
    }
    /// Intact owned native selector, if explicitly installed. A generic MCP
    /// entry remains useful for tools but cannot complete managed startup.
    pub fn profile(&self) -> Option<&CodexProfile> {
        self.profile.as_ref()
    }
    /// Installed CLI used to inspect or start its ordinary shared daemon. It may
    /// be a shim; the matching real native image comes from daemon inspection.
    pub fn cli(&self) -> &Path {
        &self.cli
    }
    /// Exact preserving configuration containing the owned MCP member.
    pub fn configuration(&self) -> &Path {
        &self.configuration
    }
    /// Exact owned shared skill file examined by the same inspection.
    pub fn skill(&self) -> &Path {
        &self.skill
    }
    /// Environment for ordinary native configuration discovery. Never redirect
    /// HOME/CODEX_HOME or let a test-only installation fall through to real user
    /// settings. Explicit native acceptance must supply its own isolated route.
    pub fn configured_environment(&self) -> Result<std::collections::BTreeMap<OsString, OsString>> {
        let actual = Environment::capture();
        if self.environment.user_config_root.is_some()
            || self.environment.home != actual.home
            || self.environment.codex_home != actual.codex_home
            || actual.resolve()?.codex_config != self.configuration
        {
            return Err(conflict());
        }
        Ok(std::collections::BTreeMap::from([(
            "AGENTCHAT_HOME".into(),
            self.state.as_os_str().into(),
        )]))
    }
}

pub(super) async fn resolve(
    state: PathBuf,
    executable: PathBuf,
    environment: Environment,
    search: Option<OsString>,
) -> Result<CodexLaunchPlan> {
    encoded(&state)?;
    encoded(&executable)?;
    let roots = environment.resolve()?;
    let installer = Installer::new(
        &state,
        roots.tool_roots(&[HostKind::Codex, HostKind::Claude]),
    )?;
    let selected = roots.clone();
    let desired_state = state.clone();
    let selected_profile = Arc::new(Mutex::new(None));
    let capture = selected_profile.clone();
    let inspection = installer
        .inspect_with(move |installed| {
            if !installed.has("codex.mcp") || !installed.has("codex.skill") {
                return Err(conflict());
            }
            *capture.lock().map_err(|_| conflict())? = Some((
                selection::codex_profile(&selected, installed)?,
                selection::managed_codex(&selected, installed)?,
            ));
            // Compare the complete bundle/state under the build-profile path
            // policy; entry presence or an old receipt is never sufficient.
            selection::inspection_artifacts(
                &selected,
                &[HostKind::Codex, HostKind::Claude],
                &executable,
                &desired_state,
                installed,
            )
        })
        .await?;
    if inspection.state == InspectionState::Busy {
        let mut error = Error::new(ErrorCode::Busy, "Native installation is being changed.");
        error.retryable = true;
        return Err(error);
    }
    if inspection.state != InspectionState::Healthy {
        return Err(conflict());
    }
    let cli = locate(search)?;
    // Native executable/version discovery happens later. This captures only
    // the CLI selector and never repairs a missing owned component.
    let (profile, managed) = selected_profile
        .lock()
        .map_err(|_| conflict())?
        .take()
        .ok_or_else(conflict)?;
    Ok(CodexLaunchPlan {
        profile,
        managed,
        cli,
        configuration: roots.codex_config,
        skill: roots.codex_skill,
        state,
        environment,
    })
}

pub(super) fn locate(search: Option<OsString>) -> Result<PathBuf> {
    super::program::locate("codex", search)
}
fn conflict() -> Error {
    Error::new(
        ErrorCode::ConfigurationConflict,
        "Managed Codex launch requires an intact current owned integration and matching native configuration discovery.",
    )
}
