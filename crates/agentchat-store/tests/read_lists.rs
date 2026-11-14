//! Diagnostic identity/channel/lease queries remain independent of participation.

mod support;

use agentchat_core::{
    ErrorCode,
    application::Mutation,
    model::Phase,
    read::{ChannelListCondition, PageRequest, Query},
    sequence::Cursor,
};
use serde_json::json;
use support::{Fixture, id};
use uuid::Uuid;

#[tokio::test]
async fn identity_status_membership_and_unread_counts_are_read_only() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    let fresh = f.context();
    let alice = f.join(&a, "alice").await;
    let bob = f.join(&b, "bob").await;
    let root = f.post(&a, "@bob a message").await;
    let root_id = id(&root.result, "message_id");
    f.run(
        &a,
        Mutation::ChannelCreate {
            name: "planning".into(),
        },
    )
    .await;
    f.run(
        &a,
        Mutation::ChannelJoin {
            name: "planning".into(),
        },
    )
    .await;
    f.run(
        &b,
        Mutation::ChannelJoin {
            name: "planning".into(),
        },
    )
    .await;
    f.app.detach(&b).await.unwrap();
    let before = f.counts().await;
    let unjoined = f.query(Some(&fresh), Query::Whoami).await;
    assert_eq!(unjoined["participation"], "unjoined");
    assert!(unjoined["actor"].is_null());
    assert_eq!(
        f.app
            .query(None, Query::Whoami)
            .await
            .map(|value| serde_json::to_value(value).unwrap())
            .unwrap_err()
            .code,
        ErrorCode::SessionRequired
    );
    assert_eq!(
        f.app
            .query(
                None,
                Query::Inbox {
                    page: PageRequest::default()
                }
            )
            .await
            .map(|value| serde_json::to_value(value).unwrap())
            .unwrap_err()
            .code,
        ErrorCode::SessionRequired
    );
    assert_eq!(
        f.app
            .query(
                Some(&fresh),
                Query::Inbox {
                    page: PageRequest::default()
                }
            )
            .await
            .map(|value| serde_json::to_value(value).unwrap())
            .unwrap_err()
            .code,
        ErrorCode::NotJoined
    );
    let status = f.health().await;
    assert_eq!(status["statistics"]["agents"], 2);
    assert_eq!(status["statistics"]["joined"], 2);
    assert_eq!(status["statistics"]["channels"], 2);
    assert_eq!(status["statistics"]["messages"], 1);
    assert_eq!(status["statistics"]["stalled_batches"], 0);
    assert!(status.get("binding").is_none());
    let agents = f
        .query(
            None,
            Query::Agents {
                page: PageRequest::default(),
            },
        )
        .await;
    assert_eq!(agents["items"].as_array().unwrap().len(), 2);
    assert_eq!(agents["items"][0]["id"], alice.to_string());
    assert_eq!(agents["items"][0]["unread_count"], 0);
    assert_eq!(agents["items"][1]["id"], bob.to_string());
    assert_eq!(agents["items"][1]["unread_count"], 1);
    assert_eq!(agents["items"][1]["presence"], "disconnected");
    let members = f
        .query(
            None,
            Query::ChannelMembers {
                channel: "PLANNING".into(),
                page: PageRequest::default(),
            },
        )
        .await;
    assert_eq!(members["items"].as_array().unwrap().len(), 2);
    assert_eq!(members["items"][1]["connected"], false);
    let channels = f
        .query(
            None,
            Query::Channels {
                condition: ChannelListCondition::Active,
                page: PageRequest::default(),
            },
        )
        .await;
    assert_eq!(channels["items"][0]["name"], "dev");
    assert_eq!(channels["items"][1]["name"], "planning");
    assert_eq!(f.counts().await, before);
    let mut resumed = b.clone();
    resumed.context_id = Uuid::new_v4();
    resumed.incarnation = Uuid::new_v4();
    f.app.attach(&resumed).await.unwrap();
    f.run(
        &resumed,
        Mutation::Read {
            message_ids: vec![root_id],
        },
    )
    .await;
    let status = f.query(Some(&resumed), Query::Whoami).await;
    assert_eq!(status["actor"]["actor_id"], bob.to_string());
    let directory = f
        .query(
            None,
            Query::Agents {
                page: PageRequest::default(),
            },
        )
        .await;
    let actor = directory["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == bob.to_string())
        .unwrap();
    assert_eq!(actor["unread_count"], 0);
    f.host.phase(&a, Phase::ApprovalBlocked);
    f.app.heartbeat(&a).await.unwrap();
    let who = f.query(Some(&a), Query::Whoami).await;
    assert_eq!(who["actor"]["presence"], "approval_blocked");
    assert_eq!(who["actor"]["connected"], true);
    f.clock.advance(45);
    assert_eq!(
        f.query(Some(&a), Query::Whoami).await["actor"]["presence"],
        "disconnected"
    );
}

