//! Actual message/event/inbox commits, explicit receipts and semantic retries.

mod support;

use agentchat_core::{ErrorCode, application::Mutation, model::Phase};
use serde_json::json;
use support::{Fixture, id};
use uuid::Uuid;

#[tokio::test]
async fn frozen_mentions_thread_validation_and_atomic_invalid_body_failure() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    let c = f.context();
    let alice = f.join(&a, "alice").await;
    let bob = f.join(&b, "bob").await;
    let carol = f.join(&c, "carol").await;
    f.host.phase(&b, Phase::Paused);
    f.app.heartbeat(&b).await.unwrap();
    f.app.detach(&c).await.unwrap();
    let root = f.post(&a, "@here please inspect").await;
    assert_eq!(root.result["mentioned_agent_ids"], json!([bob]));
    let root_id = id(&root.result, "message_id");
    assert_eq!(root.result["thread_id"], root.result["message_id"]);
    let all = f.post(&a, "@all and @BOB, plus `@missing`").await;
    let recipients = all.result["mentioned_agent_ids"].as_array().unwrap();
    assert_eq!(recipients.len(), 2);
    assert!(recipients.contains(&json!(bob)));
    assert!(recipients.contains(&json!(carol)));
    assert!(!recipients.contains(&json!(alice)));
    let mut resumed = c.clone();
    resumed.context_id = Uuid::new_v4();
    resumed.incarnation = Uuid::new_v4();
    f.app.attach(&resumed).await.unwrap();
    let frozen = f
        .rows("mentions")
        .await
        .into_iter()
        .filter(|r| r.try_get::<String>("", "message_id").unwrap() == root_id.to_string())
        .map(|r| r.try_get::<String>("", "recipient_id").unwrap())
        .collect::<Vec<_>>();
    assert_eq!(frozen, vec![bob.to_string()]);
    let rows = f.rows("deliveries").await;
    let bob_delivery = f.delivery(root_id, bob).await;
    assert!(
        rows.iter()
            .find(|r| r.try_get::<String>("", "id").unwrap() == bob_delivery.to_string())
            .unwrap()
            .try_get::<bool>("", "wake_eligible")
            .unwrap()
    );
    assert!(rows.iter().all(|row| {
        row.try_get::<String>("", "dispatch_id").unwrap() != root_id.to_string()
            || row.try_get::<String>("", "recipient_id").unwrap() != alice.to_string()
    }));
    let before = f.counts().await;
    let key = Uuid::new_v4();
    assert_eq!(
        f.app
            .execute(
                key,
                &a,
                Mutation::Post {
                    channel: Some("dev".into()),
                    body: "   ".into()
                }
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument
    );
    assert_eq!(f.counts().await, before);
    // Failed semantic attempts do not reserve a successful idempotency key.
    f.app
        .execute(
            key,
            &a,
            Mutation::Post {
                channel: Some("dev".into()),
                body: "repaired".into(),
            },
        )
        .await
        .unwrap();
    let reply = f
        .run(
            &a,
            Mutation::Reply {
                read_message_ids: Vec::new(),
                root_message_id: root_id,
                body: "passive reply".into(),
            },
        )
        .await;
    assert_eq!(reply.result["thread_id"], root.result["thread_id"]);
    assert_eq!(reply.result["mentioned_agent_ids"], json!([]));
    let before = f.counts().await;
    for target in [id(&reply.result, "message_id"), Uuid::new_v4()] {
        assert_eq!(
            f.app
                .execute(
                    Uuid::new_v4(),
                    &a,
                    Mutation::Reply {
                        read_message_ids: Vec::new(),
                        root_message_id: target,
                        body: "invalid nesting".into()
                    }
                )
                .await
                .unwrap_err()
                .code,
            ErrorCode::InvalidThread
        );
        assert_eq!(f.counts().await, before);
    }
    let passive = id(&reply.result, "message_id");
    let passive_delivery = f.delivery(passive, bob).await;
    assert!(
        !f.rows("deliveries")
            .await
            .iter()
            .find(|r| r.try_get::<String>("", "id").unwrap() == passive_delivery.to_string())
            .unwrap()
            .try_get::<bool>("", "wake_eligible")
            .unwrap()
    );
    assert_eq!(f.count("read_receipts").await, 0);
    f.store.integrity_check().await.unwrap();
}

#[tokio::test]
async fn event_sequences_order_tied_timestamps_and_a_backwards_wall_clock() {
    let f = Fixture::new().await;
    let a = f.context();
    f.join(&a, "alice").await;
    let first = f.post(&a, "same timestamp first").await;
    let tied = f.post(&a, "same timestamp second").await;
    f.clock.advance(10);
    f.app.heartbeat(&a).await.unwrap();
    let later = f.post(&a, "later clock").await;
    f.clock.advance(-5);
    let mut reconnect = a.clone();
    reconnect.context_id = Uuid::new_v4();
    reconnect.incarnation = Uuid::new_v4();
    f.app.attach(&reconnect).await.unwrap();
    let backwards = f.post(&reconnect, "clock moved backwards").await;
    let seqs = [first, tied, later, backwards]
        .map(|r| r.result["seq"].as_str().unwrap().parse::<i64>().unwrap());
    assert!(seqs.windows(2).all(|p| p[0] < p[1]));
    let mut rows = f
        .rows("dispatches")
        .await
        .into_iter()
        .filter(|row| row.try_get::<String>("", "kind").unwrap() == "message")
        .collect::<Vec<_>>();
    rows.sort_by_key(|r| r.try_get::<i64>("", "creation_seq").unwrap());
    let times = rows
        .iter()
        .map(|r| r.try_get::<String>("", "created_at").unwrap())
        .collect::<Vec<_>>();
    assert_eq!(times[0], times[1]);
    assert!(times[2] > times[3]);
}

