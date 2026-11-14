//! Actual immediate transactions serialize exact-glob exclusivity and leases.

mod support;

use agentchat_core::{
    ErrorCode,
    application::{Application, Mutation},
    glob::Glob,
};
use agentchat_store::Store;
use std::sync::Arc;
use support::{Fixture, id};
use uuid::Uuid;

fn reserve(patterns: &[&str], ttl: u32) -> Mutation {
    Mutation::Reserve {
        patterns: patterns.iter().map(|s| s.to_string()).collect(),
        ttl_seconds: Some(ttl),
        reason: None,
    }
}

#[tokio::test]
async fn groups_are_atomic_conflicts_have_witnesses_and_racing_owners_cannot_both_win() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    f.join(&a, "alice").await;
    f.join(&b, "bob").await;
    f.run(&a, reserve(&["src/*.rs", "docs/**"], 600)).await;
    let before = f.counts().await;
    let error = f
        .app
        .execute(
            Uuid::new_v4(),
            &b,
            reserve(&["new/**", "src/future.rs"], 600),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::ReservationConflict);
    assert!(!error.retryable);
    let conflict = &error.details["conflicts"][0];
    assert_eq!(conflict["owner"]["handle"], "alice");
    let witness = conflict["witness_path"].as_str().unwrap();
    assert!(
        Glob::compile(conflict["requested_pattern"].as_str().unwrap())
            .unwrap()
            .is_match(witness)
    );
    assert!(
        Glob::compile(conflict["existing_pattern"].as_str().unwrap())
            .unwrap()
            .is_match(witness)
    );
    assert_eq!(f.counts().await, before);
    // Same-owner overlap and independent paths remain legal.
    f.run(&a, reserve(&["src/future.rs"], 600)).await;
    f.run(&b, reserve(&["tests/**"], 600)).await;
    let independent_store = Arc::new(
        Store::open(
            f.root.path().join("agentchat.sqlite3"),
            f.store.identity().clone(),
        )
        .await
        .unwrap(),
    );
    let independent = Application::new(independent_store, f.clock.clone(), f.host.clone());
    let (first, second) = tokio::join!(
        f.app
            .execute(Uuid::new_v4(), &a, reserve(&["race/**"], 600)),
        independent.execute(Uuid::new_v4(), &b, reserve(&["race/new.rs"], 600))
    );
    assert!(first.is_ok() ^ second.is_ok());
    assert_eq!(
        first.err().or_else(|| second.err()).unwrap().code,
        ErrorCode::ReservationConflict
    );
    assert_eq!(f.count("reservations").await, 4);
    let before = f.counts().await;
    assert_eq!(
        f.app
            .execute(Uuid::new_v4(), &a, reserve(&["safe/**", "../invalid"], 600))
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument
    );
    assert_eq!(f.counts().await, before);
    f.store.integrity_check().await.unwrap();
}

#[tokio::test]
async fn reservation_scan_checks_later_pages_and_expiry_does_not_skip_changed_rows() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    f.join(&a, "alice").await;
    f.join(&b, "bob").await;
    for i in 0..205 {
        f.run(&a, reserve(&[&format!("bulk/{i:03}.rs")], 30)).await;
    }
    // UUID ordering is intentionally unrelated to acquisition order. Select an
    // actual second-page group, rather than assuming the last insertion is last.
    let groups = f.rows("reservations").await;
    let last = groups
        .iter()
        .map(|r| r.try_get::<String>("", "id").unwrap())
        .max()
        .unwrap();
    let pattern = f
        .rows("reservation_patterns")
        .await
        .into_iter()
        .find(|r| r.try_get::<String>("", "reservation_id").unwrap() == last)
        .unwrap()
        .try_get::<String>("", "pattern")
        .unwrap();
    let before = f.counts().await;
    assert_eq!(
        f.app
            .execute(Uuid::new_v4(), &b, reserve(&[&pattern], 30))
            .await
            .unwrap_err()
            .code,
        ErrorCode::ReservationConflict
    );
    assert_eq!(f.counts().await, before);
    f.app.detach(&a).await.unwrap();
    f.app.detach(&b).await.unwrap();
    f.clock.advance(31);
    let reopened = Arc::new(
        Store::open(
            f.root.path().join("agentchat.sqlite3"),
            f.store.identity().clone(),
        )
        .await
        .unwrap(),
    );
    let restarted = Application::new(reopened, f.clock.clone(), f.host.clone());
    let event_count = f.count("events").await;
    let maintenance = restarted.maintain().await.unwrap();
    assert_eq!(maintenance.expired, 205);
    assert_eq!(maintenance.renewed, 0);
    assert_eq!(f.count("events").await, event_count + 205);
    assert!(
        f.rows("reservations")
            .await
            .iter()
            .all(|r| r.try_get::<String>("", "state").unwrap() == "expired")
    );
    assert_eq!(restarted.maintain().await.unwrap().expired, 0);
    f.store.integrity_check().await.unwrap();
}

