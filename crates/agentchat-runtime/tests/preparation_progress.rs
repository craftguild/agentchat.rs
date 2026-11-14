//! Pending host verification and SQLite claims cannot stall unrelated RPC reads.

mod support;

use agentchat_core::{
    Error, ErrorCode, Result,
    application::Mutation,
    delivery::{BatchState, DeliveryPort, HostReceipt, WakeDispatch},
    model::Context,
    ports::{HostGate, HostState, Storage},
};
use async_trait::async_trait;
use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
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
    cancelled: Notify,
    failed: Notify,
    rejected: Mutex<HashSet<Uuid>>,
}

struct Held<'a> {
    cancelled: &'a Notify,
    released: bool,
}
impl Drop for Held<'_> {
    fn drop(&mut self) {
        if !self.released {
            self.cancelled.notify_one();
        }
    }
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
        let hold = {
            let mut target = self.target.lock().unwrap();
            if *target == Some(context.context_id) {
                target.take();
                true
            } else {
                false
            }
        };
        if hold {
            let mut held = Held {
                cancelled: &self.cancelled,
                released: false,
            };
            self.entered.notify_one();
            self.release.notified().await;
            held.released = true;
        }
        if self.rejected.lock().unwrap().contains(&context.context_id) {
            self.failed.notify_one();
            return Err(Error::new(
                ErrorCode::UnsupportedHost,
                "Synthetic rejected root.",
            ));
        }
        SyntheticHost.verify(context).await
    }
}

async fn pending_recipient(
    gate: Arc<Gate>,
    send: mpsc::Sender<Native>,
) -> (Running, agentchat_protocol::rpc::Client, Context) {
    let r = Running::with_host(
        agentchat_runtime::daemon::RuntimeOptions {
            housekeeping: Duration::from_secs(10),
            ..options()
        },
        Some(Arc::new(Port(send))),
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
    *gate.target.lock().unwrap() = Some(b.context_id);
    mutate(
        &mut sender,
        &a,
        Mutation::Post {
            channel: Some("dev".into()),
            body: "@bob wait here".into(),
        },
    )
    .await;
    tokio::time::timeout(Duration::from_secs(3), gate.entered.notified())
        .await
        .unwrap();
    (r, recipient, b)
}

#[tokio::test]
async fn leave_cancels_host_preparation_without_native_presentation() {
    let gate = Arc::new(Gate::default());
    let (send, mut receive) = mpsc::channel(4);
    let (r, mut recipient, b) = pending_recipient(gate.clone(), send).await;
    tokio::time::timeout(
        Duration::from_secs(1),
        mutate(&mut recipient, &b, Mutation::Leave),
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(1), gate.cancelled.notified())
        .await
        .unwrap();
    gate.release.notify_one();
    r.stop().await;
    assert!(receive.try_recv().is_err());
}

#[tokio::test]
async fn shutdown_cancels_host_preparation_without_native_presentation() {
    let gate = Arc::new(Gate::default());
    let (send, mut receive) = mpsc::channel(4);
    let (r, _recipient, _) = pending_recipient(gate.clone(), send).await;
    tokio::time::timeout(Duration::from_secs(1), r.stop())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), gate.cancelled.notified())
        .await
        .unwrap();
    gate.release.notify_one();
    assert!(receive.try_recv().is_err());
}

