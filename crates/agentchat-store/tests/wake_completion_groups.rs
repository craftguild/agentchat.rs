//! Exact native callback fencing under bounded shared FULL durability.
mod support;

use agentchat_core::{
    Error, ErrorCode, Result,
    application::Mutation,
    delivery::{BatchState, HostReceipt, MAX_WAKE_COMPLETION_GROUP, WakeCompletion, WakeDispatch},
    model::Context,
    ports::Storage,
};
use std::time::Duration;
use support::Fixture;
use uuid::Uuid;

fn completion(context: &Context, dispatch: &WakeDispatch, turn: &str) -> WakeCompletion {
    WakeCompletion {
        context: context.clone(),
        dispatch: dispatch.clone(),
        outcome: Ok(HostReceipt {
            turn_id: Some(turn.into()),
        }),
    }
}

#[tokio::test]
async fn an_obsolete_incarnation_and_a_left_recipient_cannot_revive_with_a_valid_neighbor() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    let c = f.context();
    let d = f.context();
    f.join(&a, "alice").await;
    let bob = f.native_join(&b, "bob").await;
    f.native_join(&c, "cara").await;
    f.native_join(&d, "dana").await;
    f.post(&a, "@bob @cara @dana original snapshot").await;
    let old_b = f.app.prepare_wake(&b).await.unwrap().unwrap();
    let old_c = f.app.prepare_wake(&c).await.unwrap().unwrap();
    let valid_d = f.app.prepare_wake(&d).await.unwrap().unwrap();
    let mut fresh = b.clone();
    fresh.incarnation = Uuid::new_v4();
    fresh.context_id = Uuid::new_v4();
    f.native_reconnect(&b, &fresh);
    assert_eq!(f.app.attach(&fresh).await.unwrap().unwrap().id, bob);
    let current = f.app.prepare_wake(&fresh).await.unwrap().unwrap();
    assert_eq!(current.hint.events, old_b.hint.events);
    assert_ne!(current.hint.batch_id, old_b.hint.batch_id);
    f.run(&c, Mutation::Leave).await;
    assert_eq!(
        f.app
            .finish_wakes(vec![
                completion(&b, &old_b, "obsolete"),
                completion(&c, &old_c, "left"),
                completion(&d, &valid_d, "current"),
            ])
            .await
            .unwrap(),
        vec![Ok(false), Ok(false), Ok(true)]
    );
    let mut tx = f.store.begin().await.unwrap();
    for dispatch in [&old_b, &old_c] {
        let batch = tx
            .wake_batch(dispatch.hint.batch_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(batch.state, BatchState::Cancelled);
        assert_eq!(batch.turn_ref, None);
    }
    let batch = tx.wake_batch(current.hint.batch_id).await.unwrap().unwrap();
    assert_eq!(batch.state, BatchState::Pending);
    assert_eq!(batch.dispatch_ref, Some(current.token));
    assert_eq!(batch.attempts, 1);
    assert_eq!(batch.turn_ref, None);
    tx.rollback().await.unwrap();
    assert_eq!(f.count("read_receipts").await, 0);
    f.store.integrity_check().await.unwrap();
}