#[tokio::test]
async fn connected_renewal_disconnect_expiry_and_replacement_generations() {
    let f = Fixture::new().await;
    let a = f.context();
    f.join(&a, "alice").await;
    let acquired = f.run(&a, reserve(&["src/**"], 30)).await;
    let group = id(&acquired.result, "reservation_id");
    let generation = acquired.result["generation"].as_i64().unwrap();
    let events = f.count("events").await;
    f.clock.advance(10);
    let maintenance = f.app.maintain().await.unwrap();
    assert_eq!(maintenance.renewed, 1);
    assert_eq!(maintenance.expired, 0);
    assert!(maintenance.event_head.is_none());
    assert_eq!(f.count("events").await, events);
    assert_eq!(
        f.rows("reservations").await[0]
            .try_get::<String>("", "expires_at")
            .unwrap(),
        "2026-10-15T00:00:40.000Z"
    );
    f.clock.advance(1);
    let renewed = f
        .run(
            &a,
            Mutation::ReservationsRenew {
                reservation_id: group,
                generation,
            },
        )
        .await;
    assert_eq!(renewed.result["generation"], generation);
    assert_eq!(renewed.result["expires_at"], "2026-10-15T00:00:41.000Z");
    assert!(renewed.event_head.is_none());
    assert_eq!(f.count("events").await, events);
    f.app.detach(&a).await.unwrap();
    assert_eq!(
        f.app
            .execute(
                Uuid::new_v4(),
                &a,
                Mutation::ReservationsRenew {
                    reservation_id: group,
                    generation
                }
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::SessionRequired
    );
    f.clock.advance(31);
    let expired = f.app.maintain().await.unwrap();
    assert_eq!(expired.expired, 1);
    assert_eq!(expired.renewed, 0);
    assert!(expired.event_head.is_some());
    assert_eq!(f.app.maintain().await.unwrap().expired, 0);
    let mut resumed = a.clone();
    resumed.context_id = Uuid::new_v4();
    resumed.incarnation = Uuid::new_v4();
    f.app.attach(&resumed).await.unwrap().unwrap();
    assert_eq!(
        f.app
            .execute(
                Uuid::new_v4(),
                &resumed,
                Mutation::ReservationsRenew {
                    reservation_id: group,
                    generation
                }
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::StaleLease
    );
    let replacement = f.run(&resumed, reserve(&["src/**"], 30)).await;
    let next = id(&replacement.result, "reservation_id");
    let next_generation = replacement.result["generation"].as_i64().unwrap();
    assert_ne!(next, group);
    assert_ne!(next_generation, generation);
    assert_eq!(
        f.app
            .execute(
                Uuid::new_v4(),
                &resumed,
                Mutation::ReservationsRelease {
                    reservation_id: next,
                    generation
                }
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::StaleLease
    );
    let released = f
        .run(
            &resumed,
            Mutation::ReservationsRelease {
                reservation_id: next,
                generation: next_generation,
            },
        )
        .await;
    assert_eq!(released.result["state"], "released");
    let again = f
        .run(
            &resumed,
            Mutation::ReservationsRelease {
                reservation_id: next,
                generation: next_generation,
            },
        )
        .await;
    assert_eq!(again.result, released.result);
    assert!(again.event_head.is_none());
    assert_eq!(
        f.app
            .execute(
                Uuid::new_v4(),
                &resumed,
                Mutation::ReservationsRenew {
                    reservation_id: next,
                    generation: next_generation
                }
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::StaleLease
    );
}

#[tokio::test]
async fn explicit_leave_releases_every_owned_group_and_stops_future_targeting() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    let alice = f.join(&a, "alice").await;
    f.join(&b, "bob").await;
    f.run(&a, reserve(&["one/**"], 30)).await;
    f.run(&a, reserve(&["two/**"], 600)).await;
    f.run(&a, Mutation::Leave).await;
    assert!(
        f.rows("reservations")
            .await
            .iter()
            .all(|r| r.try_get::<String>("", "state").unwrap() == "released")
    );
    assert!(
        f.rows("deliveries")
            .await
            .iter()
            .filter(|r| r.try_get::<String>("", "recipient_id").unwrap() == alice.to_string())
            .all(|r| r.try_get::<String>("", "state").unwrap() == "failed_left")
    );
    let before = f
        .rows("deliveries")
        .await
        .into_iter()
        .filter(|r| r.try_get::<String>("", "recipient_id").unwrap() == alice.to_string())
        .count();
    let posted = f.post(&b, "@alice cannot target a left identity").await;
    assert_eq!(posted.result["warnings"][0]["code"], "recipient_left");
    assert_eq!(
        posted.result["mention_results"][0]["recipient_id"],
        alice.to_string()
    );
    assert_eq!(posted.result["mentioned_agent_ids"], serde_json::json!([]));
    assert_eq!(
        f.rows("deliveries")
            .await
            .into_iter()
            .filter(|r| r.try_get::<String>("", "recipient_id").unwrap() == alice.to_string())
            .count(),
        before
    );
    f.run(&b, reserve(&["one/future.txt", "two/future.txt"], 30))
        .await;
    f.store.integrity_check().await.unwrap();
}
