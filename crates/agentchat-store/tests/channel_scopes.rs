//! Selective channel membership scopes agent content while observers retain history.

mod support;

use agentchat_core::{
    ErrorCode,
    application::Mutation,
    delivery::BatchState,
    model::{Delivery, DeliveryHistory, DeliveryState},
    ports::Storage,
    read::{PageRequest, Query, QueryResult},
    time::Clock,
};
use sea_orm::{
    ConnectionTrait,
    sea_query::{Alias, Query as Sql},
};
use serde_json::json;
use support::{Fixture, id};
use uuid::Uuid;

#[tokio::test]
async fn optional_channel_scope_mentions_reads_events_and_rejoin_delivery_boundary() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    f.join(&a, "alice").await;
    let bob = f.join(&b, "bob").await;
    f.run(
        &a,
        Mutation::ChannelCreate {
            name: "secret".into(),
        },
    )
    .await;
    f.run(
        &a,
        Mutation::ChannelJoin {
            name: "secret".into(),
        },
    )
    .await;

    let private = f
        .run(
            &a,
            Mutation::Post {
                channel: Some("secret".into()),
                body: "@bob @all private body".into(),
            },
        )
        .await;
    let private_id = id(&private.result, "message_id");
    assert_eq!(private.result["mentioned_agent_ids"], json!([]));
    assert_eq!(
        private.result["mention_results"],
        json!([{"handle":"bob","recipient_id":bob,"state":"not_in_channel"}])
    );
    assert_eq!(private.result["warnings"][0]["code"], "not_in_channel");
    assert_eq!(f.count("mentions").await, 0);
    assert!(
        !f.rows("sqlite_schema")
            .await
            .iter()
            .any(|row| row.try_get::<String>("", "name").unwrap() == "read_confirmations")
    );

    for command in [
        Mutation::Post {
            channel: Some("secret".into()),
            body: "unauthorized root".into(),
        },
        Mutation::Reply {
            read_message_ids: Vec::new(),
            root_message_id: private_id,
            body: "unauthorized reply".into(),
        },
    ] {
        assert_eq!(
            f.app
                .execute(Uuid::new_v4(), &b, command)
                .await
                .unwrap_err()
                .code,
            ErrorCode::NotJoined
        );
    }

    for query in [
        Query::Message {
            message_id: private_id,
            receipts: PageRequest::default(),
        },
        Query::History {
            channel: Some("secret".into()),
            thread: None,
            page: PageRequest::default(),
        },
        Query::Events {
            channel: Some("secret".into()),
            page: PageRequest::default(),
        },
    ] {
        assert_eq!(
            f.app.query(Some(&b), query).await.unwrap_err().code,
            ErrorCode::NotJoined
        );
    }
    assert_eq!(
        f.app
            .execute(
                Uuid::new_v4(),
                &b,
                Mutation::Read {
                    message_ids: vec![private_id],
                },
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotJoined
    );
    let observer = f
        .app
        .query(
            None,
            Query::Message {
                message_id: private_id,
                receipts: PageRequest::default(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(observer).unwrap()["message"]["body"],
        "@bob @all private body"
    );

    f.run(
        &b,
        Mutation::ChannelJoin {
            name: "secret".into(),
        },
    )
    .await;
    let visible = f
        .app
        .query(
            Some(&b),
            Query::History {
                channel: Some("secret".into()),
                thread: None,
                page: PageRequest::default(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(visible).unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let current = f
        .run(
            &a,
            Mutation::Post {
                channel: Some("secret".into()),
                body: "@bob current delivery".into(),
            },
        )
        .await;
    let current_id = id(&current.result, "message_id");
    let delivery = f.delivery(current_id, bob).await;
    let mut tx = f.store.begin().await.unwrap();
    let queued = tx.delivery(delivery).await.unwrap().unwrap();
    assert_eq!(queued.state, DeliveryState::Pending);
    assert!(queued.history.processing_started_at.is_none());
    assert!(queued.history.deadline_at.is_none());
    assert!(queued.history.left_at.is_none());
    tx.rollback().await.unwrap();

    f.clock.advance(1);
    f.run(
        &b,
        Mutation::ChannelLeave {
            name: "secret".into(),
        },
    )
    .await;
    let mut tx = f.store.begin().await.unwrap();
    let departed = tx.delivery(delivery).await.unwrap().unwrap();
    assert_eq!(departed.state, DeliveryState::FailedLeft);
    assert_eq!(departed.history.queued_at, queued.history.queued_at);
    assert_eq!(departed.history.left_at, Some(f.clock.now()));
    assert!(departed.history.processing_started_at.is_none());
    assert!(departed.history.deadline_at.is_none());
    assert!(departed.history.timed_out_at.is_none());
    assert!(departed.history.delivered_at.is_none());
    tx.rollback().await.unwrap();
    let inbox = f
        .app
        .query(
            Some(&b),
            Query::Inbox {
                page: PageRequest::default(),
            },
        )
        .await
        .unwrap();
    let encoded = serde_json::to_value(inbox).unwrap();
    assert!(
        !encoded["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| { item["delivery"]["id"] == json!(delivery) })
    );

    f.run(
        &b,
        Mutation::ChannelJoin {
            name: "secret".into(),
        },
    )
    .await;
    let mut tx = f.store.begin().await.unwrap();
    assert_eq!(tx.delivery(delivery).await.unwrap().unwrap(), departed);
    tx.rollback().await.unwrap();
    let inbox = f
        .app
        .query(
            Some(&b),
            Query::Inbox {
                page: PageRequest::default(),
            },
        )
        .await
        .unwrap();
    let encoded = serde_json::to_value(inbox).unwrap();
    assert!(
        !encoded["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| { item["delivery"]["id"] == json!(delivery) })
    );
}

async fn scoped_notice_is_hidden_after_membership_removal(cutover: bool) {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    let alice = f.join(&a, "alice").await;
    let bob = f.join(&b, "bob").await;
    f.run(
        &a,
        Mutation::ChannelCreate {
            name: "secret".into(),
        },
    )
    .await;
    for context in [&a, &b] {
        f.run(
            context,
            Mutation::ChannelJoin {
                name: "secret".into(),
            },
        )
        .await;
    }
    let posted = f
        .run(
            &a,
            Mutation::Post {
                channel: Some("secret".into()),
                body: "private retained body".into(),
            },
        )
        .await;
    let private_id = id(&posted.result, "message_id");
    let mut tx = f.store.begin().await.unwrap();
    use agentchat_core::model::{
        DispatchKind, DispatchNotice, DispatchPayload, EventKind, NoticeKind,
    };
    assert!("message.read".parse::<NoticeKind>().is_err());
    let channel = tx.channel("secret").await.unwrap().unwrap().id;
    let mut event = tx
        .allocate_event(EventKind::ChannelLeft, f.clock.now())
        .await
        .unwrap();
    event.actor_id = Some(alice);
    event.channel_id = Some(channel);
    event.data = json!({"agent_id":alice,"channel_id":channel});
    let legacy_event_id = event.event_id;
    tx.publish_event(&event).await.unwrap();
    let header = tx
        .allocate_dispatch(DispatchKind::Notice, Some(channel), f.clock.now())
        .await
        .unwrap();
    let notice_id = header.id;
    tx.publish_dispatch(
        &header,
        &DispatchPayload::Notice(DispatchNotice {
            dispatch_id: header.id,
            kind: NoticeKind::ChannelLeft,
            data: event.data.clone(),
        }),
        &[Delivery {
            id: Uuid::new_v4(),
            dispatch_id: header.id,
            recipient_id: bob,
            state: DeliveryState::Pending,
            wake_eligible: false,
            wake_batch_id: None,
            attempts: 0,
            history: DeliveryHistory::queued(f.clock.now()),
        }],
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    if cutover {
        f.inspect
            .execute(
                &Sql::delete()
                    .from_table(Alias::new("project_default_channels"))
                    .to_owned(),
            )
            .await
            .unwrap();
        f.app.initialize().await.unwrap();
    } else {
        f.run(
            &b,
            Mutation::ChannelLeave {
                name: "secret".into(),
            },
        )
        .await;
    }
    assert_eq!(
        f.app
            .query(
                Some(&b),
                Query::Message {
                    message_id: private_id,
                    receipts: PageRequest::default(),
                },
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotJoined
    );
    let QueryResult::Inbox(page) = f
        .app
        .query(
            Some(&b),
            Query::Inbox {
                page: PageRequest::default(),
            },
        )
        .await
        .unwrap()
    else {
        panic!("wrong query result");
    };
    assert!(page.items.iter().all(|row| row.dispatch.id != notice_id));
    let observer = f
        .query(
            None,
            Query::Events {
                channel: None,
                page: PageRequest::default(),
            },
        )
        .await;
    let retained = observer["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["event_id"] == legacy_event_id.to_string())
        .expect("observer audit retains the independently recorded occurrence");
    assert_eq!(retained["channel_id"], json!(channel));
    assert_eq!(retained["data"]["channel_id"], json!(channel));
}

#[tokio::test]
async fn optional_leave_hides_retained_legacy_unscoped_read_event() {
    scoped_notice_is_hidden_after_membership_removal(false).await;
}

#[tokio::test]
async fn legacy_cutover_hides_retained_unscoped_read_event() {
    scoped_notice_is_hidden_after_membership_removal(true).await;
}

#[tokio::test]
async fn channel_departure_fails_claimed_delivery_without_revival_after_rejoin() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    f.join(&a, "alice").await;
    let bob = f.join(&b, "bob").await;
    f.run(
        &a,
        Mutation::ChannelCreate {
            name: "secret".into(),
        },
    )
    .await;
    for context in [&a, &b] {
        f.run(
            context,
            Mutation::ChannelJoin {
                name: "secret".into(),
            },
        )
        .await;
    }
    let posted = f
        .run(
            &a,
            Mutation::Post {
                channel: Some("secret".into()),
                body: "@bob claimed wake".into(),
            },
        )
        .await;
    let delivery = f.delivery(id(&posted.result, "message_id"), bob).await;
    let claim = f.app.prepare_wake(&b).await.unwrap().unwrap();
    let refs = claim
        .hint
        .events
        .iter()
        .map(|event| event.delivery_id)
        .collect::<Vec<_>>();
    assert_eq!(refs, vec![delivery]);
    f.run(
        &b,
        Mutation::ChannelLeave {
            name: "secret".into(),
        },
    )
    .await;
    assert!(
        !f.app
            .wake_is_current(&b, claim.hint.batch_id, claim.token, &refs)
            .await
            .unwrap()
    );
    assert!(f.app.prepare_wake(&b).await.unwrap().is_none());
    let mut tx = f.store.begin().await.unwrap();
    assert!(
        !tx.wake_batch_has_pending(bob, claim.hint.batch_id)
            .await
            .unwrap()
    );
    assert_eq!(
        tx.wake_batch(claim.hint.batch_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        BatchState::Cancelled
    );
    let departed = tx.delivery(delivery).await.unwrap().unwrap();
    assert_eq!(departed.state, DeliveryState::FailedLeft);
    assert_eq!(departed.history.left_at, Some(f.clock.now()));
    assert!(departed.history.processing_started_at.is_some());
    assert!(departed.history.delivered_at.is_none());
    tx.rollback().await.unwrap();
    let mut snapshot = f.store.snapshot().await.unwrap();
    assert!(
        !snapshot
            .wake_batch_has_pending(bob, claim.hint.batch_id)
            .await
            .unwrap()
    );
    drop(snapshot);

    f.run(
        &b,
        Mutation::ChannelJoin {
            name: "secret".into(),
        },
    )
    .await;
    f.clock.advance(6);
    assert!(f.app.prepare_wake(&b).await.unwrap().is_none());
    assert!(
        !f.app
            .wake_is_current(&b, claim.hint.batch_id, claim.token, &refs)
            .await
            .unwrap()
    );
    let mut tx = f.store.begin().await.unwrap();
    assert!(
        !tx.wake_batch_has_pending(bob, claim.hint.batch_id)
            .await
            .unwrap()
    );
    assert_eq!(tx.delivery(delivery).await.unwrap().unwrap(), departed);
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn mixed_channel_wake_requires_every_original_reference_to_remain_scoped() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    f.join(&a, "alice").await;
    f.join(&b, "bob").await;
    for name in ["alpha", "beta"] {
        f.run(&a, Mutation::ChannelCreate { name: name.into() })
            .await;
        for context in [&a, &b] {
            f.run(context, Mutation::ChannelJoin { name: name.into() })
                .await;
        }
        f.run(
            &a,
            Mutation::Post {
                channel: Some(name.into()),
                body: format!("@bob {name}"),
            },
        )
        .await;
    }
    let claim = f.app.prepare_wake(&b).await.unwrap().unwrap();
    let refs = claim
        .hint
        .events
        .iter()
        .map(|event| event.delivery_id)
        .collect::<Vec<_>>();
    assert_eq!(refs.len(), 2);
    assert!(
        f.app
            .wake_is_current(&b, claim.hint.batch_id, claim.token, &refs)
            .await
            .unwrap()
    );
    let mut tx = f.store.begin().await.unwrap();
    let beta = tx.delivery(refs[1]).await.unwrap().unwrap();
    assert_eq!(beta.state, DeliveryState::Processing);
    tx.rollback().await.unwrap();
    f.run(
        &b,
        Mutation::ChannelLeave {
            name: "alpha".into(),
        },
    )
    .await;
    assert!(
        !f.app
            .wake_is_current(&b, claim.hint.batch_id, claim.token, &refs)
            .await
            .unwrap()
    );
    assert!(
        !f.app
            .wake_is_current(&b, claim.hint.batch_id, claim.token, &refs[1..])
            .await
            .unwrap()
    );
    f.clock.advance(5);
    let retry = f.app.prepare_wake(&b).await.unwrap().unwrap();
    assert_eq!(retry.hint.batch_id, claim.hint.batch_id);
    assert_eq!(retry.attempt, 2);
    assert_ne!(retry.token, claim.token);
    let retry_refs = retry
        .hint
        .events
        .iter()
        .map(|event| event.delivery_id)
        .collect::<Vec<_>>();
    assert_eq!(retry_refs, vec![beta.id]);
    assert!(
        !f.app
            .wake_is_current(&b, claim.hint.batch_id, claim.token, &refs)
            .await
            .unwrap()
    );
    assert!(
        !f.app
            .wake_is_current(&b, claim.hint.batch_id, claim.token, &retry_refs)
            .await
            .unwrap()
    );
    assert!(
        f.app
            .wake_is_current(&b, retry.hint.batch_id, retry.token, &retry_refs)
            .await
            .unwrap()
    );
    let mut tx = f.store.begin().await.unwrap();
    let beta_retry = tx.delivery(beta.id).await.unwrap().unwrap();
    assert_eq!(beta_retry.state, DeliveryState::Processing);
    assert_eq!(beta_retry.history, beta.history);
    assert_eq!(beta_retry.attempts, 2);
    assert!(beta_retry.history.delivered_at.is_none());
    tx.rollback().await.unwrap();
    f.assert_read_kinds(0, 0).await;
}
