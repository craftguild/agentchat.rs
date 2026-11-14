//! Delivery-only probes preserve full selection's scope, decoding and bound.
mod support;

use agentchat_core::{ErrorCode, delivery::MAX_WAKE_EVENTS, ports::Storage};
use sea_orm::{
    ConnectionTrait,
    sea_query::{Alias, Expr, ExprTrait, Query},
};
use support::Fixture;
use uuid::Uuid;

#[tokio::test]
async fn pending_probe_is_exactly_scoped_and_rejects_a_twenty_first_assigned_row() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    let c = f.context();
    f.join(&a, "alice").await;
    let bob = f.native_join(&b, "bob").await;
    let cara = f.native_join(&c, "cara").await;
    for n in 0..25 {
        f.post(&a, &format!("@all bounded set {n}")).await;
    }
    let b_batch = f.app.prepare_wake(&b).await.unwrap().unwrap();
    let c_batch = f.app.prepare_wake(&c).await.unwrap().unwrap();
    let mut tx = f.store.begin().await.unwrap();
    assert!(
        tx.wake_batch_has_pending(bob, b_batch.hint.batch_id)
            .await
            .unwrap()
    );
    assert!(
        tx.wake_batch_has_pending(cara, c_batch.hint.batch_id)
            .await
            .unwrap()
    );
    assert!(
        !tx.wake_batch_has_pending(cara, b_batch.hint.batch_id)
            .await
            .unwrap()
    );
    assert!(
        !tx.wake_batch_has_pending(bob, c_batch.hint.batch_id)
            .await
            .unwrap()
    );
    assert!(
        !tx.wake_batch_has_pending(bob, Uuid::new_v4())
            .await
            .unwrap()
    );
    assert!(
        !tx.wake_batch_has_pending(Uuid::new_v4(), b_batch.hint.batch_id)
            .await
            .unwrap()
    );
    assert_eq!(
        tx.wake_items(bob, Some(b_batch.hint.batch_id))
            .await
            .unwrap()
            .len(),
        MAX_WAKE_EVENTS
    );
    let mut extra = tx.wake_items(bob, None).await.unwrap().remove(0).delivery;
    extra
        .start_processing(agentchat_core::time::Clock::now(f.clock.as_ref()), 3_600)
        .unwrap();
    extra.wake_batch_id = Some(b_batch.hint.batch_id);
    extra.attempts = 1;
    tx.save_delivery(&extra).await.unwrap();
    assert_eq!(
        tx.wake_batch_has_pending(bob, b_batch.hint.batch_id)
            .await
            .unwrap_err()
            .code,
        ErrorCode::IntegrityError
    );
    assert_eq!(
        tx.wake_items(bob, Some(b_batch.hint.batch_id))
            .await
            .unwrap_err()
            .code,
        ErrorCode::IntegrityError
    );
    tx.rollback().await.unwrap();
    f.accept_native_inputs(
        &b,
        &(b_batch
            .hint
            .events
            .iter()
            .map(|r| r.delivery_id)
            .collect::<Vec<_>>()),
    )
    .await;
    let mut tx = f.store.begin().await.unwrap();
    assert!(
        !tx.wake_batch_has_pending(bob, b_batch.hint.batch_id)
            .await
            .unwrap()
    );
    assert!(
        tx.wake_items(bob, Some(b_batch.hint.batch_id))
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(tx.wake_items(bob, None).await.unwrap().len(), 5);
    assert!(
        tx.wake_batch_has_pending(cara, c_batch.hint.batch_id)
            .await
            .unwrap()
    );
    tx.rollback().await.unwrap();
    f.store.integrity_check().await.unwrap();
}

