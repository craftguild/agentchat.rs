//! Read-only deferral pruning preserves reconciliation and ordinary validation.
mod support;
use agentchat_core::{
    ErrorCode,
    delivery::{BatchState, HostReceipt, WakeBatch, WakeDispatch},
    model::Context,
    ports::Storage,
    time::Clock,
};
use sea_orm::{
    ConnectionTrait,
    sea_query::{Alias, Expr, ExprTrait, Query},
};
use std::time::Duration;
use support::Fixture;
use uuid::Uuid;

async fn fixture(count: usize) -> (Fixture, Context, Uuid, WakeDispatch) {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    f.join(&a, "alice").await;
    let bob = f.native_join(&b, "bob").await;
    for i in 0..count {
        f.post(&a, &format!("@bob retained target {i}")).await;
    }
    let claim = f.app.prepare_wake(&b).await.unwrap().unwrap();
    (f, b, bob, claim)
}
async fn batch(f: &Fixture, id: Uuid) -> WakeBatch {
    let mut tx = f.store.begin().await.unwrap();
    let b = tx.wake_batch(id).await.unwrap().unwrap();
    tx.rollback().await.unwrap();
    b
}
async fn save(f: &Fixture, b: &WakeBatch) {
    let mut tx = f.store.begin().await.unwrap();
    tx.save_wake_batch(b).await.unwrap();
    tx.commit().await.unwrap();
}
async fn selected(f: &Fixture, context: &Context) -> bool {
    f.app
        .wake_candidates(std::slice::from_ref(context))
        .await
        .unwrap()
        .contains(&context.context_id)
}
async fn text_value(f: &Fixture, table: &str, column: &str, key: &str, old: &str, value: &str) {
    assert_eq!(
        f.inspect
            .execute(
                &Query::update()
                    .table(Alias::new(table))
                    .value(Alias::new(column), value)
                    .and_where(Expr::col(Alias::new(key)).eq(old))
                    .to_owned()
            )
            .await
            .unwrap()
            .rows_affected(),
        1
    );
}

#[tokio::test]
async fn orphan_and_completion_deadlines_prune_only_the_exact_incarnation_until_due() {
    let (f, b, _, claim) = fixture(1).await;
    let before = batch(&f, claim.hint.batch_id).await;
    let counts = f.counts().await;
    assert!(!selected(&f, &b).await);
    let mut other = b.clone();
    other.context_id = Uuid::new_v4();
    other.incarnation = Uuid::new_v4();
    assert!(selected(&f, &other).await);
    assert!(!matches!(f.app.prepare_wake(&other).await, Ok(Some(_))));
    assert_eq!(batch(&f, before.id).await, before);
    assert_eq!(f.counts().await, counts);
    f.clock.advance(5);
    assert!(selected(&f, &b).await);
    f.app
        .finish_wake(&b, &claim, Ok(HostReceipt::default()))
        .await
        .unwrap();
    let accepted = batch(&f, before.id).await;
    assert_eq!(accepted.state, BatchState::Inflight);
    assert!(!selected(&f, &b).await);
    assert_eq!(batch(&f, before.id).await, accepted);
    f.clock.advance(1);
    assert!(selected(&f, &b).await);
    assert_eq!(f.app.prepare_wake(&b).await.unwrap().unwrap().attempt, 2);
    f.store.integrity_check().await.unwrap();
}

#[tokio::test]
async fn stalled_sets_are_pruned_until_complete_native_acceptance_settles_them() {
    let (f, b, _, claim) = fixture(2).await;
    let mut stalled = batch(&f, claim.hint.batch_id).await;
    stalled.state = BatchState::Stalled;
    stalled.attempts = 3;
    stalled.next_eligible_at = None;
    save(&f, &stalled).await;
    let ids = claim
        .hint
        .events
        .iter()
        .map(|r| r.delivery_id)
        .collect::<Vec<_>>();
    assert!(!selected(&f, &b).await);
    assert_eq!(batch(&f, stalled.id).await, stalled);
    f.accept_native_inputs(&b, &ids[..1]).await;
    assert!(!selected(&f, &b).await);
    assert_eq!(batch(&f, stalled.id).await, stalled);
    f.accept_native_inputs(&b, &ids[1..]).await;
    assert_eq!(batch(&f, stalled.id).await.state, BatchState::Settled);
    assert!(!selected(&f, &b).await);
    assert!(f.app.prepare_wake(&b).await.unwrap().is_none());
    assert_eq!(f.count("read_receipts").await, 0);
}

#[tokio::test]
async fn due_exhausted_attempts_still_reach_the_writer_to_materialize_stall() {
    let (f, b, _, claim) = fixture(1).await;
    let mut due = batch(&f, claim.hint.batch_id).await;
    due.attempts = 3;
    due.next_eligible_at = Some(f.clock.now());
    save(&f, &due).await;
    assert!(selected(&f, &b).await);
    assert_eq!(batch(&f, due.id).await, due);
    assert!(f.app.prepare_wake(&b).await.unwrap().is_none());
    let after = batch(&f, due.id).await;
    assert_eq!(after.state, BatchState::Stalled);
    assert_eq!(after.failure_code.as_deref(), Some("unconfirmed_input"));
    assert!(!selected(&f, &b).await);
}

