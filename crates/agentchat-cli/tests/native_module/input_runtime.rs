//! Real runtime/store/native port with explicitly synthetic launch admission.
use agentchat_core::{
    Result,
    application::{Application, Mutation},
    model::{Context, HostKind, LaunchIdentity, LaunchOwnerState, Phase},
    ports::{HostGate, HostState},
    time::SystemClock,
};
use agentchat_hosts::claude::adapter::NativeAdapter;
use agentchat_runtime::{
    context::{ContextRegistry, ContextSeed},
    project::ProjectManager,
};
use serde::Deserialize;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use uuid::Uuid;
struct Host {
    native: Arc<NativeAdapter>,
    bindings: Mutex<HashMap<Uuid, LaunchIdentity>>,
    launches: agentchat_hosts::launch::Registry,
}
impl Host {
    fn bind(&self, c: &Context) {
        self.bindings.lock().unwrap().insert(
            c.context_id,
            LaunchIdentity {
                generation: Uuid::new_v4(),
                owner: "explicit-sdk-test-only-not-a-native-launch".into(),
            },
        );
    }
}
#[async_trait::async_trait]
impl HostGate for Host {
    fn input_capabilities(
        &self,
        _context: &agentchat_core::model::Context,
    ) -> agentchat_core::Result<agentchat_core::delivery::InputCapabilities> {
        use agentchat_core::delivery::{ActiveInput, InputCapabilities, InputRecovery};
        Ok(InputCapabilities::complete(
            InputRecovery::OriginalReceipt,
            ActiveInput::WithoutTurnId,
        ))
    }
    async fn verify(&self, c: &Context) -> Result<HostState> {
        if c.host == HostKind::Claude {
            self.native.observe(c).await
        } else {
            Ok(HostState {
                phase: Phase::Idle,
                active_turn_id: None,
                wake_paused: false,
            })
        }
    }
    async fn launch_identity(&self, c: &Context) -> Result<Option<LaunchIdentity>> {
        Ok(self.bindings.lock().unwrap().get(&c.context_id).cloned())
    }
    fn launch_owner(&self, identity: &LaunchIdentity) -> LaunchOwnerState {
        if identity.owner != "explicit-sdk-test-only-not-a-native-launch" {
            return agentchat_hosts::launch::LaunchOwners::observe(identity);
        }
        if self
            .bindings
            .lock()
            .unwrap()
            .values()
            .any(|v| v == identity)
        {
            LaunchOwnerState::Current
        } else {
            LaunchOwnerState::Unknown
        }
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
}
async fn with_dispatch_seq(app: &Application, mut result: serde_json::Value) -> serde_json::Value {
    let message_id = serde_json::from_value(result["message_id"].clone()).unwrap();
    let detail = app
        .query(
            None,
            agentchat_core::read::Query::Message {
                message_id,
                receipts: agentchat_core::read::PageRequest::default(),
            },
        )
        .await
        .unwrap();
    let agentchat_core::read::QueryResult::Message(detail) = detail else {
        panic!("Message fixture result required")
    };
    result["dispatch_seq"] = serde_json::to_value(detail.message.message.creation_seq).unwrap();
    let audit: agentchat_core::sequence::Sequence =
        serde_json::from_value(result["seq"].clone()).unwrap();
    assert!(detail.message.message.creation_seq < audit);
    result
}

#[derive(Deserialize)]
struct Request {
    sequence: u64,
    context: Context,
    owner: agentchat_hosts::claude::process::ProcessIdentity,
    generation: Uuid,
}
pub async fn serve(
    manager: ProjectManager,
    workspace: PathBuf,
    expected_version: String,
    mut stop: tokio::sync::oneshot::Receiver<()>,
    started: tokio::sync::oneshot::Sender<()>,
) {
    let project = manager.discover(&workspace).await.unwrap().unwrap();
    let native = Arc::new(NativeAdapter::new(manager.home().into()));
    let host = Arc::new(Host {
        native: native.clone(),
        bindings: Mutex::default(),
        launches: agentchat_hosts::launch::Registry::new(manager.home()),
    });
    let server = agentchat_runtime::daemon::Server::bind(
        manager.clone(),
        project.clone(),
        host.clone(),
        Arc::new(SystemClock),
        agentchat_runtime::daemon::RuntimeOptions::default(),
    )
    .await
    .unwrap()
    .with_native_messages(native);
    let runtime = server.handle();
    let task = tokio::spawn(server.run());
    let mut ready = agentchat_protocol::rpc::Client::connect(
        runtime.paths().socket.clone(),
        project.identity().id,
    )
    .await
    .unwrap();
    started.send(()).unwrap();
    let work = async {
        let app = Application::new(
            manager.open(&project).await.unwrap(),
            Arc::new(SystemClock),
            host.clone(),
        );
        let registry = ContextRegistry::new(manager.clone());
        let sender = registry
            .issue(ContextSeed {
                host: HostKind::Codex,
                host_session_id: "synthetic-input-sender".into(),
                host_version: "test-only".into(),
                os_profile: "not-admitted".into(),
                working_directory: workspace,
            })
            .await
            .unwrap();
        host.bind(&sender);
        app.start_launch(&sender, Some("sender")).await.unwrap();
        let path = manager.home().join("sdk-fixture-request.json");
        let input = loop {
            if let Ok(bytes) = std::fs::read(&path)
                && let Ok(input) = serde_json::from_slice::<Request>(&bytes)
            {
                break input;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        assert_eq!(input.sequence, 1);
        let context = registry
            .resolve(input.context.context_id, Some(project.identity().id))
            .await
            .unwrap();
        assert_eq!(context, input.context);
        assert_eq!(context.host_version, expected_version);
        // SDK/native identity is simulated, but the launched Node child and its
        // Rust supervisor are actual kernel processes. This exercises the real
        // publication and direct-native-parent root-departure proof boundary.
        assert_eq!(
            agentchat_hosts::claude::process::ProcessIdentity::capture(input.owner.pid()).unwrap(),
            input.owner
        );
        let mut identity =
            agentchat_hosts::launch::LaunchOwners::capture(input.owner.pid(), std::process::id())
                .unwrap();
        // The explicit SDK fixture selects its generation before its first
        // callback, as NativeChild::spawn_with_startup_gate does in production.
        identity.generation = input.generation;
        let binding = agentchat_hosts::launch::Binding::new(
            context.clone(),
            identity.clone(),
            agentchat_hosts::launch::Endpoint::Claude,
        )
        .unwrap();
        host.launches.publish(&binding).unwrap();
        host.bindings
            .lock()
            .unwrap()
            .insert(context.context_id, identity.clone());
        // Start through the real persistent RPC connection so the coordinator
        // owns an attached recipient, rather than fabricating only its DB row.
        let started = agentchat_protocol::rpc::success(
            ready
                .call(
                    Uuid::new_v4(),
                    agentchat_protocol::rpc::Call::StartLaunch {
                        context_id: context.context_id,
                        name: Some("recipient".into()),
                    },
                )
                .await
                .unwrap(),
        )
        .unwrap();
        let boundary = serde_json::from_value(
            started
                .get("join_boundary")
                .cloned()
                .expect("launch result must include its committed join boundary"),
        )
        .unwrap();
        // Match the real supervisor ordering: the immutable first-input marker
        // is derived from the committed launch result, never readiness alone.
        host.launches.mark_started(&binding, boundary).unwrap();
        host.launches
            .publish_ready(&binding, "/tmp/synthetic-input-control.sock".into(), None)
            .unwrap();
        // This input fixture explicitly joined above. The exact committed
        // marker releases its native composer independently of later membership.
        let addressed = format!(
            "@recipient {}\r\n末尾 \t\n",
            "\0".repeat(65_536 - "@recipient \r\n末尾 \t\n".len())
        );
        assert_eq!(addressed.len(), 65_536);
        let mut results = vec![];
        for body in [
            addressed,
            "passive second original body\r\n末尾 \t\n".into(),
            "@recipient third original body".into(),
        ] {
            let posted = app
                .execute(
                    Uuid::new_v4(),
                    &sender,
                    Mutation::Post {
                        channel: Some("dev".into()),
                        body,
                    },
                )
                .await
                .unwrap()
                .result;
            results.push(with_dispatch_seq(&app, posted).await);
        }
        agentchat_hosts::claude::metadata::write(
            &manager.home().join("sdk-fixture-reply.json"),
            &serde_json::json!({"sequence":1,"posted":results,"launch_generation":identity.generation,"runtime_socket":runtime.paths().socket,"project_id":project.identity().id}),
        )
        .unwrap();
        // The warmed receiver must also admit a non-message head through its
        // native-only load/claim RPC before the next passive body may proceed.
        let request = manager.home().join("sdk-fixture-native-notice.json");
        while !request.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let notice = app
            .execute(
                Uuid::new_v4(),
                &sender,
                Mutation::Reserve {
                    patterns: vec!["sdk-native-notice-only.rs".into()],
                    ttl_seconds: Some(600),
                    reason: Some("synthetic native input regression".into()),
                },
            )
            .await
            .unwrap();
        let successor = app
            .execute(
                Uuid::new_v4(),
                &sender,
                Mutation::Post {
                    channel: Some("dev".into()),
                    body: "passive body after exact lifecycle notice".into(),
                },
            )
            .await
            .unwrap();
        let successor = with_dispatch_seq(&app, successor.result).await;
        agentchat_hosts::claude::metadata::write(
            &manager.home().join("sdk-fixture-native-notice-ready.json"),
            &serde_json::json!({"notice":notice.result,"successor":successor}),
        )
        .unwrap();
        std::future::pending::<()>().await;
    };
    tokio::select! {_ = work=>{},_ = &mut stop=>{}}
    runtime.shutdown();
    task.await.unwrap().unwrap();
}