#[tokio::test]
async fn pending_probe_filters_terminal_and_unsealed_rows_and_still_decodes_delivery_identity() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    f.join(&a, "alice").await;
    let bob = f.native_join(&b, "bob").await;
    f.post(&a, "@bob exact persisted delivery").await;
    let claim = f.app.prepare_wake(&b).await.unwrap().unwrap();
    let r = &claim.hint.events[0];
    let mut tx = f.store.begin().await.unwrap();
    let mut delivery = tx.delivery(r.delivery_id).await.unwrap().unwrap();
    tx.save_delivery(&delivery).await.unwrap();
    assert!(
        tx.wake_batch_has_pending(bob, claim.hint.batch_id)
            .await
            .unwrap()
    );
    delivery
        .depart(agentchat_core::time::Clock::now(f.clock.as_ref()))
        .unwrap();
    tx.save_delivery(&delivery).await.unwrap();
    assert!(
        !tx.wake_batch_has_pending(bob, claim.hint.batch_id)
            .await
            .unwrap()
    );
    assert!(
        tx.wake_items(bob, Some(claim.hint.batch_id))
            .await
            .unwrap()
            .is_empty()
    );
    tx.rollback().await.unwrap();

    // Deliberately construct an otherwise unreachable retained unsealed row
    // through SeaORM to test the common sealed filter, then restore it. Ordinary
    // application commit guards never permit this fixture state to be published.
    for sealed in [false, true] {
        let result = f
            .inspect
            .execute(
                &Query::update()
                    .table(Alias::new("dispatches"))
                    .value(Alias::new("sealed"), sealed)
                    .and_where(Expr::col(Alias::new("id")).eq(r.dispatch_id.to_string()))
                    .to_owned(),
            )
            .await
            .unwrap();
        assert_eq!(result.rows_affected(), 1);
        let mut tx = f.store.begin().await.unwrap();
        assert_eq!(
            tx.wake_batch_has_pending(bob, claim.hint.batch_id)
                .await
                .unwrap(),
            sealed
        );
        assert_eq!(
            !tx.wake_items(bob, Some(claim.hint.batch_id))
                .await
                .unwrap()
                .is_empty(),
            sealed
        );
        tx.rollback().await.unwrap();
    }
    // A probe must retain delivery-row validation rather than returning true
    // merely because some matching bytes exist. Restore the private fault after
    // both paths have rejected the same malformed UUID.
    let initial = f
        .rows("delivery_state_transitions")
        .await
        .into_iter()
        .find(|row| {
            row.try_get::<String>("", "delivery_id").unwrap() == r.delivery_id.to_string()
                && row.try_get::<i64>("", "ordinal").unwrap() == 1
        })
        .unwrap()
        .try_get::<String>("", "id")
        .unwrap();
    let changed = f
        .inspect
        .execute(
            &Query::update()
                .table(Alias::new("delivery_state_transitions"))
                .value(Alias::new("id"), "invalid-delivery-id")
                .and_where(Expr::col(Alias::new("id")).eq(&initial))
                .to_owned(),
        )
        .await
        .unwrap();
    assert_eq!(changed.rows_affected(), 1);
    let mut tx = f.store.begin().await.unwrap();
    assert_eq!(
        tx.wake_batch_has_pending(bob, claim.hint.batch_id)
            .await
            .unwrap_err()
            .code,
        ErrorCode::IntegrityError
    );
    assert_eq!(
        tx.wake_items(bob, Some(claim.hint.batch_id))
            .await
            .unwrap_err()
            .code,
        ErrorCode::IntegrityError
    );
    tx.rollback().await.unwrap();
    let restored = f
        .inspect
        .execute(
            &Query::update()
                .table(Alias::new("delivery_state_transitions"))
                .value(Alias::new("id"), initial.clone())
                .and_where(Expr::col(Alias::new("id")).eq("invalid-delivery-id"))
                .to_owned(),
        )
        .await
        .unwrap();
    assert_eq!(restored.rows_affected(), 1);
    assert_eq!(f.count("read_receipts").await, 0);
    f.store.integrity_check().await.unwrap();
}