#[tokio::test]
async fn malformed_deferred_batch_and_delivery_rows_fail_both_hint_and_preparation() {
    let (f, b, _, claim) = fixture(1).await;
    let initial = batch(&f, claim.hint.batch_id).await;
    let id = initial.id.to_string();
    text_value(
        &f,
        "wake_batches",
        "next_eligible_at",
        "id",
        &id,
        "invalid-time",
    )
    .await;
    assert_eq!(
        f.app
            .wake_candidates(std::slice::from_ref(&b))
            .await
            .unwrap_err()
            .code,
        ErrorCode::IntegrityError
    );
    assert_eq!(
        f.app.prepare_wake(&b).await.unwrap_err().code,
        ErrorCode::IntegrityError
    );
    text_value(
        &f,
        "wake_batches",
        "next_eligible_at",
        "id",
        &id,
        &initial.next_eligible_at.unwrap().to_string(),
    )
    .await;
    let delivery = claim.hint.events[0].delivery_id.to_string();
    let history_id = f
        .rows("delivery_state_transitions")
        .await
        .into_iter()
        .find(|r| {
            r.try_get::<String>("", "delivery_id").unwrap() == delivery
                && r.try_get::<i64>("", "ordinal").unwrap() == 1
        })
        .unwrap()
        .try_get::<String>("", "id")
        .unwrap();
    text_value(
        &f,
        "delivery_state_transitions",
        "id",
        "id",
        &history_id,
        "invalid-delivery",
    )
    .await;
    assert_eq!(
        f.app
            .wake_candidates(std::slice::from_ref(&b))
            .await
            .unwrap_err()
            .code,
        ErrorCode::IntegrityError
    );
    assert_eq!(
        f.app.prepare_wake(&b).await.unwrap_err().code,
        ErrorCode::IntegrityError
    );
    text_value(
        &f,
        "delivery_state_transitions",
        "id",
        "id",
        "invalid-delivery",
        &history_id,
    )
    .await;
    assert!(!selected(&f, &b).await);
    assert_eq!(batch(&f, initial.id).await, initial);
    f.store.integrity_check().await.unwrap();
}

#[tokio::test]
async fn a_twenty_first_assigned_row_is_rejected_even_while_retry_is_deferred() {
    let (f, b, bob, claim) = fixture(21).await;
    let mut tx = f.store.begin().await.unwrap();
    let mut extra = tx.wake_items(bob, None).await.unwrap().remove(0).delivery;
    let original = extra.clone();
    extra.wake_batch_id = Some(claim.hint.batch_id);
    tx.save_delivery(&extra).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        f.app
            .wake_candidates(std::slice::from_ref(&b))
            .await
            .unwrap_err()
            .code,
        ErrorCode::IntegrityError
    );
    assert_eq!(
        f.app.prepare_wake(&b).await.unwrap_err().code,
        ErrorCode::IntegrityError
    );
    let mut tx = f.store.begin().await.unwrap();
    tx.save_delivery(&original).await.unwrap();
    tx.commit().await.unwrap();
    assert!(!selected(&f, &b).await);
    f.clock.advance(5);
    assert!(selected(&f, &b).await);
    f.store.integrity_check().await.unwrap();
}

#[tokio::test]
async fn sealed_and_terminal_filters_match_preparation_without_early_event_hydration() {
    let (f, b, _, claim) = fixture(1).await;
    let event = claim.hint.events[0].dispatch_id.to_string();
    for sealed in [false, true] {
        f.inspect
            .execute(
                &Query::update()
                    .table(Alias::new("dispatches"))
                    .value(Alias::new("sealed"), sealed)
                    .and_where(Expr::col(Alias::new("id")).eq(&event))
                    .to_owned(),
            )
            .await
            .unwrap();
        assert_eq!(
            selected(&f, &b).await,
            !sealed,
            "empty assigned sets must remain candidates"
        );
    }
    let mut tx = f.store.begin().await.unwrap();
    let mut delivery = tx
        .delivery(claim.hint.events[0].delivery_id)
        .await
        .unwrap()
        .unwrap();
    let original = delivery.clone();
    delivery.depart(f.clock.now()).unwrap();
    tx.save_delivery(&delivery).await.unwrap();
    tx.commit().await.unwrap();
    assert!(selected(&f, &b).await);
    let mut tx = f.store.begin().await.unwrap();
    assert!(tx.save_delivery(&original).await.is_err());
    tx.rollback().await.unwrap();
    assert!(f.app.prepare_wake(&b).await.unwrap().is_none());
    assert!(!selected(&f, &b).await);
    f.store.integrity_check().await.unwrap();
}

#[tokio::test]
async fn committed_departure_is_seen_fresh_while_uncommitted_or_rolled_back_facts_stay_invisible() {
    let (f, b, _, claim) = fixture(1).await;
    let ids = claim
        .hint
        .events
        .iter()
        .map(|r| r.delivery_id)
        .collect::<Vec<_>>();
    for commit in [false, true] {
        let mut tx = f.store.begin().await.unwrap();
        let mut delivery = tx.delivery(ids[0]).await.unwrap().unwrap();
        delivery.depart(f.clock.now()).unwrap();
        tx.save_delivery(&delivery).await.unwrap();
        assert!(
            !tokio::time::timeout(Duration::from_secs(1), selected(&f, &b))
                .await
                .unwrap()
        );
        if commit {
            tx.commit().await.unwrap();
        } else {
            tx.rollback().await.unwrap();
        }
        assert_eq!(selected(&f, &b).await, commit);
    }
    assert!(f.app.prepare_wake(&b).await.unwrap().is_none());
    assert!(!selected(&f, &b).await);
}
