//! T11/T12 budgets fail closed without recording a lease or successful retry key.

mod support;

use std::sync::Arc;

use agentchat_core::{
    ErrorCode,
    application::{Application, Mutation, MutationRequest},
};
use agentchat_store::Store;
use support::Fixture;
use uuid::Uuid;

fn reserve(patterns: Vec<String>) -> Mutation {
    Mutation::Reserve {
        patterns,
        ttl_seconds: Some(30),
        reason: None,
    }
}

#[tokio::test]
async fn pattern_count_budget_has_a_distinct_error_and_preserves_atomicity() {
    let f = Fixture::new().await;
    let owner = f.context();
    f.join(&owner, "alice").await;
    let before = f.counts().await;
    assert_eq!(
        f.app
            .execute(Uuid::new_v4(), &owner, reserve(vec![]))
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument
    );
    assert_eq!(f.counts().await, before);
    let request = Uuid::new_v4();
    let patterns: Vec<_> = (0..33).map(|i| format!("future/{i}.rs")).collect();
    let error = f
        .app
        .execute(request, &owner, reserve(patterns.clone()))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::PatternTooComplex);
    assert_eq!(f.counts().await, before);

    // An error owns neither any partial group nor a successful semantic key.
    // Retrying this UUID with the valid boundary must still execute the operation.
    let valid = patterns[..32].to_vec();
    let acquired = f
        .app
        .execute(request, &owner, reserve(valid.clone()))
        .await
        .unwrap();
    assert_eq!(acquired.result["patterns"], serde_json::json!(valid));
    assert_eq!(f.count("reservations").await, 1);
    assert_eq!(f.count("reservation_patterns").await, 32);
    let after = f.counts().await;
    let replay = f
        .app
        .execute(request, &owner, reserve(patterns[..32].to_vec()))
        .await
        .unwrap();
    assert_eq!(replay.result, acquired.result);
    assert_eq!(f.counts().await, after);
    f.store.integrity_check().await.unwrap();
}

#[tokio::test]
async fn compilation_budgets_roll_back_the_entire_request_and_allow_a_valid_retry() {
    let f = Fixture::new().await;
    let owner = f.context();
    f.join(&owner, "alice").await;
    f.run(&owner, reserve(vec!["existing/**".into()])).await;
    f.clock.advance(31);
    let before = f.counts().await;
    for complex in [
        "{a,b}".repeat(11),
        "🦀".repeat(257),
        format!("*a{}", "[ab]".repeat(20)),
    ] {
        let request = Uuid::new_v4();
        let error = f
            .app
            .execute(
                request,
                &owner,
                reserve(vec!["unclaimed/**".into(), complex]),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::PatternTooComplex);
        assert_eq!(f.counts().await, before);
        // Expiry and a refreshed session were staged before compilation. The
        // failure must restore those existing values as well as avoid new rows.
        assert_eq!(
            f.rows("reservations").await[0]
                .try_get::<String>("", "state")
                .unwrap(),
            "active"
        );
        assert_eq!(
            f.rows("sessions").await[0]
                .try_get::<String>("", "last_heartbeat")
                .unwrap(),
            "2026-10-15T00:00:00.000Z"
        );
    }
    let acquired = f.run(&owner, reserve(vec!["unclaimed/**".into()])).await;
    assert_eq!(
        acquired.result["patterns"],
        serde_json::json!(["unclaimed/**"])
    );
    assert_eq!(f.count("reservations").await, 2);
    assert_eq!(
        f.rows("reservations")
            .await
            .into_iter()
            .filter(|row| row.try_get::<String>("", "state").unwrap() == "expired")
            .count(),
        1
    );
    f.store.integrity_check().await.unwrap();
}

#[tokio::test]
async fn exhausting_the_default_intersection_budget_never_grants_a_partial_group() {
    let f = Fixture::new().await;
    let alice = f.context();
    let bob = f.context();
    f.join(&alice, "alice").await;
    f.join(&bob, "bob").await;
    f.run(&alice, reserve(vec![format!("*a{}", "[abc]".repeat(11))]))
        .await;
    let before = f.counts().await;
    let request = Uuid::new_v4();
    let error = f
        .app
        .execute(
            request,
            &bob,
            reserve(vec!["fresh/**".into(), format!("*b{}", "[abc]".repeat(11))]),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::PatternTooComplex);
    assert_eq!(f.counts().await, before);
    let valid = f
        .app
        .execute(request, &bob, reserve(vec!["fresh/**".into()]))
        .await
        .unwrap();
    assert_eq!(valid.result["patterns"], serde_json::json!(["fresh/**"]));
    assert_eq!(f.count("reservations").await, 2);
    f.store.integrity_check().await.unwrap();
}