#[tokio::test]
async fn removed_ack_operations_cannot_write_and_explicit_reads_validate_the_entire_set() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    f.join(&a, "alice").await;
    let bob = f.join(&b, "bob").await;
    let c = f.context();
    let carol = f.join(&c, "carol").await;
    let root = f.post(&a, "@bob read this deliberately").await;
    let message = id(&root.result, "message_id");
    let recipient = f.delivery(message, bob).await;
    let foreign = f.delivery(message, carol).await;
    let events = f.count("events").await;
    for operation in ["ack", "delivery_ack"] {
        assert!(
            serde_json::from_value::<Mutation>(json!({
                "operation":operation,"delivery_ids":[recipient,foreign]
            }))
            .is_err()
        );
    }
    let state = f
        .rows("deliveries")
        .await
        .into_iter()
        .find(|r| r.try_get::<String>("", "id").unwrap() == recipient.to_string())
        .unwrap();
    assert_eq!(state.try_get::<String>("", "state").unwrap(), "pending");
    assert_eq!(f.count("events").await, events);
    assert_eq!(f.count("read_receipts").await, 0);
    let before = f.counts().await;
    assert_eq!(
        f.app
            .execute(
                Uuid::new_v4(),
                &b,
                Mutation::Read {
                    message_ids: vec![message, Uuid::new_v4()]
                }
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    assert_eq!(f.counts().await, before);
    f.clock.advance(2);
    let read = f
        .run(
            &b,
            Mutation::Read {
                message_ids: vec![message, message],
            },
        )
        .await;
    assert_eq!(read.result["changed"], json!([message]));
    assert_eq!(f.count("events").await, events);
    let first = f.rows("read_receipts").await[0]
        .try_get::<String>("", "first_read_at")
        .unwrap();
    f.clock.advance(2);
    let again = f
        .run(
            &b,
            Mutation::Read {
                message_ids: vec![message],
            },
        )
        .await;
    assert_eq!(again.result["already_read"], json!([message]));
    assert!(again.event_head.is_none());
    assert_eq!(
        f.rows("read_receipts").await[0]
            .try_get::<String>("", "first_read_at")
            .unwrap(),
        first
    );
    let state = f
        .rows("deliveries")
        .await
        .into_iter()
        .find(|row| row.try_get::<String>("", "id").unwrap() == recipient.to_string())
        .unwrap();
    assert_eq!(state.try_get::<String>("", "state").unwrap(), "pending");
    assert!(
        state
            .try_get::<Option<String>>("", "delivered_at")
            .unwrap()
            .is_none()
    );
    assert_eq!(f.count("read_receipts").await, 1);
    let reply = f
        .run(
            &a,
            Mutation::Reply {
                read_message_ids: Vec::new(),
                root_message_id: message,
                body: "separate receipt".into(),
            },
        )
        .await;
    assert_eq!(f.count("read_receipts").await, 1);
    assert_ne!(id(&reply.result, "message_id"), message);
    f.store.integrity_check().await.unwrap();
}

#[tokio::test]
async fn identical_concurrent_retries_preserve_results_after_leave_and_reject_reuse() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    f.join(&a, "alice").await;
    f.join(&b, "bob").await;
    let key = Uuid::new_v4();
    let command = Mutation::Post {
        channel: Some("dev".into()),
        body: "@bob exactly one committed message".into(),
    };
    let (first, second) = tokio::join!(
        f.app.execute(key, &a, command.clone()),
        f.app.execute(key, &a, command.clone())
    );
    let first = first.unwrap();
    let second = second.unwrap();
    assert_eq!(first.result, second.result);
    assert!(first.event_head.is_some() ^ second.event_head.is_some());
    assert_eq!(f.count("dispatch_messages").await, 1);
    let before = f.counts().await;
    assert_eq!(
        f.app
            .execute(key, &b, command.clone())
            .await
            .unwrap_err()
            .code,
        ErrorCode::IdempotencyConflict
    );
    assert_eq!(
        f.app
            .execute(
                key,
                &a,
                Mutation::Post {
                    channel: Some("dev".into()),
                    body: "different".into()
                }
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::IdempotencyConflict
    );
    assert_eq!(f.counts().await, before);
    f.run(&a, Mutation::Leave).await;
    let before = f.counts().await;
    let mut reconnect = a.clone();
    reconnect.context_id = Uuid::new_v4();
    reconnect.incarnation = Uuid::new_v4();
    let replay = f.app.execute(key, &reconnect, command).await.unwrap();
    assert_eq!(replay.result, first.result);
    assert!(replay.event_head.is_none());
    assert_eq!(f.counts().await, before);
    assert_eq!(f.count("dispatch_messages").await, 1);
}