#[tokio::test]
async fn a_held_then_rejected_root_does_not_hold_an_independent_recipient() {
    let gate = Arc::new(Gate::default());
    let (send, mut receive) = mpsc::channel(4);
    let (r, mut recipient, b) = pending_recipient(gate.clone(), send).await;
    let c = r.context("independent").await;
    let mut independent = r.client().await;
    join(&mut independent, &c, "cara").await;
    attach(&mut independent, &c).await;
    mutate(
        &mut independent,
        &c,
        Mutation::Post {
            channel: Some("dev".into()),
            body: "@bob @cara independent work".into(),
        },
    )
    .await;
    // A self-mention does not wake its author; let the held recipient's socket
    // post to cara while its separate preparation verification is pending.
    mutate(
        &mut recipient,
        &b,
        Mutation::Post {
            channel: Some("dev".into()),
            body: "@cara work independently".into(),
        },
    )
    .await;
    let native = tokio::time::timeout(Duration::from_secs(1), receive.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(native.dispatch.hint.context_id, c.context_id);
    gate.rejected.lock().unwrap().insert(b.context_id);
    gate.release.notify_one();
    tokio::time::timeout(Duration::from_secs(1), gate.failed.notified())
        .await
        .unwrap();
    let store = r.f.manager.open(&r.project).await.unwrap();
    let mut tx = store.begin().await.unwrap();
    let bob = tx
        .conversation(b.host, &b.host_session_id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        tx.outstanding_wake(bob.id, b.incarnation)
            .await
            .unwrap()
            .is_none()
    );
    tx.rollback().await.unwrap();
    native.finish.send(()).unwrap();
    r.stop().await;
}

struct Native {
    dispatch: WakeDispatch,
    finish: oneshot::Sender<()>,
}
struct Port(mpsc::Sender<Native>);
#[async_trait]
impl DeliveryPort for Port {
    async fn present(&self, _: &Context, dispatch: &WakeDispatch) -> Result<HostReceipt> {
        let (finish, wait) = oneshot::channel();
        self.0
            .send(Native {
                dispatch: dispatch.clone(),
                finish,
            })
            .await
            .unwrap();
        let _ = wait.await;
        Ok(HostReceipt::default())
    }
}

#[tokio::test]
async fn host_verification_and_reserved_writer_leave_read_rpc_available_before_native_io() {
    let gate = Arc::new(Gate::default());
    let (send, mut receive) = mpsc::channel(4);
    let r = Running::with_host(
        agentchat_runtime::daemon::RuntimeOptions {
            housekeeping: Duration::from_secs(10),
            ..options()
        },
        Some(Arc::new(Port(send))),
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
    *gate.target.lock().unwrap() = Some(b.context_id);
    mutate(
        &mut sender,
        &a,
        Mutation::Post {
            channel: Some("dev".into()),
            body: "@bob claim must commit first".into(),
        },
    )
    .await;
    tokio::time::timeout(Duration::from_secs(3), gate.entered.notified())
        .await
        .unwrap();
    let mut reader = r.client().await;
    let during_host = tokio::time::timeout(
        Duration::from_secs(1),
        support::runtime::health(&mut reader),
    )
    .await;
    let store = r.f.manager.open(&r.project).await.unwrap();
    let writer = store.begin().await.unwrap();
    gate.release.notify_one();
    let mut second_reader = r.client().await;
    let during_claim = tokio::time::timeout(
        Duration::from_secs(1),
        support::runtime::health(&mut second_reader),
    )
    .await;
    assert!(
        receive.try_recv().is_err(),
        "native I/O escaped while no claim could commit"
    );
    writer.rollback().await.unwrap();
    let native = tokio::time::timeout(Duration::from_secs(3), receive.recv())
        .await
        .unwrap()
        .unwrap();
    let mut read_claim = store.begin().await.unwrap();
    let batch = read_claim
        .wake_batch(native.dispatch.hint.batch_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(batch.state, BatchState::Pending);
    assert_eq!(batch.dispatch_ref, Some(native.dispatch.token));
    assert_eq!(batch.attempts, 1);
    read_claim.rollback().await.unwrap();
    native.finish.send(()).unwrap();
    r.stop().await;
    // Always release both explicit barriers and finish owned native I/O before
    // asserting latency, so the old-failing implementation still cleans up.
    let observed = [during_host.is_ok(), during_claim.is_ok()];
    assert_eq!(
        observed,
        [true, true],
        "unrelated reads waited for preparation: {observed:?}"
    );
    for status in [during_host.ok(), during_claim.ok()].into_iter().flatten() {
        assert_eq!(status["statistics"]["messages"], 1);
        assert_eq!(status["statistics"]["read_receipts"], 0);
    }
}