#[tokio::test]
async fn a_failed_compilation_savepoint_cannot_publish_or_undo_neighboring_leases() {
    let f = Fixture::new().await;
    let alice = f.context();
    let bob = f.context();
    f.join(&alice, "alice").await;
    f.join(&bob, "bob").await;
    let before_events = f.count("events").await;
    let before_keys = f.count("idempotency").await;
    let failed = Uuid::new_v4();
    let results = f
        .app
        .execute_group(vec![
            MutationRequest {
                request_id: Uuid::new_v4(),
                context: alice,
                command: reserve(vec!["before/**".into()]),
            },
            MutationRequest {
                request_id: failed,
                context: bob.clone(),
                command: reserve(vec!["unclaimed/**".into(), "{a,b}".repeat(11)]),
            },
            MutationRequest {
                request_id: Uuid::new_v4(),
                context: bob.clone(),
                command: reserve(vec!["after/**".into()]),
            },
        ])
        .await
        .unwrap();
    assert_eq!(results.len(), 3);
    assert!(results[0].is_ok() && results[2].is_ok());
    assert_eq!(
        results[1].as_ref().unwrap_err().code,
        ErrorCode::PatternTooComplex
    );
    assert_eq!(f.count("reservations").await, 2);
    assert_eq!(f.count("reservation_patterns").await, 2);
    assert_eq!(f.count("events").await, before_events + 2);
    assert_eq!(f.count("idempotency").await, before_keys + 2);
    let result = f
        .app
        .execute(failed, &bob, reserve(vec!["unclaimed/**".into()]))
        .await
        .unwrap();
    assert_eq!(
        result.result["patterns"],
        serde_json::json!(["unclaimed/**"])
    );
    assert_eq!(f.count("reservations").await, 3);
    f.store.integrity_check().await.unwrap();
}

#[tokio::test]
async fn independent_writers_serialize_future_unicode_conflicts_and_allow_disjoint_groups() {
    let f = Fixture::new().await;
    let alice = f.context();
    let bob = f.context();
    f.join(&alice, "alice").await;
    f.join(&bob, "bob").await;
    let other = Application::new(
        Arc::new(
            Store::open(
                f.root.path().join("agentchat.sqlite3"),
                f.store.identity().clone(),
            )
            .await
            .unwrap(),
        ),
        f.clock.clone(),
        f.host.clone(),
    );
    let future = "future/🦀/?.rs";
    assert!(!f.root.path().join(future).exists());
    let (a, b) = tokio::join!(
        f.app.execute(
            Uuid::new_v4(),
            &alice,
            reserve(vec![r"future/{é,🦀}/\?.rs".into()])
        ),
        other.execute(
            Uuid::new_v4(),
            &bob,
            reserve(vec!["free-before/**".into(), r"future/🦀/\?.rs".into()])
        ),
    );
    assert!(a.is_ok() ^ b.is_ok());
    let bob_won = b.is_ok();
    let error = a.err().or_else(|| b.err()).unwrap();
    assert_eq!(error.code, ErrorCode::ReservationConflict);
    assert_eq!(error.details["conflicts"][0]["witness_path"], future);
    assert_eq!(f.count("reservations").await, 1);
    assert_eq!(
        f.count("reservation_patterns").await,
        if bob_won { 2 } else { 1 }
    );
    assert!(!f.root.path().join(future).exists());
    let (a, b) = tokio::join!(
        f.app
            .execute(Uuid::new_v4(), &alice, reserve(vec!["separate/é".into()])),
        other.execute(Uuid::new_v4(), &bob, reserve(vec!["separate/🦀".into()])),
    );
    assert!(a.is_ok() && b.is_ok());
    assert_eq!(f.count("reservations").await, 3);
    f.store.integrity_check().await.unwrap();
}
