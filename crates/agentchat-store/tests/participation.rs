//! Real SQLite participation and membership policy with synthetic live hosts.

mod support;

use agentchat_core::{ErrorCode, application::Mutation, model::Phase, validation};
use support::{Fixture, id};
use uuid::Uuid;

#[tokio::test]
async fn exact_identity_immutable_names_and_explicit_optional_membership() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    let fresh = f.context();
    let initial = f.counts().await;
    assert!(f.app.attach(&fresh).await.unwrap().is_none());
    let error = f
        .app
        .execute(
            Uuid::new_v4(),
            &fresh,
            Mutation::Post {
                channel: Some("dev".into()),
                body: "uninvited".into(),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::NotJoined);
    assert_eq!(f.counts().await, initial);
    let alice = f.join(&a, "Alice").await;
    let again = f
        .run(
            &a,
            Mutation::Join {
                name: Some("ALICE".into()),
            },
        )
        .await;
    assert_eq!(id(&again.result, "agent_id"), alice);
    assert!(again.event_head.is_none());
    let before = f.counts().await;
    for (ctx, name) in [(&a, "renamed"), (&b, "alice")] {
        let error = f
            .app
            .execute(
                Uuid::new_v4(),
                ctx,
                Mutation::Join {
                    name: Some(name.into()),
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::IdentityConflict);
        assert_eq!(f.counts().await, before);
    }
    let bob_result = f.run(&b, Mutation::Join { name: None }).await;
    let bob = id(&bob_result.result, "agent_id");
    validation::handle(bob_result.result["handle"].as_str().unwrap()).unwrap();
    f.app.detach(&b).await.unwrap();
    let (first, second) = tokio::join!(
        f.app.execute(
            Uuid::new_v4(),
            &a,
            Mutation::ChannelCreate {
                name: "planning".into()
            }
        ),
        f.app.execute(
            Uuid::new_v4(),
            &a,
            Mutation::ChannelCreate {
                name: "planning".into()
            }
        )
    );
    let first = first.unwrap();
    let second = second.unwrap();
    assert_eq!(first.result, second.result);
    assert_eq!(first.result["membership_count"], 0);
    assert!(first.event_head.is_some() ^ second.event_head.is_some());
    assert_eq!(f.count("channels").await, 2);
    assert_eq!(f.count("channel_memberships").await, 2);
    let joined = f
        .run(
            &a,
            Mutation::ChannelJoin {
                name: "planning".into(),
            },
        )
        .await;
    assert_eq!(joined.result["changed"], true);
    assert_eq!(
        f.run(
            &a,
            Mutation::ChannelJoin {
                name: "planning".into(),
            },
        )
        .await
        .result["changed"],
        false
    );
    assert_eq!(f.count("channel_memberships").await, 3);
    let rows = f.rows("channel_memberships").await;
    assert_eq!(
        rows.iter()
            .filter(|r| r.try_get::<String>("", "actor_id").unwrap() == bob.to_string())
            .count(),
        1
    );
    assert_eq!(
        f.app
            .execute(
                Uuid::new_v4(),
                &a,
                Mutation::ChannelLeave { name: "dev".into() },
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument
    );
    let before = f.counts().await;
    let mut foreign = a.clone();
    foreign.project_id = Uuid::new_v4();
    assert_eq!(
        f.app
            .execute(Uuid::new_v4(), &foreign, Mutation::Leave)
            .await
            .unwrap_err()
            .code,
        ErrorCode::ProjectBindingMismatch
    );
    let mut unsupported = f.context();
    unsupported.host_version = "unverified".into();
    assert_eq!(
        f.app
            .execute(Uuid::new_v4(), &unsupported, Mutation::Join { name: None })
            .await
            .unwrap_err()
            .code,
        ErrorCode::UnsupportedHost
    );
    assert_eq!(f.counts().await, before);
    f.store.integrity_check().await.unwrap();
}

#[tokio::test]
async fn ordered_defaults_reconfigure_current_members_and_drive_future_join_and_primary() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    f.join(&a, "alice").await;
    f.join(&b, "bob").await;
    f.run(
        &a,
        Mutation::ChannelCreate {
            name: "planning".into(),
        },
    )
    .await;

    let configured = f
        .app
        .initialize_with_defaults(Some(vec![
            " Planning ".into(),
            "ops".into(),
            "PLANNING".into(),
        ]))
        .await
        .unwrap();
    assert_eq!(
        configured.result["default_channels"],
        serde_json::json!(["planning", "ops"])
    );
    assert_eq!(configured.result["primary_channel"], "planning");
    assert_eq!(configured.result["changed"], true);
    assert_eq!(f.count("channels").await, 3);
    assert_eq!(f.count("channel_memberships").await, 6);
    let events = f.rows("events").await;
    let deliveries = f.rows("deliveries").await;
    for event in events
        .iter()
        .filter(|event| event.try_get::<String>("", "kind").unwrap() == "channel.joined")
    {
        let Some(actor) = event.try_get::<Option<String>>("", "actor_id").unwrap() else {
            continue;
        };
        for notice in f
            .rows("dispatch_notices")
            .await
            .into_iter()
            .filter(|r| r.try_get::<String>("", "kind").unwrap() == "channel.joined")
        {
            let data: serde_json::Value = notice.try_get("", "data").unwrap();
            if data["agent_id"] == actor {
                let id = notice.try_get::<String>("", "dispatch_id").unwrap();
                assert!(
                    !deliveries
                        .iter()
                        .any(|d| d.try_get::<String>("", "dispatch_id").unwrap() == id
                            && d.try_get::<String>("", "recipient_id").unwrap() == actor)
                );
            }
        }
    }

    let omitted = f.app.initialize_with_defaults(None).await.unwrap();
    assert_eq!(
        omitted.result["default_channels"],
        configured.result["default_channels"]
    );
    assert_eq!(omitted.result["changed"], false);

    let c = f.context();
    let carol = f.join(&c, "carol").await;
    let rows = f.rows("channel_memberships").await;
    let carol_channels = rows
        .iter()
        .filter(|row| row.try_get::<String>("", "actor_id").unwrap() == carol.to_string())
        .count();
    assert_eq!(carol_channels, 2);

    let post = f
        .run(
            &c,
            Mutation::Post {
                channel: None,
                body: "primary route".into(),
            },
        )
        .await;
    let message = id(&post.result, "message_id");
    let row = f
        .rows("dispatches")
        .await
        .into_iter()
        .find(|row| row.try_get::<String>("", "id").unwrap() == message.to_string())
        .unwrap();
    let planning = f
        .rows("channels")
        .await
        .into_iter()
        .find(|row| row.try_get::<String>("", "name").unwrap() == "planning")
        .unwrap()
        .try_get::<String>("", "id")
        .unwrap();
    assert_eq!(row.try_get::<String>("", "channel_id").unwrap(), planning);
}

#[tokio::test]
async fn resume_fork_leave_and_stale_callbacks_preserve_exact_conversation() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    let alice = f.join(&a, "alice").await;
    let bob = f.join(&b, "bob").await;
    f.app.detach(&b).await.unwrap();
    let pending = f.post(&a, "@bob retain for resume").await;
    let mut resumed = b.clone();
    resumed.context_id = Uuid::new_v4();
    resumed.incarnation = Uuid::new_v4();
    let restored = f.app.attach(&resumed).await.unwrap().unwrap();
    assert_eq!(restored.id, bob);
    assert!(restored.joined);
    for stale in [
        f.app.detach(&b).await,
        f.app.heartbeat(&b).await,
        f.app.attach(&b).await.map(|_| ()),
    ] {
        assert_eq!(stale.unwrap_err().code, ErrorCode::IdentityConflict);
    }
    let fork = f.context();
    assert!(f.app.attach(&fork).await.unwrap().is_none());
    assert_eq!(f.count("actors").await, 2);
    let post_id = id(&pending.result, "message_id");
    let delivery = f.delivery(post_id, bob).await;
    f.run(&resumed, Mutation::Leave).await;
    let before = f.counts().await;
    assert!(f.run(&resumed, Mutation::Leave).await.event_head.is_none());
    let mut after = f.counts().await;
    assert_eq!(after["idempotency"], before["idempotency"] + 1);
    after.insert("idempotency".into(), before["idempotency"]);
    assert_eq!(
        after, before,
        "repeated Leave adds no business or history facts"
    );
    let left = f.post(&a, "passive while bob is left").await;
    let left_id = id(&left.result, "message_id");
    let rejoin = f
        .run(
            &resumed,
            Mutation::Join {
                name: Some("bob".into()),
            },
        )
        .await;
    assert_eq!(id(&rejoin.result, "agent_id"), bob);
    assert_eq!(f.count("actors").await, 2);
    let rows = f.rows("deliveries").await;
    assert_eq!(
        rows.iter()
            .find(|r| r.try_get::<String>("", "id").unwrap() == delivery.to_string())
            .unwrap()
            .try_get::<String>("", "state")
            .unwrap(),
        "failed_left"
    );
    assert_eq!(
        rows.iter()
            .filter(|r| r.try_get::<String>("", "dispatch_id").unwrap() == left_id.to_string())
            .count(),
        0
    );
    let current = f.app.attach(&resumed).await.unwrap().unwrap();
    assert!(current.joined);
    assert_ne!(alice, current.id);
    f.host.phase(&resumed, Phase::Paused);
    f.app.heartbeat(&resumed).await.unwrap();
    let session = f
        .rows("sessions")
        .await
        .into_iter()
        .find(|r| {
            r.try_get::<String>("", "incarnation").unwrap() == resumed.incarnation.to_string()
        })
        .unwrap();
    assert_eq!(session.try_get::<String>("", "state").unwrap(), "paused");
    assert!(session.try_get::<bool>("", "wake_paused").unwrap());
    f.store.integrity_check().await.unwrap();
}
