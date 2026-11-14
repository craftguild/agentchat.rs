//! Supervise the actual direct Codex child beside shared application work.
mod work;
use super::{Operations, Route};
use crate::launch::{CodexLaunchRequest, LaunchEvents, spawn_codex};
use agentchat_core::{Error, ErrorCode, Result, validation};
use agentchat_hosts::codex::{Client, Daemon, NativeRegistry, Proxy};
use std::{collections::BTreeMap, ffi::OsString};
impl Operations {
    /// Supervise one selected shared-daemon Codex CLI. Public composition must
    /// first require an intact selected live version and installed configuration;
    /// isolated acceptance uses its explicit HostGate. Every shared startup and
    /// restoration still independently verifies current original native scope.
    #[allow(clippy::too_many_arguments)]
    pub async fn supervise_codex(
        &self,
        route: &Route,
        daemon: &Daemon,
        environment: &BTreeMap<OsString, OsString>,
        request: CodexLaunchRequest,
        events: &dyn LaunchEvents,
    ) -> Result<i32> {
        let _activity = self.manager.activity(route.project.identity().id).await?;
        self.manager.validate(&route.project).await?;
        if route.context.is_some() || request.resume.is_some_and(|r| r.is_nil()) {
            return Err(required());
        }
        let name = request
            .name
            .as_deref()
            .map(validation::handle)
            .transpose()?;
        let mut environment = environment.clone();
        if environment
            .keys()
            .any(|key| key != "AGENTCHAT_HOME" && key != "AGENTCHAT_USER_CONFIG_ROOT")
            || environment
                .get(&OsString::from("AGENTCHAT_HOME"))
                .is_some_and(|home| home != self.manager.home().as_os_str())
        {
            return Err(required());
        }
        environment.insert(
            "AGENTCHAT_HOME".into(),
            self.manager.home().as_os_str().into(),
        );
        let directory = route.directory.canonicalize().map_err(|_| required())?;
        let signals = super::managed_supervision::signals()?;
        // Bring up the ordinary project RPC before native first input can race
        // participation. This creates no launch/agent and is not owner proof.
        drop(self.connect(route).await?);
        let mut native = Client::connect(&daemon.socket, &daemon.version).await?;
        let root = match request.resume {
            Some(root) => root.to_string(),
            None => native.create_thread(&directory).await?,
        };
        // The allocation client no longer needs its native subscription. The
        // real CLI resumes the materialized exact ID directly on that server.
        drop(native);
        NativeRegistry::new(self.manager.home()).check_available(&root)?;
        events.codex_selected(&root)?;
        let proxy = Proxy::bind(self.manager.home(), &daemon.socket, &root)?;
        let child = spawn_codex(
            &self.manager,
            &route.project,
            &directory,
            daemon,
            &root,
            proxy.path(),
            &environment,
            request.terminal,
        )
        .await?;
        let proof = child.proof();
        let proxy = proxy.serve(proof.clone());
        events.spawned(&child)?;
        let startup = Box::pin(work::start(
            self,
            route,
            proof,
            daemon.clone(),
            proxy,
            (root, directory),
            name,
        ));
        self.supervise_ready(route, child, startup, events, signals)
            .await
    }
}
fn required() -> Error {
    Error::new(
        ErrorCode::SessionRequired,
        "The exact managed Codex launch is unavailable or changed.",
    )
}