#[tokio::test]
async fn maximum_group_keeps_independent_outcomes_and_never_acknowledges_or_reads() {
    let f = Fixture::new().await;
    let sender = f.context();
    f.join(&sender, "sender").await;
    let mut contexts = Vec::new();
    for n in 0..MAX_WAKE_COMPLETION_GROUP {
        let context = f.context();
        f.join(&context, &format!("target{n}")).await;
        contexts.push(context);
    }
    f.post(&sender, "@all bounded callbacks").await;
    let mut completions = Vec::new();
    for (n, context) in contexts.iter().enumerate() {
        let dispatch = f.app.prepare_wake(context).await.unwrap().unwrap();
        let mut finished = completion(context, &dispatch, &format!("turn{n}"));
        if n % 2 == 1 {
            finished.outcome = Err(Error::new(
                ErrorCode::IoError,
                "Synthetic native rejection.",
            ));
        }
        completions.push(finished);
    }
    let before = f.counts().await;
    let results = f.app.finish_wakes(completions.clone()).await.unwrap();
    assert_eq!(results, vec![Ok(true); MAX_WAKE_COMPLETION_GROUP]);
    assert_eq!(f.counts().await, before);
    let mut tx = f.store.begin().await.unwrap();
    for (n, c) in completions.iter().enumerate() {
        let batch = tx
            .wake_batch(c.dispatch.hint.batch_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(batch.state, BatchState::Inflight);
        assert_eq!(batch.attempts, 1);
        assert_eq!(batch.dispatch_ref, Some(c.dispatch.token));
        assert_eq!(
            batch.next_eligible_at.unwrap().to_string(),
            "2026-10-15T00:00:01.000Z"
        );
        assert_eq!(
            batch.failure_code.as_deref(),
            (n % 2 == 1).then_some("io_error")
        );
        assert_eq!(batch.turn_ref, (n % 2 == 0).then(|| format!("turn{n}")));
        for reference in &c.dispatch.hint.events {
            assert!(
                tx.delivery(reference.delivery_id)
                    .await
                    .unwrap()
                    .unwrap()
                    .history
                    .delivered_at
                    .is_none()
            );
        }
    }
    tx.rollback().await.unwrap();
    assert_eq!(
        f.app.finish_wakes(completions.clone()).await.unwrap(),
        vec![Ok(false); MAX_WAKE_COMPLETION_GROUP]
    );
    assert_eq!(f.count("read_receipts").await, 0);
    // An obsolete group cannot mutate the next reserved attempt's token/budget.
    f.clock.advance(1);
    let mut current = Vec::new();
    for context in &contexts {
        current.push(f.app.prepare_wake(context).await.unwrap().unwrap());
    }
    assert_eq!(
        f.app.finish_wakes(completions).await.unwrap(),
        vec![Ok(false); MAX_WAKE_COMPLETION_GROUP]
    );
    let mut tx = f.store.begin().await.unwrap();
    for dispatch in current {
        let batch = tx
            .wake_batch(dispatch.hint.batch_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(batch.state, BatchState::Pending);
        assert_eq!(batch.attempts, 2);
        assert_eq!(batch.dispatch_ref, Some(dispatch.token));
    }
    tx.rollback().await.unwrap();
    f.store.integrity_check().await.unwrap();
}

#[tokio::test]
async fn invalid_contexts_and_repeated_callbacks_do_not_suppress_independent_valid_outcomes() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    let c = f.context();
    f.join(&a, "alice").await;
    f.native_join(&b, "bob").await;
    f.native_join(&c, "cara").await;
    f.post(&a, "@bob @cara exact recipients").await;
    let b = completion(
        &b,
        &f.app.prepare_wake(&b).await.unwrap().unwrap(),
        "turn-b",
    );
    let c = completion(
        &c,
        &f.app.prepare_wake(&c).await.unwrap().unwrap(),
        "turn-c",
    );
    let mut foreign = c.clone();
    foreign.context.project_id = Uuid::new_v4();
    let mut missing = c.clone();
    missing.context.host_session_id.clear();
    let results = f
        .app
        .finish_wakes(vec![
            b.clone(),
            foreign.clone(),
            b.clone(),
            missing.clone(),
            c.clone(),
        ])
        .await
        .unwrap();
    assert_eq!(results[0], Ok(true));
    assert_eq!(
        results[1].as_ref().unwrap_err().code,
        ErrorCode::ProjectBindingMismatch
    );
    assert_eq!(results[2], Ok(false));
    assert_eq!(
        results[3].as_ref().unwrap_err().code,
        ErrorCode::SessionRequired
    );
    assert_eq!(results[4], Ok(true));
    // Bounds and all-invalid descriptors must return without reserving a writer.
    let held = f.store.begin().await.unwrap();
    let result = tokio::time::timeout(Duration::from_millis(250), async {
        assert_eq!(
            f.app.finish_wakes(vec![]).await.unwrap_err().code,
            ErrorCode::InvalidArgument
        );
        assert_eq!(
            f.app
                .finish_wakes(vec![b; MAX_WAKE_COMPLETION_GROUP + 1])
                .await
                .unwrap_err()
                .code,
            ErrorCode::InvalidArgument
        );
        let bad = f.app.finish_wakes(vec![foreign, missing]).await.unwrap();
        assert!(bad.iter().all(Result::is_err));
    })
    .await;
    held.rollback().await.unwrap();
    result.unwrap();
    f.store.integrity_check().await.unwrap();
}

#[tokio::test]
async fn malformed_host_receipt_is_local_and_stale_receipts_keep_the_original_noop_semantics() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    let c = f.context();
    f.join(&a, "alice").await;
    f.native_join(&b, "bob").await;
    f.native_join(&c, "cara").await;
    f.post(&a, "@bob @cara preserve claims").await;
    let b = completion(
        &b,
        &f.app.prepare_wake(&b).await.unwrap().unwrap(),
        "turn-b",
    );
    let c = completion(
        &c,
        &f.app.prepare_wake(&c).await.unwrap().unwrap(),
        "turn-c",
    );
    let mut bad = c.clone();
    bad.outcome = Ok(HostReceipt {
        turn_id: Some(String::new()),
    });
    assert_eq!(
        f.app
            .finish_wake(&c.context, &c.dispatch, bad.outcome.clone())
            .await
            .unwrap_err()
            .code,
        ErrorCode::IntegrityError
    );
    let results = f
        .app
        .finish_wakes(vec![b.clone(), bad.clone()])
        .await
        .unwrap();
    assert_eq!(results[0], Ok(true));
    assert_eq!(
        results[1].as_ref().unwrap_err().code,
        ErrorCode::IntegrityError
    );
    let mut tx = f.store.begin().await.unwrap();
    for (index, finished) in [&b, &c].into_iter().enumerate() {
        let batch = tx
            .wake_batch(finished.dispatch.hint.batch_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            batch.state,
            if index == 0 {
                BatchState::Inflight
            } else {
                BatchState::Pending
            }
        );
        assert_eq!(
            batch.turn_ref,
            if index == 0 {
                Some("turn-b".into())
            } else {
                None
            }
        );
        assert_eq!(
            batch.next_eligible_at.unwrap().to_string(),
            if index == 0 {
                "2026-10-15T00:00:01.000Z"
            } else {
                "2026-10-15T00:00:05.000Z"
            }
        );
    }
    tx.rollback().await.unwrap();
    assert_eq!(
        f.app.finish_wakes(vec![b, c.clone()]).await.unwrap(),
        vec![Ok(false), Ok(true)]
    );
    assert!(
        !f.app
            .finish_wake(&c.context, &c.dispatch, bad.outcome)
            .await
            .unwrap()
    );
    f.store.integrity_check().await.unwrap();
}
