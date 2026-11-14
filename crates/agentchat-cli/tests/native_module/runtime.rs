//! Explicit synthetic application authority for SDK-only guard simulations.
use agentchat_core::{
    Result,
    application::{Application, Mutation},
    delivery::HostReceipt,
    model::{Context, HostKind, Phase},
    ports::{HostGate, HostState},
    time::SystemClock,
};
use agentchat_runtime::{
    context::{ContextRegistry, ContextSeed},
    project::ProjectManager,
};
use serde::Deserialize;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use uuid::Uuid;
#[derive(Default)]
struct Host {
    turn: Mutex<Option<String>>,
    bindings: crate::support::launch_bindings::LaunchBindings,
}
#[async_trait::async_trait]
impl HostGate for Host {
    async fn launch_identity(
        &self,
        context: &Context,
    ) -> Result<Option<agentchat_core::model::LaunchIdentity>> {
        Ok(self.bindings.identity(context))
    }
    fn launch_owner(
        &self,
        identity: &agentchat_core::model::LaunchIdentity,
    ) -> agentchat_core::model::LaunchOwnerState {
        self.bindings.owner(identity)
    }
    fn input_capabilities(
        &self,
        _context: &agentchat_core::model::Context,
    ) -> agentchat_core::Result<agentchat_core::delivery::InputCapabilities> {
        use agentchat_core::delivery::{ActiveInput, InputCapabilities, InputRecovery};
        Ok(InputCapabilities::complete(
            InputRecovery::RetainUncertainty,
            ActiveInput::ExactTurnId,
        ))
    }
    fn allows_unmanaged_participation(&self) -> bool {
        true
    }
    async fn verify(&self, _: &Context) -> Result<HostState> {
        let turn = self.turn.lock().unwrap().clone();
        Ok(HostState {
            phase: if turn.is_some() {
                Phase::Active
            } else {
                Phase::Idle
            },
            active_turn_id: turn,
            wake_paused: false,
        })
    }
}
#[derive(Deserialize)]
struct Request {
    sequence: u64,
    context: Context,
    turn: Option<String>,
}
pub async fn serve(
    manager: ProjectManager,
    workspace: PathBuf,
    mut stop: tokio::sync::oneshot::Receiver<()>,
    started: tokio::sync::oneshot::Sender<()>,
) {
    let request = manager.home().join("sdk-fixture-request.json");
    let project = manager.discover(&workspace).await.unwrap().unwrap();
    let host = Arc::new(Host::default());
    // Final guards use this same explicit SDK-only authority. Letting their
    // lazy connection start a production-gated daemon would correctly reject
    // synthetic membership and would not test a coherent application boundary.
    let server = agentchat_runtime::daemon::Server::bind(
        manager.clone(),
        project.clone(),
        host.clone(),
        Arc::new(SystemClock),
        agentchat_runtime::daemon::RuntimeOptions::default(),
    )
    .await
    .unwrap();
    let runtime = server.handle();
    let runtime_task = tokio::spawn(server.run());
    let _ready = agentchat_protocol::rpc::Client::connect(
        runtime.paths().socket.clone(),
        project.identity().id,
    )
    .await
    .unwrap();
    started.send(()).unwrap();
    let serving = async {
        let app = Application::new(
            manager.open(&project).await.unwrap(),
            Arc::new(SystemClock),
            host.clone(),
        );
        let registry = ContextRegistry::new(manager.clone());
        let sender = registry
            .issue(ContextSeed {
                host: HostKind::Codex,
                host_session_id: "synthetic-sdk-sender".into(),
                host_version: "fixture-only".into(),
                os_profile: "synthetic-not-admitted".into(),
                working_directory: workspace,
            })
            .await
            .unwrap();
        host.bindings.bind(&sender);
        app.start_launch(&sender, Some("sender")).await.unwrap();
        let mut previous = 0;
        let mut pending: Vec<Uuid> = vec![];
        loop {
            let Some(input) = std::fs::read(&request)
                .ok()
                .and_then(|b| serde_json::from_slice::<Request>(&b).ok())
            else {
                tokio::time::sleep(Duration::from_millis(10)).await;
                continue;
            };
            if input.sequence <= previous {
                tokio::time::sleep(Duration::from_millis(10)).await;
                continue;
            }
            assert_eq!(input.sequence, previous + 1);
            assert!(input.sequence <= 16);
            let context = registry
                .resolve(input.context.context_id, Some(project.identity().id))
                .await
                .unwrap();
            assert_eq!(context, input.context);
            *host.turn.lock().unwrap() = input.turn;
            host.bindings.bind(&context);
            app.start_launch(&context, Some("recipient")).await.unwrap();
            app.heartbeat(&context).await.unwrap();
            if !pending.is_empty() {
                crate::support::accept_native(&app, &context, std::mem::take(&mut pending)).await;
            }
            app.execute(
                Uuid::new_v4(),
                &sender,
                Mutation::Post {
                    channel: Some("dev".into()),
                    body: "@recipient SDK simulation".into(),
                },
            )
            .await
            .unwrap();
            let dispatch = app.prepare_wake(&context).await.unwrap().unwrap();
            pending = dispatch.hint.events.iter().map(|e| e.delivery_id).collect();
            let turn_id = host.turn.lock().unwrap().clone();
            app.finish_wake(&context, &dispatch, Ok(HostReceipt { turn_id }))
                .await
                .unwrap();
            agentchat_hosts::claude::metadata::write(&manager.home().join("sdk-fixture-reply.json"),&serde_json::json!({"sequence":input.sequence,"hint":dispatch.hint,"token":dispatch.token})).unwrap();
            previous = input.sequence;
        }
    };
    tokio::select! { _ = serving => {}, _ = &mut stop => {} }
    runtime.shutdown();
    runtime_task.await.unwrap().unwrap();
}
