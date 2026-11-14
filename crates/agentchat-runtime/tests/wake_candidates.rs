//! Actual RPC scheduling prunes only empty work and retains fresh host controls.
#[path = "../src/daemon/delivery.rs"]
#[allow(dead_code)]
mod dispatch;
mod support;
use agentchat_core::{
    Error, ErrorCode, Result,
    application::Mutation,
    delivery::{DeliveryPort, HostReceipt, WakeDispatch},
    model::{Context, Phase},
    ports::{HostGate, HostState, Storage, Transaction},
};
use agentchat_runtime::diagnostics::{self, RuntimeLog};
use async_trait::async_trait;
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
    time::Duration,
};
use support::{
    SyntheticHost,
    runtime::{Running, attach, join, mutate, options},
};
use tokio::sync::mpsc;
use uuid::Uuid;

#[derive(Default)]
struct Gate {
    calls: Mutex<HashMap<Uuid, usize>>,
    blocked: Mutex<HashSet<Uuid>>,
    changed: tokio::sync::Notify,
}
#[async_trait]
impl HostGate for Gate {
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
    async fn verify(&self, context: &Context) -> Result<HostState> {
        *self
            .calls
            .lock()
            .unwrap()
            .entry(context.context_id)
            .or_default() += 1;
        self.changed.notify_one();
        let mut state = SyntheticHost.verify(context).await?;
        if self.blocked.lock().unwrap().contains(&context.context_id) {
            state.phase = Phase::ApprovalBlocked;
        }
        Ok(state)
    }
}
struct Native {
    gate: Arc<Gate>,
    send: mpsc::Sender<WakeDispatch>,
}
#[async_trait]
impl DeliveryPort for Native {
    async fn present(&self, context: &Context, dispatch: &WakeDispatch) -> Result<HostReceipt> {
        assert_eq!(self.gate.verify(context).await?.phase, Phase::Idle);
        self.send.send(dispatch.clone()).await.unwrap();
        Ok(HostReceipt::default())
    }
}
fn post(body: &str) -> Mutation {
    Mutation::Post {
        channel: Some("dev".into()),
        body: body.into(),
    }
}

