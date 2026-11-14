//! A blocked coordinator accumulates eight real native callbacks without a timer.
mod support;

use agentchat_core::{
    Error, ErrorCode, Result,
    application::Mutation,
    delivery::{BatchState, DeliveryPort, HostReceipt, MAX_WAKE_COMPLETION_GROUP, WakeDispatch},
    model::Context,
    ports::{HostGate, HostState, Storage},
    read::Query,
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
    runtime::{Running, attach, join, mutate, options, query},
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

#[tokio::test(flavor = "current_thread")]
async fn already_ready_callbacks_share_one_bounded_group_and_keep_every_recipient_independent() {
    let (send, mut receiver) = mpsc::channel(32);
    let port = Arc::new(Controlled {
        send,
        finished: AtomicUsize::new(0),
        changed: Notify::new(),
    });
    let gate = Arc::new(Gate::default());
    let r = Running::with_host(options(), Some(port.clone()), gate.clone()).await;
    let a = r.context("sender").await;
    let mut sender = r.client().await;
    join(&mut sender, &a, "sender").await;
    let mut recipients = Vec::new();
    for n in 0..MAX_WAKE_COMPLETION_GROUP {
        let context = r.context(&format!("recipient{n}")).await;
        let mut client = r.client().await;
        join(&mut client, &context, &format!("target{n}")).await;
        attach(&mut client, &context).await;
        recipients.push(client);
    }
    let posted = mutate(
        &mut sender,
        &a,
        Mutation::Post {
            channel: Some("dev".into()),
            body: "@all independent native callbacks".into(),
        },
    )
    .await;
    let mut native = Vec::new();
    for _ in 0..MAX_WAKE_COMPLETION_GROUP {
        native.push(
            tokio::time::timeout(Duration::from_secs(5), receiver.recv())
                .await
                .unwrap()
                .unwrap(),
        );
    }
    *gate.target.lock().unwrap() = Some(a.context_id);
    let mut control = r.client().await;
    let heartbeat = tokio::spawn(async move {
        control
            .call(
                Uuid::new_v4(),
                Call::Heartbeat {
                    context_id: a.context_id,
                },
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
        .await
        .unwrap();
    let batch_ids = native
        .iter()
        .map(|n| n.dispatch.hint.batch_id)
        .collect::<Vec<_>>();
    for (n, native) in native.into_iter().enumerate() {
        native
            .reply
            .send(Ok(HostReceipt {
                turn_id: Some(format!("turn{n}")),
            }))
            .unwrap();
    }
    tokio::time::timeout(Duration::from_secs(2), async {
        while port.finished.load(Ordering::SeqCst) < MAX_WAKE_COMPLETION_GROUP {
            port.changed.notified().await;
        }
    })
    .await
    .unwrap();
    // On this current-thread executor each ready port future runs through its
    // non-full completion send before this waiter resumes. The coordinator is
    // still at the explicit host barrier, so all eight outcomes are queued.
    gate.release.notify_one();
    success(heartbeat.await.unwrap().unwrap()).unwrap();
    let store = r.f.manager.open(&r.project).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let mut tx = store.begin().await.unwrap();
            let mut done = true;
            for id in &batch_ids {
                let batch = tx.wake_batch(*id).await.unwrap().unwrap();
                done &= batch.state == BatchState::Inflight;
                assert_eq!(batch.attempts, 1);
            }
            tx.rollback().await.unwrap();
            if done {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let status = support::runtime::health(&mut sender).await;
    let native = &status["runtime"]["native_delivery"];
    assert_eq!(
        native["largest_completion_group"],
        MAX_WAKE_COMPLETION_GROUP
    );
    assert_eq!(native["max_completion_group"], MAX_WAKE_COMPLETION_GROUP);
    assert_eq!(native["completion_groups"], 1);
    assert_eq!(status["statistics"]["read_receipts"], 0);
    let detail = query(
        &mut sender,
        None,
        Query::Message {
            message_id: Uuid::parse_str(posted["message_id"].as_str().unwrap()).unwrap(),
            receipts: Default::default(),
        },
    )
    .await;
    assert!(
        detail["receipts"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["reads"].as_array().unwrap().is_empty())
    );
    assert!(receiver.try_recv().is_err());
    drop(recipients);
    r.stop().await;
}
