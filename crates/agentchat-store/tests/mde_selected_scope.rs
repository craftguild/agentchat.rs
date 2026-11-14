//! Actual transaction routing for accepted visible and delivery-only alternatives.
mod support;
use agentchat_core::application::Mutation;
use serde_json::json;
use support::{Fixture, id};
use uuid::Uuid;

#[tokio::test]
async fn four_representatives_freeze_bob_with_initial_history_and_without_reads() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    let alice = f.join(&a, "alice").await;
    let bob = f.join(&b, "bob").await;
    for body in [
        "![description @bob](url)",
        "![alt](url)@bob",
        "word:rocket:@bob",
        "main[^n]\n\n[^n]: @bob",
    ] {
        let result = f.post(&a, body).await;
        assert_eq!(result.result["mentioned_agent_ids"], json!([bob]));
        assert!(
            !result.result["mentioned_agent_ids"]
                .as_array()
                .unwrap()
                .contains(&json!(alice))
        );
        let message = id(&result.result, "message_id");
        let delivery = f.delivery(message, bob).await;
        let row = f
            .rows("deliveries")
            .await
            .into_iter()
            .find(|r| r.try_get::<String>("", "id").unwrap() == delivery.to_string())
            .unwrap();
        assert!(row.try_get::<bool>("", "wake_eligible").unwrap());
        assert_eq!(row.try_get::<String>("", "state").unwrap(), "pending");
        assert_eq!(row.try_get::<i64>("", "state_version").unwrap(), 1);
        assert_eq!(
            row.try_get::<String>("", "dispatch_id").unwrap(),
            message.to_string()
        );
        let header = f
            .rows("dispatches")
            .await
            .into_iter()
            .find(|r| r.try_get::<String>("", "id").unwrap() == message.to_string())
            .unwrap();
        assert_eq!(header.try_get::<String>("", "kind").unwrap(), "message");
        let history = f
            .rows("delivery_state_transitions")
            .await
            .into_iter()
            .filter(|r| r.try_get::<String>("", "delivery_id").unwrap() == delivery.to_string())
            .collect::<Vec<_>>();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].try_get::<i64>("", "ordinal").unwrap(), 1);
        assert_eq!(
            history[0]
                .try_get::<Option<String>>("", "from_state")
                .unwrap(),
            None
        );
        assert_eq!(
            history[0].try_get::<String>("", "to_state").unwrap(),
            "pending"
        );
        let row = f
            .rows("dispatch_messages")
            .await
            .into_iter()
            .find(|r| r.try_get::<String>("", "dispatch_id").unwrap() == message.to_string())
            .unwrap();
        assert_eq!(row.try_get::<String>("", "body").unwrap(), body);
    }
    assert_eq!(f.count("read_receipts").await, 0);
    f.store.integrity_check().await.unwrap();
}

#[tokio::test]
async fn alt_candidates_obey_self_warning_and_historical_retry_rules() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    f.join(&a, "alice").await;
    let bob = f.join(&b, "bob").await;
    let key = Uuid::new_v4();
    let command = Mutation::Post {
        channel: Some("dev".into()),
        body: "![description @BOB @alice @missing](url)".into(),
    };
    let first = f.app.execute(key, &a, command.clone()).await.unwrap();
    assert_eq!(first.result["mentioned_agent_ids"], json!([bob]));
    assert!(
        first.result["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["code"] == "unknown_recipient" && w["handle"] == "missing")
    );
    f.run(&b, Mutation::Leave).await;
    let before = f.counts().await;
    let replay = f.app.execute(key, &a, command).await.unwrap();
    assert_eq!(first.result, replay.result);
    assert!(replay.event_head.is_none());
    assert_eq!(f.counts().await, before);
    let new = f.post(&a, "![description @bob](url)").await;
    assert_eq!(new.result["mentioned_agent_ids"], json!([]));
    assert!(
        new.result["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["code"] == "recipient_left")
    );
    assert_eq!(f.count("read_receipts").await, 0);
}

#[tokio::test]
async fn alt_group_and_individual_candidates_share_current_channel_authority() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    let alice = f.join(&a, "alice").await;
    let bob = f.join(&b, "bob").await;
    f.run(
        &a,
        Mutation::ChannelCreate {
            name: "scoped".into(),
        },
    )
    .await;
    f.run(
        &a,
        Mutation::ChannelJoin {
            name: "scoped".into(),
        },
    )
    .await;
    let posted = f
        .run(
            &a,
            Mutation::Post {
                channel: Some("scoped".into()),
                body: "![description @bob @alice @all @here @channel](url)".into(),
            },
        )
        .await;
    assert_eq!(posted.result["mentioned_agent_ids"], json!([]));
    assert_eq!(
        posted.result["mention_results"],
        json!([
            {"handle":"alice", "recipient_id":alice, "state":"self"},
            {"handle":"bob", "recipient_id":bob, "state":"not_in_channel"}
        ])
    );
    assert_eq!(
        posted.result["warnings"],
        json!([
            {"handle":"bob", "recipient_id":bob, "code":"not_in_channel"}
        ])
    );
    let message = id(&posted.result, "message_id");
    assert!(
        !f.rows("mentions")
            .await
            .into_iter()
            .any(|row| row.try_get::<String>("", "message_id").unwrap() == message.to_string())
    );
    f.store.integrity_check().await.unwrap();
}
