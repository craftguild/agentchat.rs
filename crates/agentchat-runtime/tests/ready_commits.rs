//! Actual RPCs and native callbacks obey one ready-group bound and FIFO fences.
mod support;

use agentchat_core::{
    Error, ErrorCode, Result,
    application::{MAX_COMMIT_GROUP, Mutation},
    delivery::{BatchState, DeliveryPort, HostReceipt, WakeDispatch},
    model::Context,
    ports::{HostGate, HostState, Storage},
};
use agentchat_protocol::rpc::{Call, success};
use async_trait::async_trait;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use support::{
    SyntheticHost,
    runtime::{Running, attach, join, mutate, options},
};
use tokio::sync::{Notify, mpsc, oneshot};
use uuid::Uuid;

#[derive(Default)]
struct Gate {
    target: Mutex<Option<Uuid>>,
    entered: Notify,
    release: Notify,
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
        let block = {
            let mut target = self.target.lock().unwrap();
            if *target == Some(context.context_id) {
                target.take();
                true
            } else {
                false
            }
        };
        if block {
            self.entered.notify_one();
            self.release.notified().await;
        }
        SyntheticHost.verify(context).await
    }
}
struct Native {
    dispatch: WakeDispatch,
    reply: oneshot::Sender<Result<HostReceipt>>,
}
struct Controlled {
    send: mpsc::Sender<Native>,
    finished: AtomicUsize,
    changed: Notify,
}
#[async_trait]
impl DeliveryPort for Controlled {
    async fn present(&self, _: &Context, dispatch: &WakeDispatch) -> Result<HostReceipt> {
        let (reply, receive) = oneshot::channel();
        self.send
            .send(Native {
                dispatch: dispatch.clone(),
                reply,
            })
            .await
            .map_err(|_| Error::new(ErrorCode::RuntimeUnavailable, "Synthetic receiver closed."))?;
        let result = receive
            .await
            .map_err(|_| Error::new(ErrorCode::IoError, "Synthetic reply lost."))?;
        self.finished.fetch_add(1, Ordering::SeqCst);
        self.changed.notify_one();
        result
    }
}

async fn exercise(with_query_fence: bool) {
    let (send, mut receiver) = mpsc::channel(32);
    let port = Arc::new(Controlled {
        send,
        finished: AtomicUsize::new(0),
        changed: Notify::new(),
    });
    let gate = Arc::new(Gate::default());
    let r = Running::with_host(
        agentchat_runtime::daemon::RuntimeOptions {
            housekeeping: Duration::from_secs(10),
            ..options()
        },
        Some(port.clone()),
        gate.clone(),
    )
    .await;
    let a = r.context("sender").await;
    let b = r.context("recipient").await;
    let mut sender = r.client().await;
    let mut recipient = r.client().await;
    join(&mut sender, &a, "alice").await;
    join(&mut recipient, &b, "bob").await;
    attach(&mut recipient, &b).await;
    mutate(
        &mut sender,
        &a,
        Mutation::Post {
            channel: Some("dev".into()),
            body: "@bob original native claim".into(),
        },
    )
    .await;
    let native = tokio::time::timeout(Duration::from_secs(5), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    let mut clients = Vec::new();
    let count = if with_query_fence { 6 } else { 12 };
    for _ in 0..count {
        clients.push(r.client().await);
    }
    let mut observer = Some(r.client().await);
    let before = support::runtime::health(&mut sender).await;
    *gate.target.lock().unwrap() = Some(a.context_id);
    let context = a.clone();
    let heartbeat = tokio::spawn(async move {
        sender
            .call(
                Uuid::new_v4(),
                Call::Heartbeat {
                    context_id: context.context_id,
                },
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
        .await
        .unwrap();
    let mut tasks = Vec::new();
    let mut fence = None;
    for (n, mut client) in clients.into_iter().enumerate() {
        if with_query_fence && n == 3 {
            // All request handlers remain behind the explicit host barrier.
            // Require the actual query snapshot to exclude every later post.
            tokio::time::sleep(Duration::from_millis(100)).await;
            let mut observer = observer.take().unwrap();
            fence = Some(tokio::spawn(async move {
                support::runtime::health(&mut observer).await
            }));
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let context = a.clone();
        tasks.push(tokio::spawn(async move {
            mutate(
                &mut client,
                &context,
                Mutation::Post {
                    channel: Some("dev".into()),
                    body: format!("passive queued {n}"),
                },
            )
            .await
        }));
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    let batch_id = native.dispatch.hint.batch_id;
    let token = native.dispatch.token;
    native
        .reply
        .send(Ok(HostReceipt {
            turn_id: Some("accepted-turn".into()),
        }))
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while port.finished.load(Ordering::SeqCst) < 1 {
            port.changed.notified().await;
        }
    })
    .await
    .unwrap();
    // The current-thread native task enqueues its outcome before this waiter
    // resumes. The coordinator is still held at the explicit host barrier.
    gate.release.notify_one();
    success(heartbeat.await.unwrap().unwrap()).unwrap();
    for task in tasks {
        assert!(task.await.unwrap().get("message_id").is_some());
    }
    if let Some(fence) = fence {
        assert_eq!(fence.await.unwrap()["statistics"]["messages"], 4);
    }
    let status = support::runtime::health(&mut recipient).await;
    let metrics = &status["runtime"]["mutation_groups"];
    assert!(
        metrics["mixed_commit_groups"].as_u64().unwrap()
            > before["runtime"]["mutation_groups"]["mixed_commit_groups"]
                .as_u64()
                .unwrap()
    );
    assert!(metrics["largest_commit_group"].as_u64().unwrap() <= MAX_COMMIT_GROUP as u64);
    assert_eq!(metrics["max_commit_group"], MAX_COMMIT_GROUP);
    assert_eq!(status["statistics"]["messages"], count + 1);
    assert_eq!(status["statistics"]["read_receipts"], 0);
    let store = r.f.manager.open(&r.project).await.unwrap();
    let mut tx = store.begin().await.unwrap();
    let batch = tx.wake_batch(batch_id).await.unwrap().unwrap();
    assert_eq!(batch.state, BatchState::Inflight);
    assert_eq!(batch.dispatch_ref, Some(token));
    assert_eq!(batch.attempts, 1);
    assert!(
        !tx.wake_items(batch.recipient_id, Some(batch.id))
            .await
            .unwrap()
            .is_empty()
    );
    tx.rollback().await.unwrap();
    assert!(receiver.try_recv().is_err());
    r.stop().await;
}

#[tokio::test(flavor = "current_thread")]
async fn native_and_rpc_work_form_an_actual_mixed_group_without_implicit_ack_or_read() {
    exercise(false).await;
}
#[tokio::test(flavor = "current_thread")]
async fn an_already_queued_query_is_a_fence_for_native_selected_collection() {
    exercise(true).await;
}
