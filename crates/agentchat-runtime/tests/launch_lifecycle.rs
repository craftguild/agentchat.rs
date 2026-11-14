//! Startup/reconciliation uses the application policy through actual local RPC.
mod support;
use agentchat_core::{
    ErrorCode, Result,
    application::{ManagedActivationCause, Mutation},
    delivery::{
        DeliveryPort, HostReceipt, MessageDispatch, MessagePort, MessageProgress, WakeDispatch,
    },
    model::{Context, LaunchIdentity, LaunchOwnerState, Phase},
    ports::{HostGate, HostState},
    read::Query,
};
use agentchat_protocol::rpc::{Call, Client, success};
use async_trait::async_trait;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use support::runtime::{Running, mutate, query};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;
#[derive(Default)]
struct Host {
    receipt_recovery: AtomicBool,
    bindings: Mutex<HashMap<Uuid, LaunchIdentity>>,
    owners: Mutex<HashMap<Uuid, LaunchOwnerState>>,
    blocked: Mutex<Option<Uuid>>,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
impl Host {
    fn bind(&self, context: &Context) -> LaunchIdentity {
        let identity = LaunchIdentity {
            generation: Uuid::new_v4(),
            owner: format!("synthetic-runtime-owner-{}", Uuid::new_v4()),
        };
        self.bindings
            .lock()
            .unwrap()
            .insert(context.context_id, identity.clone());
        self.state(&identity, LaunchOwnerState::Current);
        identity
    }
    fn state(&self, identity: &LaunchIdentity, state: LaunchOwnerState) {
        self.owners
            .lock()
            .unwrap()
            .insert(identity.generation, state);
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
            if self.receipt_recovery.load(Ordering::SeqCst) {
                InputRecovery::OriginalReceipt
            } else {
                InputRecovery::RetainUncertainty
            },
            ActiveInput::ExactTurnId,
        ))
    }
    async fn verify(&self, context: &Context) -> Result<HostState> {
        let blocked = *self.blocked.lock().unwrap() == Some(context.context_id);
        if blocked {
            self.entered.notify_one();
            self.release.notified().await;
        }
        Ok(HostState {
            phase: Phase::Idle,
            active_turn_id: None,
            wake_paused: false,
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
    fn launch_owner(&self, identity: &LaunchIdentity) -> LaunchOwnerState {
        self.owners
            .lock()
            .unwrap()
            .get(&identity.generation)
            .copied()
            .unwrap_or(LaunchOwnerState::Unknown)
    }
}
struct Pending {
    received: mpsc::UnboundedSender<WakeDispatch>,
    cancelled: Arc<AtomicUsize>,
}
struct Cancelled(Arc<AtomicUsize>);
impl Drop for Cancelled {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
#[async_trait]
impl DeliveryPort for Pending {
    async fn present(&self, _: &Context, dispatch: &WakeDispatch) -> Result<HostReceipt> {
        let _guard = Cancelled(self.cancelled.clone());
        self.received.send(dispatch.clone()).unwrap();
        std::future::pending().await
    }
}
async fn start(client: &mut Client, context: &Context, name: Option<&str>) -> serde_json::Value {
    success(
        client
            .call(
                Uuid::new_v4(),
                Call::StartLaunch {
                    context_id: context.context_id,
                    name: name.map(Into::into),
                },
            )
            .await
            .unwrap(),
    )
    .unwrap()
}
async fn activate_managed(
    client: &mut Client,
    context: &Context,
    request_id: Uuid,
) -> serde_json::Value {
    success(
        client
            .call(
                Uuid::new_v4(),
                Call::ActivateManaged {
                    request_id,
                    context_id: context.context_id,
                    cause: ManagedActivationCause::Initial,
                    name: Some("managed".into()),
                },
            )
            .await
            .unwrap(),
    )
    .unwrap()
}
async fn reconcile(
    client: &mut Client,
    context: &Context,
    identity: &LaunchIdentity,
) -> serde_json::Value {
    success(
        client
            .call(
                Uuid::new_v4(),
                Call::ReconcileLaunch {
                    context_id: context.context_id,
                    generation: identity.generation,
                },
            )
            .await
            .unwrap(),
    )
    .unwrap()
}
async fn receive(receiver: &mut mpsc::UnboundedReceiver<WakeDispatch>) -> WakeDispatch {
    tokio::time::timeout(Duration::from_secs(2), receiver.recv())
        .await
        .unwrap()
        .unwrap()
}
async fn cancelled(count: &AtomicUsize, expected: usize) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while count.load(Ordering::SeqCst) < expected {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
fn post(body: &str) -> Mutation {
    Mutation::Post {
        channel: Some("dev".into()),
        body: body.into(),
    }
}
fn options() -> agentchat_runtime::daemon::RuntimeOptions {
    agentchat_runtime::daemon::RuntimeOptions {
        idle: Duration::from_secs(30),
        io_timeout: Duration::from_secs(2),
        housekeeping: Duration::from_millis(25),
    }
}

#[tokio::test]
async fn managed_activation_rpc_commits_and_replays_one_initial_generation() {
    let host = Arc::new(Host::default());
    let running = Running::with_host(options(), None, host.clone()).await;
    let context = running.context("managed").await;
    let identity = host.bind(&context);
    let mut client = running.client().await;
    let request_id = Uuid::new_v4();

    let activated = activate_managed(&mut client, &context, request_id).await;
    assert_eq!(activated["context_id"], context.context_id.to_string());
    assert_eq!(activated["generation"], identity.generation.to_string());
    assert_eq!(
        activate_managed(&mut client, &context, request_id).await,
        activated
    );
    let state = query(&mut client, Some(context.context_id), Query::Whoami).await;
    assert_eq!(state["participation"], "joined");
    assert_eq!(state["actor"]["handle"], "managed");
}
#[tokio::test]
async fn native_rpc_requires_external_startup_proof_and_retirement_cancels_local_jobs() {
    let host = Arc::new(Host::default());
    let (tx, mut rx) = mpsc::unbounded_channel();
    let count = Arc::new(AtomicUsize::new(0));
    let running = Running::with_host(
        options(),
        Some(Arc::new(Pending {
            received: tx,
            cancelled: count.clone(),
        })),
        host.clone(),
    )
    .await;
    let sender = running.context("sender").await;
    let receiver = running.context("receiver").await;
    let mut a = running.client().await;
    let mut b = running.client().await;
    let no_start = b
        .call(
            Uuid::new_v4(),
            Call::StartLaunch {
                context_id: receiver.context_id,
                name: Some("receiver".into()),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(no_start.code, ErrorCode::SessionRequired);
    let no_join = b
        .call(
            Uuid::new_v4(),
            Call::Mutate {
                actor: agentchat_protocol::rpc::ActorReference::Native {
                    context_id: receiver.context_id,
                },
                request_id: Uuid::new_v4(),
                command: Mutation::Join {
                    name: Some("receiver".into()),
                },
            },
        )
        .await
        .unwrap_err();
    assert_eq!(no_join.code, ErrorCode::SessionRequired);
    host.bind(&sender);
    let identity = host.bind(&receiver);
    start(&mut a, &sender, Some("sender")).await;
    let initial = start(&mut b, &receiver, Some("receiver")).await;
    assert_eq!(start(&mut b, &receiver, Some("receiver")).await, initial);
    let original = mutate(&mut a, &sender, post("@receiver original pending body")).await;
    let original_id = original["message_id"].clone();
    let _dispatch = receive(&mut rx).await;
    host.state(&identity, LaunchOwnerState::Gone);
    // Timer and explicit reconcile use the same positive-loss transition. Either
    // may win; this RPC still fences/cancels the original local presentation.
    reconcile(&mut b, &receiver, &identity).await;
    cancelled(&count, 1).await;
    assert_eq!(
        query(&mut b, None, Query::Whoami).await["actor"]["joined"],
        false
    );
    assert!(
        query(
            &mut b,
            None,
            Query::Inbox {
                page: Default::default()
            }
        )
        .await["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let retained = query(
        &mut a,
        None,
        Query::Message {
            message_id: serde_json::from_value(original_id).unwrap(),
            receipts: Default::default(),
        },
    )
    .await;
    assert_eq!(
        retained["message"]["body"],
        "@receiver original pending body"
    );
    mutate(&mut a, &sender, post("@receiver inactive interval")).await;
    let resumed = running.context("receiver").await;
    let successor = host.bind(&resumed);
    let mut c = running.client().await;
    assert_eq!(
        start(&mut c, &resumed, None).await["agent_id"],
        initial["agent_id"]
    );
    let fresh = mutate(&mut a, &sender, post("@receiver fresh only")).await;
    let dispatch = receive(&mut rx).await;
    assert_eq!(dispatch.hint.events.len(), 1);
    assert_eq!(
        serde_json::to_value(dispatch.hint.events[0].message_id).unwrap(),
        fresh["message_id"]
    );
    assert_eq!(
        reconcile(&mut b, &receiver, &identity).await["retired"],
        false
    );
    assert_eq!(
        query(&mut c, None, Query::Whoami).await["actor"]["joined"],
        true
    );
    assert_ne!(identity.generation, successor.generation);
    running.stop().await;
}
#[tokio::test]
async fn unknown_owner_cancels_local_io_without_discard_and_same_generation_retries_the_head() {
    let host = Arc::new(Host::default());
    let (tx, mut rx) = mpsc::unbounded_channel();
    let count = Arc::new(AtomicUsize::new(0));
    let running = Running::with_host(
        options(),
        Some(Arc::new(Pending {
            received: tx,
            cancelled: count.clone(),
        })),
        host.clone(),
    )
    .await;
    let sender = running.context("sender").await;
    let receiver = running.context("receiver").await;
    host.bind(&sender);
    let identity = host.bind(&receiver);
    let mut a = running.client().await;
    let mut b = running.client().await;
    start(&mut a, &sender, Some("sender")).await;
    start(&mut b, &receiver, Some("receiver")).await;
    mutate(&mut a, &sender, post("@receiver retain uncertain head")).await;
    let first = receive(&mut rx).await;
    host.state(&identity, LaunchOwnerState::Unknown);
    assert_eq!(
        reconcile(&mut b, &receiver, &identity).await["retired"],
        false
    );
    cancelled(&count, 1).await;
    assert_eq!(
        query(&mut b, None, Query::Whoami).await["actor"]["joined"],
        true
    );
    let before = query(
        &mut b,
        None,
        Query::Inbox {
            page: Default::default(),
        },
    )
    .await;
    assert!(
        before["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["delivery"]["id"]
                == serde_json::to_value(first.hint.events[0].delivery_id).unwrap())
    );
    host.state(&identity, LaunchOwnerState::Current);
    running.clock.advance(6);
    b.call(
        Uuid::new_v4(),
        Call::Heartbeat {
            context_id: receiver.context_id,
        },
    )
    .await
    .unwrap();
    let retried = receive(&mut rx).await;
    assert_eq!(retried.hint.events, first.hint.events);
    assert_eq!(retried.attempt, first.attempt + 1);
    assert_ne!(retried.token, first.token);
    running.stop().await;
}

struct BodyOffer {
    dispatch: MessageDispatch,
    answer: oneshot::Sender<Result<MessageProgress>>,
}
struct Bodies(mpsc::UnboundedSender<BodyOffer>);
#[async_trait]
impl MessagePort for Bodies {
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
        let (answer, received) = oneshot::channel();
        self.0
            .send(BodyOffer {
                dispatch: dispatch.clone(),
                answer,
            })
            .unwrap();
        received.await.unwrap()
    }
}
async fn body(receiver: &mut mpsc::UnboundedReceiver<BodyOffer>) -> BodyOffer {
    tokio::time::timeout(Duration::from_secs(2), receiver.recv())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn full_body_publication_polls_its_receipt_without_republishing_or_spending_attempts() {
    let host = Arc::new(Host::default());
    host.receipt_recovery.store(true, Ordering::SeqCst);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let running = Running::with_native_host(options(), Arc::new(Bodies(tx)), host.clone()).await;
    let sender = running.context("sender").await;
    let receiver = running.context("receiver").await;
    host.bind(&sender);
    host.bind(&receiver);
    let mut a = running.client().await;
    let mut b = running.client().await;
    start(&mut a, &sender, Some("sender")).await;
    start(&mut b, &receiver, Some("receiver")).await;
    mutate(&mut a, &sender, post("@receiver first published body")).await;
    mutate(&mut a, &sender, post("later passive body")).await;
    let original = body(&mut rx).await;
    assert!(!original.dispatch.awaiting_acceptance);
    let input = original.dispatch.input.clone();
    let token = original.dispatch.token;
    let started = agentchat_core::time::Clock::now(running.clock.as_ref());
    original
        .answer
        .send(Ok(MessageProgress::AwaitingAcceptance))
        .unwrap();
    for index in 0..6 {
        // The fixture's clock is manual. Observe the completed pending write
        // before advancing it, rather than mistaking wall time for app time.
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let status = support::runtime::whoami(&mut b).await;
                let head = &status["native_delivery"]["head"];
                if head["awaiting_acceptance"] == true
                    && head["next_eligible_at"].as_str().is_some_and(|value| {
                        agentchat_core::time::Timestamp::parse(value).unwrap()
                            == agentchat_core::time::Clock::now(running.clock.as_ref())
                                .after_seconds(1)
                                .unwrap()
                    })
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        running.clock.advance(1);
        let poll = match tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
            Ok(Some(poll)) => poll,
            _ => panic!(
                "No receipt poll {index}; runtime state: {}",
                support::runtime::whoami(&mut b).await
            ),
        };
        assert!(poll.dispatch.awaiting_acceptance);
        assert_eq!(poll.dispatch.input, input);
        assert_eq!(poll.dispatch.token, token);
        assert_eq!(poll.dispatch.attempt, 1);
        let status = support::runtime::whoami(&mut b).await;
        let state = &status["native_delivery"];
        assert_eq!(state["state"], "pending");
        assert_eq!(state["head"]["awaiting_acceptance"], true);
        assert_eq!(state["head"]["attempts"], 1);
        if index == 5 {
            assert!(
                agentchat_core::time::Clock::now(running.clock.as_ref())
                    > started
                        .after_seconds(agentchat_core::delivery::SUBMISSION_SECONDS as i64)
                        .unwrap()
            );
            poll.answer
                .send(Ok(MessageProgress::Accepted(HostReceipt::default())))
                .unwrap();
        } else {
            poll.answer
                .send(Ok(MessageProgress::AwaitingAcceptance))
                .unwrap();
        }
    }
    // A receipt poll can already have been offered before the accepted callback
    // commits. Model the adapter's immutable receipt lane for that same input;
    // it must never become another publication or consume another attempt.
    let mut transitional_polls = 0;
    let next = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let offer = body(&mut rx).await;
            if !offer.dispatch.awaiting_acceptance {
                break offer;
            }
            assert_eq!(offer.dispatch.input, input);
            assert_eq!(offer.dispatch.token, token);
            assert_eq!(offer.dispatch.attempt, 1);
            transitional_polls += 1;
            // Completion may already have cancelled this old in-flight poll.
            // A closed callback cannot authorize or advance the successor.
            let _ = offer
                .answer
                .send(Ok(MessageProgress::Accepted(HostReceipt::default())));
        }
    })
    .await
    .unwrap();
    println!("Validated {transitional_polls} transitional original-receipt polls.");
    assert!(!next.dispatch.awaiting_acceptance);
    assert_eq!(next.dispatch.input.body, "later passive body");
    assert!(next.dispatch.input.seq > input.seq);
    assert_eq!(next.dispatch.attempt, 1);
    assert_ne!(next.dispatch.token, token);
    next.answer
        .send(Ok(MessageProgress::Accepted(HostReceipt::default())))
        .unwrap();
    running.stop().await;
}

#[tokio::test]
async fn full_body_runtime_serializes_heads_with_automatic_receipts_without_model_or_explicit_read()
{
    let host = Arc::new(Host::default());
    let (tx, mut rx) = mpsc::unbounded_channel();
    let running = Running::with_native_host(options(), Arc::new(Bodies(tx)), host.clone()).await;
    let sender = running.context("sender").await;
    let receiver = running.context("receiver").await;
    host.bind(&sender);
    host.bind(&receiver);
    let mut a = running.client().await;
    let mut b = running.client().await;
    start(&mut a, &sender, Some("sender")).await;
    start(&mut b, &receiver, Some("receiver")).await;
    let bodies = [
        "passive full body first",
        "@receiver addressed second",
        "passive full body third",
    ];
    let mut messages = Vec::new();
    for text in bodies {
        messages.push(mutate(&mut a, &sender, post(text)).await["message_id"].clone());
    }
    let mut last = None;
    for (index, text) in bodies.into_iter().enumerate() {
        let offer = body(&mut rx).await;
        assert_eq!(offer.dispatch.input.body, text);
        assert_eq!(offer.dispatch.input.addressed, index == 1);
        assert!(offer.dispatch.input.auto_read);
        assert_eq!(
            serde_json::to_value(offer.dispatch.input.message_id).unwrap(),
            messages[index]
        );
        assert!(last.is_none_or(|seq| offer.dispatch.input.seq > seq));
        assert!(
            tokio::time::timeout(Duration::from_millis(40), rx.recv())
                .await
                .is_err()
        );
        // RPC and read-only snapshots remain usable while native consumption waits.
        let status = support::runtime::whoami(&mut b).await;
        assert_eq!(
            status["native_delivery"]["head"]["delivery_id"],
            offer.dispatch.input.delivery_id.to_string()
        );
        last = Some(offer.dispatch.input.seq);
        offer
            .answer
            .send(Ok(MessageProgress::Accepted(HostReceipt::default())))
            .unwrap();
    }
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if support::runtime::whoami(&mut b).await["native_delivery"]["state"] == "ready" {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let inbox = query(
        &mut b,
        None,
        Query::Inbox {
            page: Default::default(),
        },
    )
    .await;
    for message in messages {
        assert!(
            inbox["items"]
                .as_array()
                .unwrap()
                .iter()
                .all(|i| i["event"]["message_id"] != message)
        );
        support::runtime::assert_automatic_receipts(
            &mut b,
            serde_json::from_value(message).unwrap(),
            1,
        )
        .await;
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(40), rx.recv())
            .await
            .is_err()
    );
    running.stop().await;
}

#[tokio::test]
async fn full_body_runtime_cancels_uncertain_io_preserves_failed_head_and_fences_retirement() {
    let host = Arc::new(Host::default());
    let (tx, mut rx) = mpsc::unbounded_channel();
    let running = Running::with_native_host(options(), Arc::new(Bodies(tx)), host.clone()).await;
    let sender = running.context("sender").await;
    let receiver = running.context("receiver").await;
    host.bind(&sender);
    let identity = host.bind(&receiver);
    let mut a = running.client().await;
    let mut b = running.client().await;
    start(&mut a, &sender, Some("sender")).await;
    start(&mut b, &receiver, Some("receiver")).await;
    mutate(&mut a, &sender, post("@receiver failed head")).await;
    let first = body(&mut rx).await;
    mutate(&mut a, &sender, post("later passive body")).await;
    host.state(&identity, LaunchOwnerState::Unknown);
    assert_eq!(
        reconcile(&mut b, &receiver, &identity).await["retired"],
        false
    );
    assert!(
        first
            .answer
            .send(Ok(MessageProgress::Accepted(HostReceipt::default())))
            .is_err()
    );
    assert_eq!(
        support::runtime::whoami(&mut b).await["native_delivery"]["state"],
        "pending"
    );
    host.state(&identity, LaunchOwnerState::Current);
    running.clock.advance(6);
    b.call(
        Uuid::new_v4(),
        Call::Heartbeat {
            context_id: receiver.context_id,
        },
    )
    .await
    .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), rx.recv())
            .await
            .is_err()
    );
    let stopped = support::runtime::whoami(&mut b).await;
    assert_eq!(stopped["native_delivery"]["state"], "acceptance_unknown");
    assert_eq!(stopped["native_delivery"]["head"]["attempts"], 1);
    assert_eq!(
        stopped["native_delivery"]["head"]["submission_pending"],
        true
    );
    mutate(&mut a, &sender, post("@receiver sender chose to repost")).await;
    running.clock.advance(30);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), rx.recv())
            .await
            .is_err()
    );
    host.state(&identity, LaunchOwnerState::Gone);
    reconcile(&mut b, &receiver, &identity).await;
    assert_eq!(
        support::runtime::whoami(&mut b).await["native_delivery"]["state"],
        "cancelled"
    );
    mutate(&mut a, &sender, post("inactive interval body")).await;
    let resumed = running.context("receiver").await;
    host.bind(&resumed);
    let mut c = running.client().await;
    start(&mut c, &resumed, None).await;
    mutate(&mut a, &sender, post("fresh passive generation body")).await;
    let fresh = body(&mut rx).await;
    assert_eq!(fresh.dispatch.input.body, "fresh passive generation body");
    assert_ne!(fresh.dispatch.input.generation, identity.generation);
    running.stop().await;
    assert!(
        fresh
            .answer
            .send(Ok(MessageProgress::Accepted(HostReceipt::default())))
            .is_err()
    );
}

