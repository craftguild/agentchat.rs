//! Public initialization with preserving setup and bounded native discovery.

use super::{Environment, selection};
use crate::claude::observer::NativeProfile;
use crate::install::{InspectionState, InstallProgress, Installer};
use agentchat_core::{Error, ErrorCode, Result, model::HostKind};
use serde_json::Value;
use std::{ffi::OsString, os::unix::fs::PermissionsExt, path::PathBuf, sync::Arc};
use uuid::Uuid;

#[path = "codex_startup.rs"]
mod codex_startup;

#[cfg(test)]
#[path = "../../tests/unit/claude_setup_workflow.rs"]
mod tests;

/// Captured executable, configuration and PATH inputs for shared initialization.
/// Construction performs no filesystem writes or native-host execution.
#[derive(Clone)]
pub struct ToolSetup {
    state: PathBuf,
    executable: PathBuf,
    environment: Environment,
    search: Option<OsString>,
    discover_native: bool,
    progress: Option<Arc<dyn InstallProgress>>,
}
impl ToolSetup {
    /// Captured configuration routing shared with explicit administrative removal.
    /// Reading it neither discovers nor executes a native host.
    pub fn environment(&self) -> Environment {
        self.environment.clone()
    }
    /// Use explicit captured inputs; never change HOME/CODEX_HOME or host policy.
    pub fn new(
        state: PathBuf,
        executable: PathBuf,
        environment: Environment,
        search: Option<OsString>,
    ) -> Self {
        Self {
            state,
            executable,
            environment,
            search,
            discover_native: false,
            progress: None,
        }
    }
    /// Permit public initialization to inspect live native API candidates and
    /// install their selectors. Codex requires an already-running server;
    /// Claude inspection starts only a bounded version probe. Tests opt out
    /// unless they explicitly exercise discovery.
    pub fn with_native_discovery(mut self) -> Self {
        self.discover_native = true;
        self
    }
    /// Observe existing installer boundaries for bounded progress/fault tests.
    pub fn with_progress(mut self, progress: Arc<dyn InstallProgress>) -> Self {
        self.progress = Some(progress);
        self
    }
    /// Inspect the complete owned bundle, then resolve the current managed
    /// engine with one bounded version probe. Never repairs host configuration
    /// or grants admission; legacy diagnostic plans remain read-only.
    pub async fn claude_launch_plan(&self) -> Result<super::ClaudeLaunchPlan> {
        super::launch::resolve(
            self.state.clone(),
            self.executable.clone(),
            self.environment.clone(),
            self.search.clone(),
            true,
        )
        .await
    }
    /// Inspect the current owned Codex MCP/skill bundle without executing a
    /// host, repairing configuration or inferring native readiness/admission.
    pub async fn codex_launch_plan(&self) -> Result<super::CodexLaunchPlan> {
        super::codex::resolve(
            self.state.clone(),
            self.executable.clone(),
            self.environment.clone(),
            self.search.clone(),
        )
        .await
    }
    /// Install available hosts' MCP/skill artifacts, preserving a completed
    /// request's report. The caller first validates project retry identity.
    /// Public initialization may add best-effort native selectors. Discovery
    /// neither starts a conversation nor grants trust or live launch authority.
    pub async fn install(&self, request: Uuid, project: Uuid) -> Result<Value> {
        let roots = self.environment.resolve()?;
        super::encoded(&self.executable)?;
        super::encoded(&self.state)?;
        let installer = Installer::new(
            &self.state,
            roots.tool_roots(&[HostKind::Codex, HostKind::Claude]),
        )?;
        if let Some(report) = installer.completed_request(request, project).await? {
            return Ok(report);
        }
        let selected_codex = self.codex_profile().await?;
        let selected_claude = self.claude_profile().await;
        self.install_selected(request, project, selected_codex, selected_claude)
            .await
    }
    async fn install_selected(
        &self,
        request: Uuid,
        project: Uuid,
        selected_codex: Option<super::CodexProfile>,
        selected_claude: Option<NativeProfile>,
    ) -> Result<Value> {
        let roots = self.environment.resolve()?;
        // Keep the ownership namespace stable when another host is installed
        // later. Artifact selectors still own only the selected agentchat components.
        let mut installer = Installer::new(
            &self.state,
            roots.tool_roots(&[HostKind::Codex, HostKind::Claude]),
        )?;
        if let Some(progress) = &self.progress {
            installer = installer.with_progress(progress.clone());
        }
        let this = self.clone();
        let report_roots = roots.clone();
        installer
            .install_request(
                request,
                project,
                move |installed| {
                    let mut hosts = Vec::new();
                    for (host, program) in
                        [(HostKind::Codex, "codex"), (HostKind::Claude, "claude")]
                    {
                        if present(&this.search, program)? {
                            hosts.push(host);
                        }
                    }
                    let artifacts = selection::artifacts_with_native(
                        &roots,
                        &hosts,
                        &this.executable,
                        &this.state,
                        installed,
                        selected_codex.as_ref(),
                        selected_claude.as_ref(),
                    )?;
                    Ok((artifacts, hosts))
                },
                move |installed, hosts| super::report::installed(&report_roots, hosts, installed),
            )
            .await
    }
    /// Inspect current owned tool configuration without writes or host execution.
    /// Unowned foreign entries and historical request receipts are not health
    /// authority. A selected adapter is still not live conversation authority.
    pub async fn inspect(&self) -> Result<Value> {
        Ok(serde_json::to_value(self.inspect_typed().await?)
            .expect("health DTO is JSON serializable"))
    }
    /// Typed ownership inspection for shared read composition; no JSON bridge.
    pub async fn inspect_typed(
        &self,
    ) -> Result<agentchat_protocol::integration::IntegrationHealth> {
        let roots = self.environment.resolve()?;
        super::encoded(&self.executable)?;
        super::encoded(&self.state)?;
        let hosts = [HostKind::Codex, HostKind::Claude];
        let installer = Installer::new(&self.state, roots.tool_roots(&hosts))?;
        let this = self.clone();
        let report = installer
            .inspect_with(move |installed| {
                selection::inspection_artifacts(
                    &roots,
                    &hosts,
                    &this.executable,
                    &this.state,
                    installed,
                )
            })
            .await?;
        let state = match report.state {
            InspectionState::NotInstalled => "not_installed",
            InspectionState::Healthy => "partial",
            InspectionState::Busy => "busy",
            _ => "attention_required",
        };
        let native_codex = compatible_codex(self.codex_launch_plan().await.ok());
        let native_claude = compatible_claude(
            super::launch::resolve(
                self.state.clone(),
                self.executable.clone(),
                self.environment.clone(),
                None,
                false,
            )
            .await
            .ok(),
        );
        Ok(agentchat_protocol::integration::IntegrationHealth {
            state: state.into(),
            tools: Some(report),
            lifecycle: Some(lifecycle(native_codex.as_ref(), native_claude.as_ref())),
            error: None,
            reason: None,
        })
    }

