//! Managed launch may add a missing selector without rewriting init history.
use super::ToolSetup;
use crate::{
    codex::{Client, Daemon},
    install::Installer,
    setup::{CodexLaunchPlan, CodexProfile, selection},
};
use agentchat_core::{Error, ErrorCode, Result, model::HostKind};
use uuid::Uuid;

#[cfg(test)]
#[path = "../../tests/unit/codex_startup.rs"]
mod tests;

impl ToolSetup {
    /// Prepare an initialized project's owned Codex integration. Validate normal
    /// configuration discovery before starting any host or changing settings.
    /// Reuse a verified live server, or delegate one ensure/start to the native CLI.
    /// Only a missing native selector is added through the preserving installer;
    /// completed init receipts and existing native selections remain immutable.
    /// This grants no conversation identity, trust or launch readiness.
    pub async fn prepare_codex_launch(&self, project: Uuid) -> Result<(CodexLaunchPlan, Daemon)> {
        let plan = self.codex_launch_plan().await?;
        plan.configured_environment()?;
        let prepared = self.prepare_codex_plan(project, plan).await?;
        prepared.0.configured_environment()?;
        Ok(prepared)
    }

    // The public entry point checks ordinary configuration before this effectful
    // stage. Separate fixture tests exercise it with only owned temporary roots.
    async fn prepare_codex_plan(
        &self,
        project: Uuid,
        mut plan: CodexLaunchPlan,
    ) -> Result<(CodexLaunchPlan, Daemon)> {
        let daemon = Daemon::ensure_running(plan.cli()).await?;
        crate::require_native_profile(
            HostKind::Codex,
            &daemon.version,
            &format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        )?;
        if plan.is_managed() {
            if let Some(selected) = plan.profile() {
                verify_profile(selected, &daemon)?;
            } else {
                Client::connect(&daemon.socket, &daemon.version).await?;
            }
            plan.select_current(CodexProfile {
                executable: daemon.executable.clone(),
                version: daemon.version.clone(),
            });
            return Ok((plan, daemon));
        }
        if let Some(profile) = plan.profile() {
            verify_profile(profile, &daemon)?;
            return Ok((plan, daemon));
        }
        Client::connect(&daemon.socket, &daemon.version).await?;
        let profile = CodexProfile {
            executable: daemon.executable.clone(),
            version: daemon.version.clone(),
        };
        self.install_codex_selector(project, profile).await?;
        let mut plan = self.codex_launch_plan().await?;
        if plan.is_managed() {
            plan.select_current(CodexProfile {
                executable: daemon.executable.clone(),
                version: daemon.version.clone(),
            });
        }
        verify_profile(plan.profile().ok_or_else(changed)?, &daemon)?;
        Ok((plan, daemon))
    }

    async fn install_codex_selector(&self, project: Uuid, profile: CodexProfile) -> Result<()> {
        let roots = self.environment.resolve()?;
        let mut installer = Installer::new(
            &self.state,
            roots.tool_roots(&[HostKind::Codex, HostKind::Claude]),
        )?;
        if let Some(progress) = &self.progress {
            installer = installer.with_progress(progress.clone());
        }
        let this = self.clone();
        let report_roots = roots.clone();
        // A new internal installation key cannot mutate or replay an earlier
        // successful project-init receipt. Selection is checked again under the
        // installer lock; a concurrent different native selection is not replaced.
        installer
            .install_request(
                Uuid::new_v4(),
                project,
                move |installed| {
                    if selection::codex_profile(&roots, installed)?
                        .as_ref()
                        .is_some_and(|existing| existing != &profile)
                    {
                        return Err(changed());
                    }
                    let artifacts = selection::artifacts_with_native(
                        &roots,
                        &[HostKind::Codex],
                        &this.executable,
                        &this.state,
                        installed,
                        Some(&profile),
                        None,
                    )?;
                    Ok((artifacts, vec![HostKind::Codex]))
                },
                move |installed, hosts| {
                    crate::setup::report::installed(&report_roots, hosts, installed)
                },
            )
            .await?;
        Ok(())
    }
}

fn verify_profile(profile: &CodexProfile, daemon: &Daemon) -> Result<()> {
    if profile.version != daemon.version || profile.executable != daemon.executable {
        return Err(changed());
    }
    Ok(())
}
fn changed() -> Error {
    Error::new(
        ErrorCode::ConfigurationConflict,
        "The Codex server differs from the installed native selection. Inspect the configuration before an explicit update.",
    )
}
