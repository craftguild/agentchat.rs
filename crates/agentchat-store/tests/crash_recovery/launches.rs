//! Actual child exits cut the shared startup and generation-cancellation path.
use super::*;
use agentchat_core::ports::{HostGate, HostState};

pub(super) struct Host {
    pub(super) context: Context,
    pub(super) identity: LaunchIdentity,
    pub(super) state: LaunchOwnerState,
    pub(super) recovery: agentchat_core::delivery::InputRecovery,
}
#[async_trait]
impl HostGate for Host {
    fn input_capabilities(
        &self,
        _context: &agentchat_core::model::Context,
    ) -> agentchat_core::Result<agentchat_core::delivery::InputCapabilities> {
        use agentchat_core::delivery::{ActiveInput, InputCapabilities};
        Ok(InputCapabilities::complete(
            self.recovery,
            ActiveInput::ExactTurnId,
        ))
    }
    async fn verify(&self, context: &Context) -> Result<HostState> {
        SyntheticHost::default().verify(context).await
    }
    async fn launch_identity(&self, context: &Context) -> Result<Option<LaunchIdentity>> {
        Ok((context == &self.context).then(|| self.identity.clone()))
    }
    fn launch_owner(&self, _: &LaunchIdentity) -> LaunchOwnerState {
        self.state
    }
}

async fn drain_notices(app: &Application, context: &Context) {
    for _ in 0..200 {
        let Some(dispatch) = app.prepare_native(context).await.unwrap() else {
            return;
        };
        assert!(
            matches!(
                &dispatch,
                agentchat_core::delivery::NativeDispatch::Notice(_)
            ),
            "fixture must drain only pre-body typed notices"
        );
        assert!(
            app.finish_native(
                context,
                &dispatch,
                Ok(agentchat_core::delivery::MessageProgress::Accepted(
                    HostReceipt::default()
                ))
            )
            .await
            .unwrap()
        );
    }
    panic!("bounded fixture notice drain exceeded 200 inputs");
}

