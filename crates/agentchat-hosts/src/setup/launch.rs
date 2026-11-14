//! Read-only managed launch inputs from one intact owned installation snapshot.
use super::{Environment, encoded, selection};
use crate::{
    claude::{module, observer::NativeProfile},
    install::{InspectionState, Installer},
};
use agentchat_core::{Error, ErrorCode, Result, model::HostKind};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

/// Owned bundle selected for subsequent managed startup. A plan supplies
/// neither native executable/version verification, current host controls, trust,
/// process identity nor readiness. Recheck those and the live API surface at
/// startup; a cached plan cannot authorize a later launch or configuration edit.
pub struct ClaudeLaunchPlan {
    profile: NativeProfile,
    plugin: PathBuf,
    settings: PathBuf,
    registry: PathBuf,
    state: PathBuf,
    environment: Environment,
    managed: bool,
}
impl ClaudeLaunchPlan {
    /// Engine selected for this launch. Managed plans observe the current PATH
    /// candidate once; legacy plans retain their historical fixed selection.
    pub fn profile(&self) -> &NativeProfile {
        &self.profile
    }
    /// Whether the owned bundle dispatches through an original launch snapshot.
    pub fn is_managed(&self) -> bool {
        self.managed
    }
    /// Owned plugin directory containing its manifest and module entry point.
    pub fn plugin_directory(&self) -> &Path {
        &self.plugin
    }
    /// Verified owned hooks within the existing preserving settings document.
    pub fn settings(&self) -> &Path {
        &self.settings
    }
    /// Verified native MCP selector within the preserving registry document.
    pub fn registry(&self) -> &Path {
        &self.registry
    }
    /// Scoped native environment for normal configuration discovery, without
    /// settings/MCP overrides. An isolated tool-only layout cannot silently
    /// fall back to the real home profile. HOME/CODEX_HOME remain inherited.
    pub fn configured_environment(&self) -> Result<BTreeMap<OsString, OsString>> {
        let roots = self.environment.resolve()?;
        let directory = if self.environment.user_config_root.is_some() {
            roots.isolated_claude
        } else {
            self.environment.claude_config_dir.clone()
        };
        let mut env = BTreeMap::from([("AGENTCHAT_HOME".into(), self.state.as_os_str().into())]);
        if let Some(root) = &self.environment.user_config_root {
            env.insert("AGENTCHAT_USER_CONFIG_ROOT".into(), root.as_os_str().into());
        }
        if let Some(directory) = directory {
            if self.registry != directory.join(".claude.json")
                || self.settings != directory.join("settings.json")
            {
                return Err(conflict());
            }
            env.insert("CLAUDE_CONFIG_DIR".into(), directory.into_os_string());
        } else if self.environment.user_config_root.is_some()
            || self.environment.home.as_ref().map(|p| p.as_os_str())
                != std::env::var_os("HOME").as_deref()
            || std::env::var_os("CLAUDE_CONFIG_DIR").is_some()
        {
            return Err(conflict());
        }
        Ok(env)
    }
}

pub(super) async fn resolve(
    state: PathBuf,
    executable: PathBuf,
    environment: Environment,
    search: Option<OsString>,
    observe_current: bool,
) -> Result<ClaudeLaunchPlan> {
    encoded(&state)?;
    encoded(&executable)?;
    let roots = environment.resolve()?;
    let installer = Installer::new(
        &state,
        roots.tool_roots(&[HostKind::Codex, HostKind::Claude]),
    )?;
    let selected = Arc::new(Mutex::new(None));
    let capture = selected.clone();
    let desired_roots = roots.clone();
    let desired_state = state.clone();
    let inspection = installer
        .inspect_with(move |installed| {
            let profile = selection::claude_selection(&desired_roots, installed)?;
            let desired = selection::inspection_artifacts(
                &desired_roots,
                &[HostKind::Codex, HostKind::Claude],
                &executable,
                &desired_state,
                installed,
            )?;
            if desired
                .iter()
                .any(|a| a.key.starts_with("claude.") && !installed.has(&a.key))
            {
                return Err(conflict());
            }
            // The selector and every owned component are examined under the
            // same existing read lock. Capturing a selector is insufficient:
            // discard it unless the complete inspection also reports healthy.
            *capture.lock().map_err(|_| conflict())? = Some(profile);
            Ok(desired)
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
    let selection = selected
        .lock()
        .map_err(|_| conflict())?
        .take()
        .ok_or_else(conflict)?;
    let (profile, managed) = match selection {
        selection::ClaudeSelection::Fixed(profile) => (profile, false),
        selection::ClaudeSelection::Managed if observe_current => {
            (super::claude_profile::discover(search).await?, true)
        }
        selection::ClaudeSelection::Managed => {
            return Err(Error::new(
                ErrorCode::UnsupportedHost,
                "The managed Claude engine is selected at launch, not by installation inspection.",
            ));
        }
    };
    if !module::has_startup_gate_source(&profile.version) {
        return Err(Error::new(
            ErrorCode::UnsupportedHost,
            "The installed native bundle has no managed first-input startup gate.",
        ));
    }
    Ok(ClaudeLaunchPlan {
        profile,
        plugin: roots.claude_skill.parent().ok_or_else(conflict)?.to_owned(),
        settings: roots.claude_settings,
        registry: roots.claude_registry,
        state,
        environment,
        managed,
    })
}
fn conflict() -> Error {
    Error::new(
        ErrorCode::ConfigurationConflict,
        "Managed launch requires an intact current owned native installation; run agentchat install to inspect or update it.",
    )
}
