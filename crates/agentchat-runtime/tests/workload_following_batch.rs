//! Reuse normal stalled settlement, then exercise the untested next-hint boundary.
#[path = "workload/actors.rs"]
#[allow(dead_code)]
mod actors;
#[path = "workload/capture.rs"]
#[allow(dead_code)]
mod capture;
#[path = "workload/progress.rs"]
#[allow(dead_code)]
mod drain_progress;
mod support;
use agentchat_core::{
    application::Mutation,
    delivery::InputCapabilities,
    delivery::{BatchState, WakeDispatch},
    model::{Context, LaunchIdentity, LaunchOwnerState},
    ports::{HostGate, HostState, Storage},
    read::Query,
};
use agentchat_protocol::rpc::{ActorReference, Call, Client, success};
use agentchat_runtime::{context::ContextRegistry, daemon::Server};
use capture::{Capture, Input, SyntheticPort};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use support::runtime::{options, query};
use tokio::sync::mpsc;
use uuid::Uuid;

// Controlled setup pause permits all20 Posts before ordinary scheduling.
// No dispatch, receipt or retry is fabricated by this host observation.
#[derive(Default)]
struct PausedHost {
    inner: support::native_host::NativeHost,
    ready: AtomicBool,
}
#[async_trait::async_trait]
impl HostGate for PausedHost {
    fn input_capabilities(&self, c: &Context) -> agentchat_core::Result<InputCapabilities> {
        self.inner.input_capabilities(c)
    }
    async fn launch_identity(&self, c: &Context) -> agentchat_core::Result<Option<LaunchIdentity>> {
        self.inner.launch_identity(c).await
    }
    fn launch_owner(&self, id: &LaunchIdentity) -> LaunchOwnerState {
        self.inner.launch_owner(id)
    }
    async fn verify(&self, c: &Context) -> agentchat_core::Result<HostState> {
        let mut state = self.inner.verify(c).await?;
        state.wake_paused = !self.ready.load(Ordering::SeqCst);
        Ok(state)
    }
}
async fn start(client: &mut Client, context: &Context, name: &str) -> serde_json::Value {
    success(
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
    .unwrap()
}

async fn next(receiver: &mut mpsc::Receiver<Input>) -> WakeDispatch {
    let Input::Native(dispatch) = tokio::time::timeout(Duration::from_secs(3), receiver.recv())
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("original native hint")
    };
    dispatch
}
async fn state(store: &agentchat_store::Store, id: Uuid, expected: BatchState) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let mut snapshot = store.snapshot().await.unwrap();
            let batch = snapshot.wake_batch(id).await.unwrap().unwrap();
            if batch.state == expected {
                return;
            }
            drop(snapshot);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn twenty_presented_ids_survive_stall_then_twenty_first_target_crosses_normal_fifo() {
    let f = support::Fixture::new();
    let mut request = f.request();
    request.receipt_settings.auto_read = Some(false);
    let project = f
        .manager
        .initialize(&f.workspace, request)
        .await
        .unwrap()
        .project;
    let registry = ContextRegistry::new(f.manager.clone());
    let a = registry
        .issue(support::runtime::seed(&f, "following-sender"))
        .await
        .unwrap();
    let b = registry
        .issue(support::runtime::seed(&f, "following-recipient"))
        .await
        .unwrap();
    let host = Arc::new(PausedHost::default());
    host.inner.bind(&a);
    host.inner.bind(&b);
    let capture = Arc::new(Capture::new());
    capture.start(Instant::now());
    let (route, mut receive) = mpsc::channel(4);
    let port = Arc::new(SyntheticPort {
        capture: capture.clone(),
        routes: HashMap::from([(b.context_id, route)]),
    });
    let clock = Arc::new(support::runtime::ManualClock::default());
    let server = Server::bind(
        f.manager.clone(),
        project.clone(),
        host.clone(),
        clock.clone(),
        options(),
    )
    .await
    .unwrap()
    .with_delivery(port)
    .with_commit_observer(capture.clone());
    let app = server.application();
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    let mut sender = Client::connect(&handle.paths().socket, project.identity().id)
        .await
        .unwrap();
    let mut recipient = Client::connect(&handle.paths().socket, project.identity().id)
        .await
        .unwrap();
    start(&mut sender, &a, "alice").await;
    let joined = start(&mut recipient, &b, "bob").await;
    let agent = serde_json::from_value(joined["agent_id"].clone()).unwrap();
    let progress = drain_progress::Progress::new();
    let receiver_progress = progress.register(
        agent,
        serde_json::from_value(joined["generation"].clone()).unwrap(),
    );
    let store = f.manager.open(&project).await.unwrap();
    let mut messages = Vec::new();
    for number in 0..20 {
        let request = Uuid::new_v4();
        capture.track(request);
        let posted = success(
            sender
                .call(
                    request,
                    Call::Mutate {
                        request_id: request,
                        actor: ActorReference::Native {
                            context_id: a.context_id,
                        },
                        command: Mutation::Post {
                            channel: Some("dev".into()),
                            body: format!("@bob workload message actor=0 number={number}"),
                        },
                    },
                )
                .await
                .unwrap(),
        )
        .unwrap();
        messages.push(serde_json::from_value::<Uuid>(posted["message_id"].clone()).unwrap());
    }
    assert!(receive.try_recv().is_err(), "setup host pause is respected");
    host.ready.store(true, Ordering::SeqCst);
    success(
        recipient
            .call(
                Uuid::new_v4(),
                Call::Heartbeat {
                    context_id: b.context_id,
                },
            )
            .await
            .unwrap(),
    )
    .unwrap();
    let first = next(&mut receive).await;
    assert_eq!(first.hint.events.len(), 20);
    let mut presented = first
        .hint
        .events
        .iter()
        .map(|r| r.delivery_id)
        .collect::<HashSet<_>>();
    let original_ids = presented.clone();
    state(&store, first.hint.batch_id, BatchState::Inflight).await;
    let mut last = first.clone();
    for (seconds, attempt) in [(1, 2), (5, 3)] {
        clock.advance(seconds);
        let retry = next(&mut receive).await;
        assert_eq!(retry.hint.batch_id, first.hint.batch_id);
        assert_eq!(retry.attempt, attempt);
        assert_ne!(retry.token, last.token);
        presented.extend(retry.hint.events.iter().map(|r| r.delivery_id));
        assert_eq!(presented, original_ids);
        state(&store, first.hint.batch_id, BatchState::Inflight).await;
        last = retry;
    }
    clock.advance(5);
    state(&store, first.hint.batch_id, BatchState::Stalled).await;
    let request = Uuid::new_v4();
    capture.track(request);
    let posted = success(
        sender
            .call(
                request,
                Call::Mutate {
                    request_id: request,
                    actor: ActorReference::Native {
                        context_id: a.context_id,
                    },
                    command: Mutation::Post {
                        channel: Some("dev".into()),
                        body: "@bob workload message actor=0 number=20".into(),
                    },
                },
            )
            .await
            .unwrap(),
    )
    .unwrap();
    let successor = serde_json::from_value::<Uuid>(posted["message_id"].clone()).unwrap();
    assert!(
        receive.try_recv().is_err(),
        "new batch cannot overtake the retained set"
    );
    let mut read = HashSet::new();
    for _ in 0..5 {
        actors::drain(
            &mut recipient,
            &b,
            agent,
            &mut read,
            &presented,
            &app,
            &receiver_progress,
        )
        .await;
        if read.len() == 20 {
            break;
        }
    }
    assert_eq!(read, messages.into_iter().collect());
    assert_eq!(presented, original_ids);
    // The ordinary full-input path has released the old batch. Even an already
    // admitted next hint cannot make an unseen targeted body pass our gate.
    state(&store, first.hint.batch_id, BatchState::Settled).await;
    actors::drain(
        &mut recipient,
        &b,
        agent,
        &mut read,
        &presented,
        &app,
        &receiver_progress,
    )
    .await;
    assert_eq!(read.len(), 20);
    let detail = query(
        &mut sender,
        None,
        Query::Message {
            message_id: successor,
            receipts: Default::default(),
        },
    )
    .await;
    assert!(
        detail["receipts"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["delivery"]["delivered_at"].is_null())
    );
    let following = next(&mut receive).await;
    assert_ne!(following.hint.batch_id, first.hint.batch_id);
    assert_ne!(following.token, last.token);
    assert_eq!(following.attempt, 1);
    assert_eq!(following.hint.events.len(), 1);
    assert_eq!(following.hint.events[0].message_id, successor);
    assert!(following.hint.events[0].seq > first.hint.events.last().unwrap().seq);
    presented.extend(following.hint.events.iter().map(|r| r.delivery_id));
    assert!(original_ids.is_subset(&presented));
    assert_eq!(presented.len(), 21);
    actors::drain(
        &mut recipient,
        &b,
        agent,
        &mut read,
        &presented,
        &app,
        &receiver_progress,
    )
    .await;
    assert!(read.contains(&successor));
    assert_eq!(read.len(), 21);
    assert_eq!(progress.snapshot()[&agent].read, 21);
    state(&store, following.hint.batch_id, BatchState::Settled).await;
    handle.shutdown();
    task.await.unwrap().unwrap();
}
