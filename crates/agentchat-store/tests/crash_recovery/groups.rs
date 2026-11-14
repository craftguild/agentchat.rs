//! Hard exits before/after savepoint release and the actual outer commit.
use super::*;
use agentchat_core::application::MutationRequest;

fn second_key(key: Uuid) -> Uuid {
    Uuid::from_u128(key.as_u128() ^ 1)
}
fn failed_key(key: Uuid) -> Uuid {
    Uuid::from_u128(key.as_u128() ^ 2)
}
fn second_post() -> Mutation {
    Mutation::Post {
        channel: Some("dev".into()),
        body: "@bob second grouped post".into(),
    }
}
pub(super) fn requests(context: &Context, key: Uuid) -> Vec<MutationRequest> {
    vec![
        MutationRequest {
            request_id: key,
            context: context.clone(),
            command: post(),
        },
        MutationRequest {
            request_id: failed_key(key),
            context: context.clone(),
            command: Mutation::Reply {
                read_message_ids: Vec::new(),
                root_message_id: Uuid::nil(),
                body: "missing root must roll back".into(),
            },
        },
        MutationRequest {
            request_id: second_key(key),
            context: context.clone(),
            command: second_post(),
        },
    ]
}

#[tokio::test]
async fn ten_group_process_cuts_never_publish_a_savepoint_as_durable_success() {
    for point in [
        "group_before_release_1",
        "group_after_release_1",
        "group_second_state",
        "group_second_deliveries",
        "group_after_error_rollback",
        "group_before_release_2",
        "group_after_release_2",
        "group_before_outer_commit",
        "group_reject_outer_commit",
        "group_after_outer_commit",
    ] {
        let f = Fixture::new().await;
        let a = f.context();
        let b = f.context();
        f.join(&a, "alice").await;
        f.join(&b, "bob").await;
        let before = f.counts().await;
        let audit_count = f.count("events").await;
        let head = f.store.snapshot().await.unwrap().dispatch_head().get();
        let request = Uuid::new_v4();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "crash_child"])
            .env("AGENTCHAT_TEST_CRASH_ACTION", "group")
            .env(
                "AGENTCHAT_TEST_CRASH_DB",
                f.root.path().join("agentchat.sqlite3"),
            )
            .env(
                "AGENTCHAT_TEST_CRASH_CONTEXT",
                serde_json::to_string(&a).unwrap(),
            )
            .env("AGENTCHAT_TEST_CRASH_REQUEST", request.to_string())
            .env("AGENTCHAT_TEST_CRASH_POINT", point)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("Group child did not reach {point}");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(status.code(), Some(86), "{point}");
        let recovered = Store::open(
            f.root.path().join("agentchat.sqlite3"),
            f.store.identity().clone(),
        )
        .await
        .unwrap();
        recovered.integrity_check().await.unwrap();
        recovered.close().await.unwrap();
        let committed = point == "group_after_outer_commit";
        if !committed {
            assert_eq!(f.counts().await, before, "{point}");
        }
        let mut tx = f.store.begin().await.unwrap();
        assert!(tx.idempotency(failed_key(request)).await.unwrap().is_none());
        for key in [request, second_key(request)] {
            assert_eq!(
                tx.idempotency(key).await.unwrap().is_some(),
                committed,
                "{point}"
            );
        }
        tx.rollback().await.unwrap();
        for (offset, key, command) in [
            (1, request, post()),
            (2, second_key(request), second_post()),
        ] {
            let result = f.app.execute(key, &a, command.clone()).await.unwrap();
            assert_eq!(result.event_head.is_none(), committed, "{point}");
            assert_eq!(
                result.result["seq"],
                (head + offset * 2).to_string(),
                "{point}"
            );
            let before_retry = f.counts().await;
            assert_eq!(
                f.app.execute(key, &a, command).await.unwrap().result,
                result.result
            );
            assert_eq!(f.counts().await, before_retry);
        }
        assert_eq!(f.count("dispatch_messages").await, 2);
        assert_eq!(f.count("delivery_timeout_notices").await, 0);
        assert_eq!(f.count("events").await, audit_count + 2);
        for row in f.rows("dispatch_messages").await {
            let message = row.try_get::<String>("", "dispatch_id").unwrap();
            let event = f
                .rows("events")
                .await
                .into_iter()
                .find(|r| {
                    r.try_get::<Option<String>>("", "message_id")
                        .unwrap()
                        .as_deref()
                        == Some(&message)
                })
                .unwrap();
            assert!(event.try_get::<bool>("", "sealed").unwrap());
            // Own activity stays in audit without a self-delivery obligation.
            assert_eq!(
                f.rows("deliveries")
                    .await
                    .iter()
                    .filter(|r| r.try_get::<String>("", "dispatch_id").unwrap() == message)
                    .count(),
                1
            );
        }
        f.store.integrity_check().await.unwrap();
    }
}
