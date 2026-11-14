//! Synthetic host lifecycle over the actual SDK and SeaORM initialization path.
//! This proves application/retirement composition, not real-host admission.
mod support;

use agentchat_core::{Error, ErrorCode, Result, project::Scope};
use agentchat_hosts::setup::{Environment, ToolSetup};
use agentchat_mcp::{
    BootstrapKey, CallEvidence, CallScope, CallVerifier, ConversationKey, Conversations,
};
use agentchat_protocol::{
    envelope::Envelope,
    framing::{self, FramedReader},
    operations::{Attachment, Backend, InitializationIdentity, Operation, Target},
};
use agentchat_runtime::{
    operations::{Launcher, Operations},
    project::Project,
};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use support::Fixture;
use tokio::{
    io::BufReader,
    sync::{Notify, Semaphore},
};
use uuid::Uuid;

struct NoLaunch;
#[async_trait]
impl Launcher for NoLaunch {
    async fn launch(&self, _: &Project, _: &Path) -> Result<()> {
        panic!("Bootstrap initialization must not launch a project runtime");
    }
}
struct Authority(Mutex<CallScope>);
#[async_trait]
impl CallVerifier for Authority {
    async fn verify(&self, _: CallEvidence<'_>) -> Result<ConversationKey> {
        Err(Error::new(
            ErrorCode::SessionRequired,
            "Synthetic bootstrap has no application context.",
        ))
    }
    async fn verify_scope(&self, _: CallEvidence<'_>) -> Result<CallScope> {
        Ok(*self.0.lock().unwrap())
    }
}
struct HeldResult {
    operations: Operations,
    first: AtomicBool,
    committed: Notify,
    release: Semaphore,
}
#[async_trait]
impl Backend for HeldResult {
    async fn invoke(&self, id: Uuid, target: Target, operation: Operation) -> Envelope<Value> {
        let init = matches!(operation, Operation::Initialize { .. });
        let result = self.operations.invoke(id, target, operation).await;
        if init && self.first.swap(false, Ordering::SeqCst) {
            self.committed.notify_one();
            self.release.acquire().await.unwrap().forget();
        }
        result
    }
    async fn attach(&self, target: Target) -> Result<Box<dyn Attachment>> {
        self.operations.attach(target).await
    }
}

