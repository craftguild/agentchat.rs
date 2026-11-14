//! Full-body FIFO settlement, independent reads and generation/crash fences.
#[path = "message_streams/claude_uncertainty.rs"]
mod claude_uncertainty;
#[path = "message_streams/settlement.rs"]
mod settlement;
mod support;
#[path = "message_streams/uncertainty.rs"]
mod uncertainty;
use agentchat_core::{
    Error, ErrorCode, Result,
    application::{Application, Mutation},
    delivery::{HostReceipt, MessageDispatch, MessageProgress, MessageStream},
    model::{Context, DeliveryState, LaunchIdentity, LaunchOwnerState, Phase},
    ports::{HostGate, HostState, Storage},
    read::{PageRequest, Query},
};
use async_trait::async_trait;
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use support::{Fixture, id};
use uuid::Uuid;

struct Gate {
    host: Arc<support::SyntheticHost>,
    clock: Arc<support::TestClock>,
    advance_on_observe: Mutex<Option<i64>>,
    recovery: Mutex<agentchat_core::delivery::InputRecovery>,
    bindings: Mutex<HashMap<Uuid, LaunchIdentity>>,
    state: Mutex<LaunchOwnerState>,
    unavailable: AtomicBool,
    changed: Mutex<HashSet<Uuid>>,
}
impl Gate {
    fn new(f: &Fixture) -> Arc<Self> {
        Arc::new(Self {
            host: f.host.clone(),
            clock: f.clock.clone(),
            advance_on_observe: Mutex::new(None),
            recovery: Mutex::new(agentchat_core::delivery::InputRecovery::OriginalReceipt),
            bindings: Mutex::new(HashMap::new()),
            state: Mutex::new(LaunchOwnerState::Current),
            unavailable: AtomicBool::new(false),
            changed: Mutex::default(),
        })
    }
    fn bind(&self, context: &Context) -> LaunchIdentity {
        let identity = LaunchIdentity {
            generation: Uuid::new_v4(),
            owner: "synthetic-original-owner".into(),
        };
        self.bindings
            .lock()
            .unwrap()
            .insert(context.context_id, identity.clone());
        identity
    }
    fn reconnect(&self, context: &Context, identity: &LaunchIdentity) {
        self.bindings
            .lock()
            .unwrap()
            .insert(context.context_id, identity.clone());
    }
    fn app(self: &Arc<Self>, f: &Fixture) -> Application {
        Application::new(f.store.clone(), f.clock.clone(), self.clone())
    }
}
#[async_trait]
impl HostGate for Gate {
    fn input_capabilities(
        &self,
        _context: &agentchat_core::model::Context,
    ) -> agentchat_core::Result<agentchat_core::delivery::InputCapabilities> {
        use agentchat_core::delivery::{ActiveInput, InputCapabilities};
        Ok(InputCapabilities::complete(
            *self.recovery.lock().unwrap(),
            ActiveInput::ExactTurnId,
        ))
    }
    async fn verify(&self, context: &Context) -> Result<HostState> {
        if let Some(seconds) = self.advance_on_observe.lock().unwrap().take() {
            self.clock.advance(seconds);
        }
        if self.unavailable.load(Ordering::SeqCst) {
            return Err(Error::new(
                ErrorCode::RuntimeUnavailable,
                "Synthetic native observation is unavailable.",
            ));
        }
        self.host.verify(context).await
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
        *self.state.lock().unwrap()
    }
    fn launch_root_changed(
        &self,
        _project: Uuid,
        _host: agentchat_core::model::HostKind,
        _root: &str,
        identity: &LaunchIdentity,
    ) -> bool {
        self.changed.lock().unwrap().contains(&identity.generation)
    }
}

