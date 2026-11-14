//! Shared managed-launch policy; the CLI owns only terminal argument adaptation.
use super::{Operations, Route};
use crate::launch::{ClaudeLaunchRequest, CodexLaunchRequest};
use agentchat_core::{Error, ErrorCode, Result, model::HostKind, validation};
use uuid::Uuid;

/// User-selected native startup options. No prompt, arbitrary host flags, owner
/// proof, model, trust, sandbox or approval override can be supplied here.
pub struct NativeLaunch {
    /// Native host to start; human test actors are not launch targets.
    pub host: HostKind,
    /// Exact native root to resume, otherwise allocate a fresh conversation.
    pub resume: Option<String>,
    /// Optional participant handle, normalized by shared core validation.
    pub name: Option<String>,
    /// Native terminal routing; machine output uses the controlling terminal.
    pub terminal: agentchat_hosts::launch::NativeTerminal,
}

impl Operations {
    /// Run a native terminal under fresh launch authority until its actual exit.
    /// Structural adapter checks precede native/configuration side effects.
    /// Live API checks occur as each adapter operation is used; readiness,
    /// startup commit and controls remain independent.
    pub async fn launch_native(&self, route: &Route, request: NativeLaunch) -> Result<i32> {
        let _activity = self.manager.activity(route.project.identity().id).await?;
        self.manager.validate(&route.project).await?;
        if route.context.is_some() || !matches!(request.host, HostKind::Codex | HostKind::Claude) {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "Native launch requires a fresh host identity and an explicit project route.",
            ));
        }
        let name = request
            .name
            .as_deref()
            .map(validation::handle)
            .transpose()?;
        let root = request
            .resume
            .as_deref()
            .map(Uuid::parse_str)
            .transpose()
            .map_err(|_| {
                Error::new(
                    ErrorCode::InvalidArgument,
                    "Native resume requires an exact conversation UUID.",
                )
            })?;
        if root.is_some_and(|root| root.is_nil()) {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "A nil conversation UUID cannot be resumed.",
            ));
        }
        agentchat_hosts::require_native_launch(request.host)?;
        let setup = self.host_tools.as_ref().ok_or_else(|| {
            Error::new(
                ErrorCode::ConfigurationConflict,
                "Native launch requires the installed host integration.",
            )
        })?;
        if request.host == HostKind::Codex {
            let (plan, daemon) = setup
                .prepare_codex_launch(route.project.identity().id)
                .await?;
            let environment = plan.configured_environment()?;
            return self
                .supervise_codex(
                    route,
                    &daemon,
                    &environment,
                    CodexLaunchRequest {
                        resume: root,
                        name,
                        terminal: request.terminal,
                    },
                    &(),
                )
                .await;
        }
        let plan = setup.claude_launch_plan().await?;
        if !plan.is_managed() {
            return Err(agentchat_core::Error::new(
                agentchat_core::ErrorCode::ConfigurationConflict,
                "The owned Claude integration uses a fixed engine path; run agentchat install to update it to managed dispatch.",
            ));
        }
        agentchat_hosts::require_native_profile(
            request.host,
            &plan.profile().version,
            &format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        )?;
        self.supervise_claude(
            route,
            &plan,
            ClaudeLaunchRequest {
                root: root.unwrap_or_else(Uuid::new_v4),
                resume: request.resume.is_some(),
                name,
                terminal: request.terminal,
            },
            &(),
        )
        .await
    }
}
