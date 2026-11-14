//! Actual shared-daemon fixture; using it never grants or expands admission.
use super::Result;
use agentchat_core::{
    Error, ErrorCode,
    delivery::{MessageDispatch, MessagePort, MessageProgress, NoticeDispatch},
    model::{Context, HostKind, LaunchIdentity, LaunchOwnerState},
    ports::{HostGate, HostState},
    time::SystemClock,
};
use agentchat_hosts::{
    claude::metadata,
    codex::{Daemon, NativeAdapter, NativeRegistry},
    launch::{LaunchOwners, NativeChild, NativeTerminal, Registry},
    setup::{Environment, ToolSetup},
};
use agentchat_runtime::{
    daemon::{RuntimeOptions, Server},
    launch::{CodexLaunchRequest, LaunchEvents, StartedLaunch},
    operations::{Launcher, Operations, Route},
    project::{Project, ProjectManager},
};
use async_trait::async_trait;
use serde_json::json;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};
use uuid::Uuid;

struct Candidate {
    native: NativeAdapter,
    launches: Registry,
    project: Uuid,
    directory: PathBuf,
    version: String,
    hold: Option<super::acceptance_hold::AcceptanceHold>,
}
#[async_trait]
impl HostGate for Candidate {
    fn input_capabilities(
        &self,
        context: &Context,
    ) -> agentchat_core::Result<agentchat_core::delivery::InputCapabilities> {
        self.selected(context)?;
        Ok(self.native.input_capabilities())
    }
    async fn verify(&self, c: &Context) -> agentchat_core::Result<HostState> {
        self.selected(c)?;
        self.native.observe(c).await
    }
    async fn launch_identity(&self, c: &Context) -> agentchat_core::Result<Option<LaunchIdentity>> {
        self.selected(c)?;
        self.launches
            .resolve(c)
            .map(|binding| binding.map(|binding| binding.identity))
    }
    fn launch_owner(&self, identity: &LaunchIdentity) -> LaunchOwnerState {
        LaunchOwners::observe(identity)
    }
    fn launch_root_changed(
        &self,
        project: Uuid,
        host: HostKind,
        root: &str,
        identity: &LaunchIdentity,
    ) -> bool {
        self.launches.root_changed(project, host, root, identity)
    }
    fn launch_root_departed(
        &self,
        project: Uuid,
        host: HostKind,
        root: &str,
        identity: &LaunchIdentity,
    ) -> bool {
        // The actual-host fixture must preserve the production distinction:
        // a provisional Codex transition suspends delivery, but only the
        // correlated immutable departure record may retire participation.
        self.launches.root_departed(project, host, root, identity)
    }
}
#[async_trait]
impl MessagePort for Candidate {
    async fn present_message(
        &self,
        c: &Context,
        dispatch: &MessageDispatch,
    ) -> agentchat_core::Result<MessageProgress> {
        self.selected(c)?;
        let progress = self.native.present_message(c, dispatch).await?;
        if let Some(hold) = &self.hold {
            hold.observe(c, dispatch, &progress)?;
        }
        Ok(progress)
    }
    async fn present_notice(
        &self,
        c: &Context,
        dispatch: &NoticeDispatch,
    ) -> agentchat_core::Result<MessageProgress> {
        self.selected(c)?;
        self.native.present_notice(c, dispatch).await
    }
}
impl Candidate {
    fn selected(&self, c: &Context) -> agentchat_core::Result<()> {
        if c.host != HostKind::Codex
            || c.host_version != self.version
            || c.project_id != self.project
            || c.working_directory != self.directory
            || c.os_profile != format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
        {
            return Err(unavailable());
        }
        Ok(())
    }
}
pub(super) async fn serve(
    state: PathBuf,
    project: PathBuf,
    version: String,
    hold_accepted: Option<PathBuf>,
) -> Result<()> {
    if version.is_empty() || version.len() > 128 || version.chars().any(char::is_control) {
        return Err(unavailable().into());
    }
    let manager = ProjectManager::new(state)?;
    let hold = hold_accepted
        .map(|path| super::acceptance_hold::AcceptanceHold::new(path, manager.home()))
        .transpose()?;
    let directory = project.canonicalize()?;
    let project = manager
        .discover(&directory)
        .await?
        .ok_or_else(unavailable)?;
    let host = Arc::new(Candidate {
        native: NativeAdapter::new(manager.home().into()),
        launches: Registry::new(manager.home()),
        project: project.identity().id,
        directory,
        version,
        hold,
    });
    let server = Server::bind(
        manager,
        project,
        host.clone(),
        Arc::new(SystemClock),
        RuntimeOptions::default(),
    )
    .await?
    .with_native_messages(host);
    let handle = server.handle();
    println!(
        "{}",
        json!({"candidate_ready":true,"socket":handle.paths().socket,"instance":handle.instance_id(),"admitted":false})
    );
    let serving = server.run();
    tokio::pin!(serving);
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        result=&mut serving=>result?,
        _=terminate.recv()=>{ handle.shutdown(); serving.await?; },
    }
    Ok(())
}