#[tokio::test]
async fn full_body_final_check_is_unbound_and_never_waits_for_native_verification() {
    let host = Arc::new(Host::default());
    let (tx, mut rx) = mpsc::unbounded_channel();
    let running = Running::with_native_host(options(), Arc::new(Bodies(tx)), host.clone()).await;
    let sender = running.context("sender").await;
    let receiver = running.context("receiver").await;
    host.bind(&sender);
    host.bind(&receiver);
    let mut a = running.client().await;
    let mut b = running.client().await;
    let mut checker = running.client().await;
    start(&mut a, &sender, Some("sender")).await;
    start(&mut b, &receiver, Some("receiver")).await;
    mutate(&mut a, &sender, post("passive complete body")).await;
    let offered = body(&mut rx).await;
    *host.blocked.lock().unwrap() = Some(receiver.context_id);
    let id = receiver.context_id;
    let waiting = tokio::spawn(async move {
        b.call(Uuid::new_v4(), Call::Heartbeat { context_id: id })
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), host.entered.notified())
        .await
        .unwrap();
    let checked = tokio::time::timeout(
        Duration::from_secs(1),
        checker.call(
            Uuid::new_v4(),
            Call::CheckMessage {
                context_id: id,
                generation: offered.dispatch.input.generation,
                delivery_id: offered.dispatch.input.delivery_id,
                token: offered.dispatch.token,
            },
        ),
    )
    .await;
    *host.blocked.lock().unwrap() = None;
    host.release.notify_waiters();
    waiting.await.unwrap().unwrap();
    assert_eq!(
        success(
            checked
                .expect("body snapshot waited behind native verification")
                .unwrap()
        )
        .unwrap()["current"],
        true
    );
    assert_eq!(
        checker
            .read(Uuid::new_v4(), None, agentchat_core::read::requests::Whoami,)
            .await
            .unwrap_err()
            .code,
        ErrorCode::SessionRequired
    );
    running.stop().await;
    assert!(
        offered
            .answer
            .send(Ok(MessageProgress::Accepted(HostReceipt::default())))
            .is_err()
    );
}
