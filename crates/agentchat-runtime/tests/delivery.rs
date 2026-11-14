//! Real runtime/SQLite with bounded controllable native presentation futures.
mod support;
use agentchat_core::{
    Error, ErrorCode, Result,
    application::Mutation,
    delivery::{BatchState, DeliveryPort, HostReceipt, SUBMISSION_SECONDS, WakeDispatch},
    model::Context,
    ports::Storage,
    read::Query,
};
use agentchat_protocol::rpc::{Call, Client, success};
use agentchat_store::Store;
use async_trait::async_trait;
use std::{sync::Arc, time::Duration};
use support::runtime::{Running, attach, join, mutate, options, query};
use tokio::sync::{mpsc, oneshot};

struct Native {
    context: Context,
    dispatch: WakeDispatch,
    reply: oneshot::Sender<Result<HostReceipt>>,
}
struct Controlled(mpsc::Sender<Native>);
#[async_trait]
impl DeliveryPort for Controlled {
    async fn present(&self, context: &Context, dispatch: &WakeDispatch) -> Result<HostReceipt> {
        let (reply, receive) = oneshot::channel();
        self.0
            .send(Native {
                context: context.clone(),
                dispatch: dispatch.clone(),
                reply,
            })
            .await
            .map_err(|_| Error::new(ErrorCode::RuntimeUnavailable, "Synthetic receiver closed."))?;
        receive
            .await
            .map_err(|_| Error::new(ErrorCode::IoError, "Synthetic transport failed."))?
    }
}
fn port() -> (Arc<Controlled>, mpsc::Receiver<Native>) {
    let (send, receive) = mpsc::channel(32);
    (Arc::new(Controlled(send)), receive)
}
async fn next(receiver: &mut mpsc::Receiver<Native>) -> Native {
    tokio::time::timeout(Duration::from_secs(2), receiver.recv())
        .await
        .unwrap()
        .unwrap()
}
async fn state(store: &Store, id: uuid::Uuid, expected: BatchState) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let batch = store
                .begin()
                .await
                .unwrap()
                .wake_batch(id)
                .await
                .unwrap()
                .unwrap();
            if batch.state == expected {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn slow_hint_io_does_not_hold_sqlite_or_rpc_and_full_acceptance_advances_mentions() {
    let (port, mut receiver) = port();
    let host = Arc::new(support::native_host::NativeHost::default());
    let r = Running::with_host(options(), Some(port), host.clone()).await;
    let a = r.context("sender").await;
    let b = r.context("recipient").await;
    host.bind(&a);
    host.bind(&b);
    let mut sender = r.client().await;
    let mut target = r.client().await;
    start(&mut sender, &a, "alice").await;
    start(&mut target, &b, "bob").await;
    attach(&mut sender, &a).await;
    attach(&mut target, &b).await;
    mutate(
        &mut sender,
        &a,
        Mutation::Post {
            channel: Some("dev".into()),
            body: "@bob first".into(),
        },
    )
    .await;
    let first = next(&mut receiver).await;
    assert_eq!(first.context.context_id, b.context_id);
    let stored = r.f.manager.open(&r.project).await.unwrap();
    assert_eq!(first.dispatch.attempt, 1);
    // Finish before the native timeout, then prove the callback is still held.
    // Real FULL I/O may exceed a separate one-second throughput assumption.
    tokio::time::timeout(Duration::from_secs(SUBMISSION_SECONDS - 1), async {
        mutate(
            &mut sender,
            &a,
            Mutation::Post {
                channel: Some("dev".into()),
                body: "@bob queued while native I/O waits".into(),
            },
        )
        .await;
        assert_eq!(
            support::runtime::health(&mut sender).await["statistics"]["messages"],
            2
        );
    })
    .await
    .unwrap();
    assert!(
        !first.reply.is_closed(),
        "native callback must remain pending"
    );
    assert!(receiver.try_recv().is_err());
    first
        .reply
        .send(Err(Error::new(
            ErrorCode::IoError,
            "Synthetic failed attempt.",
        )))
        .unwrap();
    state(&stored, first.dispatch.hint.batch_id, BatchState::Inflight).await;
    r.clock.advance(1);
    let second = next(&mut receiver).await;
    assert_eq!(second.dispatch.hint.events, first.dispatch.hint.events);
    assert_eq!(second.dispatch.attempt, 2);
    assert_ne!(second.dispatch.token, first.dispatch.token);
    second.reply.send(Ok(HostReceipt::default())).unwrap();
    let hinted = query(
        &mut sender,
        None,
        Query::Message {
            message_id: first.dispatch.hint.events[0].message_id,
            receipts: Default::default(),
        },
    )
    .await;
    assert!(
        hinted["receipts"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| {
                row["reads"].as_array().unwrap().is_empty()
                    && row["delivery"]["delivered_at"].is_null()
            }),
        "hint completion alone cannot record full-body acceptance or reading"
    );
    r.accept_native(
        &b,
        first
            .dispatch
            .hint
            .events
            .iter()
            .map(|e| e.delivery_id)
            .collect(),
    )
    .await;
    let third = next(&mut receiver).await;
    assert_eq!(third.dispatch.attempt, 1);
    assert_ne!(third.dispatch.hint.batch_id, first.dispatch.hint.batch_id);
    let detail = query(
        &mut sender,
        None,
        Query::Message {
            message_id: first.dispatch.hint.events[0].message_id,
            receipts: Default::default(),
        },
    )
    .await;
    assert!(detail["receipts"]["items"].as_array().unwrap().iter().all(
        |row| row["delivery"]["state"] == "delivered"
            && row["reads"].as_array().unwrap().len() == 1
            && row["reads"][0]["kind"] == "automatic"
    ));
    mutate(&mut target, &b, Mutation::Leave).await;
    state(&stored, third.dispatch.hint.batch_id, BatchState::Cancelled).await;
    tokio::task::yield_now().await;
    assert!(third.reply.send(Ok(HostReceipt::default())).is_err());
    assert!(receiver.try_recv().is_err());
    r.stop().await;
}

#[tokio::test]
async fn native_timeout_consumes_one_attempt_without_starving_observation() {
    let (port, mut receiver) = port();
    let r = Running::with_delivery(options(), Some(port)).await;
    let a = r.context("sender").await;
    let b = r.context("recipient").await;
    let mut sender = r.client().await;
    let mut target = r.client().await;
    join(&mut sender, &a, "alice").await;
    join(&mut target, &b, "bob").await;
    attach(&mut target, &b).await;
    mutate(
        &mut sender,
        &a,
        Mutation::Post {
            channel: Some("dev".into()),
            body: "@bob timeout".into(),
        },
    )
    .await;
    let first = next(&mut receiver).await;
    let stored = r.f.manager.open(&r.project).await.unwrap();
    assert_eq!(
        support::runtime::health(&mut sender).await["statistics"]["messages"],
        1
    );
    tokio::time::timeout(Duration::from_secs(7), async {
        loop {
            let batch = stored
                .begin()
                .await
                .unwrap()
                .wake_batch(first.dispatch.hint.batch_id)
                .await
                .unwrap()
                .unwrap();
            if batch.state == BatchState::Inflight {
                assert_eq!(batch.attempts, 1);
                assert_eq!(batch.failure_code.as_deref(), Some("runtime_unavailable"));
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(first.reply.send(Ok(HostReceipt::default())).is_err());
    r.clock.advance(1);
    let second = next(&mut receiver).await;
    assert_eq!(second.dispatch.hint.events, first.dispatch.hint.events);
    assert_eq!(second.dispatch.attempt, 2);
    second.reply.send(Ok(HostReceipt::default())).unwrap();
    r.stop().await;
}

#[tokio::test]
async fn restart_cancels_native_future_and_verified_restore_reuses_delivery_ids() {
    let (port, mut receiver) = port();
    let mut r = Running::with_delivery(options(), Some(port.clone())).await;
    let a = r.context("sender").await;
    let b = r.context("recipient").await;
    let mut sender = r.client().await;
    let mut target = r.client().await;
    join(&mut sender, &a, "alice").await;
    join(&mut target, &b, "bob").await;
    attach(&mut target, &b).await;
    mutate(
        &mut sender,
        &a,
        Mutation::Post {
            channel: Some("dev".into()),
            body: "@bob persisted claim".into(),
        },
    )
    .await;
    let first = next(&mut receiver).await;
    r.restart_with_delivery(options(), Some(port)).await;
    assert!(first.reply.send(Ok(HostReceipt::default())).is_err());
    let mut restored = r.client().await;
    assert_eq!(
        query(&mut restored, Some(b.context_id), Query::Whoami).await["actor"]["connected"],
        false
    );
    assert!(receiver.try_recv().is_err());
    let fresh = r.context("recipient").await;
    attach(&mut restored, &fresh).await;
    let second = next(&mut receiver).await;
    assert_eq!(second.dispatch.hint.events, first.dispatch.hint.events);
    assert_ne!(second.dispatch.hint.batch_id, first.dispatch.hint.batch_id);
    assert_eq!(second.dispatch.attempt, 1);
    assert_ne!(second.context.incarnation, first.context.incarnation);
    second.reply.send(Ok(HostReceipt::default())).unwrap();
    r.stop().await;
}

#[tokio::test]
async fn processing_timeout_notifies_only_author_and_late_acceptance_preserves_history() {
    let (port, mut receiver) = port();
    let host = Arc::new(support::native_host::NativeHost::default());
    let r = Running::with_host(options(), Some(port), host.clone()).await;
    support::runtime::set_confirmation_timeout(&r, 1).await;
    let a = r.context("read-author").await;
    let b = r.context("read-recipient").await;
    host.bind(&a);
    host.bind(&b);
    let mut sender = r.client().await;
    let mut target = r.client().await;
    start(&mut sender, &a, "alice").await;
    start(&mut target, &b, "bob").await;
    attach(&mut sender, &a).await;
    attach(&mut target, &b).await;
    let posted = mutate(
        &mut sender,
        &a,
        Mutation::Post {
            channel: Some("dev".into()),
            body: "@bob important task".into(),
        },
    )
    .await;
    let original = next(&mut receiver).await;
    assert_eq!(original.context.context_id, b.context_id);
    assert_eq!(original.dispatch.hint.events[0].kind, "message.created");
    let recipient_delivery = original.dispatch.hint.events[0].delivery_id;
    original.reply.send(Ok(HostReceipt::default())).unwrap();
    let app = r.application().await;
    let complete = app.prepare_native(&b).await.unwrap().unwrap();
    assert_eq!(complete.delivery_id(), recipient_delivery);
    let agentchat_core::delivery::NativeInput::Message(body) = complete.input() else {
        panic!("complete body")
    };
    assert_eq!(body.body, "@bob important task");
    r.clock.advance(2);
    let timeout = next(&mut receiver).await;
    assert_eq!(timeout.context.context_id, a.context_id);
    assert_eq!(timeout.dispatch.hint.events.len(), 1);
    assert_eq!(
        timeout.dispatch.hint.events[0].kind,
        "message.delivery_confirmation_timeout"
    );
    assert_eq!(
        timeout.dispatch.hint.events[0].message_id.to_string(),
        posted["message_id"].as_str().unwrap()
    );
    let recipient_inbox = query(
        &mut target,
        None,
        Query::Inbox {
            page: Default::default(),
        },
    )
    .await;
    assert!(
        recipient_inbox["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["event"]["kind"] != "message.delivery_confirmation_timeout"),
        "timeout notification is author-only, with no passive recipient delivery"
    );
    let notice_delivery = timeout.dispatch.hint.events[0].delivery_id;
    timeout.reply.send(Ok(HostReceipt::default())).unwrap();
    r.accept_native(&a, vec![notice_delivery]).await;
    let events = query(
        &mut sender,
        None,
        Query::Events {
            channel: None,
            page: Default::default(),
        },
    )
    .await;
    assert_eq!(
        events["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["kind"] == "message.delivery_confirmation_timeout")
            .count(),
        1
    );
    let message = uuid::Uuid::parse_str(posted["message_id"].as_str().unwrap()).unwrap();
    let detail = query(
        &mut sender,
        None,
        Query::Message {
            message_id: message,
            receipts: Default::default(),
        },
    )
    .await;
    let delivery = &detail["receipts"]["items"][0]["delivery"];
    assert_eq!(delivery["delivery_id"], recipient_delivery.to_string());
    assert!(
        delivery["delivered_at"].is_null(),
        "a wake hint is not full-body acceptance"
    );
    assert!(delivery.get("acknowledged_at").is_none());
    assert_eq!(delivery["state"], "failed_timeout");
    assert!(!delivery["timed_out_at"].is_null());
    assert!(
        app.finish_native(
            &b,
            &complete,
            Ok(agentchat_core::delivery::MessageProgress::Accepted(
                HostReceipt::default()
            ))
        )
        .await
        .unwrap()
    );
    let late = query(
        &mut sender,
        None,
        Query::Message {
            message_id: message,
            receipts: Default::default(),
        },
    )
    .await;
    let receipt = &late["receipts"]["items"][0];
    assert_eq!(receipt["delivery"]["state"], "delivered_late");
    assert_eq!(
        receipt["delivery"]["timed_out_at"],
        delivery["timed_out_at"]
    );
    assert!(!receipt["delivery"]["delivered_at"].is_null());
    assert_eq!(receipt["reads"][0]["kind"], "automatic");
    assert_eq!(
        receipt["reads"][0]["at"],
        receipt["delivery"]["delivered_at"]
    );
    assert!(receiver.try_recv().is_err());
    r.stop().await;
}

async fn start(client: &mut Client, context: &Context, name: &str) {
    success(
        client
            .call(
                uuid::Uuid::new_v4(),
                Call::StartLaunch {
                    context_id: context.context_id,
                    name: Some(name.into()),
                },
            )
            .await
            .unwrap(),
    )
    .unwrap();
}
