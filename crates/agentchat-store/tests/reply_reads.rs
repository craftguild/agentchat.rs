//! Read-with-reply is one normalized, atomic application operation.
mod support;
use agentchat_core::{ErrorCode, application::Mutation};
use serde_json::json;
use support::{Fixture, id};
use uuid::Uuid;

fn reply(root: Uuid, reads: Vec<Uuid>) -> Mutation {
    Mutation::Reply {
        read_message_ids: reads,
        root_message_id: root,
        body: "@alice I accept the request.".into(),
    }
}

#[tokio::test]
async fn explicit_reads_and_reply_commit_without_positive_events_and_replay_as_one_key() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    f.join(&a, "alice").await;
    f.join(&b, "bob").await;
    let root = id(
        &f.run(
            &a,
            Mutation::Post {
                channel: Some("dev".into()),
                body: "@bob please inspect".into(),
            },
        )
        .await
        .result,
        "message_id",
    );
    let other = id(
        &f.post(&a, "separate complete context").await.result,
        "message_id",
    );
    let key = Uuid::new_v4();
    let result = f
        .app
        .execute(key, &b, reply(root, vec![root, other, root]))
        .await
        .unwrap();
    let mut normalized = vec![root, other];
    normalized.sort();
    assert_eq!(result.result["read_result"]["changed"], json!(normalized));
    assert_eq!(result.result["read_result"]["already_read"], json!([]));
    let events = f.rows("events").await;
    assert!(
        events
            .iter()
            .all(|r| r.try_get::<String>("", "kind").unwrap() != "message.read")
    );
    assert_eq!(f.count("read_receipts").await, 2);
    let before = f.counts().await;
    let receipts = f
        .rows("read_receipts")
        .await
        .into_iter()
        .map(|r| r.try_get::<String>("", "first_read_at").unwrap())
        .collect::<Vec<_>>();
    f.clock.advance(2);
    assert_eq!(
        f.app
            .execute(key, &b, reply(root, normalized))
            .await
            .unwrap()
            .result,
        result.result
    );
    assert_eq!(f.counts().await, before);
    let second = f
        .app
        .execute(Uuid::new_v4(), &b, reply(root, vec![root, other]))
        .await
        .unwrap();
    assert_eq!(second.result["read_result"]["changed"], json!([]));
    assert_eq!(
        f.rows("read_receipts")
            .await
            .into_iter()
            .map(|r| r.try_get::<String>("", "first_read_at").unwrap())
            .collect::<Vec<_>>(),
        receipts
    );
    f.clock.advance(9);
    assert_eq!(
        f.app
            .maintain()
            .await
            .unwrap()
            .delivery_confirmation_timeouts,
        0
    );
}

#[tokio::test]
async fn invalid_read_set_or_reply_rolls_back_every_read_and_event() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    f.join(&a, "alice").await;
    f.join(&b, "bob").await;
    let root = id(&f.post(&a, "@bob root").await.result, "message_id");
    let other_project = Fixture::new().await;
    let foreign = other_project.context();
    other_project.join(&foreign, "foreign").await;
    let foreign_id = id(
        &other_project.post(&foreign, "foreign body").await.result,
        "message_id",
    );
    let before = f.counts().await;
    for (target, reads, code) in [
        (root, vec![root, Uuid::new_v4()], ErrorCode::NotFound),
        (root, vec![root, foreign_id], ErrorCode::NotFound),
        (Uuid::new_v4(), vec![root], ErrorCode::InvalidThread),
    ] {
        assert_eq!(
            f.app
                .execute(Uuid::new_v4(), &b, reply(target, reads))
                .await
                .unwrap_err()
                .code,
            code
        );
        assert_eq!(f.counts().await, before);
    }
    assert_eq!(
        f.app
            .execute(Uuid::new_v4(), &b, reply(root, vec![root; 201]))
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument
    );
    assert_eq!(f.counts().await, before);
}

#[tokio::test]
async fn omitted_and_empty_reads_share_the_original_retry_semantics() {
    let f = Fixture::new().await;
    let a = f.context();
    f.join(&a, "alice").await;
    let root = id(&f.post(&a, "root").await.result, "message_id");
    let key = Uuid::new_v4();
    let omitted: Mutation = serde_json::from_value(
        json!({"operation":"reply","root_message_id":root,"body":"@alice I accept the request."}),
    )
    .unwrap();
    let first = f.app.execute(key, &a, omitted).await.unwrap();
    assert!(first.result.get("read_result").is_none());
    let before = f.counts().await;
    assert_eq!(
        f.app
            .execute(key, &a, reply(root, Vec::new()))
            .await
            .unwrap()
            .result,
        first.result
    );
    assert_eq!(f.counts().await, before);
}
