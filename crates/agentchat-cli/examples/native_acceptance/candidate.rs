//! Select the actual adapter only for one explicitly initialized test project.
use agentchat_core::{
    Error, ErrorCode, Result,
    delivery::{
        DeliveryPort, HostReceipt, MessageDispatch, MessagePort, MessageProgress, NoticeDispatch,
        WakeDispatch,
    },
    model::{Context, HostKind, LaunchIdentity, LaunchOwnerState},
    ports::{HostGate, HostState},
    time::SystemClock,
};
use agentchat_hosts::claude::adapter::NativeAdapter;
use agentchat_runtime::{
    daemon::{RuntimeOptions, Server},
    project::ProjectManager,
};
use async_trait::async_trait;
use std::{path::PathBuf, sync::Arc};
use uuid::Uuid;

struct Candidate {
    native: NativeAdapter,
    project: Uuid,
    directory: PathBuf,
    version: String,
    launches: agentchat_hosts::launch::Registry,
    hold: Option<super::acceptance_hold::AcceptanceHold>,
}

impl Candidate {
    fn selected(&self, context: &Context) -> Result<()> {
        // The empty production allowlist stays untouched. A test candidate
        // still needs all native lifecycle, freshness and final-guard proofs.
        if context.host != HostKind::Claude
            || context.host_version != self.version
            || context.os_profile != format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
            || context.project_id != self.project
            || context.working_directory != self.directory
        {
            return Err(Error::new(
                ErrorCode::UnsupportedHost,
                "Outside the explicit isolated native candidate profile.",
            ));
        }
        Ok(())
    }
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
    async fn launch_identity(&self, c: &Context) -> Result<Option<LaunchIdentity>> {
        self.selected(c)?;
        self.launches.resolve(c).map(|b| b.map(|b| b.identity))
    }
    fn launch_owner(&self, id: &LaunchIdentity) -> LaunchOwnerState {
        agentchat_hosts::launch::LaunchOwners::observe(id)
    }
    fn launch_root_changed(
        &self,
        project: Uuid,
        host: HostKind,
        root: &str,
        id: &LaunchIdentity,
    ) -> bool {
        self.launches.root_changed(project, host, root, id)
    }
    async fn verify(&self, context: &Context) -> Result<HostState> {
        self.selected(context)?;
        self.native.observe(context).await
    }
}
#[async_trait]
impl MessagePort for Candidate {
    async fn present_notice(
        &self,
        c: &Context,
        dispatch: &NoticeDispatch,
    ) -> Result<MessageProgress> {
        self.selected(c)?;
        self.native.present_notice(c, dispatch).await
    }
    async fn present_message(
        &self,
        c: &Context,
        dispatch: &MessageDispatch,
    ) -> Result<MessageProgress> {
        self.selected(c)?;
        let progress = self.native.present_message(c, dispatch).await?;
        if let Some(hold) = &self.hold {
            hold.observe(c, dispatch, &progress)?;
        }
        Ok(progress)
    }
}

#[async_trait]
impl DeliveryPort for Candidate {
    async fn present(&self, context: &Context, dispatch: &WakeDispatch) -> Result<HostReceipt> {
        self.selected(context)?;
        self.native.present(context, dispatch).await
    }
}

pub(super) async fn serve(
    home: PathBuf,
    directory: PathBuf,
    version: String,
    hold_accepted: Option<PathBuf>,
) -> Result<()> {
    if !agentchat_hosts::claude::module::candidate_version(&version) {
        return Err(Error::new(
            ErrorCode::UnsupportedHost,
            "Select an exact supported candidate version.",
        ));
    }
    let manager = ProjectManager::new(home)?;
    let hold = hold_accepted
        .map(|path| super::acceptance_hold::AcceptanceHold::new(path, manager.home()))
        .transpose()?;
    let directory = directory.canonicalize().map_err(|_| {
        Error::new(
            ErrorCode::InvalidArgument,
            "Candidate project is unavailable.",
        )
    })?;
    let project = manager.discover(&directory).await?.ok_or_else(|| {
        Error::new(
            ErrorCode::NotInitialized,
            "Initialize the disposable project through public agentchat first.",
        )
    })?;
    let candidate = Arc::new(Candidate {
        launches: agentchat_hosts::launch::Registry::new(manager.home()),
        native: NativeAdapter::new(manager.home().into()),
        project: project.identity().id,
        directory,
        version: version.clone(),
        hold,
    });
    let server = Server::bind(
        manager,
        project,
        candidate.clone(),
        Arc::new(SystemClock),
        RuntimeOptions::default(),
    )
    .await?;
    let server = if agentchat_hosts::claude::module::has_startup_gate_source(&version) {
        server.with_native_messages(candidate)
    } else {
        server.with_delivery(candidate)
    };
    let handle = server.handle();
    println!(
        "{}",
        serde_json::json!({
            "candidate_ready": true, "instance": handle.instance_id(), "socket": handle.paths().socket,
            "admitted": false, "actual_native_adapter": true
            , "ordered_native_notices": agentchat_hosts::claude::module::has_startup_gate_source(&version)
        })
    );
    let serving = server.run();
    tokio::pin!(serving);
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|_| {
        Error::new(
            ErrorCode::IoError,
            "Candidate termination signal is unavailable.",
        )
    })?;
    tokio::select! {
        result = &mut serving => result,
        _ = terminate.recv() => { handle.shutdown(); serving.await },
        _ = tokio::signal::ctrl_c() => { handle.shutdown(); serving.await }
    }
}
