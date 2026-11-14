//! Own audit events create no receiving obligation; timeout notices remain author-only.
mod support;
use agentchat_core::{
    application::Mutation,
    ports::Storage,
    read::{PageRequest, Query},
    time::Clock,
};
use sea_orm::{
    ConnectionTrait,
    sea_query::{Alias, Query as SqlQuery},
};
use serde_json::json;
use support::{Fixture, id, native::Gate};
use uuid::Uuid;

#[tokio::test]
async fn own_events_stay_in_audit_and_retained_self_rows_stay_off_inbox() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    let alice = f.join(&a, "alice").await;
    f.join(&b, "bob").await;
    let posted = f.post(&a, "own immutable body").await;
    let message = id(&posted.result, "message_id");
    let events = f.rows("events").await;
    let event = events
        .iter()
        .find(|r| {
            r.try_get::<Option<String>>("", "message_id").unwrap() == Some(message.to_string())
        })
        .unwrap();
    assert_eq!(
        event.try_get::<Option<String>>("", "actor_id").unwrap(),
        Some(alice.to_string())
    );
    assert!(!f.rows("deliveries").await.iter().any(|r| {
        r.try_get::<String>("", "dispatch_id").unwrap() == message.to_string()
            && r.try_get::<String>("", "recipient_id").unwrap() == alice.to_string()
    }));

    // A deliberately retained self row is hidden at query time without
    // fabricating acceptance or deleting its original history.
    let legacy = Uuid::new_v4();
    f.inspect
        .execute(
            &SqlQuery::insert()
                .into_table(Alias::new("deliveries"))
                .columns(
                    [
                        "id",
                        "dispatch_id",
                        "state_version",
                        "recipient_id",
                        "state",
                        "wake_eligible",
                        "attempts",
                        "queued_at",
                    ]
                    .map(Alias::new),
                )
                .values([
                    legacy.to_string().into(),
                    message.to_string().into(),
                    1i64.into(),
                    alice.to_string().into(),
                    "pending".into(),
                    false.into(),
                    0i32.into(),
                    f.clock.now().to_string().into(),
                ])
                .unwrap()
                .to_owned(),
        )
        .await
        .unwrap();
    f.inspect
        .execute(
            &SqlQuery::insert()
                .into_table(Alias::new("delivery_state_transitions"))
                .columns(["id", "delivery_id", "ordinal", "to_state", "changed_at"].map(Alias::new))
                .values_panic([
                    Uuid::new_v4().to_string().into(),
                    legacy.to_string().into(),
                    1i64.into(),
                    "pending".into(),
                    f.clock.now().to_string().into(),
                ])
                .to_owned(),
        )
        .await
        .unwrap();
    let other = id(
        &f.post(&b, "@alice another participant's body").await.result,
        "message_id",
    );
    let inbox = f
        .query(
            Some(&a),
            Query::Inbox {
                page: PageRequest {
                    limit: Some(200),
                    ..Default::default()
                },
            },
        )
        .await;
    let items = inbox["items"].as_array().unwrap();
    assert!(
        !items
            .iter()
            .any(|x| x["dispatch"]["id"] == message.to_string())
    );
    assert!(
        items
            .iter()
            .any(|x| x["dispatch"]["id"] == other.to_string())
    );
    let mut tx = f.store.begin().await.unwrap();
    let retained = tx.delivery(legacy).await.unwrap().unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(
        retained.state,
        agentchat_core::model::DeliveryState::Pending
    );
    assert!(retained.history.delivered_at.is_none());
    assert!(retained.history.processing_started_at.is_none());
    let history = f
        .query(
            None,
            Query::History {
                channel: None,
                thread: None,
                page: PageRequest::default(),
            },
        )
        .await;
    assert!(
        history["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["id"] == json!(message))
    );
}

#[tokio::test]
async fn actorless_delivery_deadline_notice_remains_visible_to_its_original_author() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    f.join(&a, "alice").await;
    let gate = Gate::new(&f);
    gate.bind(&b);
    let app = gate.app(&f);
    app.start_launch(&b, Some("bob")).await.unwrap();
    f.timeout(1).await;
    let posted = f
        .run(
            &a,
            Mutation::Post {
                channel: Some("dev".into()),
                body: "@bob timed request".into(),
            },
        )
        .await;
    let original = app.prepare_message(&b).await.unwrap().unwrap();
    assert!(!original.awaiting_acceptance);
    f.clock.advance(2);
    assert_eq!(
        f.app
            .maintain()
            .await
            .unwrap()
            .delivery_confirmation_timeouts,
        1
    );
    let inbox = f
        .query(
            Some(&a),
            Query::Inbox {
                page: PageRequest {
                    limit: Some(200),
                    ..Default::default()
                },
            },
        )
        .await;
    let notice = inbox["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["notice"]["kind"] == "message.delivery_confirmation_timeout")
        .unwrap();
    assert!(notice.get("event").is_none());
    assert_eq!(notice["dispatch"]["kind"], "notice");
    assert_eq!(
        notice["notice"]["data"]["message_id"],
        posted.result["message_id"]
    );
    assert_eq!(notice["delivery"]["wake_eligible"], true);
}
