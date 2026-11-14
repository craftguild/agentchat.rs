//! Actual RPC/SeaORM scheduling with synthetic Codex controls and acceptance.
#[path = "codex_message_fifo/pipeline.rs"]
mod pipeline;
mod support;
#[path = "codex_message_fifo/uncertainty.rs"]
mod uncertainty;
use agentchat_core::{
    Error, ErrorCode, Result,
    application::Mutation,
    delivery::{HostReceipt, MessageDispatch, MessagePort, MessageProgress},
    model::{Context, HostKind, LaunchIdentity, LaunchOwnerState, Phase},
    ports::{HostGate, HostState},
    read::Query,
};
use agentchat_protocol::rpc::{Call, Client, success};
use async_trait::async_trait;
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use support::runtime::{Running, mutate, query};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

#[derive(Default)]
struct Host(Mutex<HashMap<Uuid, LaunchIdentity>>);
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
    async fn launch_identity(&self, context: &Context) -> Result<Option<LaunchIdentity>> {
        Ok(self.0.lock().unwrap().get(&context.context_id).cloned())
    }
    fn launch_owner(&self, identity: &LaunchIdentity) -> LaunchOwnerState {
        if self
            .0
            .lock()
            .unwrap()
            .values()
            .any(|value| value == identity)
        {
            LaunchOwnerState::Current
        } else {
            LaunchOwnerState::Unknown
        }
    }
    async fn verify(&self, context: &Context) -> Result<HostState> {
        assert_eq!(context.host, HostKind::Codex);
        assert_eq!(context.host_version, "fixture-only");
        Ok(HostState {
            phase: Phase::Active,
            active_turn_id: None,
            wake_paused: false,
        })
    }
}
struct Pending {
    dispatch: MessageDispatch,
    result: oneshot::Sender<Result<MessageProgress>>,
}
struct Port(mpsc::UnboundedSender<Pending>);
#[async_trait]
impl MessagePort for Port {
    async fn present_notice(
        &self,
        context: &Context,
        dispatch: &agentchat_core::delivery::NoticeDispatch,
    ) -> Result<MessageProgress> {
        // This explicit synthetic consumer receives the complete typed notice
        // before confirming it; notices retain their own place in Core's FIFO.
        let text = dispatch.input.text()?;
        assert_eq!(
            agentchat_core::delivery::NoticeInput::from_text(&text)?,
            dispatch.input
        );
        assert_eq!(context.project_id, dispatch.input.project_id);
        assert!(!dispatch.awaiting_acceptance);
        Ok(MessageProgress::Accepted(HostReceipt::default()))
    }
    async fn present_message(
        &self,
        _: &Context,
        dispatch: &MessageDispatch,
    ) -> Result<MessageProgress> {
        let (result, receiver) = oneshot::channel();
        self.0
            .send(Pending {
                dispatch: dispatch.clone(),
                result,
            })
            .unwrap();
        receiver.await.unwrap()
    }
}
async fn start(running: &Running, host: &Host, name: &str) -> (Client, Context, Value) {
    let context = running.context(name).await;
    host.0.lock().unwrap().insert(
        context.context_id,
        LaunchIdentity {
            generation: Uuid::new_v4(),
            owner: format!("explicit-synthetic-owner-{name}"),
        },
    );
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
    (client, context, result)
}
async fn fixture() -> (Running, Arc<Host>, mpsc::UnboundedReceiver<Pending>) {
    let host = Arc::new(Host::default());
    let (sender, receiver) = mpsc::unbounded_channel();
    let running = Running::with_native_host(
        support::runtime::options(),
        Arc::new(Port(sender)),
        host.clone(),
    )
    .await;
    (running, host, receiver)
}
async fn post(client: &mut Client, context: &Context, body: &str) -> Value {
    mutate(
        client,
        context,
        Mutation::Post {
            channel: Some("dev".into()),
            body: body.into(),
        },
    )
    .await
}
async fn receive(receiver: &mut mpsc::UnboundedReceiver<Pending>) -> Pending {
    tokio::time::timeout(Duration::from_secs(3), receiver.recv())
        .await
        .unwrap()
        .unwrap()
}
fn accept(pending: Pending) {
    assert!(!pending.dispatch.awaiting_acceptance);
    pending
        .result
        .send(Ok(MessageProgress::Accepted(HostReceipt {
            turn_id: pending
                .dispatch
                .input
                .addressed
                .then(|| "native-accepted-turn".into()),
        })))
        .unwrap();
}
async fn cursor(client: &mut Client, expected: &Value) -> Value {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let status = query(client, None, Query::Whoami).await;
            if status["native_delivery"]["settled_seq"] == *expected {
                return status;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn addressed_and_passive_bodies_share_one_head_with_automatic_receipts_without_explicit_read()
{
    let (running, host, mut receiver) = fixture().await;
    let (mut sender, sender_context, _) = start(&running, &host, "sender").await;
    let (mut recipient, _, joined) = start(&running, &host, "receiver").await;
    let first = format!("@receiver {}", "x".repeat(65_526));
    assert_eq!(first.len(), 65_536);
    let bodies = [
        first,
        "`@receiver` remains passive\n\0🦀".into(),
        "@receiver third".into(),
    ];
    let mut posts = Vec::new();
    for body in &bodies {
        posts.push(post(&mut sender, &sender_context, body).await);
    }
    let pending = receive(&mut receiver).await;
    assert_eq!(pending.dispatch.input.body, bodies[0]);
    assert!(pending.dispatch.input.addressed);
    let boundary = cursor(&mut recipient, &joined["join_boundary"]).await;
    assert!(
        boundary["statistics"]
            .get("acknowledged_deliveries")
            .is_none()
    );
    assert_eq!(boundary["statistics"]["read_receipts"], 0);
    assert!(
        tokio::time::timeout(Duration::from_millis(80), receiver.recv())
            .await
            .is_err()
    );
    let mut previous = pending.dispatch.input.seq;
    accept(pending);
    for index in 1..3 {
        let pending = receive(&mut receiver).await;
        assert_eq!(pending.dispatch.input.body, bodies[index]);
        assert_eq!(
            pending.dispatch.input.message_id.to_string(),
            posts[index]["message_id"].as_str().unwrap()
        );
        assert_eq!(pending.dispatch.input.addressed, index == 2);
        assert!(pending.dispatch.input.seq > previous);
        let status = cursor(&mut recipient, &serde_json::to_value(previous).unwrap()).await;
        assert!(
            status["statistics"]
                .get("acknowledged_deliveries")
                .is_none()
        );
        assert_eq!(status["statistics"]["read_receipts"], index);
        previous = pending.dispatch.input.seq;
        accept(pending);
    }
    let status = cursor(&mut recipient, &serde_json::to_value(previous).unwrap()).await;
    assert!(status["native_delivery"]["head"].is_null());
    assert!(
        status["statistics"]
            .get("acknowledged_deliveries")
            .is_none()
    );
    assert_eq!(status["statistics"]["read_receipts"], 3);
    for posted in posts {
        support::runtime::assert_automatic_receipts(
            &mut recipient,
            Uuid::parse_str(posted["message_id"].as_str().unwrap()).unwrap(),
            1,
        )
        .await;
    }
    running.stop().await;
}

#[tokio::test]
async fn rejected_addressed_head_keeps_passive_successor_blocked_until_same_message_retry() {
    let (running, host, mut receiver) = fixture().await;
    let (mut sender, sender_context, _) = start(&running, &host, "sender").await;
    let (mut recipient, _, _) = start(&running, &host, "receiver").await;
    post(&mut sender, &sender_context, "@receiver first").await;
    post(&mut sender, &sender_context, "passive successor").await;
    let first = receive(&mut receiver).await;
    let original = first.dispatch.clone();
    first
        .result
        .send(Ok(MessageProgress::NotAccepted(Error::new(
            ErrorCode::Busy,
            "Synthetic native refusal.",
        ))))
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let status = support::runtime::whoami(&mut recipient).await;
            let head = &status["native_delivery"]["head"];
            if head["failure_code"] == "busy" {
                assert_eq!(
                    head["seq"],
                    serde_json::to_value(original.input.seq).unwrap()
                );
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(80), receiver.recv())
            .await
            .is_err()
    );
    running.clock.advance(5);
    let retry = receive(&mut receiver).await;
    assert_eq!(retry.dispatch.input, original.input);
    assert_ne!(retry.dispatch.token, original.token);
    assert_eq!(retry.dispatch.attempt, 2);
    assert!(
        tokio::time::timeout(Duration::from_millis(80), receiver.recv())
            .await
            .is_err()
    );
    accept(retry);
    let successor = receive(&mut receiver).await;
    assert!(!successor.dispatch.input.addressed);
    assert_eq!(successor.dispatch.input.body, "passive successor");
    assert!(successor.dispatch.input.seq > original.input.seq);
    let final_sequence = serde_json::to_value(successor.dispatch.input.seq).unwrap();
    let successor_message = successor.dispatch.input.message_id;
    accept(successor);
    let status = cursor(&mut recipient, &final_sequence).await;
    assert!(
        status["statistics"]
            .get("acknowledged_deliveries")
            .is_none()
    );
    assert_eq!(status["statistics"]["read_receipts"], 2);
    for message in [original.input.message_id, successor_message] {
        support::runtime::assert_automatic_receipts(&mut recipient, message, 1).await;
    }
    running.stop().await;
}