async fn drain_jobs(dispatcher: &mut dispatch::Dispatches) {
    // Native receipt observation precedes the job's final outcome send. Wait
    // for all owned jobs before dropping Application and closing its real DB.
    tokio::time::timeout(Duration::from_secs(3), async {
        while dispatcher.status().jobs != 0 {
            let outcome = dispatcher.receiver.recv().await.unwrap();
            let (_, failure, completed_job) = dispatcher.take_ready(Some(outcome), 8);
            assert!(completed_job);
            assert!(failure.is_none());
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn empty_work_skips_host_preparation_but_targeted_work_still_obeys_current_approval_state() {
    let gate = Arc::new(Gate::default());
    let (send, mut receive) = mpsc::channel(8);
    let r = Running::with_host(
        agentchat_runtime::daemon::RuntimeOptions {
            housekeeping: Duration::from_secs(10),
            ..options()
        },
        Some(Arc::new(Native {
            gate: gate.clone(),
            send,
        })),
        gate.clone(),
    )
    .await;
    let a = r.context("sender").await;
    let b = r.context("recipient").await;
    let mut sender = r.client().await;
    let mut recipient = r.client().await;
    join(&mut sender, &a, "alice").await;
    join(&mut recipient, &b, "bob").await;
    attach(&mut sender, &a).await;
    attach(&mut recipient, &b).await;
    let before = support::runtime::health(&mut sender).await;
    let calls = gate.calls.lock().unwrap()[&b.context_id];
    mutate(&mut sender, &a, post("passive body")).await;
    let passive = support::runtime::health(&mut sender).await;
    assert_eq!(gate.calls.lock().unwrap()[&b.context_id], calls);
    assert!(
        passive["runtime"]["native_delivery"]["candidate_pruned"]
            .as_u64()
            .unwrap()
            > before["runtime"]["native_delivery"]["candidate_pruned"]
                .as_u64()
                .unwrap()
    );
    gate.blocked.lock().unwrap().insert(b.context_id);
    mutate(&mut sender, &a, post("@bob preserve approval controls")).await;
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let changed = gate.changed.notified();
            if gate.calls.lock().unwrap()[&b.context_id] > calls {
                break;
            }
            changed.await;
        }
    })
    .await
    .unwrap();
    let blocked = support::runtime::health(&mut sender).await;
    assert!(gate.calls.lock().unwrap()[&b.context_id] > calls);
    assert!(receive.try_recv().is_err());
    assert_eq!(
        blocked["runtime"]["native_delivery"]["candidate_fallbacks"],
        0
    );
    assert_eq!(blocked["statistics"]["read_receipts"], 0);
    gate.blocked.lock().unwrap().remove(&b.context_id);
    agentchat_protocol::rpc::success(
        recipient
            .call(
                Uuid::new_v4(),
                agentchat_protocol::rpc::Call::Heartbeat {
                    context_id: b.context_id,
                },
            )
            .await
            .unwrap(),
    )
    .unwrap();
    let claim = tokio::time::timeout(Duration::from_secs(3), receive.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claim.hint.context_id, b.context_id);
    assert_eq!(claim.hint.events.len(), 1);
    drop((sender, recipient));
    r.stop().await;
}

#[tokio::test]
async fn an_unconfigured_native_port_never_runs_candidate_reads() {
    let r = Running::new(options()).await;
    let a = r.context("sender").await;
    let mut client = r.client().await;
    join(&mut client, &a, "alice").await;
    attach(&mut client, &a).await;
    mutate(&mut client, &a, post("retained without a native port")).await;
    let status = support::runtime::health(&mut client).await;
    assert_eq!(status["runtime"]["native_delivery"]["candidate_scans"], 0);
    assert_eq!(
        status["runtime"]["native_delivery"]["candidate_contexts"],
        0
    );
    drop(client);
    r.stop().await;
}

struct FailedHint(Arc<agentchat_store::Store>);
#[async_trait]
impl Storage for FailedHint {
    fn project_id(&self) -> Uuid {
        self.0.project_id()
    }
    async fn begin(&self) -> Result<Box<dyn Transaction>> {
        self.0.begin().await
    }
    async fn snapshot(&self) -> Result<Box<dyn agentchat_core::read::Snapshot>> {
        self.0.snapshot().await
    }
    async fn wake_candidates(
        &self,
        _: &[Context],
        _: agentchat_core::time::Timestamp,
    ) -> Result<Vec<Uuid>> {
        Err(Error::new(
            ErrorCode::RuntimeUnavailable,
            "Synthetic candidate read failure.",
        ))
    }
}

struct Counted {
    store: Arc<agentchat_store::Store>,
    begins: std::sync::atomic::AtomicUsize,
}
#[async_trait]
impl Storage for Counted {
    fn project_id(&self) -> Uuid {
        self.store.project_id()
    }
    async fn begin(&self) -> Result<Box<dyn Transaction>> {
        self.begins
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.store.begin().await
    }
    async fn snapshot(&self) -> Result<Box<dyn agentchat_core::read::Snapshot>> {
        self.store.snapshot().await
    }
    async fn wake_candidates(
        &self,
        contexts: &[Context],
        now: agentchat_core::time::Timestamp,
    ) -> Result<Vec<Uuid>> {
        self.store.wake_candidates(contexts, now).await
    }
}

#[tokio::test]
async fn deferred_sets_skip_host_and_writer_work_then_due_sets_still_get_fresh_claims() {
    use agentchat_core::application::Application;
    use agentchat_runtime::context::ContextRegistry;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use support::{
        Fixture,
        runtime::{ManualClock, seed},
    };
    let f = Fixture::new();
    let project = f
        .manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap()
        .project;
    let store = f.manager.open(&project).await.unwrap();
    let counted = Arc::new(Counted {
        store: store.clone(),
        begins: AtomicUsize::new(0),
    });
    let gate = Arc::new(Gate::default());
    let clock = Arc::new(ManualClock::default());
    let app = Arc::new(Application::new(
        counted.clone(),
        clock.clone(),
        gate.clone(),
    ));
    let registry = ContextRegistry::new(f.manager.clone());
    let mut contexts = Vec::new();
    for name in ["alice", "bob", "cara"] {
        let context = registry.issue(seed(&f, name)).await.unwrap();
        app.execute(
            Uuid::new_v4(),
            &context,
            Mutation::Join {
                name: Some(name.into()),
            },
        )
        .await
        .unwrap();
        contexts.push(context);
    }
    app.execute(
        Uuid::new_v4(),
        &contexts[0],
        post("@all defer only existing retry windows"),
    )
    .await
    .unwrap();
    for context in &contexts[1..] {
        app.prepare_wake(context).await.unwrap().unwrap();
    }
    let before = gate.calls.lock().unwrap().clone();
    let begins = counted.begins.load(Ordering::SeqCst);
    let (send, mut receive) = mpsc::channel(8);
    let diagnostics = RuntimeLog::open(project.data_dir(), contexts[0].project_id, Uuid::new_v4())
        .await
        .unwrap();
    let mut dispatcher = dispatch::Dispatches::new(
        Some(dispatch::DispatchPort::Hint(Arc::new(Native {
            gate: gate.clone(),
            send,
        }))),
        diagnostics.sink(),
    );
    let held = store.begin().await.unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        dispatcher.start(&app, contexts.clone()),
    )
    .await;
    held.rollback().await.unwrap();
    result
        .expect("deferred scan must not reserve the held writer")
        .unwrap();
    assert_eq!(*gate.calls.lock().unwrap(), before);
    assert_eq!(counted.begins.load(Ordering::SeqCst), begins);
    assert_eq!(dispatcher.status().candidate_pruned, 3);
    assert!(receive.try_recv().is_err());
    clock.advance(5);
    dispatcher.start(&app, contexts.clone()).await.unwrap();
    let received = tokio::time::timeout(Duration::from_secs(3), async {
        let mut ids = HashSet::new();
        for _ in 0..2 {
            let claim = receive.recv().await.unwrap();
            assert_eq!(claim.attempt, 2);
            ids.insert(claim.hint.context_id);
        }
        ids
    })
    .await
    .unwrap();
    assert_eq!(
        received,
        HashSet::from([contexts[1].context_id, contexts[2].context_id])
    );
    assert_eq!(counted.begins.load(Ordering::SeqCst), begins + 2);
    for context in &contexts[1..] {
        assert_eq!(
            gate.calls.lock().unwrap()[&context.context_id],
            before[&context.context_id] + 2
        );
    }
    drain_jobs(&mut dispatcher).await;
    drop(dispatcher);
    diagnostics.close().await.unwrap();
    drop(app);
    drop(counted);
    Arc::try_unwrap(store).ok().unwrap().close().await.unwrap();
}

