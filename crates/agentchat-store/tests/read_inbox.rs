//! Typed mixed inboxes, atomic Dispatch fanout and independent read effects.

mod support;

use agentchat_core::{
    ErrorCode,
    application::Mutation,
    model::{
        Delivery, DeliveryHistory, DeliveryState, DispatchKind, DispatchNotice, DispatchPayload,
        EventKind, NoticeKind,
    },
    ports::Storage,
    read::{MAX_PAGE_BYTES, PageRequest, Query},
    time::Clock,
};
use sea_orm::{
    ConnectionTrait,
    sea_query::{Alias, Query as SqlQuery},
};
use serde_json::{Value, json};
use support::{Fixture, id};
use uuid::Uuid;

#[tokio::test]
async fn inbox_fetch_and_explicit_reads_never_settle_delivery() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    f.join(&a, "alice").await;
    let bob = f.join(&b, "bob").await;
    let root = f.post(&a, "@bob please review").await;
    let root_id = id(&root.result, "message_id");
    let before = f.counts().await;
    let first = f
        .query(
            Some(&b),
            Query::Inbox {
                page: PageRequest::default(),
            },
        )
        .await;
    let second = f
        .query(
            Some(&b),
            Query::Inbox {
                page: PageRequest::default(),
            },
        )
        .await;
    assert_eq!(first, second);
    let message = first["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["dispatch"]["id"] == root_id.to_string())
        .unwrap();
    let delivery = id(&message["delivery"], "id");
    assert_eq!(message["delivery"]["wake_eligible"], true);
    assert_eq!(message["message"]["body"], "@bob please review");
    assert!(message["message"].get("unread").is_none());
    assert!(message["message"].get("read_at").is_none());
    assert_eq!(message["delivery"]["state"], "pending");
    assert_eq!(f.counts().await, before);
    assert_eq!(f.count("read_receipts").await, 0);
    let own = f
        .query(
            Some(&a),
            Query::Inbox {
                page: PageRequest::default(),
            },
        )
        .await;
    assert!(
        own["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["dispatch"]["id"] != root_id.to_string())
    );
    f.run(
        &b,
        Mutation::Read {
            message_ids: vec![root_id],
        },
    )
    .await;
    let inbox = f
        .query(
            Some(&b),
            Query::Inbox {
                page: PageRequest::default(),
            },
        )
        .await;
    assert!(
        inbox["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["delivery"]["id"] == delivery.to_string())
    );
    assert_eq!(f.count("read_receipts").await, 1);
    let status = f.query(Some(&b), Query::Whoami).await;
    assert_eq!(status["actor"]["actor_id"], bob.to_string());
    let detail = f
        .query(
            None,
            Query::Message {
                message_id: root_id,
                receipts: PageRequest::default(),
            },
        )
        .await;
    let receipt = &detail["receipts"]["items"][0];
    assert_eq!(receipt["recipient"]["actor_id"], bob.to_string());
    assert_eq!(receipt["delivery"]["state"], "pending");
    assert_eq!(
        receipt["delivery"]["queued_at"],
        detail["message"]["created_at"]
    );
    assert_eq!(receipt["delivery"]["processing_started_at"], Value::Null);
    assert_eq!(receipt["delivery"]["deadline_at"], Value::Null);
    assert_eq!(receipt["delivery"]["delivered_at"], Value::Null);
    assert!(receipt["delivery"].get("accepted_at").is_none());
    assert!(receipt["delivery"].get("acknowledged_at").is_none());
    assert!(receipt["delivery"].get("confirmation").is_none());
    assert_eq!(receipt["reads"].as_array().unwrap().len(), 1);
    assert_eq!(receipt["reads"][0]["kind"], "explicit");
}

#[tokio::test]
async fn chunked_recipient_publication_remains_atomic_across_later_chunk_failure() {
    let f = Fixture::new().await;
    let mut contexts = Vec::new();
    let mut agents = Vec::new();
    for i in 0..17 {
        let context = f.context();
        agents.push(f.join(&context, &format!("agent-{i}")).await);
        contexts.push(context);
    }
    let before = f.counts().await;
    let recipients = |dispatch_id| {
        agents
            .iter()
            .map(|agent| Delivery {
                id: Uuid::new_v4(),
                dispatch_id,
                recipient_id: *agent,
                state: DeliveryState::Pending,
                wake_eligible: false,
                wake_batch_id: None,
                attempts: 0,
                history: DeliveryHistory::queued(f.clock.now()),
            })
            .collect::<Vec<_>>()
    };
    for foreign_key_error in [false, true] {
        let mut tx = f.store.begin().await.unwrap();
        let header = tx
            .allocate_dispatch(DispatchKind::Notice, None, f.clock.now())
            .await
            .unwrap();
        let mut invalid = recipients(header.id);
        if foreign_key_error {
            invalid[16].recipient_id = Uuid::new_v4();
        } else {
            invalid[16].dispatch_id = Uuid::new_v4();
        }
        assert_eq!(
            tx.publish_dispatch(&header, &notice(header.id, agents[0]), &invalid)
                .await
                .unwrap_err()
                .code,
            if foreign_key_error {
                ErrorCode::StorageError
            } else {
                ErrorCode::IntegrityError
            }
        );
        // Sixteen rows have been staged before the later chunk's application
        // or actual FK failure. Ignoring that error still cannot commit them.
        assert_eq!(f.counts().await, before);
        assert_eq!(
            tx.commit().await.unwrap_err().code,
            ErrorCode::IntegrityError
        );
        assert_eq!(f.counts().await, before);
    }
    let mut tx = f.store.begin().await.unwrap();
    let header = tx
        .allocate_dispatch(DispatchKind::Notice, None, f.clock.now())
        .await
        .unwrap();
    let payload = notice(header.id, agents[0]);
    tx.publish_dispatch(&header, &payload, &recipients(header.id))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        f.rows("deliveries")
            .await
            .iter()
            .filter(|r| r.try_get::<String>("", "dispatch_id").unwrap() == header.id.to_string())
            .count(),
        17
    );
    for context in contexts {
        let inbox = f
            .query(
                Some(&context),
                Query::Inbox {
                    page: PageRequest {
                        limit: Some(200),
                        after: Default::default(),
                    },
                },
            )
            .await;
        let item = inbox["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["dispatch"]["id"] == header.id.to_string())
            .unwrap();
        assert_eq!(item["notice"]["data"]["handle"], "recorded");
        assert_eq!(item["batch_state"], Value::Null);
    }
    f.store.integrity_check().await.unwrap();
}

fn notice(dispatch_id: Uuid, actor: Uuid) -> DispatchPayload {
    DispatchPayload::Notice(DispatchNotice {
        dispatch_id,
        kind: NoticeKind::AgentJoined,
        data: json!({"agent_id":actor,"handle":"recorded"}),
    })
}

#[tokio::test]
async fn unknown_kinds_payload_fields_and_major_versions_are_refused_without_side_effects() {
    let f = Fixture::new().await;
    let a = f.context();
    let alice = f.join(&a, "alice").await;
    assert!("future.informational".parse::<EventKind>().is_err());
    assert!("message.read_timeout".parse::<NoticeKind>().is_err());
    let before = f.counts().await;
    let mut tx = f.store.begin().await.unwrap();
    let header = tx
        .allocate_dispatch(DispatchKind::Notice, None, f.clock.now())
        .await
        .unwrap();
    let mut payload = notice(header.id, alice);
    let DispatchPayload::Notice(ref mut content) = payload else {
        unreachable!()
    };
    content.data["unknown_field"] = json!(true);
    assert!(tx.publish_dispatch(&header, &payload, &[]).await.is_err());
    assert!(tx.commit().await.is_err());
    assert_eq!(f.counts().await, before);
    let result = f
        .inspect
        .execute(
            &SqlQuery::update()
                .table(Alias::new("events"))
                .value(Alias::new("version"), 2)
                .to_owned(),
        )
        .await;
    assert!(result.is_err(), "schema rejects an unknown major version");
    assert_eq!(f.counts().await, before);
}

#[tokio::test]
async fn stalled_batch_state_is_visible_without_retrying_or_changing_bookkeeping() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    f.join(&a, "alice").await;
    let bob = f.join(&b, "bob").await;
    let root = f.post(&a, "@bob pending stalled delivery").await;
    let root_id = id(&root.result, "message_id");
    let delivery = f.delivery(root_id, bob).await;
    let batch = Uuid::new_v4();
    f.inspect
        .execute(
            &SqlQuery::insert()
                .into_table(Alias::new("wake_batches"))
                .columns(
                    [
                        "id",
                        "recipient_id",
                        "incarnation",
                        "state",
                        "attempts",
                        "failure_code",
                    ]
                    .map(Alias::new),
                )
                .values([
                    batch.to_string().into(),
                    bob.to_string().into(),
                    b.incarnation.to_string().into(),
                    "stalled".into(),
                    3i32.into(),
                    "test_safe_attempts_exhausted".into(),
                ])
                .unwrap()
                .to_owned(),
        )
        .await
        .unwrap();
    let mut tx = f.store.begin().await.unwrap();
    let mut stored = tx.delivery(delivery).await.unwrap().unwrap();
    stored.start_processing(f.clock.now(), 3_600).unwrap();
    stored.wake_batch_id = Some(batch);
    stored.attempts = 3;
    tx.save_delivery(&stored).await.unwrap();
    tx.commit().await.unwrap();
    let before = f.counts().await;
    let result = f
        .query(
            Some(&b),
            Query::Inbox {
                page: PageRequest::default(),
            },
        )
        .await;
    let row = result["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["delivery"]["id"] == delivery.to_string())
        .unwrap();
    assert_eq!(row["batch_state"], "stalled");
    assert_eq!(row["delivery"]["attempts"], 3);
    assert_eq!(f.health().await["statistics"]["stalled_batches"], 1);
    assert_eq!(f.counts().await, before);
    f.store.integrity_check().await.unwrap();
}

#[tokio::test]
async fn malformed_oversized_stored_event_fails_instead_of_emitting_an_invalid_frame() {
    let f = Fixture::new().await;
    let a = f.context();
    f.join(&a, "alice").await;
    f.inspect
        .execute(
            &SqlQuery::update()
                .table(Alias::new("events"))
                .value(
                    Alias::new("data"),
                    json!({"oversized":"x".repeat(MAX_PAGE_BYTES)}),
                )
                .to_owned(),
        )
        .await
        .unwrap();
    let before = f.counts().await;
    assert_eq!(
        f.app
            .query(
                None,
                Query::Events {
                    channel: None,
                    page: PageRequest::default()
                }
            )
            .await
            .map(|value| serde_json::to_value(value).unwrap())
            .unwrap_err()
            .code,
        ErrorCode::IntegrityError
    );
    assert_eq!(f.counts().await, before);
}