#[tokio::test]
async fn committed_bootstrap_initialization_replays_after_retirement_without_a_context_or_participant()
 {
    let f = Fixture::new();
    let setup = ToolSetup::new(
        f.manager.home().into(),
        env!("CARGO_BIN_EXE_agentchat").into(),
        Environment {
            user_config_root: Some(f.config.clone()),
            ..Environment::default()
        },
        None,
    );
    let backend = Arc::new(HeldResult {
        operations: Operations::new(f.manager.clone(), Arc::new(NoLaunch)).with_host_tools(setup),
        first: AtomicBool::new(true),
        committed: Notify::new(),
        release: Semaphore::new(0),
    });
    let roots = Arc::new(Conversations::new(backend.clone()));
    let first = BootstrapKey {
        lifecycle_id: Uuid::new_v4(),
        generation: 1,
    };
    let native_identity = InitializationIdentity::new(
        agentchat_core::model::HostKind::Codex,
        "fixture-bootstrap-root".into(),
    )
    .unwrap();
    roots
        .activate_bootstrap(first, native_identity.clone())
        .await
        .unwrap();
    let authority = Arc::new(Authority(Mutex::new(CallScope::Bootstrap(first))));
    let server = agentchat_mcp::Server::gateway(roots.clone(), authority.clone());
    let (client, transport) = tokio::io::duplex(32768);
    let (read, write) = tokio::io::split(transport);
    let task = tokio::spawn(agentchat_mcp::serve(server, read, write));
    let (client_read, mut input) = tokio::io::split(client);
    let mut output = FramedReader::new(BufReader::new(client_read));
    framing::write(&mut input,&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"synthetic-bootstrap","version":"1"}}})).await.unwrap();
    let response: Value = output.read().await.unwrap().unwrap();
    assert_eq!(response["id"], 1);
    framing::write(
        &mut input,
        &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
    )
    .await
    .unwrap();
    let request = Uuid::new_v4();
    let args = json!({"project_path":f.workspace,"scope":"directory","request_id":request});
    framing::write(&mut input,&json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"agentchat_init","arguments":args}})).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), backend.committed.notified())
        .await
        .unwrap();
    let project = f.manager.discover(&f.workspace).await.unwrap().unwrap();
    let id = project.identity().id;
    assert_eq!(project.identity().scope, Scope::Directory);
    assert!(!f.manager.home().join("contexts").exists());
    let next = BootstrapKey {
        lifecycle_id: Uuid::new_v4(),
        generation: 2,
    };
    roots
        .activate_bootstrap(next, native_identity.clone())
        .await
        .unwrap();
    *authority.0.lock().unwrap() = CallScope::Bootstrap(next);
    let retired: Value = output.read().await.unwrap().unwrap();
    assert_eq!(retired["id"], 2);
    assert!(retired.get("error").is_some());
    framing::write(&mut input,&json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"agentchat_init","arguments":args}})).await.unwrap();
    let retried: Value = output.read().await.unwrap().unwrap();
    let envelope = &retried["result"]["structuredContent"];
    assert_eq!(envelope["request_id"], request.to_string());
    assert_eq!(envelope["project_id"], id.to_string());
    assert_eq!(envelope["result"]["created"], true);
    assert_eq!(envelope["result"]["integrations"]["state"], "not_installed");
    assert_eq!(
        envelope["result"]["capabilities"],
        agentchat_hosts::capabilities()
    );
    assert_eq!(
        f.manager
            .discover(&f.workspace)
            .await
            .unwrap()
            .unwrap()
            .identity()
            .id,
        id
    );
    // The native actor exists before the application descriptor. Promotion
    // must preserve retry ownership, rather than switching Local -> Conversation.
    let context = f.context("fixture-bootstrap-root").await;
    let promoted = ConversationKey {
        context_id: context.context_id,
        generation: 3,
    };
    roots.activate(promoted).await.unwrap();
    *authority.0.lock().unwrap() = CallScope::Conversation(promoted);
    framing::write(&mut input,&json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"agentchat_init","arguments":args}})).await.unwrap();
    let promoted_replay: Value = output.read().await.unwrap().unwrap();
    assert_eq!(promoted_replay["result"]["structuredContent"], *envelope);
    let different = InitializationIdentity::new(
        agentchat_core::model::HostKind::Codex,
        "another-native-root".into(),
    )
    .unwrap();
    let conflict = backend
        .operations
        .invoke(
            Uuid::new_v4(),
            Target {
                context_id: Some(context.context_id),
                project_path: Some(f.workspace.clone()),
                initialization_identity: Some(different.clone()),
                ..Target::default()
            },
            Operation::Initialize {
                auto_read: None,
                delivery_confirmation_timeout_seconds: None,
                scope: Some(Scope::Directory),
                default_channels: None,
                relocate: false,
                new_project: false,
            },
        )
        .await;
    let conflict = serde_json::to_value(conflict).unwrap();
    assert_eq!(conflict["error"]["code"], "identity_conflict");
    let fork = BootstrapKey {
        lifecycle_id: Uuid::new_v4(),
        generation: 4,
    };
    roots.activate_bootstrap(fork, different).await.unwrap();
    *authority.0.lock().unwrap() = CallScope::Bootstrap(fork);
    framing::write(&mut input,&json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"agentchat_init","arguments":args}})).await.unwrap();
    let foreign: Value = output.read().await.unwrap().unwrap();
    assert_eq!(
        foreign["result"]["structuredContent"]["error"]["code"],
        "idempotency_conflict"
    );
    let store = f.manager.open(&project).await.unwrap();
    let app = agentchat_core::application::Application::new(
        store,
        Arc::new(agentchat_core::time::SystemClock),
        Arc::new(agentchat_hosts::AdmittedHosts::new(
            f.manager.home().to_owned(),
        )),
    );
    let status = app
        .health()
        .await
        .map(|value| serde_json::to_value(value).unwrap())
        .unwrap();
    assert_eq!(status["statistics"]["agents"], 0);
    assert_eq!(status["statistics"]["messages"], 0);
    assert_eq!(status["head"], "2");
    assert_eq!(status["channel_policy"]["ordered_defaults"], json!(["dev"]));
    assert!(f.manager.home().join("contexts").exists());
    assert!(std::fs::read_dir(&f.config).unwrap().next().is_none());
    drop(input);
    drop(output);
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}
