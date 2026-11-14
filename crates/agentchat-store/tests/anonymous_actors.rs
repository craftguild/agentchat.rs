//! Anonymous callers share one actor, ordinary policy and no native receiver.

mod support;

use agentchat_core::{
    ErrorCode,
    application::Mutation,
    model::OperationActor,
    ports::Storage,
    read::{PageRequest, Query, QueryResult},
};
use sea_orm::{
    ConnectionTrait,
    sea_query::{Alias, Expr, ExprTrait, Query as Sql},
};
use support::Fixture;
use uuid::Uuid;

fn user() -> OperationActor {
    OperationActor::User
}

#[tokio::test]
async fn same_text_owner_labels_remain_typed_and_cannot_borrow_each_others_lease() {
    let f = Fixture::new().await;
    let native = f.context();
    f.join(&native, "user").await;
    f.app
        .execute_actor(Uuid::new_v4(), &user(), Mutation::Join { name: None })
        .await
        .unwrap();
    let owned = f
        .app
        .execute_actor(
            Uuid::new_v4(),
            &user(),
            Mutation::Reserve {
                patterns: vec!["human/**".into()],
                ttl_seconds: None,
                reason: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(owned.result["owner"]["kind"], "user");
    f.run(
        &native,
        Mutation::Reserve {
            patterns: vec!["native/**".into()],
            ttl_seconds: None,
            reason: None,
        },
    )
    .await;
    let conflict = f
        .app
        .execute(
            Uuid::new_v4(),
            &native,
            Mutation::Reserve {
                patterns: vec!["human/file".into()],
                ttl_seconds: None,
                reason: None,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(conflict.code, ErrorCode::ReservationConflict);
    assert_eq!(conflict.details["conflicts"][0]["owner"]["kind"], "user");
    let QueryResult::Reservations(page) = f
        .app
        .query(
            None,
            Query::Reservations {
                owner: Some("user".into()),
                all: false,
                page: PageRequest::default(),
            },
        )
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(page.items.len(), 2);
    assert_ne!(page.items[0].owner.kind, page.items[1].owner.kind);
    let error = f
        .app
        .execute(
            Uuid::new_v4(),
            &native,
            Mutation::ReservationsRenew {
                reservation_id: Uuid::parse_str(owned.result["reservation_id"].as_str().unwrap())
                    .unwrap(),
                generation: owned.result["generation"].as_i64().unwrap(),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::StaleLease);
}

#[tokio::test]
async fn anonymous_lookup_is_inert_and_content_never_downgrades_to_observer() {
    let f = Fixture::new().await;
    let native = f.context();
    f.join(&native, "alice").await;
    f.run(
        &native,
        Mutation::Post {
            channel: None,
            body: "retained observer body".into(),
        },
    )
    .await;
    let before = f.store.snapshot().await.unwrap().head();
    let QueryResult::Whoami(identity) = f
        .app
        .query_actor(Some(&user()), Query::Whoami)
        .await
        .unwrap()
    else {
        panic!()
    };
    assert!(
        identity.actor.is_none()
            && identity.context_id.is_none()
            && identity.native_delivery.is_none()
    );
    assert_eq!(
        identity.participation,
        agentchat_core::read::Participation::Unjoined
    );
    assert!(
        f.store
            .snapshot()
            .await
            .unwrap()
            .user()
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(f.store.snapshot().await.unwrap().head(), before);
    let history = Query::History {
        channel: None,
        thread: None,
        page: PageRequest::default(),
    };
    assert!(f.app.query_actor(None, history.clone()).await.is_ok());
    assert_eq!(
        f.app
            .query_actor(Some(&user()), history.clone())
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotJoined
    );
    f.app
        .execute_actor(Uuid::new_v4(), &user(), Mutation::Join { name: None })
        .await
        .unwrap();
    assert!(
        f.app
            .query_actor(Some(&user()), history.clone())
            .await
            .is_ok()
    );
    f.app
        .execute_actor(Uuid::new_v4(), &user(), Mutation::Leave)
        .await
        .unwrap();
    assert_eq!(
        f.app
            .query_actor(Some(&user()), history)
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotJoined
    );
}

#[tokio::test]
async fn concurrent_join_retries_and_local_callers_share_identity_and_reservations() {
    let f = Fixture::new().await;
    let key = Uuid::new_v4();
    let actor = user();
    let (a, b) = tokio::join!(
        f.app
            .execute_actor(key, &actor, Mutation::Join { name: None }),
        f.app
            .execute_actor(Uuid::new_v4(), &actor, Mutation::Join { name: None }),
    );
    let a = a.unwrap();
    let b = b.unwrap();
    assert_eq!(a.result["actor_id"], b.result["actor_id"]);
    assert_eq!(
        f.app
            .execute_actor(key, &actor, Mutation::Join { name: None })
            .await
            .unwrap()
            .result,
        a.result
    );
    assert_eq!(f.app.health().await.unwrap().statistics.agents, 0);
    assert_eq!(f.app.health().await.unwrap().statistics.joined, 0);
    let reserved = f
        .app
        .execute_actor(
            Uuid::new_v4(),
            &user(),
            Mutation::Reserve {
                patterns: vec!["src/**".into()],
                ttl_seconds: Some(60),
                reason: None,
            },
        )
        .await
        .unwrap();
    let id = Uuid::parse_str(reserved.result["reservation_id"].as_str().unwrap()).unwrap();
    let generation = reserved.result["generation"].as_i64().unwrap();
    f.app
        .execute_actor(
            Uuid::new_v4(),
            &user(),
            Mutation::ReservationsRenew {
                reservation_id: id,
                generation,
            },
        )
        .await
        .unwrap();
    f.app
        .execute_actor(
            Uuid::new_v4(),
            &user(),
            Mutation::ReservationsRelease {
                reservation_id: id,
                generation,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        f.app
            .execute_actor(
                Uuid::new_v4(),
                &user(),
                Mutation::Join {
                    name: Some("renamed".into())
                }
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument
    );
}

#[tokio::test]
async fn user_membership_posts_and_explicit_reads_never_create_native_receivers() {
    let f = Fixture::new().await;
    let native = f.context();
    f.join(&native, "user").await;
    let joined = f
        .app
        .execute_actor(Uuid::new_v4(), &user(), Mutation::Join { name: None })
        .await
        .unwrap();
    let id = Uuid::parse_str(joined.result["actor_id"].as_str().unwrap()).unwrap();
    f.app
        .initialize_with_defaults(Some(vec!["dev".into(), "general".into()]))
        .await
        .unwrap();
    let channel = f
        .store
        .snapshot()
        .await
        .unwrap()
        .channel("general")
        .await
        .unwrap()
        .unwrap();
    assert!(
        f.store
            .snapshot()
            .await
            .unwrap()
            .is_member(channel.id, id)
            .await
            .unwrap()
    );
    let posted = f
        .run(
            &native,
            Mutation::Post {
                channel: None,
                body: "@all complete body".into(),
            },
        )
        .await;
    let message = Uuid::parse_str(posted.result["message_id"].as_str().unwrap()).unwrap();
    f.app
        .execute_actor(
            Uuid::new_v4(),
            &user(),
            Mutation::Read {
                message_ids: vec![message],
            },
        )
        .await
        .unwrap();
    assert!(
        f.store
            .snapshot()
            .await
            .unwrap()
            .receipt(message, id)
            .await
            .unwrap()
            .is_some()
    );
    for (table, column) in [
        ("deliveries", "recipient_id"),
        ("sessions", "agent_id"),
        ("launches", "agent_id"),
    ] {
        assert!(
            f.inspect
                .query_one(
                    &Sql::select()
                        .column(Alias::new(column))
                        .from(Alias::new(table))
                        .and_where(Expr::col(Alias::new(column)).eq(id.to_string()))
                        .limit(1)
                        .to_owned()
                )
                .await
                .unwrap()
                .is_none()
        );
    }
    assert!(
        serde_json::from_value::<Mutation>(serde_json::json!({
            "operation":"ack","delivery_ids":[Uuid::new_v4()]
        }))
        .is_err()
    );
    assert_eq!(
        f.app
            .query_actor(
                Some(&user()),
                Query::Inbox {
                    page: PageRequest::default()
                }
            )
            .await
            .unwrap_err()
            .details["reason"],
        "nonreceiving_actor"
    );
    let written = f
        .app
        .execute_actor(
            Uuid::new_v4(),
            &user(),
            Mutation::Post {
                channel: None,
                body: "anonymous author".into(),
            },
        )
        .await
        .unwrap();
    let message = Uuid::parse_str(written.result["message_id"].as_str().unwrap()).unwrap();
    let QueryResult::Message(detail) = f
        .app
        .query_actor(
            Some(&user()),
            Query::Message {
                message_id: message,
                receipts: PageRequest::default(),
            },
        )
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(detail.message.author.actor_id, id);
    assert_eq!(
        detail.message.author.kind,
        agentchat_core::model::ActorKind::User
    );
    assert_eq!(f.app.health().await.unwrap().statistics.agents, 1);
    assert_eq!(f.app.health().await.unwrap().statistics.joined, 1);
}