    async fn claude_profile(&self) -> Option<NativeProfile> {
        if !self.discover_native {
            return None;
        }
        let profile = super::claude_profile::discover(self.search.clone())
            .await
            .ok()?;
        crate::require_native_profile(
            HostKind::Claude,
            &profile.version,
            &format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        )
        .ok()?;
        Some(profile)
    }

    async fn codex_profile(&self) -> Result<Option<super::CodexProfile>> {
        if !self.discover_native || !present(&self.search, "codex")? {
            return Ok(None);
        }
        let Some(cli) = optional_candidate(super::codex::locate(self.search.clone()))? else {
            return Ok(None);
        };
        let Some(daemon) = optional_candidate(crate::codex::Daemon::inspect(&cli).await)? else {
            return Ok(None);
        };
        // A successful same-user experimental initialize is the first live
        // API-surface check. Preserve a named surface error for init callers;
        // ordinary absence still permits tool-only installation.
        if optional_candidate(crate::codex::Client::connect(&daemon.socket, &daemon.version).await)?
            .is_none()
        {
            return Ok(None);
        }
        if optional_candidate(crate::require_native_profile(
            HostKind::Codex,
            &daemon.version,
            &format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        ))?
        .is_none()
        {
            return Ok(None);
        }
        Ok(Some(super::CodexProfile {
            executable: daemon.executable,
            version: daemon.version,
        }))
    }
}
fn optional_candidate<T>(result: Result<T>) -> Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.code == ErrorCode::UnsupportedProtocol => Err(error),
        Err(_) => Ok(None),
    }
}
fn compatible_codex(plan: Option<super::CodexLaunchPlan>) -> Option<super::CodexProfile> {
    let os_profile = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    plan.and_then(|plan| plan.profile().cloned())
        .filter(|profile| {
            crate::require_native_profile(HostKind::Codex, &profile.version, &os_profile).is_ok()
        })
}
fn compatible_claude(plan: Option<super::ClaudeLaunchPlan>) -> Option<NativeProfile> {
    let os_profile = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    plan.map(|plan| plan.profile().clone()).filter(|profile| {
        crate::require_native_profile(HostKind::Claude, &profile.version, &os_profile).is_ok()
    })
}
pub(super) fn lifecycle(
    codex: Option<&super::CodexProfile>,
    claude: Option<&NativeProfile>,
) -> agentchat_protocol::integration::LifecycleHealth {
    use agentchat_protocol::integration::{LifecycleHealth, LifecycleProfile};
    let mut profiles = Vec::new();
    for (host, version) in [
        (HostKind::Codex, codex.map(|p| p.version.as_str())),
        (HostKind::Claude, claude.map(|p| p.version.as_str())),
    ] {
        if let Some(version) = version {
            profiles.push(LifecycleProfile {
                host,
                host_version: version.into(),
                compatibility: "best_effort_api_surface".into(),
                os_profile: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            });
        }
    }
    if !profiles.is_empty() {
        LifecycleHealth::Available { profiles }
    } else {
        LifecycleHealth::Unavailable {
            reason: Some(
                "No current intact native installation exposes the required host API surface."
                    .into(),
            ),
        }
    }
}
fn present(search: &Option<OsString>, program: &str) -> Result<bool> {
    let Some(search) = search else {
        return Ok(false);
    };
    for directory in std::env::split_paths(search) {
        match std::fs::metadata(directory.join(program)) {
            Ok(meta) if meta.is_file() && meta.permissions().mode() & 0o111 != 0 => {
                return Ok(true);
            }
            Ok(_) => {}
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) => {}
            Err(_) => {
                return Err(Error::new(
                    ErrorCode::IoError,
                    "Host executable discovery failed.",
                ));
            }
        }
    }
    Ok(false)
}
