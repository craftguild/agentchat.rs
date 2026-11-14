//! Real RPC/storage proof that event notices cannot overtake complete bodies.
mod support;
use agentchat_core::{
    Error, ErrorCode, Result,
    application::Mutation,
    delivery::{
        HostReceipt, MessageDispatch, MessagePort, MessageProgress, NativeInput, NoticeDispatch,
    },
    model::{Context, HostKind, LaunchIdentity, LaunchOwnerState, Phase},
    ports::{HostGate, HostState},
    read::Query,
};
use agentchat_protocol::rpc::{Call, Client, success};
use async_trait::async_trait;
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
    time::Duration,
};
use support::runtime::{Running, mutate, query};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

#[derive(Default)]
struct Host {
    bindings: Mutex<HashMap<Uuid, LaunchIdentity>>,
    gone: Mutex<HashSet<Uuid>>,
    unavailable: Mutex<HashSet<Uuid>>,
}
impl Host {
    fn retire(&self, context: Uuid) {
        let generation = self.bindings.lock().unwrap()[&context].generation;
        self.gone.lock().unwrap().insert(generation);
    }
    fn refuse_native(&self, context: Uuid) {
        self.unavailable.lock().unwrap().insert(context);
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
    async fn launch_identity(&self, context: &Context) -> Result<Option<LaunchIdentity>> {
        Ok(self
            .bindings
            .lock()
            .unwrap()
            .get(&context.context_id)
            .cloned())
    }
    fn launch_owner(&self, identity: &LaunchIdentity) -> LaunchOwnerState {
        if self.gone.lock().unwrap().contains(&identity.generation) {
            return LaunchOwnerState::Gone;
        }
        if self
            .bindings
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
        if self
            .unavailable
            .lock()
            .unwrap()
            .contains(&context.context_id)
        {
            return Err(Error::new(
                ErrorCode::SessionRequired,
                "synthetic native route is no longer eligible",
            ));
        }
        Ok(HostState {
            phase: Phase::Active,
            active_turn_id: None,
            wake_paused: false,
        })
    }
}

#[tokio::test]
async fn native_preparation_refusal_and_owner_loss_are_not_control_failures() {
    let host = Arc::new(Host::default());
    let (sender, mut receiver) = mpsc::unbounded_channel();
    // A preparation refusal preserves a physically live launch. Later positive
    // owner loss is reconciled by ordinary maintenance, whose bounded scan may
    // need another cycle; use the fixture's normal cadence for that boundary.
    let running = Running::with_native_host(
        support::runtime::options(),
        Arc::new(Port(sender)),
        host.clone(),
    )
    .await;
    let (mut alice, alice_context) = start(&running, &host, "alice-race").await;
    let (mut bob, bob_context) = start(&running, &host, "bob-race").await;
    drain(
        &running,
        &mut receiver,
        &mut [(&mut alice, &alice_context), (&mut bob, &bob_context)],
    )
    .await;
    let before = support::runtime::whoami(&mut bob).await;
    let scans = before["runtime"]["native_delivery"]["candidate_scans"]
        .as_u64()
        .unwrap();

    host.refuse_native(alice_context.context_id);
    mutate(
        &mut bob,
        &bob_context,
        Mutation::Post {
            channel: Some("dev".into()),
            body: "schedule while alice exits".into(),
        },
    )
    .await;

    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let status = support::runtime::whoami(&mut bob).await;
            if status["runtime"]["native_delivery"]["candidate_scans"]
                .as_u64()
                .is_some_and(|value| value > scans)
                && status["runtime"]["native_delivery"]["jobs"] == 0
            {
                assert_eq!(status["statistics"]["joined"], 2);
                assert_eq!(status["runtime"]["attached_connections"], 2);
                assert!(
                    status["runtime"]["last_control_error"].is_null(),
                    "{status}"
                );
                let alice_status = support::runtime::whoami(&mut alice).await;
                assert!(
                    alice_status["native_delivery"]["head"].is_null(),
                    "{alice_status}"
                );
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    host.retire(alice_context.context_id);
    drop(alice);
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let status = support::runtime::whoami(&mut bob).await;
            if status["statistics"]["joined"] == 1 && status["runtime"]["attached_connections"] == 1
            {
                assert!(
                    status["runtime"]["last_control_error"].is_null(),
                    "{status}"
                );
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let retired = next_for(&mut receiver, bob_context.context_id).await;
    let Input::Notice(notice) = &retired.input else {
        panic!("owner loss must remain an ordered native notice");
    };
    assert_eq!(notice.input.notice.kind.as_str(), "agent.launch_retired");
    // This is the exact RPC called by Claude's native-only load/claim boundary.
    // A message-only candidate projection cannot authorize the notice head.
    let input = retired.input.input();
    let mut checker = running.client().await;
    let check = Call::CheckNative {
        context_id: bob_context.context_id,
        generation: input.generation(),
        delivery_id: input.delivery_id(),
        token: retired.input.token(),
    };
    assert_eq!(
        success(checker.call(Uuid::new_v4(), check).await.unwrap()).unwrap()["current"],
        true,
        "the exact current lifecycle notice must reach native entry"
    );
    let message_only = Call::CheckMessage {
        context_id: bob_context.context_id,
        generation: input.generation(),
        delivery_id: input.delivery_id(),
        token: retired.input.token(),
    };
    assert_eq!(
        success(checker.call(Uuid::new_v4(), message_only).await.unwrap()).unwrap()["current"],
        false,
        "the existing message-only RPC keeps its original contract"
    );
    let wrong_token = Call::CheckNative {
        context_id: bob_context.context_id,
        generation: input.generation(),
        delivery_id: input.delivery_id(),
        token: Uuid::new_v4(),
    };
    assert_eq!(
        success(checker.call(Uuid::new_v4(), wrong_token).await.unwrap()).unwrap()["current"],
        false
    );
    let wrong_generation = Call::CheckNative {
        context_id: bob_context.context_id,
        generation: Uuid::new_v4(),
        delivery_id: input.delivery_id(),
        token: retired.input.token(),
    };
    assert_eq!(
        success(
            checker
                .call(Uuid::new_v4(), wrong_generation)
                .await
                .unwrap()
        )
        .unwrap()["current"],
        false,
        "a notice from another launch generation cannot reach native entry"
    );
    assert_eq!(
        checker
            .read(Uuid::new_v4(), None, agentchat_core::read::requests::Whoami,)
            .await
            .unwrap_err()
            .code,
        ErrorCode::SessionRequired,
        "a native fence must not bind or refresh the checking connection"
    );
    accept(retired);
    assert_no_input_for(&mut receiver, alice_context.context_id).await;
    running.stop().await;
}

#[tokio::test]
async fn session_loss_after_native_claim_retains_uncertainty_and_blocks_its_successor() {
    let host = Arc::new(Host::default());
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let running = Running::with_native_host(
        support::runtime::options(),
        Arc::new(Port(sender)),
        host.clone(),
    )
    .await;
    let (mut alice, alice_context) = start(&running, &host, "alice-uncertain").await;
    let (mut bob, bob_context) = start(&running, &host, "bob-uncertain").await;
    drain(
        &running,
        &mut receiver,
        &mut [(&mut alice, &alice_context), (&mut bob, &bob_context)],
    )
    .await;

    mutate(
        &mut bob,
        &bob_context,
        Mutation::Post {
            channel: Some("dev".into()),
            body: "first possibly accepted body".into(),
        },
    )
    .await;
    mutate(
        &mut bob,
        &bob_context,
        Mutation::Post {
            channel: Some("dev".into()),
            body: "successor must remain blocked".into(),
        },
    )
    .await;
    let first = next_for(&mut receiver, alice_context.context_id).await;
    let Input::Message(message) = &first.input else {
        panic!("first input must be a message");
    };
    assert_eq!(message.input.body, "first possibly accepted body");
    first
        .result
        .send(Err(Error::new(
            ErrorCode::SessionRequired,
            "synthetic session loss after native submission",
        )))
        .unwrap();

    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let status = support::runtime::whoami(&mut alice).await;
            if status["native_delivery"]["state"] == "acceptance_unknown" {
                assert_eq!(
                    status["native_delivery"]["head"]["failure_code"],
                    "session_required"
                );
                assert_eq!(
                    status["native_delivery"]["head"]["submission_pending"],
                    true
                );
                assert!(
                    status["runtime"]["last_control_error"].is_null(),
                    "{status}"
                );
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    running.clock.advance(30);
    assert_no_input_for(&mut receiver, alice_context.context_id).await;
    let status = support::runtime::whoami(&mut alice).await;
    assert_eq!(status["native_delivery"]["state"], "acceptance_unknown");
    assert_eq!(status["native_delivery"]["head"]["attempts"], 1);
    running.stop().await;
}

#[derive(Clone)]
enum Input {
    Message(MessageDispatch),
    Notice(NoticeDispatch),
}
impl Input {
    fn token(&self) -> Uuid {
        match self {
            Self::Message(value) => value.token,
            Self::Notice(value) => value.token,
        }
    }
    fn attempt(&self) -> u32 {
        match self {
            Self::Message(value) => value.attempt,
            Self::Notice(value) => value.attempt,
        }
    }
    fn input(&self) -> NativeInput {
        match self {
            Self::Message(value) => NativeInput::Message(value.input.clone()),
            Self::Notice(value) => NativeInput::Notice(value.input.clone()),
        }
    }
}
struct Pending {
    context: Context,
    input: Input,
    result: oneshot::Sender<Result<MessageProgress>>,
}
struct Port(mpsc::UnboundedSender<Pending>);
#[async_trait]
impl MessagePort for Port {
    async fn present_message(
        &self,
        context: &Context,
        dispatch: &MessageDispatch,
    ) -> Result<MessageProgress> {
        self.present(context, Input::Message(dispatch.clone()))
            .await
    }
    async fn present_notice(
        &self,
        context: &Context,
        dispatch: &NoticeDispatch,
    ) -> Result<MessageProgress> {
        self.present(context, Input::Notice(dispatch.clone())).await
    }
}
impl Port {
    async fn present(&self, context: &Context, input: Input) -> Result<MessageProgress> {
        let (result, receiver) = oneshot::channel();
        self.0
            .send(Pending {
                context: context.clone(),
                input,
                result,
            })
            .unwrap();
        receiver.await.unwrap()
    }
}

async fn start(running: &Running, host: &Host, name: &str) -> (Client, Context) {
    let context = running.context(name).await;
    host.bindings.lock().unwrap().insert(
        context.context_id,
        LaunchIdentity {
            generation: Uuid::new_v4(),
            owner: format!("explicit-synthetic-owner-{name}"),
        },
    );
    let mut client = running.client().await;
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
    .unwrap();
    (client, context)
}

#[tokio::test]
async fn retired_native_context_is_pruned_without_input_or_control_failure() {
    let host = Arc::new(Host::default());
    let (sender, mut receiver) = mpsc::unbounded_channel();
    // Launch retirement uses an advisory finite scan. The next ordinary cycle
    // can finish a scan that was already past this generation when its owner
    // disappeared; a ten-second artificial cadence cannot promise three seconds.
    let running = Running::with_native_host(
        support::runtime::options(),
        Arc::new(Port(sender)),
        host.clone(),
    )
    .await;
    let (alice_supervisor, alice_context) = start(&running, &host, "alice").await;
    let mut alice_native = running.client().await;
    success(
        alice_native
            .call(
                Uuid::new_v4(),
                Call::Attach {
                    context_id: alice_context.context_id,
                },
            )
            .await
            .unwrap(),
    )
    .unwrap();
    let (mut bob, bob_context) = start(&running, &host, "bob").await;
    drain(
        &running,
        &mut receiver,
        &mut [(&mut alice_native, &alice_context)],
    )
    .await;
    host.retire(alice_context.context_id);
    // The native MCP attachment remains connected when the launch supervisor
    // disappears.  Its stale context must be reconciled before scheduling.
    drop(alice_supervisor);

    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let status = support::runtime::whoami(&mut bob).await;
            if status["statistics"]["joined"] == 1 && status["runtime"]["attached_connections"] == 1
            {
                assert!(
                    status["runtime"]["last_control_error"].is_null(),
                    "{status}"
                );
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let retired = next_for(&mut receiver, bob_context.context_id).await;
    let Input::Notice(notice) = &retired.input else {
        panic!("launch retirement must remain an ordered native notice");
    };
    assert_eq!(notice.input.notice.kind.as_str(), "agent.launch_retired");
    accept(retired);
    assert_no_input_for(&mut receiver, alice_context.context_id).await;
    let status = support::runtime::whoami(&mut bob).await;
    assert_eq!(status["runtime"]["attached_connections"], 1);
    assert!(
        status["runtime"]["last_control_error"].is_null(),
        "{status}"
    );
    running.stop().await;
}
async fn next(receiver: &mut mpsc::UnboundedReceiver<Pending>) -> Pending {
    tokio::time::timeout(Duration::from_secs(3), receiver.recv())
        .await
        .unwrap()
        .unwrap()
}
async fn next_for(receiver: &mut mpsc::UnboundedReceiver<Pending>, context: Uuid) -> Pending {
    loop {
        let pending = next(receiver).await;
        if pending.context.context_id == context {
            return pending;
        }
        accept(pending);
    }
}
fn accept(pending: Pending) {
    let turn_id = pending
        .input
        .input()
        .addressed()
        .then(|| "accepted-native-input".into());
    pending
        .result
        .send(Ok(MessageProgress::Accepted(HostReceipt { turn_id })))
        .unwrap();
}
async fn drain(
    running: &Running,
    receiver: &mut mpsc::UnboundedReceiver<Pending>,
    clients: &mut [(&mut Client, &Context)],
) {
    // A quiet 100ms interval does not prove lifecycle notices were prepared.
    // Settle each exact recipient stream through actual native receipts and a
    // read-only Store candidate snapshot; own/non-native events need not advance
    // settled_seq to the project head. The dispatcher also schedules empty
    // preparation jobs, so global jobs/completion counters need not be zero
    // when these exact streams have no head or next candidate. Retain the
    // existing three-second bound.
    use agentchat_core::ports::Storage;
    let store = running.f.manager.open(&running.project).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            while let Ok(pending) = receiver.try_recv() {
                accept(pending);
            }
            let mut settled = true;
            for (_, context) in clients.iter_mut() {
                let mut snapshot = store.snapshot().await.unwrap();
                let agent = snapshot
                    .conversation(context.host, &context.host_session_id)
                    .await
                    .unwrap()
                    .unwrap();
                let launch = snapshot.latest_launch(agent.id).await.unwrap().unwrap();
                let stream = snapshot
                    .message_stream(launch.identity.generation)
                    .await
                    .unwrap()
                    .unwrap();
                settled &= !stream.cancelled
                    && stream.head.is_none()
                    && snapshot
                        .next_native(agent.id, stream.settled_seq)
                        .await
                        .unwrap()
                        .is_none();
            }
            if settled && receiver.is_empty() {
                break;
            }
            tokio::select! {
                Some(pending)=receiver.recv()=>accept(pending),
                _=tokio::time::sleep(Duration::from_millis(5))=>{}
            }
        }
    })
    .await
    .unwrap();
    Arc::try_unwrap(store)
        .ok()
        .expect("exclusive fixture store")
        .close()
        .await
        .unwrap();
}
async fn assert_no_input_for(receiver: &mut mpsc::UnboundedReceiver<Pending>, context: Uuid) {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(100);
    while let Ok(Some(pending)) = tokio::time::timeout_at(deadline, receiver.recv()).await {
        assert_ne!(pending.context.context_id, context);
        accept(pending);
    }
}

#[tokio::test]
async fn late_acceptance_preserves_one_timeout_notice_and_settles_the_original_fifo_head() {
    let host = Arc::new(Host::default());
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let running = Running::with_native_host(
        support::runtime::options(),
        Arc::new(Port(sender)),
        host.clone(),
    )
    .await;
    let (mut alice, alice_context) = start(&running, &host, "alice").await;
    let (mut bob, bob_context) = start(&running, &host, "bob").await;
    // Launch/join lifecycle events are valid ordered notices. Settle them so
    // the assertions below start at a deliberate mixed message/event boundary.
    drain(
        &running,
        &mut receiver,
        &mut [(&mut alice, &alice_context), (&mut bob, &bob_context)],
    )
    .await;
    let status = support::runtime::whoami(&mut alice).await;
    assert!(
        status["runtime"]["last_control_error"].is_null(),
        "{status}"
    );

    mutate(
        &mut bob,
        &bob_context,
        Mutation::Post {
            channel: Some("dev".into()),
            body: "first body for alice".into(),
        },
    )
    .await;
    let alice_first = next_for(&mut receiver, alice_context.context_id).await;
    assert_eq!(alice_first.context.context_id, alice_context.context_id);
    let Input::Message(first) = &alice_first.input else {
        panic!("first input must be a message");
    };
    assert_eq!(first.input.body, "first body for alice");

    support::runtime::set_confirmation_timeout(&running, 1).await;
    let posted = mutate(
        &mut alice,
        &alice_context,
        Mutation::Post {
            channel: Some("dev".into()),
            body: "@bob confirm this".into(),
        },
    )
    .await;
    let original_message = Uuid::parse_str(posted["message_id"].as_str().unwrap()).unwrap();
    let bob_message = next_for(&mut receiver, bob_context.context_id).await;
    assert_eq!(bob_message.context.context_id, bob_context.context_id);
    let Input::Message(to_bob) = &bob_message.input else {
        panic!("recipient input must be a message");
    };
    let recipient_delivery = to_bob.input.delivery_id;
    let early = query(
        &mut alice,
        None,
        Query::Message {
            message_id: original_message,
            receipts: Default::default(),
        },
    )
    .await;
    let early_delivery = &early["receipts"]["items"][0]["delivery"];
    assert_eq!(
        early_delivery["delivery_id"],
        recipient_delivery.to_string()
    );
    assert!(early_delivery["delivered_at"].is_null());
    assert!(early_delivery.get("acknowledged_at").is_none());
    assert_eq!(early_delivery["state"], "processing");
    running.clock.advance(2);
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let events = query(
                &mut alice,
                None,
                Query::Events {
                    channel: None,
                    page: Default::default(),
                },
            )
            .await;
            if events["items"].as_array().unwrap().iter().any(|event| {
                event["kind"] == "message.delivery_confirmation_timeout"
                    && event["message_id"] == original_message.to_string()
            }) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let bob_inbox = query(
        &mut bob,
        None,
        Query::Inbox {
            page: Default::default(),
        },
    )
    .await;
    assert!(
        bob_inbox["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(
                |row| row["event"]["kind"] != "message.delivery_confirmation_timeout"
                    || row["event"]["message_id"] != original_message.to_string()
            ),
        "the recipient cannot receive the author's timeout as passive native input"
    );
    // Only this original complete body can confirm native acceptance;
    // its late acceptance preserves the terminal timeout rather than erasing it.
    accept(bob_message);
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let detail = query(
                &mut alice,
                None,
                Query::Message {
                    message_id: original_message,
                    receipts: Default::default(),
                },
            )
            .await;
            let delivery = &detail["receipts"]["items"][0]["delivery"];
            if !delivery["delivered_at"].is_null() {
                assert!(delivery.get("acknowledged_at").is_none());
                assert_eq!(delivery["state"], "delivered_late");
                assert!(!delivery["timed_out_at"].is_null());
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    mutate(
        &mut bob,
        &bob_context,
        Mutation::Post {
            channel: Some("dev".into()),
            body: "body after timeout".into(),
        },
    )
    .await;
    assert_no_input_for(&mut receiver, alice_context.context_id).await;

    let first_seq = alice_first.input.input().seq();
    // The original message keeps its frozen one-second timeout. The queued
    // notice has not started processing, so select its independent retry budget
    // before its first claim. A definite refusal retries after five seconds.
    support::runtime::set_confirmation_timeout(
        &running,
        agentchat_core::delivery::SUBMISSION_SECONDS as u32,
    )
    .await;
    accept(alice_first);
    let notice = next_for(&mut receiver, alice_context.context_id).await;
    assert_eq!(notice.context.context_id, alice_context.context_id);
    let Input::Notice(original_notice) = &notice.input else {
        panic!("timeout must retain an explicit notice type");
    };
    assert_eq!(
        original_notice.input.notice.kind.as_str(),
        "message.delivery_confirmation_timeout"
    );
    assert_eq!(
        original_notice.input.dispatch.kind,
        agentchat_core::model::DispatchKind::Notice
    );
    assert!(original_notice.input.dispatch.creation_seq > first_seq);
    let notice_input = original_notice.input.clone();
    let notice_token = notice.input.token();
    notice
        .result
        .send(Ok(MessageProgress::NotAccepted(Error::new(
            ErrorCode::Busy,
            "synthetic definite refusal",
        ))))
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let status = support::runtime::whoami(&mut alice).await;
            if status["native_delivery"]["head"]["failure_code"] == "busy" {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    // Retry exactly at this notice's inclusive first processing deadline.
    running
        .clock
        .advance(agentchat_core::delivery::SUBMISSION_SECONDS as i64);
    let retry = next_for(&mut receiver, alice_context.context_id).await;
    let Input::Notice(retried_notice) = &retry.input else {
        panic!("failed notice cannot be replaced by its message successor");
    };
    assert_eq!(retried_notice.input, notice_input);
    assert_eq!(retry.input.attempt(), 2);
    assert_ne!(retry.input.token(), notice_token);
    accept(retry);

    let successor = next_for(&mut receiver, alice_context.context_id).await;
    let Input::Message(successor_message) = &successor.input else {
        panic!("message successor must follow accepted notice");
    };
    assert_eq!(successor_message.input.body, "body after timeout");
    assert!(successor_message.input.seq > notice_input.dispatch.creation_seq);
    accept(successor);
    let events = query(
        &mut alice,
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
            .filter(
                |event| event["kind"] == "message.delivery_confirmation_timeout"
                    && event["message_id"] == original_message.to_string()
            )
            .count(),
        1,
        "late acceptance cannot generate a second timeout for the original message"
    );
    running.stop().await;
}

#[tokio::test]
async fn late_completion_publishes_timeout_progress_without_housekeeping_or_another_write() {
    let host = Arc::new(Host::default());
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let mut options = support::runtime::options();
    options.housekeeping = Duration::from_secs(10);
    let started = tokio::time::Instant::now();
    let running = Running::with_native_host(options, Arc::new(Port(sender)), host.clone()).await;
    let (mut alice, alice_context) = start(&running, &host, "notice-author").await;
    let (mut bob, bob_context) = start(&running, &host, "notice-recipient").await;
    drain(
        &running,
        &mut receiver,
        &mut [(&mut alice, &alice_context), (&mut bob, &bob_context)],
    )
    .await;
    support::runtime::set_confirmation_timeout(&running, 1).await;
    let posted = mutate(
        &mut alice,
        &alice_context,
        Mutation::Post {
            channel: Some("dev".into()),
            body: "@notice-recipient late native result".into(),
        },
    )
    .await;
    let pending = next_for(&mut receiver, bob_context.context_id).await;
    let message = Uuid::parse_str(posted["message_id"].as_str().unwrap()).unwrap();
    let Input::Message(body) = &pending.input else {
        panic!("complete message")
    };
    assert_eq!(body.input.message_id, message);

    let after = success(
        running
            .client()
            .await
            .read(
                Uuid::new_v4(),
                None,
                agentchat_core::read::requests::ViewerHead,
            )
            .await
            .unwrap(),
    )
    .unwrap();
    let mut observer = running
        .client()
        .await
        .subscribe_changes(after)
        .await
        .unwrap();
    let initial = observer.next().await.unwrap();
    assert_eq!(initial.scan, after);
    assert!(!initial.has_more);
    running.clock.advance(2);
    accept(pending);
    // No mutation, maintenance timer or receipt poll can wake the observer here.
    let progress = tokio::time::timeout(Duration::from_secs(3), observer.next())
        .await
        .unwrap()
        .unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the notification must precede the next periodic housekeeping tick"
    );
    assert!(progress.scan.seq > after.seq);
    let events = query(
        &mut alice,
        None,
        Query::Events {
            channel: None,
            page: agentchat_core::read::PageRequest {
                after: after.seq,
                ..Default::default()
            },
        },
    )
    .await;
    assert_eq!(
        events["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(
                |event| event["kind"] == "message.delivery_confirmation_timeout"
                    && event["message_id"] == message.to_string()
            )
            .count(),
        1
    );
    let detail = query(
        &mut alice,
        None,
        Query::Message {
            message_id: message,
            receipts: Default::default(),
        },
    )
    .await;
    let delivery = &detail["receipts"]["items"][0]["delivery"];
    assert_eq!(delivery["state"], "delivered_late");
    assert_eq!(delivery["timed_out_at"], delivery["delivered_at"]);
    drop(observer);
    running.stop().await;
}