#[tokio::test]
async fn positive_root_departure_retires_a_live_owner_without_replay_or_receipt_loss() {
    let (f, gate, app, sender, context, identity) = setup().await;
    f.post(&sender, "@receiver accepted before native root change")
        .await;
    let accepted = app.prepare_message(&context).await.unwrap().unwrap();
    accept(&app, &context, &accepted).await;
    command(
        &app,
        &context,
        Mutation::Read {
            message_ids: vec![accepted.input.message_id],
        },
    )
    .await;
    f.post(&sender, "@receiver pending before native root change")
        .await;
    f.post(&sender, "later passive body cannot overtake").await;
    let pending = app.prepare_message(&context).await.unwrap().unwrap();
    published(&app, &context, &pending).await;
    gate.changed.lock().unwrap().insert(identity.generation);
    assert_eq!(*gate.state.lock().unwrap(), LaunchOwnerState::Current);
    assert_eq!(
        app.launch_state(&context).await.unwrap(),
        agentchat_core::model::LaunchState::Inactive
    );
    assert!(
        !app.message_is_current(
            &context,
            identity.generation,
            pending.token,
            pending.input.delivery_id
        )
        .await
        .unwrap()
    );
    f.clock.advance(2);
    assert!(app.prepare_message(&context).await.unwrap().is_none());
    assert!(
        !app.finish_message(&context, &pending, Ok(HostReceipt::default()))
            .await
            .unwrap()
    );
    assert_eq!(
        app.reconcile_launch(identity.generation)
            .await
            .unwrap()
            .result["retired"],
        true
    );
    let retired = stream(&f, &identity).await;
    assert!(retired.cancelled);
    assert_eq!(retired.settled_seq, accepted.input.seq);
    assert_eq!(retired.head.unwrap().token, pending.token);
    let mut snapshot = f.store.snapshot().await.unwrap();
    let agent = snapshot
        .conversation(context.host, &context.host_session_id)
        .await
        .unwrap()
        .unwrap();
    assert!(!agent.joined);
    assert_eq!(
        snapshot
            .latest_launch(agent.id)
            .await
            .unwrap()
            .unwrap()
            .retirement_reason
            .as_deref(),
        Some("root_changed")
    );
    drop(snapshot);
    assert_eq!(f.count("read_receipts").await, 1);
    assert!(app.start_launch(&context, None).await.is_err());
    f.post(&sender, "offline interval is not collected").await;
    let mut fresh = context.clone();
    fresh.context_id = Uuid::new_v4();
    fresh.incarnation = Uuid::new_v4();
    let next = gate.bind(&fresh);
    app.start_launch(&fresh, Some("receiver")).await.unwrap();
    assert!(stream(&f, &next).await.settled_seq > pending.input.seq);
    assert!(app.prepare_message(&fresh).await.unwrap().is_none());
    assert!(
        !app.finish_message(&context, &pending, Ok(HostReceipt::default()))
            .await
            .unwrap()
    );
    assert_eq!(f.count("read_receipts").await, 1);
}
async fn setup() -> (
    Fixture,
    Arc<Gate>,
    Application,
    Context,
    Context,
    LaunchIdentity,
) {
    let f = Fixture::new().await;
    let sender = f.context();
    f.join(&sender, "sender").await;
    let context = f.context();
    let gate = Gate::new(&f);
    let identity = gate.bind(&context);
    let app = gate.app(&f);
    app.initialize_with_settings(
        None,
        agentchat_core::project::ReceiptSettingsUpdate {
            auto_read: Some(false),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    app.start_launch(&context, Some("receiver")).await.unwrap();
    (f, gate, app, sender, context, identity)
}
async fn stream(f: &Fixture, identity: &LaunchIdentity) -> MessageStream {
    f.store
        .snapshot()
        .await
        .unwrap()
        .message_stream(identity.generation)
        .await
        .unwrap()
        .unwrap()
}
async fn accept(app: &Application, context: &Context, dispatch: &MessageDispatch) {
    assert!(
        app.finish_message(context, dispatch, Ok(HostReceipt::default()))
            .await
            .unwrap()
    );
}
fn failed() -> Result<MessageProgress> {
    Ok(MessageProgress::NotAccepted(Error::new(
        ErrorCode::RuntimeUnavailable,
        "Synthetic definite native refusal.",
    )))
}
async fn command(app: &Application, context: &Context, mutation: Mutation) {
    app.execute(Uuid::new_v4(), context, mutation)
        .await
        .unwrap();
}

// Body-focused cases explicitly consume setup notices before publishing bodies.
// Claiming and accepting the exact notice is real synthetic native settlement;
// this helper never searches ahead or discards a claimed message.
async fn drain_setup_notices(app: &Application, context: &Context) {
    while let Some(dispatch) = app.prepare_native(context).await.unwrap() {
        assert!(matches!(
            dispatch,
            agentchat_core::delivery::NativeDispatch::Notice(_)
        ));
        assert!(
            app.finish_native(
                context,
                &dispatch,
                Ok(MessageProgress::Accepted(HostReceipt::default()))
            )
            .await
            .unwrap()
        );
    }
}

async fn published(app: &Application, context: &Context, dispatch: &MessageDispatch) {
    assert!(
        app.finish_message_progress(context, dispatch, Ok(MessageProgress::AwaitingAcceptance))
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn channel_leave_fences_and_cancels_a_claimed_unsubmitted_body_without_rejoin_replay() {
    let (f, _gate, app, sender, context, identity) = setup().await;
    f.run(
        &sender,
        Mutation::ChannelCreate {
            name: "secret".into(),
        },
    )
    .await;
    command(
        &app,
        &context,
        Mutation::ChannelJoin {
            name: "secret".into(),
        },
    )
    .await;
    f.run(
        &sender,
        Mutation::ChannelJoin {
            name: "secret".into(),
        },
    )
    .await;
    drain_setup_notices(&app, &context).await;
    f.run(
        &sender,
        Mutation::Post {
            channel: Some("secret".into()),
            body: "@receiver old secret body".into(),
        },
    )
    .await;
    let dispatch = app.prepare_message(&context).await.unwrap().unwrap();
    assert!(
        app.message_is_current(
            &context,
            identity.generation,
            dispatch.token,
            dispatch.input.delivery_id,
        )
        .await
        .unwrap()
    );
    command(
        &app,
        &context,
        Mutation::ChannelLeave {
            name: "secret".into(),
        },
    )
    .await;
    assert!(
        !app.message_is_current(
            &context,
            identity.generation,
            dispatch.token,
            dispatch.input.delivery_id,
        )
        .await
        .unwrap()
    );
    command(
        &app,
        &context,
        Mutation::ChannelJoin {
            name: "secret".into(),
        },
    )
    .await;
    assert!(
        !app.finish_message_progress(&context, &dispatch, failed())
            .await
            .unwrap()
    );
    let state = stream(&f, &identity).await;
    assert!(state.head.is_none());
    assert_eq!(state.settled_seq, dispatch.input.seq);
    let row = f
        .rows("deliveries")
        .await
        .into_iter()
        .find(|row| {
            row.try_get::<String>("", "id").unwrap() == dispatch.input.delivery_id.to_string()
        })
        .unwrap();
    assert_eq!(row.try_get::<String>("", "state").unwrap(), "failed_left");

    assert!(
        !app.message_is_current(
            &context,
            identity.generation,
            dispatch.token,
            dispatch.input.delivery_id,
        )
        .await
        .unwrap()
    );
    f.run(
        &sender,
        Mutation::Post {
            channel: Some("secret".into()),
            body: "@receiver fresh after rejoin".into(),
        },
    )
    .await;
    assert_eq!(
        app.prepare_message(&context)
            .await
            .unwrap()
            .unwrap()
            .input
            .body,
        "@receiver fresh after rejoin"
    );
}

#[tokio::test]
async fn channel_departure_retires_the_original_receipt_lane_without_backlog_transfer() {
    let (f, _gate, app, sender, context, identity) = setup().await;
    f.run(
        &sender,
        Mutation::ChannelCreate {
            name: "secret".into(),
        },
    )
    .await;
    command(
        &app,
        &context,
        Mutation::ChannelJoin {
            name: "secret".into(),
        },
    )
    .await;
    f.run(
        &sender,
        Mutation::ChannelJoin {
            name: "secret".into(),
        },
    )
    .await;
    drain_setup_notices(&app, &context).await;
    f.run(
        &sender,
        Mutation::Post {
            channel: Some("secret".into()),
            body: "@receiver published before leave".into(),
        },
    )
    .await;
    let dispatch = app.prepare_message(&context).await.unwrap().unwrap();
    published(&app, &context, &dispatch).await;
    command(
        &app,
        &context,
        Mutation::ChannelLeave {
            name: "secret".into(),
        },
    )
    .await;
    command(
        &app,
        &context,
        Mutation::ChannelJoin {
            name: "secret".into(),
        },
    )
    .await;
    f.clock.advance(2);
    assert!(app.prepare_message(&context).await.unwrap().is_none());
    assert!(
        !app.finish_message(&context, &dispatch, Ok(HostReceipt::default()))
            .await
            .unwrap()
    );
    let state = stream(&f, &identity).await;
    assert!(state.head.is_none());
    assert_eq!(state.settled_seq, dispatch.input.seq);
    let mut tx = f.store.begin().await.unwrap();
    let departed = tx
        .delivery(dispatch.input.delivery_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(departed.state, DeliveryState::FailedLeft);
    assert!(departed.history.left_at.is_some());
    assert!(departed.history.delivered_at.is_none());
    tx.rollback().await.unwrap();
    f.run(
        &sender,
        Mutation::Post {
            channel: Some("secret".into()),
            body: "@receiver fresh membership only".into(),
        },
    )
    .await;
    let next = app.prepare_message(&context).await.unwrap().unwrap();
    assert_eq!(next.input.body, "@receiver fresh membership only");
    assert_ne!(next.input.delivery_id, dispatch.input.delivery_id);
    let mut tx = f.store.begin().await.unwrap();
    assert_eq!(
        tx.delivery(dispatch.input.delivery_id)
            .await
            .unwrap()
            .unwrap(),
        departed
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn channel_departure_retires_a_claimed_notice_and_refuses_its_obsolete_callback() {
    let (f, _gate, app, sender, context, identity) = setup().await;
    f.run(
        &sender,
        Mutation::ChannelCreate {
            name: "secret".into(),
        },
    )
    .await;
    command(
        &app,
        &context,
        Mutation::ChannelJoin {
            name: "secret".into(),
        },
    )
    .await;
    f.run(
        &sender,
        Mutation::ChannelJoin {
            name: "secret".into(),
        },
    )
    .await;
    let dispatch = app.prepare_native(&context).await.unwrap().unwrap();
    assert!(matches!(
        &dispatch,
        agentchat_core::delivery::NativeDispatch::Notice(_)
    ));
    assert!(
        app.native_is_current(
            &context,
            identity.generation,
            dispatch.token(),
            dispatch.delivery_id(),
        )
        .await
        .unwrap()
    );
    command(
        &app,
        &context,
        Mutation::ChannelLeave {
            name: "secret".into(),
        },
    )
    .await;
    assert!(
        !app.native_is_current(
            &context,
            identity.generation,
            dispatch.token(),
            dispatch.delivery_id(),
        )
        .await
        .unwrap()
    );
    command(
        &app,
        &context,
        Mutation::ChannelJoin {
            name: "secret".into(),
        },
    )
    .await;
    assert!(
        !app.native_is_current(
            &context,
            identity.generation,
            dispatch.token(),
            dispatch.delivery_id(),
        )
        .await
        .unwrap()
    );
    assert!(
        !app.finish_native(&context, &dispatch, failed())
            .await
            .unwrap()
    );
    assert!(stream(&f, &identity).await.head.is_none());
}

#[tokio::test]
async fn body_only_api_holds_a_published_notice_until_its_original_native_receipt_settles() {
    let (f, _gate, app, sender, context, identity) = setup().await;
    f.run(
        &sender,
        Mutation::Reserve {
            patterns: vec!["src/ordered-notice/**".into()],
            ttl_seconds: None,
            reason: None,
        },
    )
    .await;
    let notice = app.prepare_native(&context).await.unwrap().unwrap();
    assert!(matches!(
        notice,
        agentchat_core::delivery::NativeDispatch::Notice(_)
    ));
    app.finish_native(&context, &notice, Ok(MessageProgress::AwaitingAcceptance))
        .await
        .unwrap();
    f.post(&sender, "@receiver body must remain behind notice")
        .await;
    f.clock.advance(2);
    let original = stream(&f, &identity).await;
    assert!(app.prepare_message(&context).await.unwrap().is_none());
    assert_eq!(stream(&f, &identity).await, original);
    let poll = app.prepare_native(&context).await.unwrap().unwrap();
    assert!(poll.awaiting_acceptance());
    assert_eq!(poll.token(), notice.token());
    assert_eq!(poll.delivery_id(), notice.delivery_id());
    app.finish_native(
        &context,
        &poll,
        Ok(MessageProgress::Accepted(HostReceipt::default())),
    )
    .await
    .unwrap();
    let body = app.prepare_message(&context).await.unwrap().unwrap();
    assert_eq!(body.input.body, "@receiver body must remain behind notice");
    assert!(body.input.seq > notice.seq());
    assert_eq!(f.count("read_receipts").await, 0);
}

#[tokio::test]
#[ignore = "owned subprocess; pending backup parent invokes this exact cut"]
async fn pending_backup_child() {
    struct Crash;
    impl agentchat_store::BackupProgress for Crash {
        fn reached(&self, stage: agentchat_store::BackupStage) {
            if matches!(
                stage,
                agentchat_store::BackupStage::CopyChunk {
                    table: agentchat_store::BackupTable::MessageStreams,
                    rows: 1,
                }
            ) {
                std::process::exit(86);
            }
        }
    }
    let project =
        Uuid::parse_str(&std::env::var("AGENTCHAT_TEST_PENDING_PROJECT").unwrap()).unwrap();
    let store = agentchat_store::Store::open(
        std::path::PathBuf::from(std::env::var_os("AGENTCHAT_TEST_PENDING_SOURCE").unwrap()),
        agentchat_core::project::ProjectIdentity {
            id: project,
            scope: agentchat_core::project::Scope::Directory,
        },
    )
    .await
    .unwrap();
    store
        .backup_observed(
            std::path::PathBuf::from(std::env::var_os("AGENTCHAT_TEST_PENDING_BACKUP").unwrap()),
            Arc::new(Crash),
        )
        .await
        .unwrap();
    panic!("Pending-input backup did not reach its selected crash cut");
}

#[tokio::test]
async fn pending_backup_copy_process_exit_preserves_the_live_head_and_retry() {
    let (f, _gate, app, sender, context, identity) = setup().await;
    f.post(
        &sender,
        "@receiver pending body survives backup interruption",
    )
    .await;
    let dispatch = app.prepare_message(&context).await.unwrap().unwrap();
    published(&app, &context, &dispatch).await;
    let expected = stream(&f, &identity).await;
    let backup = f.root.path().join("pending-cut.sqlite3");
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "pending_backup_child"])
        .env(
            "AGENTCHAT_TEST_PENDING_PROJECT",
            context.project_id.to_string(),
        )
        .env(
            "AGENTCHAT_TEST_PENDING_SOURCE",
            f.root.path().join("agentchat.sqlite3"),
        )
        .env("AGENTCHAT_TEST_PENDING_BACKUP", &backup)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("Pending-input backup child exceeded its bound");
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    };
    assert_eq!(status.code(), Some(86));
    assert!(!backup.exists());
    f.store.integrity_check().await.unwrap();
    assert_eq!(stream(&f, &identity).await, expected);
    f.store.backup(&backup).await.unwrap();
    let copy = agentchat_store::Store::open(&backup, f.store.identity().clone())
        .await
        .unwrap();
    assert_eq!(
        copy.snapshot()
            .await
            .unwrap()
            .message_stream(identity.generation)
            .await
            .unwrap()
            .unwrap(),
        expected
    );
    assert_eq!(f.count("read_receipts").await, 0);
}

#[tokio::test]
async fn published_head_waits_without_advancing_or_spending_attempts_and_backup_retains_it() {
    let (f, gate, app, sender, context, identity) = setup().await;
    f.post(&sender, "@receiver retained complete first body")
        .await;
    f.post(&sender, "later passive body").await;
    let before = stream(&f, &identity).await.settled_seq;
    let original = app.prepare_message(&context).await.unwrap().unwrap();
    assert!(!original.awaiting_acceptance);
    published(&app, &context, &original).await;
    let pending = stream(&f, &identity).await;
    assert_eq!(pending.settled_seq, before);
    assert!(pending.head.as_ref().unwrap().awaiting_acceptance);
    let backup = f.root.path().join("pending-native-input.sqlite3");
    f.store.backup(&backup).await.unwrap();
    let copy = agentchat_store::Store::open(&backup, f.store.identity().clone())
        .await
        .unwrap();
    assert_eq!(
        copy.snapshot()
            .await
            .unwrap()
            .message_stream(identity.generation)
            .await
            .unwrap()
            .unwrap(),
        pending
    );

    // A healthy launch may wait longer than three submission windows. Polling
    // never republishes, changes identity or consumes an attempt.
    for _ in 0..6 {
        f.clock.advance(6);
        app.heartbeat(&context).await.unwrap();
        let poll = app.prepare_message(&context).await.unwrap().unwrap();
        assert!(poll.awaiting_acceptance);
        assert_eq!(poll.input, original.input);
        assert_eq!(poll.token, original.token);
        assert_eq!(poll.attempt, 1);
        assert!(
            app.message_is_current(
                &context,
                identity.generation,
                poll.token,
                poll.input.delivery_id
            )
            .await
            .unwrap()
        );
        let status = app
            .query(Some(&context), Query::Whoami)
            .await
            .map(|value| serde_json::to_value(value).unwrap())
            .unwrap();
        assert_eq!(status["native_delivery"]["state"], "pending");
        assert_eq!(
            status["native_delivery"]["head"]["awaiting_acceptance"],
            true
        );
        published(&app, &context, &poll).await;
        assert_eq!(stream(&f, &identity).await.settled_seq, before);
    }
    f.host.phase(&context, Phase::Paused);
    app.heartbeat(&context).await.unwrap();
    assert!(app.prepare_message(&context).await.unwrap().is_none());
    f.clock.advance(1);
    gate.unavailable.store(true, Ordering::SeqCst);
    let receipt_poll = app.prepare_message(&context).await.unwrap().unwrap();
    assert!(receipt_poll.awaiting_acceptance);
    assert_eq!(receipt_poll.input, original.input);
    assert_eq!(receipt_poll.token, original.token);
    assert_eq!(receipt_poll.attempt, original.attempt);
    assert!(
        !app.message_is_current(
            &context,
            identity.generation,
            original.token,
            original.input.delivery_id
        )
        .await
        .unwrap()
    );
    accept(&app, &context, &receipt_poll).await;
    assert_eq!(stream(&f, &identity).await.settled_seq, original.input.seq);
    assert_eq!(f.count("read_receipts").await, 0);
    f.host.phase(&context, Phase::Idle);
    gate.unavailable.store(false, Ordering::SeqCst);
    app.heartbeat(&context).await.unwrap();
    let next = app.prepare_message(&context).await.unwrap().unwrap();
    assert_eq!(next.input.body, "later passive body");
    assert_eq!(next.attempt, 1);
}

#[tokio::test]
async fn third_attempt_publication_is_pending_until_explicit_failure_not_a_clock_timeout() {
    let (f, _gate, app, sender, context, _identity) = setup().await;
    f.post(&sender, "@receiver bounded actual submission failures")
        .await;
    for attempt in 1..=2 {
        let dispatch = app.prepare_message(&context).await.unwrap().unwrap();
        assert_eq!(dispatch.attempt, attempt);
        assert!(
            app.finish_message_progress(&context, &dispatch, failed())
                .await
                .unwrap()
        );
        f.clock.advance(6);
    }
    let third = app.prepare_message(&context).await.unwrap().unwrap();
    assert_eq!(third.attempt, 3);
    published(&app, &context, &third).await;
    f.clock.advance(6);
    app.heartbeat(&context).await.unwrap();
    let status = app
        .query(Some(&context), Query::Whoami)
        .await
        .map(|value| serde_json::to_value(value).unwrap())
        .unwrap();
    assert_eq!(status["native_delivery"]["state"], "pending");
    let poll = app.prepare_message(&context).await.unwrap().unwrap();
    assert_eq!(poll.token, third.token);
    assert_eq!(poll.attempt, 3);
    assert!(poll.awaiting_acceptance);
    assert!(
        app.finish_message_progress(&context, &poll, failed())
            .await
            .unwrap()
    );
    let status = app
        .query(Some(&context), Query::Whoami)
        .await
        .map(|value| serde_json::to_value(value).unwrap())
        .unwrap();
    assert_eq!(status["native_delivery"]["state"], "stalled");
    assert_eq!(
        status["native_delivery"]["head"]["awaiting_acceptance"],
        false
    );
    f.clock.advance(6);
    assert!(app.prepare_message(&context).await.unwrap().is_none());
}

#[tokio::test]
async fn reconnect_polls_the_original_publication_before_authorizing_a_retry() {
    let (f, gate, app, sender, context, identity) = setup().await;
    f.post(&sender, "@receiver original locally published body")
        .await;
    let original = app.prepare_message(&context).await.unwrap().unwrap();
    published(&app, &context, &original).await;
    let mut next = context.clone();
    next.context_id = Uuid::new_v4();
    next.incarnation = Uuid::new_v4();
    gate.reconnect(&next, &identity);
    app.invalidate_connections().await.unwrap();
    app.attach(&next).await.unwrap().unwrap();
    let before = stream(&f, &identity).await;
    assert!(
        !app.finish_message_progress(&context, &original, Ok(MessageProgress::AwaitingAcceptance))
            .await
            .unwrap()
    );
    assert_eq!(stream(&f, &identity).await, before);
    f.clock.advance(2);
    let poll = app.prepare_message(&next).await.unwrap().unwrap();
    assert_eq!(poll.token, original.token);
    assert_eq!(poll.attempt, original.attempt);
    assert!(poll.awaiting_acceptance);
    assert_eq!(stream(&f, &identity).await, before);
    // Only the current connection's receipt-only result can resolve this old
    // publication as unknown/refused. It cannot emit another native input.
    assert!(
        app.finish_message_progress(&next, &poll, failed())
            .await
            .unwrap()
    );
    f.clock.advance(6);
    let replacement = app.prepare_message(&next).await.unwrap().unwrap();
    assert_eq!(replacement.input, original.input);
    assert_eq!(replacement.attempt, 2);
    assert!(!replacement.awaiting_acceptance);
    assert_ne!(replacement.token, original.token);
    assert!(
        !app.finish_message(&context, &original, Ok(HostReceipt::default()))
            .await
            .unwrap()
    );
    published(&app, &next, &replacement).await;
    accept(&app, &next, &replacement).await;
    assert!(app.prepare_message(&next).await.unwrap().is_none());
}

#[tokio::test]
async fn reconnect_accepts_an_original_receipt_without_native_phase_or_another_attempt() {
    let (f, gate, app, sender, context, identity) = setup().await;
    f.post(&sender, "@receiver accepted before the connection changed")
        .await;
    f.post(&sender, "passive successor").await;
    let original = app.prepare_message(&context).await.unwrap().unwrap();
    published(&app, &context, &original).await;
    let mut next = context.clone();
    next.context_id = Uuid::new_v4();
    next.incarnation = Uuid::new_v4();
    gate.reconnect(&next, &identity);
    app.invalidate_connections().await.unwrap();
    app.attach(&next).await.unwrap().unwrap();
    gate.unavailable.store(true, Ordering::SeqCst);
    f.clock.advance(2);
    let poll = app.prepare_message(&next).await.unwrap().unwrap();
    assert_eq!(poll.input, original.input);
    assert_eq!(poll.token, original.token);
    assert_eq!(poll.attempt, original.attempt);
    assert!(poll.awaiting_acceptance);
    assert!(
        !app.message_is_current(
            &next,
            identity.generation,
            poll.token,
            poll.input.delivery_id
        )
        .await
        .unwrap()
    );
    accept(&app, &next, &poll).await;
    let accepted = stream(&f, &identity).await;
    assert_eq!(accepted.settled_seq, original.input.seq);
    assert!(accepted.head.is_none());
    assert_eq!(f.count("read_receipts").await, 0);
    gate.unavailable.store(false, Ordering::SeqCst);
    let successor = app.prepare_message(&next).await.unwrap().unwrap();
    assert_eq!(successor.input.body, "passive successor");
    assert_eq!(successor.attempt, 1);
}

#[tokio::test]
async fn released_v5_namespace_is_refused_without_conversion_or_backup() {
    old_namespace_is_refused(5).await;
}

#[tokio::test]
async fn accepted_input_survives_pause_and_reconnect_and_confirms_arrival_without_explicit_read() {
    let (f, gate, app, sender, context, identity) = setup().await;
    f.timeout(2).await;
    let posted = f
        .run(
            &sender,
            Mutation::Post {
                channel: Some("dev".into()),
                body: "@receiver queued native input".into(),
            },
        )
        .await;
    let message = id(&posted.result, "message_id");
    f.timeout(60).await;
    f.post(&sender, "later passive body").await;
    f.host.turn(&context, Some("still-running-native-turn"));
    app.heartbeat(&context).await.unwrap();
    let first = app.prepare_message(&context).await.unwrap().unwrap();
    assert_eq!(first.input.message_id, message);

    // The native queue has accepted the body, but a user pause reaches durable
    // session state before its receipt is committed. No model start/answer is
    // supplied: queue acceptance alone must complete the original handoff.
    f.host.phase(&context, Phase::Paused);
    app.heartbeat(&context).await.unwrap();
    accept(&app, &context, &first).await;
    let accepted = stream(&f, &identity).await;
    assert_eq!(accepted.settled_seq, first.input.seq);
    assert!(accepted.head.is_none());
    assert!(app.prepare_message(&context).await.unwrap().is_none());
    assert_eq!(f.count("read_receipts").await, 0);
    let detail = || Query::Message {
        message_id: message,
        receipts: PageRequest::default(),
    };
    assert_eq!(
        app.query(None, detail())
            .await
            .map(|value| serde_json::to_value(value).unwrap())
            .unwrap()["receipts"]["items"][0]["delivery"]["state"],
        "delivered"
    );
    f.clock.advance(3);
    assert_eq!(
        app.maintain().await.unwrap().delivery_confirmation_timeouts,
        0
    );
    assert_eq!(
        app.query(None, detail())
            .await
            .map(|value| serde_json::to_value(value).unwrap())
            .unwrap()["receipts"]["items"][0]["delivery"]["state"],
        "delivered"
    );
    assert_eq!(stream(&f, &identity).await, accepted);

    app.invalidate_connections().await.unwrap();
    let reopened = Arc::new(
        agentchat_store::Store::open(
            f.root.path().join("agentchat.sqlite3"),
            f.store.identity().clone(),
        )
        .await
        .unwrap(),
    );
    let app = Application::new(reopened, f.clock.clone(), gate.clone());
    let mut next_context = context.clone();
    next_context.context_id = Uuid::new_v4();
    next_context.incarnation = Uuid::new_v4();
    gate.reconnect(&next_context, &identity);
    // A fresh native observation represents ordinary user continuation. Merely
    // reopening storage or reconnecting does not manufacture this control fact.
    f.host.phase(&next_context, Phase::Idle);
    app.attach(&next_context).await.unwrap().unwrap();
    let next = app.prepare_message(&next_context).await.unwrap().unwrap();
    assert_eq!(next.input.body, "later passive body");
    assert_eq!(next.attempt, 1);
    assert!(next.input.seq > first.input.seq);
    assert!(
        !app.finish_message(&context, &first, Ok(HostReceipt::default()))
            .await
            .unwrap()
    );
    assert_eq!(f.count("read_receipts").await, 0);
    let mut tx = f.store.begin().await.unwrap();
    let delivery = tx.delivery(first.input.delivery_id).await.unwrap().unwrap();
    assert_eq!(delivery.state, DeliveryState::Delivered);
    assert!(delivery.history.delivered_at.is_some());
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn settled_cursor_survives_owner_loss_and_cannot_be_replayed_into_another_root() {
    let (f, gate, app, sender, context, identity) = setup().await;
    f.post(&sender, "@receiver accepted before the root switch")
        .await;
    let accepted = app.prepare_message(&context).await.unwrap().unwrap();
    accept(&app, &context, &accepted).await;
    f.post(&sender, "@receiver unaccepted at owner exit").await;
    let pending = app.prepare_message(&context).await.unwrap().unwrap();
    *gate.state.lock().unwrap() = LaunchOwnerState::Gone;
    app.reconcile_launch(identity.generation).await.unwrap();
    let retired = stream(&f, &identity).await;
    assert!(retired.cancelled);
    assert_eq!(retired.settled_seq, accepted.input.seq);
    assert_eq!(retired.head.as_ref().unwrap().seq, pending.input.seq);

    let mut other_root = f.context();
    other_root.working_directory = context.working_directory.clone();
    let successor = gate.bind(&other_root);
    *gate.state.lock().unwrap() = LaunchOwnerState::Current;
    app.start_launch(&other_root, Some("different-root"))
        .await
        .unwrap();
    let boundary = stream(&f, &successor).await;
    assert!(boundary.settled_seq > pending.input.seq);
    assert!(app.prepare_message(&other_root).await.unwrap().is_none());
    for old in [&accepted, &pending] {
        assert!(
            !app.finish_message(&context, old, Ok(HostReceipt::default()))
                .await
                .unwrap()
        );
        assert!(
            !app.finish_message(&other_root, old, Ok(HostReceipt::default()))
                .await
                .unwrap()
        );
    }
    assert_eq!(stream(&f, &identity).await, retired);
    assert_eq!(stream(&f, &successor).await, boundary);
    f.post(&sender, "newly addressed to @different-root").await;
    let next = app.prepare_message(&other_root).await.unwrap().unwrap();
    assert_eq!(next.input.body, "newly addressed to @different-root");
    assert_eq!(next.input.generation, successor.generation);
    assert_eq!(f.count("read_receipts").await, 0);
}

#[tokio::test]
async fn alternating_complete_bodies_follow_events_and_delivery_never_implies_read() {
    let (f, _gate, app, sender, context, identity) = setup().await;
    let agent = f
        .store
        .snapshot()
        .await
        .unwrap()
        .conversation(context.host, &context.host_session_id)
        .await
        .unwrap()
        .unwrap()
        .id;
    let huge = format!("@receiver {}", "文🙂\\\n\t\"".repeat(5000));
    let bodies = [
        huge,
        "passive Markdown\n\n**full body**\n".into(),
        "@receiver third body".into(),
    ];
    let mut ids = Vec::new();
    for (index, body) in bodies.iter().enumerate() {
        let posted = if index == 2 {
            f.run(
                &sender,
                Mutation::Reply {
                    read_message_ids: Vec::new(),
                    root_message_id: ids[0],
                    body: body.clone(),
                },
            )
            .await
        } else {
            f.post(&sender, body).await
        };
        ids.push(id(&posted.result, "message_id"));
    }
    let mut last = stream(&f, &identity).await.settled_seq;
    for (index, body) in bodies.iter().enumerate() {
        let dispatch = app.prepare_message(&context).await.unwrap().unwrap();
        assert_eq!(dispatch.input.body, body.trim());
        assert_eq!(dispatch.input.message_id, ids[index]);
        assert_eq!(
            dispatch.input.thread_id,
            if index == 2 { ids[0] } else { ids[index] }
        );
        assert_eq!(dispatch.input.author.handle, "sender");
        assert_eq!(dispatch.input.channel, "dev");
        assert_eq!(dispatch.input.addressed, index != 1);
        assert!(dispatch.input.seq > last);
        assert!(app.prepare_message(&context).await.unwrap().is_none());
        assert!(
            app.message_is_current(
                &context,
                identity.generation,
                dispatch.token,
                dispatch.input.delivery_id
            )
            .await
            .unwrap()
        );
        accept(&app, &context, &dispatch).await;
        assert!(
            !app.message_is_current(
                &context,
                identity.generation,
                dispatch.token,
                dispatch.input.delivery_id
            )
            .await
            .unwrap()
        );
        assert!(
            !app.finish_message(&context, &dispatch, Ok(HostReceipt::default()))
                .await
                .unwrap()
        );
        assert!(
            f.store
                .snapshot()
                .await
                .unwrap()
                .receipt(ids[index], agent)
                .await
                .unwrap()
                .is_none()
        );
        let mut tx = f.store.begin().await.unwrap();
        let row = tx
            .delivery(dispatch.input.delivery_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.state, DeliveryState::Delivered);
        assert!(row.history.delivered_at.is_some());
        tx.rollback().await.unwrap();
        last = dispatch.input.seq;
    }
    assert!(app.prepare_message(&context).await.unwrap().is_none());
    assert_eq!(stream(&f, &identity).await.settled_seq, last);
    command(
        &app,
        &context,
        Mutation::Post {
            channel: Some("dev".into()),
            body: "own echo".into(),
        },
    )
    .await;
    assert!(app.prepare_message(&context).await.unwrap().is_none());
}

#[tokio::test]
async fn failed_head_survives_explicit_read_reopen_and_newer_passive_messages() {
    let (f, gate, app, sender, context, identity) = setup().await;
    f.post(&sender, "@receiver head").await;
    f.post(&sender, "later passive body").await;
    let first = app.prepare_message(&context).await.unwrap().unwrap();
    let boundary = stream(&f, &identity).await.settled_seq;
    assert!(
        app.finish_message_progress(&context, &first, failed())
            .await
            .unwrap()
    );
    command(
        &app,
        &context,
        Mutation::Read {
            message_ids: vec![first.input.message_id],
        },
    )
    .await;
    assert_eq!(stream(&f, &identity).await.settled_seq, boundary);
    assert!(app.prepare_message(&context).await.unwrap().is_none());
    f.clock.advance(6);
    let reopened = Arc::new(
        agentchat_store::Store::open(
            f.root.path().join("agentchat.sqlite3"),
            f.store.identity().clone(),
        )
        .await
        .unwrap(),
    );
    let app = Application::new(reopened.clone(), f.clock.clone(), gate);
    let retry = app.prepare_message(&context).await.unwrap().unwrap();
    assert_eq!(retry.input, first.input);
    assert_eq!(retry.attempt, 2);
    assert_ne!(retry.token, first.token);
    assert!(
        !app.finish_message(&context, &first, Ok(HostReceipt::default()))
            .await
            .unwrap()
    );
    accept(&app, &context, &retry).await;
    let next = app.prepare_message(&context).await.unwrap().unwrap();
    assert_eq!(next.input.body, "later passive body");
    assert!(next.input.seq > retry.input.seq);
    assert!(!next.input.addressed);
    let mut tx = reopened.begin().await.unwrap();
    assert_eq!(
        tx.delivery(first.input.delivery_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        DeliveryState::Delivered
    );
}

#[tokio::test]
async fn reconnect_fences_failures_and_replaced_tokens_without_resetting_the_head_budget() {
    let (f, gate, app, sender, context, identity) = setup().await;
    *gate.recovery.lock().unwrap() = agentchat_core::delivery::InputRecovery::RetainUncertainty;
    f.post(&sender, "@receiver first").await;
    f.post(&sender, "second").await;
    let first = app.prepare_message(&context).await.unwrap().unwrap();
    // Only a definite negative result permits replacement after reconnect.
    // The separate uncertainty cases retain orphaned original attempts.
    app.finish_message_progress(&context, &first, failed())
        .await
        .unwrap();
    let mut next_context = context.clone();
    next_context.context_id = Uuid::new_v4();
    next_context.incarnation = Uuid::new_v4();
    gate.reconnect(&next_context, &identity);
    app.invalidate_connections().await.unwrap();
    app.attach(&next_context).await.unwrap().unwrap();
    assert!(
        !app.finish_message_progress(&context, &first, failed())
            .await
            .unwrap()
    );
    assert!(app.prepare_message(&next_context).await.unwrap().is_none());
    f.clock.advance(6);
    let second = app.prepare_message(&next_context).await.unwrap().unwrap();
    assert_eq!(first.input, second.input);
    assert_eq!(second.attempt, 2);
    assert!(
        !app.finish_message(&context, &first, Ok(HostReceipt::default()))
            .await
            .unwrap()
    );
    assert!(
        app.finish_message_progress(&next_context, &second, failed())
            .await
            .unwrap()
    );
    f.clock.advance(6);
    let third = app.prepare_message(&next_context).await.unwrap().unwrap();
    assert_eq!(third.input, first.input);
    assert_eq!(third.attempt, 3);
    f.clock.advance(6); // A killed third submission remains the blocked head.
    assert!(app.prepare_message(&next_context).await.unwrap().is_none());
    command(
        &app,
        &next_context,
        Mutation::Read {
            message_ids: vec![first.input.message_id],
        },
    )
    .await;
    assert!(app.prepare_message(&next_context).await.unwrap().is_none());
    assert_eq!(stream(&f, &identity).await.head.unwrap().attempts, 3);
    let status = app
        .query(Some(&next_context), Query::Whoami)
        .await
        .map(|value| serde_json::to_value(value).unwrap())
        .unwrap();
    assert_eq!(status["native_delivery"]["state"], "acceptance_unknown");
    assert_eq!(
        status["native_delivery"]["head"]["delivery_id"],
        first.input.delivery_id.to_string()
    );
}

#[tokio::test]
async fn known_native_acceptance_after_reconnect_completes_only_its_unchanged_head() {
    let (f, gate, app, sender, context, identity) = setup().await;
    f.post(&sender, "@receiver already accepted in native queue")
        .await;
    f.post(&sender, "next FIFO body").await;
    let first = app.prepare_message(&context).await.unwrap().unwrap();
    let mut connection = context.clone();
    connection.context_id = Uuid::new_v4();
    connection.incarnation = Uuid::new_v4();
    gate.reconnect(&connection, &identity);
    app.invalidate_connections().await.unwrap();
    app.attach(&connection).await.unwrap().unwrap();
    let before = stream(&f, &identity).await;
    assert!(
        !app.finish_message_progress(
            &context,
            &first,
            Err(Error::new(
                ErrorCode::RuntimeUnavailable,
                "Synthetic ambiguous result from the retired connection.",
            )),
        )
        .await
        .unwrap()
    );
    assert_eq!(stream(&f, &identity).await, before);

    // This completion is the original port's known input result, delivered
    // after a same-generation reconnect but before another attempt replaced
    // its durable token. Requiring the new connection would cause retransmit.
    accept(&app, &context, &first).await;
    let accepted = stream(&f, &identity).await;
    assert_eq!(accepted.settled_seq, first.input.seq);
    assert!(accepted.head.is_none());
    assert!(
        !app.finish_message(&context, &first, Ok(HostReceipt::default()))
            .await
            .unwrap()
    );
    let next = app.prepare_message(&connection).await.unwrap().unwrap();
    assert_eq!(next.input.body, "next FIFO body");
    assert_eq!(next.attempt, 1);
    assert_eq!(f.count("read_receipts").await, 0);
}

#[tokio::test]
async fn released_v4_namespace_is_refused_without_conversion_or_backup() {
    old_namespace_is_refused(4).await;
}

#[tokio::test]
async fn grouped_message_completions_isolate_bad_receipts_from_valid_heads_and_mutations() {
    use agentchat_core::{
        application::{CommitOutcome, CommitRequest, MutationRequest},
        delivery::MessageCompletion,
    };
    let (f, gate, app, sender, context, identity) = setup().await;
    let other = f.context();
    let other_identity = gate.bind(&other);
    app.start_launch(&other, Some("other")).await.unwrap();
    drain_setup_notices(&app, &context).await;
    drain_setup_notices(&app, &other).await;
    f.post(&sender, "@receiver @other shared original body")
        .await;
    let first = app.prepare_message(&context).await.unwrap().unwrap();
    let second = app.prepare_message(&other).await.unwrap().unwrap();
    let first_before = stream(&f, &identity).await;
    let outcomes = app
        .commit_ready(vec![
            CommitRequest::MessageCompletion(MessageCompletion {
                context: context.clone(),
                dispatch: first.clone(),
                outcome: Ok(MessageProgress::Accepted(HostReceipt {
                    turn_id: Some("x".repeat(257)),
                })),
            }),
            CommitRequest::Mutation(MutationRequest {
                request_id: Uuid::new_v4(),
                context: other.clone(),
                command: Mutation::Post {
                    channel: Some("dev".into()),
                    body: "independent committed message".into(),
                },
            }),
            CommitRequest::MessageCompletion(MessageCompletion {
                context: other.clone(),
                dispatch: second.clone(),
                outcome: Ok(MessageProgress::Accepted(HostReceipt::default())),
            }),
        ])
        .await
        .unwrap();
    assert_eq!(
        outcomes[0].as_ref().unwrap_err().code,
        ErrorCode::IntegrityError
    );
    assert!(matches!(outcomes[1], Ok(CommitOutcome::Mutation(_))));
    assert!(matches!(
        outcomes[2],
        Ok(CommitOutcome::MessageCompletion(true))
    ));
    assert_eq!(stream(&f, &identity).await, first_before);
    let accepted = stream(&f, &other_identity).await;
    assert_eq!(accepted.settled_seq, second.input.seq);
    assert!(accepted.head.is_none());
    accept(&app, &context, &first).await;
    assert_eq!(f.count("read_receipts").await, 0);
}

#[tokio::test]
async fn unknown_preserves_work_and_confirmed_loss_cancels_before_a_fresh_boundary() {
    let (f, gate, app, sender, context, identity) = setup().await;
    f.post(&sender, "@receiver old generation").await;
    let first = app.prepare_message(&context).await.unwrap().unwrap();
    let before = stream(&f, &identity).await;
    *gate.state.lock().unwrap() = LaunchOwnerState::Unknown;
    assert!(app.prepare_message(&context).await.unwrap().is_none());
    assert!(
        !app.finish_message(&context, &first, Ok(HostReceipt::default()))
            .await
            .unwrap()
    );
    assert!(
        !app.message_is_current(
            &context,
            identity.generation,
            first.token,
            first.input.delivery_id
        )
        .await
        .unwrap()
    );
    app.reconcile_launch(identity.generation).await.unwrap();
    assert_eq!(stream(&f, &identity).await, before);
    *gate.state.lock().unwrap() = LaunchOwnerState::Gone;
    app.reconcile_launch(identity.generation).await.unwrap();
    let retired = stream(&f, &identity).await;
    assert!(retired.cancelled);
    assert_eq!(retired.head, before.head);
    f.post(&sender, "inactive interval").await;
    let mut next_context = context.clone();
    next_context.context_id = Uuid::new_v4();
    next_context.incarnation = Uuid::new_v4();
    let successor = gate.bind(&next_context);
    *gate.state.lock().unwrap() = LaunchOwnerState::Current;
    app.start_launch(&next_context, Some("receiver"))
        .await
        .unwrap();
    assert!(app.prepare_message(&next_context).await.unwrap().is_none());
    assert!(
        !app.finish_message(&context, &first, Ok(HostReceipt::default()))
            .await
            .unwrap()
    );
    app.reconcile_launch(identity.generation).await.unwrap();
    f.post(&sender, "@receiver fresh generation").await;
    let next = app.prepare_message(&next_context).await.unwrap().unwrap();
    assert_eq!(next.input.body, "@receiver fresh generation");
    assert_eq!(next.input.generation, successor.generation);
    assert!(next.input.seq > stream(&f, &successor).await.settled_seq);
    assert_eq!(f.count("dispatch_messages").await, 3);
}

#[tokio::test]
async fn host_blocks_and_expired_final_permits_do_not_advance_or_publish() {
    let (f, gate, app, sender, context, identity) = setup().await;
    *gate.recovery.lock().unwrap() = agentchat_core::delivery::InputRecovery::RetainUncertainty;
    f.post(&sender, "passive body").await;
    let initial = stream(&f, &identity).await;
    for phase in [Phase::ApprovalBlocked, Phase::Paused, Phase::Disconnected] {
        f.host.phase(&context, phase);
        assert!(app.prepare_message(&context).await.unwrap().is_none());
        assert_eq!(stream(&f, &identity).await, initial);
    }
    f.host.phase(&context, Phase::Active); // turn/start requires no steering turn ID.
    let first = app.prepare_message(&context).await.unwrap().unwrap();
    assert!(
        app.message_is_current(
            &context,
            identity.generation,
            first.token,
            first.input.delivery_id
        )
        .await
        .unwrap()
    );
    f.clock.advance(6);
    assert!(
        !app.message_is_current(
            &context,
            identity.generation,
            first.token,
            first.input.delivery_id
        )
        .await
        .unwrap()
    );
    assert!(app.prepare_message(&context).await.unwrap().is_none());
    assert_eq!(
        app.query(Some(&context), Query::Whoami)
            .await
            .map(|value| serde_json::to_value(value).unwrap())
            .unwrap()["native_delivery"]["state"],
        "acceptance_unknown"
    );
    command(&app, &context, Mutation::Leave).await;
    assert!(stream(&f, &identity).await.cancelled);
    assert!(
        !app.finish_message(&context, &first, Ok(HostReceipt::default()))
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn observation_crossing_deadline_expires_without_a_dispatch_token_or_native_attempt() {
    use agentchat_core::{
        delivery::{NativeDispatch, NativeInput},
        time::Clock,
    };
    for (mixed, notice) in [(false, false), (true, false), (true, true)] {
        let (f, gate, app, sender, context, identity) = setup().await;
        f.timeout(1).await;
        if notice {
            f.run(
                &sender,
                Mutation::Reserve {
                    patterns: vec!["src/observation-deadline/**".into()],
                    ttl_seconds: None,
                    reason: None,
                },
            )
            .await;
        } else {
            f.post(&sender, "@receiver expires during native observation")
                .await;
        }
        let boundary = stream(&f, &identity).await;
        let mut snapshot = f.store.snapshot().await.unwrap();
        let receiver = snapshot
            .conversation(context.host, &context.host_session_id)
            .await
            .unwrap()
            .unwrap();
        let original = snapshot
            .next_native(receiver.id, boundary.settled_seq)
            .await
            .unwrap()
            .unwrap();
        let delivery_id = original.delivery().id;
        assert_eq!(original.delivery().state, DeliveryState::Pending);
        assert_eq!(original.delivery().history.processing_started_at, None);
        drop(snapshot);
        f.post(&sender, "@receiver next eligible body").await;
        let processing_origin = f.clock.now();
        *gate.advance_on_observe.lock().unwrap() = Some(2);
        let first = if mixed {
            app.prepare_native(&context).await.unwrap()
        } else {
            app.prepare_message(&context)
                .await
                .unwrap()
                .map(NativeDispatch::Message)
        };
        assert!(first.is_none());
        assert_eq!(stream(&f, &identity).await, boundary);
        let mut tx = f.store.begin().await.unwrap();
        let expired = tx.delivery(delivery_id).await.unwrap().unwrap();
        assert_eq!(expired.state, DeliveryState::FailedTimeout);
        assert_eq!(expired.attempts, 0);
        assert_eq!(
            expired.history.processing_started_at,
            Some(processing_origin)
        );
        assert_eq!(
            expired.history.deadline_at,
            Some(processing_origin.after_seconds(1).unwrap())
        );
        assert_eq!(expired.history.timed_out_at, Some(f.clock.now()));
        assert!(expired.history.delivered_at.is_none());
        tx.rollback().await.unwrap();
        assert_eq!(
            f.count("delivery_timeout_notices").await,
            usize::from(!notice)
        );
        assert_eq!(
            app.maintain().await.unwrap().delivery_confirmation_timeouts,
            0
        );
        let successor = if mixed {
            app.prepare_native(&context).await.unwrap().unwrap()
        } else {
            NativeDispatch::Message(app.prepare_message(&context).await.unwrap().unwrap())
        };
        let NativeInput::Message(body) = successor.input() else {
            panic!("message successor");
        };
        assert_eq!(body.body, "@receiver next eligible body");
        assert_ne!(successor.delivery_id(), delivery_id);
        assert_eq!(successor.attempt(), 1);
        let mut tx = f.store.begin().await.unwrap();
        assert_eq!(tx.delivery(delivery_id).await.unwrap().unwrap(), expired);
        let current = tx.delivery(successor.delivery_id()).await.unwrap().unwrap();
        assert_eq!(current.history.processing_started_at, Some(f.clock.now()));
        tx.rollback().await.unwrap();
        assert_eq!(f.count("read_receipts").await, 0);
    }
}

#[tokio::test]
async fn processing_deadline_ends_fresh_native_permits_but_not_original_result_recovery() {
    use agentchat_core::{
        delivery::{InputRecovery, NativeDispatch},
        time::Clock,
    };
    for recovery in [
        InputRecovery::OriginalReceipt,
        InputRecovery::RetainUncertainty,
    ] {
        for notice in [false, true] {
            let (f, gate, app, sender, context, identity) = setup().await;
            *gate.recovery.lock().unwrap() = recovery;
            f.timeout(1).await;
            if notice {
                f.run(
                    &sender,
                    Mutation::Reserve {
                        patterns: vec!["src/deadline-guard/**".into()],
                        ttl_seconds: None,
                        reason: None,
                    },
                )
                .await;
            } else {
                f.post(&sender, "@receiver original deadline boundary")
                    .await;
            }
            let original = app.prepare_native(&context).await.unwrap().unwrap();
            assert_eq!(matches!(original, NativeDispatch::Notice(_)), notice);
            let claimed = stream(&f, &identity).await;
            f.clock.advance(1);
            assert!(
                app.native_is_current(
                    &context,
                    identity.generation,
                    original.token(),
                    original.delivery_id()
                )
                .await
                .unwrap()
            );
            if !notice {
                assert!(
                    app.message_is_current(
                        &context,
                        identity.generation,
                        original.token(),
                        original.delivery_id()
                    )
                    .await
                    .unwrap()
                );
            }
            f.clock.advance(1);
            assert!(
                !app.native_is_current(
                    &context,
                    identity.generation,
                    original.token(),
                    original.delivery_id()
                )
                .await
                .unwrap()
            );
            if !notice {
                assert!(
                    !app.message_is_current(
                        &context,
                        identity.generation,
                        original.token(),
                        original.delivery_id()
                    )
                    .await
                    .unwrap()
                );
            }
            // Read-only final guards do not invent a timeout observation or
            // resolve the original uncertainty just because wall time elapsed.
            assert_eq!(stream(&f, &identity).await, claimed);
            assert!(
                app.finish_native(
                    &context,
                    &original,
                    Ok(MessageProgress::Accepted(HostReceipt::default()))
                )
                .await
                .unwrap()
            );
            let mut tx = f.store.begin().await.unwrap();
            let delivery = tx.delivery(original.delivery_id()).await.unwrap().unwrap();
            assert_eq!(delivery.state, DeliveryState::DeliveredLate);
            assert_eq!(delivery.history.timed_out_at, Some(f.clock.now()));
            assert_eq!(delivery.history.delivered_at, Some(f.clock.now()));
            tx.rollback().await.unwrap();
            assert!(stream(&f, &identity).await.head.is_none());
            assert_eq!(stream(&f, &identity).await.settled_seq, original.seq());
            assert_eq!(f.count("read_receipts").await, 0);
        }
    }
}

#[tokio::test]
async fn parallel_claims_reserve_one_head_and_backup_retains_it_without_receipts() {
    let (f, _gate, app, sender, context, identity) = setup().await;
    f.post(&sender, "@receiver original body").await;
    let (a, b) = tokio::join!(app.prepare_message(&context), app.prepare_message(&context));
    let claims = [a.unwrap(), b.unwrap()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    assert_eq!(claims.len(), 1);
    let expected = stream(&f, &identity).await;
    let path = f.root.path().join("stream-backup.sqlite3");
    f.store.backup(&path).await.unwrap();
    let copy = agentchat_store::Store::open(&path, f.store.identity().clone())
        .await
        .unwrap();
    copy.integrity_check().await.unwrap();
    let mut snapshot = copy.snapshot().await.unwrap();
    assert_eq!(
        snapshot
            .message_stream(identity.generation)
            .await
            .unwrap()
            .unwrap(),
        expected
    );
    assert_eq!(
        snapshot
            .message(claims[0].input.message_id)
            .await
            .unwrap()
            .unwrap()
            .body,
        claims[0].input.body
    );
    assert_eq!(f.count("read_receipts").await, 0);
    let inbox = app
        .query(
            Some(&context),
            Query::Inbox {
                page: PageRequest::default(),
            },
        )
        .await
        .map(|value| serde_json::to_value(value).unwrap())
        .unwrap();
    assert!(
        inbox["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["delivery"]["id"] == claims[0].input.delivery_id.to_string())
    );
}

async fn old_namespace_is_refused(version: u32) {
    use agentchat_core::project::{ProjectIdentity, Scope};
    use agentchat_store::{Store, migrations::Migrator};
    use sea_orm::{
        ConnectOptions, ConnectionTrait, Database,
        sea_query::{Alias, Query as Sql},
    };
    use sea_orm_migration::MigratorTrait;
    let root = tempfile::TempDir::new().unwrap();
    let path = root.path().join("released.sqlite3");
    let target = path.clone();
    let identity = ProjectIdentity {
        id: Uuid::new_v4(),
        scope: Scope::Directory,
    };
    let mut options = ConnectOptions::new("sqlite::memory:");
    // Released actor reconstruction requires the ordinary private schema-only
    // connection. This disposable builder closes it before the enforced open.
    options
        .max_connections(1)
        .sqlx_logging(false)
        .map_sqlx_sqlite_opts(move |options| {
            options
                .filename(&target)
                .in_memory(false)
                .create_if_missing(true)
                .foreign_keys(false)
        });
    let db = Database::connect(options).await.unwrap();
    Migrator::up(&db, Some(version)).await.unwrap();
    db.execute(
        &Sql::insert()
            .into_table(Alias::new("project_meta"))
            .columns(
                [
                    "singleton",
                    "project_id",
                    "scope",
                    "created_at",
                    "schema_version",
                    "format_version",
                ]
                .map(Alias::new),
            )
            .values_panic([
                1.into(),
                identity.id.to_string().into(),
                "directory".into(),
                "2026-10-30T00:00:00.000Z".into(),
                version.into(),
                1.into(),
            ])
            .to_owned(),
    )
    .await
    .unwrap();
    db.close().await.unwrap();
    let before = std::fs::read(&path).unwrap();
    let error = Store::open(&path, identity).await.err().unwrap();
    assert_eq!(error.code, ErrorCode::UnsupportedSchema);
    assert!(error.message.contains("agentchat install --new-project"));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(
        !std::fs::read_dir(root.path())
            .unwrap()
            .filter_map(std::result::Result::ok)
            .any(|entry| entry.file_name().to_string_lossy().contains("backup"))
    );
}
