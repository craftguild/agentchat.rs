//! Successful posts retain routing diagnostics rather than re-resolving on retry.

mod support;

use agentchat_core::{ErrorCode, application::Mutation, model::Phase};
use serde_json::json;
use support::{Fixture, id};
use uuid::Uuid;

#[tokio::test]
async fn unknown_names_preserve_body_event_and_successful_retry_without_retargeting() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    let alice = f.join(&a, "alice").await;
    let bob = f.join(&b, "bob").await;
    let key = Uuid::new_v4();
    let body = "@alice @bob @missing @MISSING `@hidden` complete body";
    let command = Mutation::Post {
        channel: Some("dev".into()),
        body: body.into(),
    };
    let posted = f.app.execute(key, &a, command.clone()).await.unwrap();
    assert_eq!(posted.result["mentioned_agent_ids"], json!([bob]));
    assert_eq!(
        posted.result["mention_results"],
        json!([
            {"handle":"alice","recipient_id":alice,"state":"self"},
            {"handle":"bob","recipient_id":bob,"state":"queued"},
            {"handle":"missing","recipient_id":null,"state":"unknown"}
        ])
    );
    assert_eq!(
        posted.result["warnings"],
        json!([
            {"handle":"missing","recipient_id":null,"code":"unknown_recipient"}
        ])
    );
    assert_eq!(f.count("actors").await, 2);
    let message = id(&posted.result, "message_id");
    let saved = f.rows("dispatch_messages").await;
    assert_eq!(saved[0].try_get::<String>("", "body").unwrap(), body);
    let event = f
        .rows("events")
        .await
        .into_iter()
        .find(|row| {
            row.try_get::<Option<String>>("", "message_id").unwrap() == Some(message.to_string())
        })
        .unwrap();
    let data: serde_json::Value =
        serde_json::from_str(&event.try_get::<String>("", "data").unwrap()).unwrap();
    assert_eq!(data["warnings"], posted.result["warnings"]);
    assert_eq!(data["mention_results"], posted.result["mention_results"]);
    let later = f.context();
    let missing = f.join(&later, "missing").await;
    let before = f.counts().await;
    let retry = f.app.execute(key, &a, command).await.unwrap();
    assert_eq!(retry.result, posted.result);
    assert!(retry.event_head.is_none());
    assert_eq!(f.counts().await, before);
    assert!(!f.rows("deliveries").await.into_iter().any(|row| {
        row.try_get::<String>("", "recipient_id").unwrap() == missing.to_string()
            && row.try_get::<String>("", "dispatch_id").unwrap() == message.to_string()
    }));
    f.store.integrity_check().await.unwrap();
}

#[tokio::test]
async fn offline_paused_and_approval_recipients_keep_durable_targeted_deliveries() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    let c = f.context();
    let d = f.context();
    f.join(&a, "alice").await;
    let bob = f.join(&b, "bob").await;
    let carol = f.join(&c, "carol").await;
    let dan = f.join(&d, "dan").await;
    f.app.detach(&b).await.unwrap();
    f.host.phase(&c, Phase::Paused);
    f.app.heartbeat(&c).await.unwrap();
    f.host.phase(&d, Phase::ApprovalBlocked);
    f.app.heartbeat(&d).await.unwrap();
    let posted = f.post(&a, "@bob @carol @dan @all").await;
    assert_eq!(
        posted.result["warnings"],
        json!([
            {"handle":"bob","recipient_id":bob,"code":"recipient_offline"},
            {"handle":"carol","recipient_id":carol,"code":"recipient_deferred"},
            {"handle":"dan","recipient_id":dan,"code":"recipient_deferred"}
        ])
    );
    let message = id(&posted.result, "message_id");
    for recipient in [bob, carol, dan] {
        let delivery = f.delivery(message, recipient).await;
        let row = f
            .rows("deliveries")
            .await
            .into_iter()
            .find(|r| r.try_get::<String>("", "id").unwrap() == delivery.to_string())
            .unwrap();
        assert!(row.try_get::<bool>("", "wake_eligible").unwrap());
    }
    f.store.integrity_check().await.unwrap();
}

#[tokio::test]
async fn diagnostic_budget_counts_distinct_visible_individual_handles_and_rolls_back() {
    let f = Fixture::new().await;
    let a = f.context();
    f.join(&a, "alice").await;
    let body = (0..200)
        .map(|n| format!("@user{n}"))
        .collect::<Vec<_>>()
        .join(" ");
    let command = |body| Mutation::Post {
        channel: Some("dev".into()),
        body,
    };
    let key = Uuid::new_v4();
    let before = f.counts().await;
    let error = f
        .app
        .execute(key, &a, command(format!("{body} @overflow")))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert_eq!(f.counts().await, before);
    let result = f
        .app
        .execute(key, &a, command(format!("{body} @user0 @all `@excluded`")))
        .await
        .unwrap();
    assert_eq!(result.result["warnings"].as_array().unwrap().len(), 200);
    assert!(serde_json::to_vec(&result.result).unwrap().len() < 64 * 1024);
    f.store.integrity_check().await.unwrap();
}

#[tokio::test]
async fn a_reply_to_its_root_succeeds_with_an_unresolved_name_without_implicit_wake() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    f.join(&a, "alice").await;
    f.join(&b, "bob").await;
    let root = f.post(&a, "request").await;
    let root = id(&root.result, "message_id");
    let reply = f
        .run(
            &b,
            Mutation::Reply {
                read_message_ids: Vec::new(),
                root_message_id: root,
                body: "@missing status".into(),
            },
        )
        .await;
    assert_eq!(reply.result["thread_id"], json!(root));
    assert_eq!(reply.result["mentioned_agent_ids"], json!([]));
    assert_eq!(reply.result["warnings"][0]["code"], "unknown_recipient");
    assert_eq!(f.count("read_receipts").await, 0);
}
