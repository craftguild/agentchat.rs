//! Actual WAL crash cuts cannot publish reading without its combined reply.
use super::*;
use serde_json::json;

fn command(root: Uuid) -> Mutation {
    Mutation::Reply {
        read_message_ids: vec![root],
        root_message_id: root,
        body: "@alice Read the complete request and accept it.".into(),
    }
}
pub(super) async fn run(app: &Application, context: &Context, request: Uuid) {
    let root = std::env::var("AGENTCHAT_TEST_READ_REPLY_ROOT")
        .unwrap()
        .parse()
        .unwrap();
    let result = app.execute(request, context, command(root)).await;
    if matches!(
        std::env::var("AGENTCHAT_TEST_CRASH_POINT")
            .unwrap()
            .as_str(),
        "read_fail_after_insert" | "group_reject_outer_commit"
    ) {
        assert_eq!(
            result.unwrap_err().code,
            agentchat_core::ErrorCode::StorageError
        );
        std::process::exit(86);
    }
    result.unwrap();
}
struct Child(std::process::Child);
impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn eight_read_reply_crashes_and_storage_failures_preserve_all_effects_together() {
    for point in [
        "read_after_insert",
        "read_fail_after_insert",
        "after_event",
        "after_deliveries",
        "group_before_outer_commit",
        "group_reject_outer_commit",
        "group_after_outer_commit",
    ] {
        let f = Fixture::new().await;
        let a = f.context();
        let b = f.context();
        f.join(&a, "alice").await;
        let bob = f.join(&b, "bob").await;
        let root = support::id(
            &f.run(
                &a,
                Mutation::Post {
                    channel: Some("dev".into()),
                    body: "@bob complete durable request".into(),
                },
            )
            .await
            .result,
            "message_id",
        );
        let before = f.counts().await;
        let request = Uuid::new_v4();
        let mut child = Child(
            Command::new(std::env::current_exe().unwrap())
                .args(["--ignored", "--exact", "crash_child"])
                .env(
                    "AGENTCHAT_TEST_CRASH_DB",
                    f.root.path().join("agentchat.sqlite3"),
                )
                .env(
                    "AGENTCHAT_TEST_CRASH_CONTEXT",
                    serde_json::to_string(&b).unwrap(),
                )
                .env("AGENTCHAT_TEST_CRASH_REQUEST", request.to_string())
                .env("AGENTCHAT_TEST_CRASH_POINT", point)
                .env("AGENTCHAT_TEST_CRASH_ACTION", "read_reply")
                .env("AGENTCHAT_TEST_READ_REPLY_ROOT", root.to_string())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        let until = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                assert_eq!(status.code(), Some(86), "{point}");
                break;
            }
            assert!(
                Instant::now() < until,
                "Read/reply child did not reach {point}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let reopened = Store::open(
            f.root.path().join("agentchat.sqlite3"),
            f.store.identity().clone(),
        )
        .await
        .unwrap();
        reopened.integrity_check().await.unwrap();
        let committed = point == "group_after_outer_commit";
        if !committed {
            assert_eq!(f.counts().await, before, "{point}");
        }
        let mut tx = reopened.begin().await.unwrap();
        assert_eq!(
            tx.receipt(root, bob).await.unwrap().is_some(),
            committed,
            "{point}"
        );
        assert_eq!(
            tx.idempotency(request).await.unwrap().is_some(),
            committed,
            "{point}"
        );
        let delivery = tx
            .delivery(f.delivery(root, bob).await)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(delivery.state, DeliveryState::Pending);
        assert!(delivery.history.processing_started_at.is_none());
        assert!(delivery.history.delivered_at.is_none());
        tx.rollback().await.unwrap();
        reopened.close().await.unwrap();
        assert_eq!(
            f.count("dispatch_messages").await,
            if committed { 2 } else { 1 },
            "{point}"
        );
        let result = f.app.execute(request, &b, command(root)).await.unwrap();
        assert_eq!(result.result["read_result"]["changed"], json!([root]));
        assert_eq!(f.count("dispatch_messages").await, 2);
        assert_eq!(f.count("read_receipts").await, 1);
        let before_replay = f.counts().await;
        let mut replay = command(root);
        if let Mutation::Reply {
            read_message_ids, ..
        } = &mut replay
        {
            *read_message_ids = vec![root, root];
        }
        assert_eq!(
            f.app.execute(request, &b, replay).await.unwrap().result,
            result.result
        );
        assert_eq!(f.counts().await, before_replay);
    }
}
