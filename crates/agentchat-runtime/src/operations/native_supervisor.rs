//! One configured supervisor for the public entry and explicit native acceptance.
use super::{Operations, Route};
use crate::launch::{ClaudeLaunchRequest, LaunchEvents, spawn_claude};
use agentchat_core::{Error, ErrorCode, Result, validation};
use agentchat_hosts::{claude::adapter::NativeAdapter, launch::ClaudeStartup};
use std::time::Duration;

impl Operations {
    /// Supervise an intact configured Claude plan through actual exit. Public
    /// callers first require adapter eligibility; isolated acceptance may use its
    /// explicit runtime HostGate. This lower-level operation grants no compatibility:
    /// shared Join still checks native/owner authority before committing.
    /// Diagnostics do not select a different owner or replace any native proof.
    pub async fn supervise_claude(
        &self,
        route: &Route,
        plan: &agentchat_hosts::setup::ClaudeLaunchPlan,
        request: ClaudeLaunchRequest,
        events: &dyn LaunchEvents,
    ) -> Result<i32> {
        let _activity = self.manager.activity(route.project.identity().id).await?;
        self.manager.validate(&route.project).await?;
        if route.context.is_some() {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "Configured launch cannot borrow an existing actor.",
            ));
        }
        let name = request
            .name
            .as_deref()
            .map(validation::handle)
            .transpose()?;
        let directory = route.directory.canonicalize().map_err(|_| {
            Error::new(
                ErrorCode::IoError,
                "Native launch directory is unavailable.",
            )
        })?;
        let signals = super::managed_supervision::signals()?;
        let child = spawn_claude(
            &self.manager,
            &route.project,
            &directory,
            plan,
            request.root,
            request.resume,
            request.terminal,
        )
        .await?;
        events.spawned(&child)?;
        let proof = child.proof();
        let startup = ClaudeStartup::new(
            request.root,
            route.project.identity().id,
            directory,
            plan.profile().version.clone(),
        )?;
        let future = Box::pin(async move {
            let adapter = NativeAdapter::new(self.manager.home().into());
            loop {
                if let Some(ready) = startup
                    .observe_proof(self.manager.home(), &proof, &adapter)
                    .await?
                {
                    let (socket, authorization) = crate::launch::authorize_claude_start(
                        &self.manager,
                        &route.project,
                        &proof,
                        &ready,
                        name.clone(),
                    )
                    .await?;
                    let mut control = super::activation::Control::new(
                        socket,
                        authorization,
                        super::activation::Native::Claude {
                            startup,
                            ready: Box::new(ready),
                        },
                    );
                    control.activate_initial(self, route, &proof).await?;
                    return Ok(control);
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        });
        self.supervise_ready(route, child, future, events, signals)
            .await
    }
}