#[tokio::test]
async fn first_join_cursor_is_stable_after_leave_rejoin_and_stale_contexts_fail() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    let alice = f.join(&a, "alice").await;
    let bob = f.join(&b, "bob").await;
    let first = f
        .query(
            None,
            Query::Agents {
                page: PageRequest {
                    limit: Some(1),
                    after: Cursor::START,
                },
            },
        )
        .await;
    assert_eq!(first["items"][0]["id"], alice.to_string());
    let after = Cursor::parse(first["next_cursor"].as_str().unwrap()).unwrap();
    f.run(&a, Mutation::Leave).await;
    f.run(&a, Mutation::Join { name: None }).await;
    let next = f
        .query(
            None,
            Query::Agents {
                page: PageRequest {
                    limit: Some(1),
                    after,
                },
            },
        )
        .await;
    assert_eq!(next["items"][0]["id"], bob.to_string());
    assert!(next["next_cursor"].is_null());
    f.run(&b, Mutation::Leave).await;
    let members = f
        .query(
            None,
            Query::ChannelMembers {
                channel: "dev".into(),
                page: PageRequest::default(),
            },
        )
        .await;
    assert_eq!(members["items"].as_array().unwrap().len(), 1);
    let mut resumed = a.clone();
    resumed.context_id = Uuid::new_v4();
    resumed.incarnation = Uuid::new_v4();
    f.app.attach(&resumed).await.unwrap();
    assert_eq!(
        f.app
            .query(Some(&a), Query::Whoami)
            .await
            .map(|value| serde_json::to_value(value).unwrap())
            .unwrap_err()
            .code,
        ErrorCode::IdentityConflict
    );
    assert_eq!(
        f.query(Some(&resumed), Query::Whoami).await["actor"]["actor_id"],
        alice.to_string()
    );
}

#[tokio::test]
async fn reservation_reads_show_effective_expiry_without_events_renewal_or_repair() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    f.join(&a, "alice").await;
    f.join(&b, "bob").await;
    let first = f
        .run(
            &a,
            Mutation::Reserve {
                patterns: vec!["one/**".into()],
                ttl_seconds: Some(30),
                reason: None,
            },
        )
        .await;
    let second = f
        .run(
            &a,
            Mutation::Reserve {
                patterns: vec!["two/**".into()],
                ttl_seconds: Some(30),
                reason: Some("bounded reason".into()),
            },
        )
        .await;
    f.run(
        &b,
        Mutation::Reserve {
            patterns: vec!["other/**".into()],
            ttl_seconds: Some(600),
            reason: None,
        },
    )
    .await;
    f.run(
        &a,
        Mutation::ReservationsRelease {
            reservation_id: id(&first.result, "reservation_id"),
            generation: first.result["generation"].as_i64().unwrap(),
        },
    )
    .await;
    let before = f.counts().await;
    let active = f
        .query(
            None,
            Query::Reservations {
                owner: Some("ALICE".into()),
                all: false,
                page: PageRequest::default(),
            },
        )
        .await;
    assert_eq!(active["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        active["items"][0]["reservation_id"],
        second.result["reservation_id"]
    );
    f.clock.advance(31);
    let empty = f
        .query(
            None,
            Query::Reservations {
                owner: Some("alice".into()),
                all: false,
                page: PageRequest::default(),
            },
        )
        .await;
    assert_eq!(empty["items"], json!([]));
    let all = f
        .query(
            None,
            Query::Reservations {
                owner: Some("alice".into()),
                all: true,
                page: PageRequest::default(),
            },
        )
        .await;
    assert_eq!(all["items"].as_array().unwrap().len(), 2);
    assert_eq!(all["items"][0]["state"], "released");
    assert_eq!(all["items"][1]["state"], "expired");
    assert_eq!(all["items"][1]["patterns"], json!(["two/**"]));
    let missing = f
        .app
        .query(
            None,
            Query::Reservations {
                owner: Some("missing".into()),
                all: true,
                page: PageRequest::default(),
            },
        )
        .await
        .map(|value| serde_json::to_value(value).unwrap())
        .unwrap_err();
    assert_eq!(missing.code, ErrorCode::NotFound);
    assert_eq!(f.counts().await, before);
    assert_eq!(
        f.rows("reservations")
            .await
            .iter()
            .filter(|r| r.try_get::<String>("", "state").unwrap() == "active")
            .count(),
        2
    );
}
