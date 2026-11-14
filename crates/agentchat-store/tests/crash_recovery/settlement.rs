//! Correlated original completion survives real exits as one delivery/cursor commit.
use super::*;
use agentchat_core::delivery::{InputRecovery, MessageDispatch, Presentation};

fn host(context: &Context, identity: &LaunchIdentity) -> Arc<launches::Host> {
    Arc::new(launches::Host {
        context: context.clone(),
        identity: identity.clone(),
        state: LaunchOwnerState::Current,
        recovery: InputRecovery::RetainUncertainty,
    })
}

#[tokio::test]
#[ignore = "owned child; parent executes every completion durability cut"]
async fn crash_child() {
    let context: Context =
        serde_json::from_str(&std::env::var("AGENTCHAT_TEST_SETTLEMENT_CONTEXT").unwrap()).unwrap();
    let identity: LaunchIdentity =
        serde_json::from_str(&std::env::var("AGENTCHAT_TEST_SETTLEMENT_IDENTITY").unwrap())
            .unwrap();
    let claim: serde_json::Value =
        serde_json::from_str(&std::env::var("AGENTCHAT_TEST_SETTLEMENT_DISPATCH").unwrap())
            .unwrap();
    let dispatch = MessageDispatch {
        input: serde_json::from_value(claim["input"].clone()).unwrap(),
        token: serde_json::from_value(claim["token"].clone()).unwrap(),
        attempt: 1,
        presentation: Presentation::Idle,
        awaiting_acceptance: false,
        frame_revision: agentchat_core::project::NativeFrameRevision::DispatchV4,
    };
    let point = std::env::var("AGENTCHAT_TEST_SETTLEMENT_POINT").unwrap();
    let store = Arc::new(
        Store::open(
            std::env::var_os("AGENTCHAT_TEST_SETTLEMENT_DB").unwrap(),
            ProjectIdentity {
                id: context.project_id,
                scope: Scope::Directory,
            },
        )
        .await
        .unwrap(),
    );
    let app = Application::new(
        Arc::new(CrashStore {
            store,
            point: point.clone(),
        }),
        Arc::new(TestClock::fixed()),
        host(&context, &identity),
    );
    let result = app
        .finish_message(&context, &dispatch, Ok(HostReceipt::default()))
        .await;
    if point == "settlement_fail_after_delivery" {
        assert_eq!(
            result.unwrap_err().code,
            agentchat_core::ErrorCode::StorageError
        );
        std::process::exit(86);
    }
    result.unwrap();
    panic!("Selected settlement cut did not exit");
}

struct Child(std::process::Child);
impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn five_completion_crash_cuts_preserve_both_history_and_fifo_or_neither() {
    for point in [
        "settlement_after_delivery",
        "message_after_stream",
        "group_before_outer_commit",
        "group_after_outer_commit",
        "settlement_fail_after_delivery",
    ] {
        let f = Fixture::new().await;
        let sender = f.context();
        f.join(&sender, "alice").await;
        let context = f.context();
        let identity = LaunchIdentity {
            generation: Uuid::new_v4(),
            owner: "settlement-test-owner".into(),
        };
        let app = Application::new(f.store.clone(), f.clock.clone(), host(&context, &identity));
        app.start_launch(&context, Some("bob")).await.unwrap();
        f.post(&sender, "original complete input").await;
        f.post(&sender, "successor complete input").await;
        let dispatch = app.prepare_message(&context).await.unwrap().unwrap();
        let mut tx = f.store.begin().await.unwrap();
        let before = tx
            .delivery(dispatch.input.delivery_id)
            .await
            .unwrap()
            .unwrap();
        let stream = tx
            .message_stream(identity.generation)
            .await
            .unwrap()
            .unwrap();
        tx.rollback().await.unwrap();
        let counts = f.counts().await;
        let mut child = Child(
            Command::new(std::env::current_exe().unwrap())
                .args(["--ignored", "--exact", "settlement::crash_child"])
                .env(
                    "AGENTCHAT_TEST_SETTLEMENT_DB",
                    f.root.path().join("agentchat.sqlite3"),
                )
                .env(
                    "AGENTCHAT_TEST_SETTLEMENT_CONTEXT",
                    serde_json::to_string(&context).unwrap(),
                )
                .env(
                    "AGENTCHAT_TEST_SETTLEMENT_IDENTITY",
                    serde_json::to_string(&identity).unwrap(),
                )
                .env(
                    "AGENTCHAT_TEST_SETTLEMENT_DISPATCH",
                    serde_json::json!({"input":dispatch.input,"token":dispatch.token}).to_string(),
                )
                .env("AGENTCHAT_TEST_SETTLEMENT_POINT", point)
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        let until = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                assert_eq!(status.code(), Some(86), "{point}");
                break;
            }
            assert!(
                Instant::now() < until,
                "Completion child did not reach {point}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        let reopened = Arc::new(
            Store::open(
                f.root.path().join("agentchat.sqlite3"),
                f.store.identity().clone(),
            )
            .await
            .unwrap(),
        );
        reopened.integrity_check().await.unwrap();
        let mut tx = reopened.begin().await.unwrap();
        let row = tx
            .delivery(dispatch.input.delivery_id)
            .await
            .unwrap()
            .unwrap();
        let recovered = tx
            .message_stream(identity.generation)
            .await
            .unwrap()
            .unwrap();
        let committed = point == "group_after_outer_commit";
        if committed {
            assert_eq!(row.state, DeliveryState::Delivered);
            assert!(row.history.delivered_at.is_some());
            assert_eq!(recovered.settled_seq, dispatch.input.seq);
            assert!(recovered.head.is_none());
        } else {
            assert_eq!(row, before, "{point}");
            assert_eq!(recovered, stream, "{point}");
        }
        tx.rollback().await.unwrap();
        let mut expected = counts.clone();
        if committed {
            *expected.get_mut("delivery_state_transitions").unwrap() += 1;
        }
        assert_eq!(f.counts().await, expected);
        let recovered_app = Application::new(reopened, f.clock.clone(), host(&context, &identity));
        assert_eq!(
            recovered_app
                .finish_message(&context, &dispatch, Ok(HostReceipt::default()))
                .await
                .unwrap(),
            !committed
        );
        let next = recovered_app
            .prepare_message(&context)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(next.input.body, "successor complete input");
        assert_eq!(f.count("read_receipts").await, 0);
    }
}