#[tokio::test]
async fn failed_candidate_read_falls_back_to_every_recipient_without_losing_native_progress() {
    use agentchat_core::application::Application;
    use agentchat_runtime::context::ContextRegistry;
    use support::{
        Fixture,
        runtime::{ManualClock, seed},
    };

    let f = Fixture::new();
    let project = f
        .manager
        .initialize(&f.workspace, f.request())
        .await
        .unwrap()
        .project;
    let store = f.manager.open(&project).await.unwrap();
    let gate = Arc::new(Gate::default());
    let app = Arc::new(Application::new(
        Arc::new(FailedHint(store.clone())),
        Arc::new(ManualClock::default()),
        gate.clone(),
    ));
    let registry = ContextRegistry::new(f.manager.clone());
    let mut contexts = Vec::new();
    for name in ["alice", "bob", "cara"] {
        let context = registry.issue(seed(&f, name)).await.unwrap();
        app.execute(
            Uuid::new_v4(),
            &context,
            Mutation::Join {
                name: Some(name.into()),
            },
        )
        .await
        .unwrap();
        contexts.push(context);
    }
    app.execute(
        Uuid::new_v4(),
        &contexts[0],
        post("@all progress despite failed hint"),
    )
    .await
    .unwrap();
    let before = gate.calls.lock().unwrap().clone();
    let (send, mut receive) = mpsc::channel(8);
    let diagnostics = RuntimeLog::open(project.data_dir(), contexts[0].project_id, Uuid::new_v4())
        .await
        .unwrap();
    let mut dispatcher = dispatch::Dispatches::new(
        Some(dispatch::DispatchPort::Hint(Arc::new(Native {
            gate: gate.clone(),
            send,
        }))),
        diagnostics.sink(),
    );
    let mut inputs = contexts.clone();
    inputs.push(contexts[1].clone());
    assert_eq!(
        dispatcher.start(&app, inputs).await.unwrap_err().code,
        ErrorCode::RuntimeUnavailable
    );
    let received = tokio::time::timeout(Duration::from_secs(3), async {
        let mut ids = HashSet::new();
        for _ in 0..2 {
            ids.insert(receive.recv().await.unwrap().hint.context_id);
        }
        ids
    })
    .await
    .unwrap();
    assert_eq!(
        received,
        HashSet::from([contexts[1].context_id, contexts[2].context_id])
    );
    assert_eq!(
        gate.calls.lock().unwrap()[&contexts[0].context_id],
        before[&contexts[0].context_id] + 1
    );
    for context in &contexts[1..] {
        assert_eq!(
            gate.calls.lock().unwrap()[&context.context_id],
            before[&context.context_id] + 2
        );
    }
    let status = dispatcher.status();
    assert_eq!(status.candidate_fallbacks, 1);
    assert_eq!(status.candidate_contexts, 3);
    assert_eq!(status.candidate_pruned, 0);
    drain_jobs(&mut dispatcher).await;
    drop(dispatcher);
    diagnostics.close().await.unwrap();
    drop(app);
    Arc::try_unwrap(store).ok().unwrap().close().await.unwrap();
}