#[tokio::test]
#[ignore = "owned subprocess; parent invokes every launch durability cut"]
async fn crash_child() {
    let context: Context =
        serde_json::from_str(&std::env::var("AGENTCHAT_TEST_LAUNCH_CONTEXT").unwrap()).unwrap();
    let identity: LaunchIdentity =
        serde_json::from_str(&std::env::var("AGENTCHAT_TEST_LAUNCH_IDENTITY").unwrap()).unwrap();
    let action = std::env::var("AGENTCHAT_TEST_LAUNCH_ACTION").unwrap();
    let path = std::env::var_os("AGENTCHAT_TEST_LAUNCH_DB").unwrap();
    let store = Arc::new(
        Store::open(
            std::path::PathBuf::from(path),
            ProjectIdentity {
                id: context.project_id,
                scope: Scope::Directory,
            },
        )
        .await
        .unwrap(),
    );
    let generation = identity.generation;
    let app = Application::new(
        Arc::new(CrashStore {
            store,
            point: std::env::var("AGENTCHAT_TEST_LAUNCH_POINT").unwrap(),
        }),
        Arc::new(TestClock::fixed()),
        Arc::new(Host {
            recovery: if action == "await" {
                agentchat_core::delivery::InputRecovery::OriginalReceipt
            } else {
                agentchat_core::delivery::InputRecovery::RetainUncertainty
            },
            context: context.clone(),
            identity,
            state: if action == "retire" {
                LaunchOwnerState::Gone
            } else {
                LaunchOwnerState::Current
            },
        }),
    );
    if action == "start" {
        app.start_launch(&context, Some("owner")).await.unwrap();
    } else if action == "retire" {
        app.reconcile_launch(generation).await.unwrap();
    } else if action == "prepare" {
        app.prepare_message(&context).await.unwrap().unwrap();
    } else {
        let claim: serde_json::Value =
            serde_json::from_str(&std::env::var("AGENTCHAT_TEST_LAUNCH_DISPATCH").unwrap())
                .unwrap();
        let dispatch = agentchat_core::delivery::MessageDispatch {
            input: serde_json::from_value(claim["input"].clone()).unwrap(),
            token: serde_json::from_value(claim["token"].clone()).unwrap(),
            attempt: serde_json::from_value(claim["attempt"].clone()).unwrap(),
            presentation: agentchat_core::delivery::Presentation::Idle,
            awaiting_acceptance: false,
            frame_revision: agentchat_core::project::NativeFrameRevision::DispatchV4,
        };
        if action == "await" {
            app.finish_message_progress(
                &context,
                &dispatch,
                Ok(agentchat_core::delivery::MessageProgress::AwaitingAcceptance),
            )
            .await
            .unwrap();
            panic!("Selected pending-publication cut did not exit.");
        }
        let outcome = if action == "accept" {
            Ok(HostReceipt::default())
        } else {
            Err(agentchat_core::Error::new(
                agentchat_core::ErrorCode::RuntimeUnavailable,
                "Synthetic native failure.",
            ))
        };
        assert!(
            app.finish_message(&context, &dispatch, outcome)
                .await
                .unwrap()
        );
    }
    panic!("Selected launch cut did not exit.");
}
struct Child(std::process::Child);
impl Child {
    fn run(f: &Fixture, context: &Context, identity: &LaunchIdentity, action: &str, point: &str) {
        Self::run_dispatch(f, context, identity, action, point, None);
    }
    fn run_dispatch(
        f: &Fixture,
        context: &Context,
        identity: &LaunchIdentity,
        action: &str,
        point: &str,
        dispatch: Option<&agentchat_core::delivery::MessageDispatch>,
    ) {
        let mut child = Self(
            Command::new(std::env::current_exe().unwrap())
                .args(["--ignored", "--exact", "launches::crash_child"])
                .env(
                    "AGENTCHAT_TEST_LAUNCH_DB",
                    f.root.path().join("agentchat.sqlite3"),
                )
                .env(
                    "AGENTCHAT_TEST_LAUNCH_CONTEXT",
                    serde_json::to_string(context).unwrap(),
                )
                .env(
                    "AGENTCHAT_TEST_LAUNCH_IDENTITY",
                    serde_json::to_string(identity).unwrap(),
                )
                .env("AGENTCHAT_TEST_LAUNCH_ACTION", action)
                .env("AGENTCHAT_TEST_LAUNCH_POINT", point)
                .env(
                    "AGENTCHAT_TEST_LAUNCH_DISPATCH",
                    dispatch
                        .map(|d| {
                            serde_json::json!({"input":d.input,"token":d.token,"attempt":d.attempt})
                                .to_string()
                        })
                        .unwrap_or_default(),
                )
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        let until = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                assert_eq!(status.code(), Some(86), "{action}/{point}");
                return;
            }
            assert!(Instant::now() < until, "{action}/{point}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn identity() -> LaunchIdentity {
    LaunchIdentity {
        generation: Uuid::new_v4(),
        owner: "synthetic-only-original-owner".into(),
    }
}
#[tokio::test]
async fn startup_crashes_never_publish_membership_without_its_generation() {
    for point in [
        "after_event",
        "after_deliveries",
        "launch_after_start",
        "message_after_stream",
        "before_commit",
        "after_commit",
    ] {
        let f = Fixture::new().await;
        let context = f.context();
        let identity = identity();
        let before = f.counts().await;
        Child::run(&f, &context, &identity, "start", point);
        f.store.integrity_check().await.unwrap();
        if point != "after_commit" {
            assert_eq!(f.counts().await, before, "{point}");
        }
        let mut tx = f.store.begin().await.unwrap();
        let launch = tx.launch(identity.generation).await.unwrap();
        assert_eq!(launch.is_some(), point == "after_commit", "{point}");
        tx.rollback().await.unwrap();
        let app = Application::new(
            f.store.clone(),
            f.clock.clone(),
            Arc::new(Host {
                recovery: agentchat_core::delivery::InputRecovery::RetainUncertainty,
                context: context.clone(),
                identity: identity.clone(),
                state: LaunchOwnerState::Current,
            }),
        );
        app.start_launch(&context, Some("owner")).await.unwrap();
        assert_eq!(f.count("launches").await, 1);
        assert_eq!(f.count("actors").await, 1);
        assert_eq!(f.count("message_streams").await, 1);
    }
}

#[tokio::test]
async fn body_claim_and_acceptance_crashes_never_advance_to_a_later_head() {
    for (action, points) in [
        (
            "prepare",
            vec![
                "processing_before_commit",
                "processing_after_commit",
                "message_after_stream",
                "after_assignment",
                "before_commit",
                "after_commit",
            ],
        ),
        (
            "accept",
            vec!["message_after_stream", "before_commit", "after_commit"],
        ),
        (
            "fail",
            vec!["message_after_stream", "before_commit", "after_commit"],
        ),
        (
            "await",
            vec!["message_after_stream", "before_commit", "after_commit"],
        ),
    ] {
        for point in points {
            let f = Fixture::new().await;
            f.app
                .initialize_with_settings(
                    None,
                    agentchat_core::project::ReceiptSettingsUpdate {
                        auto_read: Some(true),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            let context = f.context();
            let identity = identity();
            let app = Application::new(
                f.store.clone(),
                f.clock.clone(),
                Arc::new(Host {
                    recovery: if action == "await" {
                        agentchat_core::delivery::InputRecovery::OriginalReceipt
                    } else {
                        agentchat_core::delivery::InputRecovery::RetainUncertainty
                    },
                    context: context.clone(),
                    identity: identity.clone(),
                    state: LaunchOwnerState::Current,
                }),
            );
            app.start_launch(&context, Some("owner")).await.unwrap();
            let sender = f.context();
            f.join(&sender, "sender").await;
            drain_notices(&app, &context).await;
            let original = support::id(
                &f.post(&sender, "@owner head body").await.result,
                "message_id",
            );
            f.post(&sender, "later passive body").await;
            let dispatch = if action == "prepare" {
                None
            } else {
                app.prepare_message(&context).await.unwrap()
            };
            let mut snapshot = f.store.snapshot().await.unwrap();
            let before = snapshot
                .message_stream(identity.generation)
                .await
                .unwrap()
                .unwrap();
            drop(snapshot);
            Child::run_dispatch(&f, &context, &identity, action, point, dispatch.as_ref());
            f.store.integrity_check().await.unwrap();
            let mut snapshot = f.store.snapshot().await.unwrap();
            let after = snapshot
                .message_stream(identity.generation)
                .await
                .unwrap()
                .unwrap();
            if point != "after_commit" {
                assert_eq!(after, before, "{action}/{point}");
            } else if action == "accept" {
                assert_eq!(after.settled_seq, dispatch.as_ref().unwrap().input.seq);
                assert!(after.head.is_none());
            } else {
                assert_eq!(after.settled_seq, before.settled_seq);
                assert!(after.head.is_some());
                if action == "fail" {
                    assert_eq!(
                        after.head.as_ref().unwrap().failure_code.as_deref(),
                        Some("runtime_unavailable")
                    );
                } else if action == "await" {
                    assert!(after.head.as_ref().unwrap().awaiting_acceptance);
                }
            }
            drop(snapshot);
            if point.starts_with("processing_") {
                let id = f
                    .delivery(
                        original,
                        f.store
                            .snapshot()
                            .await
                            .unwrap()
                            .conversation(context.host, &context.host_session_id)
                            .await
                            .unwrap()
                            .unwrap()
                            .id,
                    )
                    .await;
                let mut tx = f.store.begin().await.unwrap();
                let delivery = tx.delivery(id).await.unwrap().unwrap();
                assert_eq!(
                    delivery.history.processing_started_at.is_some(),
                    point == "processing_after_commit"
                );
                assert_eq!(
                    delivery.history.deadline_at.is_some(),
                    point == "processing_after_commit"
                );
                assert_eq!(delivery.attempts, 0);
                assert!(delivery.history.delivered_at.is_none());
                tx.rollback().await.unwrap();
            }
            f.clock.advance(6);
            let next = app.prepare_message(&context).await.unwrap();
            let accepted = action == "accept" && point == "after_commit";
            let published = action == "await";
            let unclaimed = action == "prepare" && point != "after_commit";
            // A committed Codex claim is uncertain even when its child died
            // before output. Only a rolled-back claim can submit again; a
            // published receipt poll reuses the original token without output.
            assert_eq!(next.is_some(), accepted || published || unclaimed);
            if let Some(next) = &next {
                assert_eq!(next.input.message_id != original, accepted);
                if published {
                    let old = dispatch.as_ref().unwrap();
                    assert!(next.awaiting_acceptance);
                    assert_eq!(next.token, old.token);
                    assert_eq!(next.attempt, old.attempt);
                }
            } else {
                let mut snapshot = f.store.snapshot().await.unwrap();
                assert_eq!(
                    snapshot.message_stream(identity.generation).await.unwrap(),
                    Some(after.clone()),
                    "Blocked preparation must preserve the exact durable head."
                );
            }
            f.assert_read_kinds(usize::from(accepted), 0).await;
            if let Some(old) = &dispatch {
                let completed = app
                    .finish_message(&context, old, Ok(HostReceipt::default()))
                    .await
                    .unwrap();
                // The original correlated success may settle every unresolved
                // cut, including an ambiguous error committed before the crash.
                assert_eq!(completed, !accepted, "{action}/{point}");
            }
        }
    }
}

struct Pair(Vec<(Context, LaunchIdentity)>);
#[async_trait]
impl HostGate for Pair {
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
    async fn verify(&self, context: &Context) -> Result<HostState> {
        SyntheticHost::default().verify(context).await
    }
    async fn launch_identity(&self, context: &Context) -> Result<Option<LaunchIdentity>> {
        Ok(self
            .0
            .iter()
            .find(|(c, _)| c == context)
            .map(|(_, i)| i.clone()))
    }
    fn launch_owner(&self, _: &LaunchIdentity) -> LaunchOwnerState {
        LaunchOwnerState::Current
    }
}
#[tokio::test]
async fn a_second_body_completion_storage_failure_rolls_back_every_provisional_cursor() {
    use agentchat_core::{application::CommitRequest, delivery::MessageCompletion};
    let f = Fixture::new().await;
    f.app
        .initialize_with_settings(
            None,
            agentchat_core::project::ReceiptSettingsUpdate {
                auto_read: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let a = f.context();
    let b = f.context();
    let ia = identity();
    let ib = identity();
    let host = Arc::new(Pair(vec![(a.clone(), ia.clone()), (b.clone(), ib.clone())]));
    let app = Application::new(f.store.clone(), f.clock.clone(), host.clone());
    app.start_launch(&a, Some("alice")).await.unwrap();
    app.start_launch(&b, Some("bob")).await.unwrap();
    let sender = f.context();
    f.join(&sender, "sender").await;
    drain_notices(&app, &a).await;
    drain_notices(&app, &b).await;
    f.post(&sender, "@alice @bob full original body").await;
    let ca = app.prepare_message(&a).await.unwrap().unwrap();
    let cb = app.prepare_message(&b).await.unwrap().unwrap();
    let mut snapshot = f.store.snapshot().await.unwrap();
    let before_a = snapshot.message_stream(ia.generation).await.unwrap();
    let before_b = snapshot.message_stream(ib.generation).await.unwrap();
    drop(snapshot);
    MESSAGE_STREAM_WRITES.store(0, Ordering::SeqCst);
    let failing = Application::new(
        Arc::new(CrashStore {
            store: f.store.clone(),
            point: "body_fail_second_stream".into(),
        }),
        f.clock.clone(),
        host,
    );
    let error = failing
        .commit_ready(vec![
            CommitRequest::MessageCompletion(MessageCompletion {
                context: a.clone(),
                dispatch: ca.clone(),
                outcome: Ok(agentchat_core::delivery::MessageProgress::Accepted(
                    HostReceipt::default(),
                )),
            }),
            CommitRequest::MessageCompletion(MessageCompletion {
                context: b.clone(),
                dispatch: cb.clone(),
                outcome: Ok(agentchat_core::delivery::MessageProgress::Accepted(
                    HostReceipt::default(),
                )),
            }),
        ])
        .await
        .unwrap_err();
    assert_eq!(error.code, agentchat_core::ErrorCode::StorageError);
    let mut snapshot = f.store.snapshot().await.unwrap();
    assert_eq!(
        snapshot.message_stream(ia.generation).await.unwrap(),
        before_a
    );
    assert_eq!(
        snapshot.message_stream(ib.generation).await.unwrap(),
        before_b
    );
    drop(snapshot);
    let mut tx = f.store.begin().await.unwrap();
    for delivery in [ca.input.delivery_id, cb.input.delivery_id] {
        let row = tx.delivery(delivery).await.unwrap().unwrap();
        assert_eq!(row.state, DeliveryState::Processing);
        assert!(row.history.delivered_at.is_none());
    }
    tx.rollback().await.unwrap();
    assert!(
        app.finish_message(&a, &ca, Ok(HostReceipt::default()))
            .await
            .unwrap()
    );
    assert!(
        app.finish_message(&b, &cb, Ok(HostReceipt::default()))
            .await
            .unwrap()
    );
    let mut tx = f.store.begin().await.unwrap();
    for delivery in [ca.input.delivery_id, cb.input.delivery_id] {
        let row = tx.delivery(delivery).await.unwrap().unwrap();
        assert_eq!(row.state, DeliveryState::Delivered);
        assert!(row.history.delivered_at.is_some());
    }
    tx.rollback().await.unwrap();
    f.assert_read_kinds(2, 0).await;
}
#[tokio::test]
async fn retirement_crashes_commit_all_cancellation_or_keep_the_original_live_generation() {
    for point in [
        "launch_after_retire",
        "launch_after_cancel",
        "after_deliveries",
        "before_commit",
        "after_commit",
    ] {
        let f = Fixture::new().await;
        let context = f.context();
        let identity = identity();
        let app = Application::new(
            f.store.clone(),
            f.clock.clone(),
            Arc::new(Host {
                recovery: agentchat_core::delivery::InputRecovery::RetainUncertainty,
                context: context.clone(),
                identity: identity.clone(),
                state: LaunchOwnerState::Current,
            }),
        );
        let started = app.start_launch(&context, Some("owner")).await.unwrap();
        let agent = support::id(&started.result, "agent_id");
        let sender = f.context();
        f.join(&sender, "sender").await;
        let message = support::id(
            &f.post(&sender, "@owner durable original body").await.result,
            "message_id",
        );
        let delivery = f.delivery(message, agent).await;
        let dispatch = app.prepare_wake(&context).await.unwrap().unwrap();
        let before = f.counts().await;
        Child::run(&f, &context, &identity, "retire", point);
        f.store.integrity_check().await.unwrap();
        if point != "after_commit" {
            assert_eq!(f.counts().await, before, "{point}");
        }
        let mut tx = f.store.begin().await.unwrap();
        assert_eq!(
            tx.launch(identity.generation)
                .await
                .unwrap()
                .unwrap()
                .retired_at
                .is_some(),
            point == "after_commit",
            "{point}"
        );
        assert_eq!(
            tx.agent(agent).await.unwrap().unwrap().joined,
            point != "after_commit",
            "{point}"
        );
        assert_eq!(
            tx.delivery(delivery).await.unwrap().unwrap().state == DeliveryState::FailedLeft,
            point == "after_commit",
            "{point}"
        );
        tx.rollback().await.unwrap();
        let app = Application::new(
            f.store.clone(),
            f.clock.clone(),
            Arc::new(Host {
                recovery: agentchat_core::delivery::InputRecovery::RetainUncertainty,
                context: context.clone(),
                identity: identity.clone(),
                state: LaunchOwnerState::Gone,
            }),
        );
        app.reconcile_launch(identity.generation).await.unwrap();
        assert!(
            !app.finish_wake(&context, &dispatch, Ok(HostReceipt::default()))
                .await
                .unwrap()
        );
        let mut tx = f.store.begin().await.unwrap();
        assert!(tx.message(message).await.unwrap().is_some());
        assert_eq!(
            tx.delivery(delivery).await.unwrap().unwrap().state,
            DeliveryState::FailedLeft
        );
        assert!(!tx.agent(agent).await.unwrap().unwrap().joined);
    }
}