#[derive(clap::Args)]
pub(super) struct Inputs {
    state: PathBuf,
    project: PathBuf,
    cli: PathBuf,
    config: PathBuf,
    report: PathBuf,
    #[arg(long)]
    resume: Option<Uuid>,
    #[arg(long)]
    name: Option<String>,
    /// Inspect an existing owned bundle with this release caller before the
    /// isolated fixture enters the ordinary shared supervisor. No installation
    /// or normal-global-configuration discovery is performed by this seam.
    #[arg(long)]
    agentchat: Option<PathBuf>,
}
struct Existing;
#[async_trait]
impl Launcher for Existing {
    async fn launch(&self, _: &Project, _: &Path) -> agentchat_core::Result<()> {
        Err(unavailable())
    }
}
struct Report(PathBuf);
impl LaunchEvents for Report {
    fn codex_selected(&self, root: &str) -> agentchat_core::Result<()> {
        metadata::write(
            &self.0.with_extension("selected.json"),
            &json!({"root":root,"native_ready":false,"membership_committed":false,"admitted":false}),
        )
    }
    fn spawned(&self, child: &NativeChild) -> agentchat_core::Result<()> {
        metadata::write(
            &self.0.with_extension("spawn.json"),
            &json!({"owner":child.owner(),"launch":child.identity(),"admitted":false}),
        )
    }
    fn authorized(
        &self,
        child: &NativeChild,
        ready: &agentchat_hosts::launch::ReadyAuthorization,
    ) -> agentchat_core::Result<()> {
        metadata::write(
            &self.0,
            &json!({"owner":child.owner(),"context":ready.binding().context,
            "launch":ready.binding().identity,"root":ready.binding().context.host_session_id,
            "native_ready":true,"membership_committed":false,"shared_supervisor":true,"admitted":false}),
        )
    }
    fn activated(&self, value: &StartedLaunch) -> agentchat_core::Result<()> {
        metadata::write(
            &self.0.with_extension("activated.json"),
            &json!({"context":value.context,"launch":value.identity,
            "agent_id":value.agent_id,"boundary":value.boundary,"membership_committed":true,"shared_supervisor":true,"admitted":false}),
        )
    }
    fn connection_lost(&self, error: &Error) -> agentchat_core::Result<()> {
        metadata::write(
            &self.0.with_extension("connection-error.json"),
            &json!({"error":error,"admitted":false}),
        )
    }
    fn reattached(&self, value: &StartedLaunch) -> agentchat_core::Result<()> {
        metadata::write(
            &self.0.with_extension("reattached.json"),
            &json!({"context":value.context,"launch":value.identity,
            "agent_id":value.agent_id,"boundary":value.boundary,"admitted":false}),
        )
    }
    fn exited(
        &self,
        child: &NativeChild,
        status: std::process::ExitStatus,
    ) -> agentchat_core::Result<()> {
        metadata::write(
            &self.0.with_extension("exit.json"),
            &json!({"exit_code":status.code(),"success":status.success(),
            "original_owner_gone":child.state()==LaunchOwnerState::Gone,"admitted":false}),
        )
    }
}
pub(super) async fn startup(input: Inputs) -> Result<()> {
    if [
        &input.state,
        &input.project,
        &input.cli,
        &input.config,
        &input.report,
    ]
    .iter()
    .any(|p| !p.is_absolute())
    {
        return Err(unavailable().into());
    }
    // Reject an unusable test report before allocating a native root/MCP.
    metadata::private_directory(input.report.parent().ok_or_else(unavailable)?)?;
    let manager = ProjectManager::new(input.state)?;
    let directory = input.project.canonicalize()?;
    let project = manager
        .discover(&directory)
        .await?
        .ok_or_else(unavailable)?;
    let daemon = if let Some(caller) = &input.agentchat {
        if !caller.is_absolute() {
            return Err(unavailable().into());
        }
        let plan = ToolSetup::new(
            manager.home().into(),
            caller.clone(),
            Environment {
                user_config_root: Some(input.config.clone()),
                ..Default::default()
            },
            std::env::var_os("PATH"),
        )
        .codex_launch_plan()
        .await?;
        if !plan.is_managed() {
            return Err(unavailable().into());
        }
        let daemon = Daemon::inspect(plan.cli()).await?;
        if daemon.executable.canonicalize()? != input.cli.canonicalize()? {
            return Err("PATH-selected daemon differs from the exact fixture native image".into());
        }
        agentchat_hosts::require_native_profile(
            HostKind::Codex,
            &daemon.version,
            &format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        )?;
        metadata::write(
            &input.report.with_extension("inspection.json"),
            &json!({"caller":caller,"debug_assertions":cfg!(debug_assertions),
                "selected_cli":plan.cli(),"configuration":plan.configuration(),"skill":plan.skill(),
                "managed_dispatch":true,"native_executable":daemon.executable,
                "native_version":daemon.version,"normal_global_discovery":false,
                "installation_performed":false,"admitted":false}),
        )?;
        daemon
    } else {
        Daemon::inspect(&input.cli).await?
    };
    let events = Report(input.report.clone());
    let operations = Operations::new(manager.clone(), Arc::new(Existing));
    let code = operations
        .supervise_codex(
            &Route {
                project,
                directory,
                context: None,
                user: false,
            },
            &daemon,
            &BTreeMap::from([
                ("AGENTCHAT_HOME".into(), manager.home().as_os_str().into()),
                (
                    "AGENTCHAT_USER_CONFIG_ROOT".into(),
                    input.config.into_os_string(),
                ),
            ]),
            CodexLaunchRequest {
                resume: input.resume,
                name: input.name,
                terminal: NativeTerminal::Inherited,
            },
            &events,
        )
        .await?;
    if code != 0 || !input.report.exists() {
        return Err(format!("Codex startup/exit did not complete: {code}").into());
    }
    let route: serde_json::Value = serde_json::from_slice(&std::fs::read(&input.report)?)?;
    let root = route["root"].as_str().ok_or_else(unavailable)?;
    if NativeRegistry::new(manager.home()).lookup(root)?.is_none() {
        return Err(unavailable().into());
    }
    Ok(())
}
fn unavailable() -> Error {
    Error::new(
        ErrorCode::SessionRequired,
        "Outside the explicitly initialized Codex startup fixture.",
    )
}
