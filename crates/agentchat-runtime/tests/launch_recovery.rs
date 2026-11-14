//! Real local RPC/core recovery with explicitly synthetic native evidence.
mod support;
use agentchat_core::{
    Error, ErrorCode, Result,
    application::Mutation,
    model::{Context, LaunchIdentity, LaunchOwnerState, Phase},
    ports::{HostGate, HostState},
    read::Query,
};
use agentchat_protocol::rpc::{Call, Client, success};
use agentchat_runtime::launch::{StartedLaunch, reattach_launch};
use async_trait::async_trait;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use support::runtime::{Running, mutate, query};
use uuid::Uuid;

struct Host {
    bindings: Mutex<HashMap<Uuid, LaunchIdentity>>,
    current: Mutex<HashMap<String, Uuid>>,
    owner: Mutex<LaunchOwnerState>,
    paused: AtomicBool,
    departed: AtomicBool,
}
impl Host {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            bindings: Mutex::default(),
            current: Mutex::default(),
            owner: Mutex::new(LaunchOwnerState::Current),
            paused: AtomicBool::new(false),
            departed: AtomicBool::new(false),
        })
    }
    fn bind(&self, context: &Context, identity: &LaunchIdentity) {
        self.bindings
            .lock()
            .unwrap()
            .insert(context.context_id, identity.clone());
        self.current
            .lock()
            .unwrap()
            .insert(context.host_session_id.clone(), context.context_id);
    }
}
#[async_trait]
impl HostGate for Host {
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
    async fn verify(&self, context: &Context) -> Result<HostState> {
        if self.current.lock().unwrap().get(&context.host_session_id) != Some(&context.context_id) {
            return Err(Error::new(
                ErrorCode::SessionRequired,
                "Synthetic native context has retired.",
            ));
        }
        let paused = self.paused.load(Ordering::SeqCst);
        Ok(HostState {
            phase: if paused { Phase::Paused } else { Phase::Idle },
            active_turn_id: None,
            wake_paused: paused,
        })
    }
    async fn launch_identity(&self, context: &Context) -> Result<Option<LaunchIdentity>> {
        Ok(self
            .bindings
            .lock()
            .unwrap()
            .get(&context.context_id)
            .cloned())
    }
    fn launch_owner(&self, _: &LaunchIdentity) -> LaunchOwnerState {
        *self.owner.lock().unwrap()
    }
    fn launch_root_changed(
        &self,
        _: Uuid,
        _: agentchat_core::model::HostKind,
        _: &str,
        _: &LaunchIdentity,
    ) -> bool {
        self.departed.load(Ordering::SeqCst)
    }
}
async fn start(running: &Running, host: &Host, root: &str, name: &str) -> (Client, StartedLaunch) {
    let context = running.context(root).await;
    let identity = LaunchIdentity {
        generation: Uuid::new_v4(),
        owner: format!("synthetic-only-{}", Uuid::new_v4()),
    };
    host.bind(&context, &identity);
    let mut client = running.client().await;
    let result = success(
        client
            .call(
                Uuid::new_v4(),
                Call::StartLaunch {
                    context_id: context.context_id,
                    name: Some(name.into()),
                },
            )
            .await
            .unwrap(),
    )
    .unwrap();
    let started = StartedLaunch {
        context,
        identity,
        agent_id: serde_json::from_value(result["agent_id"].clone()).unwrap(),
        boundary: serde_json::from_value(result["join_boundary"].clone()).unwrap(),
    };
    (client, started)
}
async fn fixture() -> (Running, Arc<Host>, Client, StartedLaunch) {
    let host = Host::new();
    let running = Running::with_host(support::runtime::options(), None, host.clone()).await;
    let (client, started) = start(&running, &host, "receiver", "receiver").await;
    (running, host, client, started)
}
async fn replacement(running: &Running, host: &Host, original: &StartedLaunch) -> Context {
    let context = running.context(&original.context.host_session_id).await;
    host.bind(&context, &original.identity);
    context
}
#[tokio::test]
async fn replacement_attaches_same_generation_boundary_and_work_without_releasing_pause() {
    let (running, host, mut old, original) = fixture().await;
    let (mut sender, sender_launch) = start(&running, &host, "sender", "sender").await;
    mutate(
        &mut sender,
        &sender_launch.context,
        Mutation::Post {
            channel: Some("dev".into()),
            body: "@receiver retained pending input".into(),
        },
    )
    .await;
    let before = support::runtime::whoami(&mut old).await;
    success(
        old.call(
            Uuid::new_v4(),
            Call::Detach {
                context_id: original.context.context_id,
            },
        )
        .await
        .unwrap(),
    )
    .unwrap();
    let context = replacement(&running, &host, &original).await;
    host.paused.store(true, Ordering::SeqCst);
    let mut client = running.client().await;
    let restored = reattach_launch(&mut client, &context, &original)
        .await
        .unwrap();
    assert_eq!(restored.identity, original.identity);
    assert_eq!(restored.agent_id, original.agent_id);
    assert_eq!(restored.boundary, original.boundary);
    assert_eq!(restored.context, context);
    let after = support::runtime::whoami(&mut client).await;
    assert_eq!(before["head"], after["head"]);
    assert_eq!(before["native_delivery"], after["native_delivery"]);
    assert_eq!(before["statistics"], after["statistics"]);
    let who = query(&mut client, None, Query::Whoami).await;
    assert_eq!(who["actor"]["connected"], true);
    assert_eq!(who["actor"]["presence"], "paused");
    assert_eq!(
        old.call(
            Uuid::new_v4(),
            Call::Heartbeat {
                context_id: original.context.context_id
            }
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::SessionRequired
    );
    running.stop().await;
}
#[tokio::test]
async fn explicit_leave_prevents_repeated_recovery_without_a_new_join_event() {
    let (running, host, mut old, original) = fixture().await;
    mutate(&mut old, &original.context, Mutation::Leave).await;
    let before = support::runtime::whoami(&mut old).await;
    for _ in 0..2 {
        let context = replacement(&running, &host, &original).await;
        let mut client = running.client().await;
        assert_eq!(
            reattach_launch(&mut client, &context, &original)
                .await
                .err()
                .unwrap()
                .code,
            ErrorCode::NotJoined
        );
        let after = support::runtime::health(&mut client).await;
        assert_eq!(after["head"], before["head"]);
        assert_eq!(after["statistics"]["joined"], 0);
        assert_eq!(
            client
                .query(Uuid::new_v4(), None, Query::Whoami)
                .await
                .unwrap_err()
                .code,
            ErrorCode::SessionRequired
        );
    }
    running.stop().await;
}
#[tokio::test]
async fn positive_root_departure_stays_terminal_when_the_original_root_returns() {
    let (running, host, mut old, original) = fixture().await;
    host.departed.store(true, Ordering::SeqCst);
    success(
        old.call(
            Uuid::new_v4(),
            Call::ReconcileLaunch {
                context_id: original.context.context_id,
                generation: original.identity.generation,
            },
        )
        .await
        .unwrap(),
    )
    .unwrap();
    let context = replacement(&running, &host, &original).await;
    let mut client = running.client().await;
    assert_eq!(
        reattach_launch(&mut client, &context, &original)
            .await
            .err()
            .unwrap()
            .code,
        ErrorCode::NotJoined
    );
    assert_eq!(
        support::runtime::health(&mut client).await["statistics"]["joined"],
        0
    );
    assert_eq!(
        client
            .query(Uuid::new_v4(), None, Query::Whoami)
            .await
            .unwrap_err()
            .code,
        ErrorCode::SessionRequired
    );
    running.stop().await;
}
#[tokio::test]
async fn unknown_owner_suspends_recovery_without_retiring_the_launch() {
    let (running, host, mut old, original) = fixture().await;
    let context = replacement(&running, &host, &original).await;
    let mut client = running.client().await;
    *host.owner.lock().unwrap() = LaunchOwnerState::Unknown;
    assert_eq!(
        reattach_launch(&mut client, &context, &original)
            .await
            .err()
            .unwrap()
            .code,
        ErrorCode::SessionRequired
    );
    assert_eq!(
        support::runtime::whoami(&mut old).await["statistics"]["joined"],
        1
    );
    *host.owner.lock().unwrap() = LaunchOwnerState::Current;
    let mut client = running.client().await;
    assert_eq!(
        reattach_launch(&mut client, &context, &original)
            .await
            .unwrap()
            .identity,
        original.identity
    );
    running.stop().await;
}
#[tokio::test]
async fn changed_scope_or_a_different_generation_cannot_borrow_the_original_launch() {
    let (running, host, mut old, original) = fixture().await;
    let before = support::runtime::whoami(&mut old).await;
    for change in ["root", "project", "version", "os", "cwd", "host"] {
        let mut context = original.context.clone();
        context.context_id = Uuid::new_v4();
        context.incarnation = Uuid::new_v4();
        match change {
            "root" => context.host_session_id = Uuid::new_v4().to_string(),
            "project" => context.project_id = Uuid::new_v4(),
            "version" => context.host_version = "different".into(),
            "os" => context.os_profile = "different".into(),
            "cwd" => context.working_directory = running.f.root.path().into(),
            "host" => context.host = agentchat_core::model::HostKind::Claude,
            _ => unreachable!(),
        }
        let mut client = running.client().await;
        assert_eq!(
            reattach_launch(&mut client, &context, &original)
                .await
                .err()
                .unwrap()
                .code,
            ErrorCode::SessionRequired,
            "{change}"
        );
    }
    let context = running.context(&original.context.host_session_id).await;
    host.bind(
        &context,
        &LaunchIdentity {
            generation: Uuid::new_v4(),
            owner: original.identity.owner.clone(),
        },
    );
    let mut client = running.client().await;
    assert_eq!(
        reattach_launch(&mut client, &context, &original)
            .await
            .err()
            .unwrap()
            .code,
        ErrorCode::SessionRequired
    );
    assert_eq!(
        support::runtime::whoami(&mut old).await["head"],
        before["head"]
    );
    running.stop().await;
}
