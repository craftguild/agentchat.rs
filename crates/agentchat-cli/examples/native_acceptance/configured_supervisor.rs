//! Observe the same supervisor used by the public operation; no second loop.
use super::{Result, startup::Inputs};
use agentchat_core::{Error, ErrorCode};
use agentchat_hosts::{
    claude::metadata,
    launch::{NativeChild, NativeTerminal, ReadyAuthorization},
    setup::ClaudeLaunchPlan,
};
use agentchat_runtime::{
    launch::{ClaudeLaunchRequest, LaunchEvents, StartedLaunch},
    operations::{Launcher, Operations, Route},
    project::{Project, ProjectManager},
};
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    process::ExitStatus,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

struct ExistingRuntime;
#[async_trait::async_trait]
impl Launcher for ExistingRuntime {
    async fn launch(&self, _: &Project, _: &Path) -> agentchat_core::Result<()> {
        // This fixture must use its explicit project-scoped candidate. Missing
        // it cannot start another daemon or inherit the developer's real roots.
        Err(Error::new(
            ErrorCode::RuntimeUnavailable,
            "The explicit native candidate runtime is unavailable.",
        ))
    }
}
struct Report {
    path: PathBuf,
    started: AtomicBool,
}
impl LaunchEvents for Report {
    fn spawned(&self, child: &NativeChild) -> agentchat_core::Result<()> {
        metadata::write(
            &self.path.with_extension("spawn.json"),
            &json!({
                "owner":child.owner(),"launch":child.identity(),"spawned":true,
                "startup_observed":false,"membership_established":false,"admitted":false,
            }),
        )
    }
    fn authorized(
        &self,
        child: &NativeChild,
        ready: &ReadyAuthorization,
    ) -> agentchat_core::Result<()> {
        metadata::write(
            &self.path,
            &json!({
                "owner":child.owner(),"launch":child.identity(),"startup_observed":true,
                "context":ready.binding().context,"membership_established":false,
                "shared_supervisor":true,"admitted":false,
            }),
        )?;
        self.started.store(true, Ordering::SeqCst);
        Ok(())
    }
    fn activated(&self, value: &StartedLaunch) -> agentchat_core::Result<()> {
        metadata::write(
            &self.path.with_extension("activated.json"),
            &json!({
                "context":value.context,"launch":value.identity,"agent_id":value.agent_id,
                "boundary":value.boundary,"membership_established":true,"shared_supervisor":true,"admitted":false,
            }),
        )
    }
    fn connection_lost(&self, error: &Error) -> agentchat_core::Result<()> {
        metadata::write(
            &self.path.with_extension("heartbeat-error.json"),
            &json!({"error":error,"admitted":false}),
        )
    }
    fn reattached(&self, started: &StartedLaunch) -> agentchat_core::Result<()> {
        metadata::write(
            &self.path.with_extension("reattached.json"),
            &json!({
                "context":started.context,"launch":started.identity,"agent_id":started.agent_id,
                "boundary":started.boundary,"shared_supervisor":true,"admitted":false,
            }),
        )
    }
    fn context_refresh_requested(
        &self,
        context: &agentchat_core::model::Context,
    ) -> agentchat_core::Result<()> {
        metadata::write(
            &self.path.with_extension("refresh-requested.json"),
            &json!({
                "context":context,"native_request_accepted":true,"membership_restored":false,"admitted":false,
            }),
        )
    }
    fn exited(&self, child: &NativeChild, status: ExitStatus) -> agentchat_core::Result<()> {
        metadata::write(
            &self.path.with_extension("exit.json"),
            &json!({
                "owner":child.owner(),"exit_code":status.code(),"success":status.success(),
                "original_owner_gone":child.state()==agentchat_core::model::LaunchOwnerState::Gone,
                "shared_supervisor":true,"admitted":false,
            }),
        )
    }
}
pub(super) async fn run(
    manager: &ProjectManager,
    project: &Project,
    directory: PathBuf,
    plan: &ClaudeLaunchPlan,
    input: &Inputs,
) -> Result<()> {
    let operations = Operations::new(manager.clone(), Arc::new(ExistingRuntime));
    let route = Route {
        project: project.clone(),
        directory,
        context: None,
        user: false,
    };
    let events = Report {
        path: input.report.clone(),
        started: AtomicBool::new(false),
    };
    let code = operations
        .supervise_claude(
            &route,
            plan,
            ClaudeLaunchRequest {
                root: input.root,
                resume: input.resume,
                name: input.name.clone(),
                terminal: if input.terminal_json {
                    NativeTerminal::Controlling
                } else {
                    NativeTerminal::Inherited
                },
            },
            &events,
        )
        .await?;
    if code != 0 || !events.started.load(Ordering::SeqCst) {
        return Err(
            format!("Native supervisor ended without successful startup/exit: {code}").into(),
        );
    }
    Ok(())
}
