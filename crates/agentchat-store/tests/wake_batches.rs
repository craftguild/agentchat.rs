//! Real SQLite wake bookkeeping, ordering, retry fencing and control isolation.
mod support;
use agentchat_core::{
    Error, ErrorCode,
    application::{Application, Mutation},
    delivery::{BatchState, HostReceipt, MAX_WAKE_BYTES, Presentation, WakeBatch},
    model::{Context, Phase},
    ports::Storage,
    read::{PageRequest, Query},
};
use agentchat_store::Store;
use std::sync::Arc;
use support::{Fixture, id};
use uuid::Uuid;

async fn actors(f: &Fixture) -> (Context, Context, Uuid) {
    let a = f.context();
    let b = f.context();
    f.join(&a, "alice").await;
    let agent = f.native_join(&b, "bob").await;
    (a, b, agent)
}
async fn batch(f: &Fixture, id: Uuid) -> WakeBatch {
    f.store
        .begin()
        .await
        .unwrap()
        .wake_batch(id)
        .await
        .unwrap()
        .unwrap()
}
fn failed() -> agentchat_core::Result<HostReceipt> {
    Err(Error::new(
        ErrorCode::IoError,
        "Synthetic transport failure.",
    ))
}

#[tokio::test]
async fn third_accepted_submission_waits_for_ack_grace_and_associated_turn_before_stalling() {
    let f = Fixture::new().await;
    let (a, b, _) = actors(&f).await;
    f.post(&a, "@bob accepted but not acknowledged").await;
    for (attempt, delay) in [(1, 1), (2, 5)] {
        let dispatch = f.app.prepare_wake(&b).await.unwrap().unwrap();
        assert_eq!(dispatch.attempt, attempt);
        f.app.finish_wake(&b, &dispatch, failed()).await.unwrap();
        f.clock.advance(delay);
    }
    let third = f.app.prepare_wake(&b).await.unwrap().unwrap();
    f.app
        .finish_wake(
            &b,
            &third,
            Ok(HostReceipt {
                turn_id: Some("receiving-turn".into()),
            }),
        )
        .await
        .unwrap();
    f.clock.advance(4);
    assert!(f.app.prepare_wake(&b).await.unwrap().is_none());
    assert_eq!(
        batch(&f, third.hint.batch_id).await.state,
        BatchState::Inflight
    );
    f.host.turn(&b, Some("receiving-turn"));
    f.app.heartbeat(&b).await.unwrap();
    f.clock.advance(1);
    assert!(f.app.prepare_wake(&b).await.unwrap().is_none());
    assert_eq!(
        batch(&f, third.hint.batch_id).await.state,
        BatchState::Inflight
    );
    f.host.turn(&b, None);
    f.app.heartbeat(&b).await.unwrap();
    assert!(f.app.prepare_wake(&b).await.unwrap().is_none());
    let stalled = batch(&f, third.hint.batch_id).await;
    assert_eq!(stalled.state, BatchState::Stalled);
    assert_eq!(stalled.failure_code.as_deref(), Some("unconfirmed_input"));
    f.accept_native_inputs(
        &b,
        &(third
            .hint
            .events
            .iter()
            .map(|r| r.delivery_id)
            .collect::<Vec<_>>()),
    )
    .await;
    assert_eq!(
        batch(&f, third.hint.batch_id).await.state,
        BatchState::Settled
    );
    assert_eq!(f.count("read_receipts").await, 0);
}

#[tokio::test]
async fn only_mentions_wake_and_legacy_hint_acceptance_never_acknowledges_or_reads() {
    let f = Fixture::new().await;
    let (a, b, _) = actors(&f).await;
    f.post(&a, "passive context").await;
    assert!(f.app.prepare_wake(&b).await.unwrap().is_none());
    let message = f.post(&a, "@bob targeted").await;
    let before = f.counts().await;
    let dispatch = f.app.prepare_wake(&b).await.unwrap().unwrap();
    assert_eq!(dispatch.hint.events.len(), 1);
    assert_eq!(
        dispatch.hint.events[0].message_id,
        id(&message.result, "message_id")
    );
    assert_eq!(dispatch.hint.pending_count, 2);
    assert!(f.app.prepare_wake(&a).await.unwrap().is_none());
    assert!(f.app.prepare_wake(&b).await.unwrap().is_none());
    assert!(
        f.app
            .finish_wake(
                &b,
                &dispatch,
                Ok(HostReceipt {
                    turn_id: Some("native-turn".into())
                })
            )
            .await
            .unwrap()
    );
    assert!(
        !f.app
            .finish_wake(&b, &dispatch, Ok(HostReceipt::default()))
            .await
            .unwrap()
    );
    assert_eq!(
        batch(&f, dispatch.hint.batch_id).await.state,
        BatchState::Inflight
    );
    let mut after = f.counts().await;
    assert_eq!(after["wake_batches"], before["wake_batches"] + 1);
    assert_eq!(
        after["delivery_state_transitions"],
        before["delivery_state_transitions"] + 1
    );
    after.insert("wake_batches".into(), before["wake_batches"]);
    after.insert(
        "delivery_state_transitions".into(),
        before["delivery_state_transitions"],
    );
    assert_eq!(before, after);
    let inbox = f
        .query(
            Some(&b),
            Query::Inbox {
                page: PageRequest::default(),
            },
        )
        .await;
    assert_eq!(inbox["items"].as_array().unwrap().len(), 2); // Own join/membership rows are excluded.
    assert!(
        inbox["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["delivery"]["delivered_at"].is_null())
    );
    assert_eq!(f.count("read_receipts").await, 0);
}

#[tokio::test]
async fn stable_batch_retries_at_one_then_five_seconds_and_stalls_after_three_failures() {
    let f = Fixture::new().await;
    let (a, b, _) = actors(&f).await;
    f.post(&a, "@bob retry").await;
    let first = f.app.prepare_wake(&b).await.unwrap().unwrap();
    f.app.finish_wake(&b, &first, failed()).await.unwrap();
    assert!(f.app.prepare_wake(&b).await.unwrap().is_none());
    f.clock.advance(1);
    let second = f.app.prepare_wake(&b).await.unwrap().unwrap();
    assert_eq!(first.hint, second.hint);
    assert_ne!(first.token, second.token);
    assert_eq!(second.attempt, 2);
    assert!(
        !f.app
            .finish_wake(&b, &first, Ok(HostReceipt::default()))
            .await
            .unwrap()
    );
    f.app.finish_wake(&b, &second, failed()).await.unwrap();
    f.clock.advance(4);
    assert!(f.app.prepare_wake(&b).await.unwrap().is_none());
    f.clock.advance(1);
    let third = f.app.prepare_wake(&b).await.unwrap().unwrap();
    assert_eq!(third.hint, first.hint);
    assert_eq!(third.attempt, 3);
    f.app.finish_wake(&b, &third, failed()).await.unwrap();
    f.clock.advance(10);
    assert!(f.app.prepare_wake(&b).await.unwrap().is_none());
    let retained = batch(&f, third.hint.batch_id).await;
    assert_eq!(retained.state, BatchState::Stalled);
    assert_eq!(retained.failure_code.as_deref(), Some("io_error"));
    assert_eq!(f.health().await["statistics"]["stalled_batches"], 1);
    let inbox = f
        .query(
            Some(&b),
            Query::Inbox {
                page: PageRequest::default(),
            },
        )
        .await;
    assert!(
        inbox["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["batch_state"] == "stalled")
    );
    f.run(
        &b,
        Mutation::Read {
            message_ids: vec![first.hint.events[0].message_id],
        },
    )
    .await;
    assert!(f.app.prepare_wake(&b).await.unwrap().is_none());
    assert_eq!(batch(&f, third.hint.batch_id).await.attempts, 3);
}

#[tokio::test]
async fn active_associated_turn_suppresses_reinjection_and_changed_turn_uses_exact_steer_id() {
    let f = Fixture::new().await;
    let (a, b, _) = actors(&f).await;
    f.host.turn(&b, Some("turn-a"));
    f.app.heartbeat(&b).await.unwrap();
    f.post(&a, "@bob active").await;
    let first = f.app.prepare_wake(&b).await.unwrap().unwrap();
    assert_eq!(
        first.presentation,
        Presentation::Active {
            expected_turn_id: Some("turn-a".into())
        }
    );
    f.app
        .finish_wake(
            &b,
            &first,
            Ok(HostReceipt {
                turn_id: Some("turn-a".into()),
            }),
        )
        .await
        .unwrap();
    f.clock.advance(10);
    assert!(f.app.prepare_wake(&b).await.unwrap().is_none());
    f.host.turn(&b, Some("turn-b"));
    f.app.heartbeat(&b).await.unwrap();
    let next = f.app.prepare_wake(&b).await.unwrap().unwrap();
    assert_eq!(
        next.presentation,
        Presentation::Active {
            expected_turn_id: Some("turn-b".into())
        }
    );
    assert_eq!(next.hint.events, first.hint.events);
}

#[tokio::test]
async fn lost_callback_waits_for_orphan_deadline_and_unknown_active_turn_to_finish() {
    let f = Fixture::new().await;
    let (a, b, _) = actors(&f).await;
    f.post(&a, "@bob orphan").await;
    let first = f.app.prepare_wake(&b).await.unwrap().unwrap();
    f.clock.advance(4);
    assert!(f.app.prepare_wake(&b).await.unwrap().is_none());
    f.host.turn(&b, Some("possibly-accepted-turn"));
    f.app.heartbeat(&b).await.unwrap();
    f.clock.advance(1);
    assert!(f.app.prepare_wake(&b).await.unwrap().is_none());
    f.host.turn(&b, None);
    f.app.heartbeat(&b).await.unwrap();
    let next = f.app.prepare_wake(&b).await.unwrap().unwrap();
    assert_eq!(next.attempt, 2);
    assert_eq!(next.hint.events, first.hint.events);
    assert!(
        !f.app
            .finish_wake(&b, &first, Ok(HostReceipt::default()))
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn ack_releases_batch_and_new_mentions_wait_behind_its_stable_set() {
    let f = Fixture::new().await;
    let (a, b, _) = actors(&f).await;
    f.post(&a, "@bob first").await;
    f.post(&a, "@bob second").await;
    let first = f.app.prepare_wake(&b).await.unwrap().unwrap();
    f.app.finish_wake(&b, &first, failed()).await.unwrap();
    let later = f.post(&a, "@bob behind batch").await;
    f.clock.advance(1);
    let second = f.app.prepare_wake(&b).await.unwrap().unwrap();
    assert_eq!(first.hint.events, second.hint.events);
    assert_eq!(second.hint.pending_count, 3);
    f.accept_native_inputs(&b, &[first.hint.events[0].delivery_id])
        .await;
    assert_eq!(
        batch(&f, first.hint.batch_id).await.state,
        BatchState::Pending
    );
    f.accept_native_inputs(&b, &[first.hint.events[1].delivery_id])
        .await;
    assert_eq!(
        batch(&f, first.hint.batch_id).await.state,
        BatchState::Settled
    );
    assert!(
        !f.app
            .finish_wake(&b, &second, Ok(HostReceipt::default()))
            .await
            .unwrap()
    );
    let next = f.app.prepare_wake(&b).await.unwrap().unwrap();
    assert_ne!(next.hint.batch_id, first.hint.batch_id);
    assert_eq!(next.attempt, 1);
    assert_eq!(
        next.hint.events[0].message_id,
        id(&later.result, "message_id")
    );
    assert_eq!(f.count("read_receipts").await, 0);
}

#[tokio::test]
async fn verified_reconnect_requeues_same_delivery_ids_and_fences_obsolete_callbacks() {
    let f = Fixture::new().await;
    let (a, b, _) = actors(&f).await;
    f.post(&a, "@bob reconnect").await;
    let first = f.app.prepare_wake(&b).await.unwrap().unwrap();
    f.app.detach(&b).await.unwrap();
    assert!(f.app.prepare_wake(&b).await.unwrap().is_none());
    let mut fresh = b.clone();
    fresh.context_id = Uuid::new_v4();
    fresh.incarnation = Uuid::new_v4();
    f.native_reconnect(&b, &fresh);
    f.app.attach(&fresh).await.unwrap();
    assert_eq!(
        batch(&f, first.hint.batch_id).await.state,
        BatchState::Cancelled
    );
    let next = f.app.prepare_wake(&fresh).await.unwrap().unwrap();
    assert_eq!(next.hint.events, first.hint.events);
    assert_eq!(next.attempt, 1);
    assert_ne!(next.hint.batch_id, first.hint.batch_id);
    assert!(
        !f.app
            .finish_wake(&b, &first, Ok(HostReceipt::default()))
            .await
            .unwrap()
    );
    assert_eq!(
        f.app.attach(&b).await.unwrap_err().code,
        ErrorCode::IdentityConflict
    );
    assert_eq!(batch(&f, next.hint.batch_id).await.attempts, 1);
}

#[tokio::test]
async fn pause_approval_disconnect_and_stale_evidence_do_not_consume_attempts() {
    let f = Fixture::new().await;
    let (a, b, _) = actors(&f).await;
    f.post(&a, "@bob wait").await;
    for phase in [Phase::Paused, Phase::ApprovalBlocked] {
        f.host.phase(&b, phase);
        f.app.heartbeat(&b).await.unwrap();
        assert!(f.app.prepare_wake(&b).await.unwrap().is_none());
    }
    f.host.phase(&b, Phase::Idle);
    f.app.heartbeat(&b).await.unwrap();
    f.clock.advance(45);
    assert!(f.app.prepare_wake(&b).await.unwrap().is_none());
    assert_eq!(f.count("wake_batches").await, 0);
}

#[tokio::test]
async fn independent_pool_claim_race_reserves_one_attempt_only() {
    let f = Fixture::new().await;
    let (a, b, _) = actors(&f).await;
    f.post(&a, "@bob race").await;
    let store = Arc::new(
        Store::open(
            &f.root.path().join("agentchat.sqlite3"),
            f.store.identity().clone(),
        )
        .await
        .unwrap(),
    );
    let other = Application::new(store, f.clock.clone(), f.host.clone());
    let (left, right) = tokio::join!(f.app.prepare_wake(&b), other.prepare_wake(&b));
    let values = [left.unwrap(), right.unwrap()];
    assert_eq!(values.iter().filter(|r| r.is_some()).count(), 1);
    let dispatch = values.into_iter().flatten().next().unwrap();
    assert_eq!(batch(&f, dispatch.hint.batch_id).await.attempts, 1);
    assert_eq!(f.count("wake_batches").await, 1);
}

#[tokio::test]
async fn twenty_reference_hint_stays_bounded_with_worst_case_escaped_preview() {
    let f = Fixture::new().await;
    let (a, b, _) = actors(&f).await;
    let body = format!("@bob {}", "\0".repeat(65_530));
    let mut messages = Vec::new();
    for _ in 0..25 {
        messages.push(id(&f.post(&a, &body).await.result, "message_id"));
    }
    let dispatch = f.app.prepare_wake(&b).await.unwrap().unwrap();
    assert_eq!(dispatch.hint.events.len(), 20);
    assert_eq!(dispatch.hint.pending_count, 25);
    assert!(serde_json::to_vec(&dispatch.hint).unwrap().len() <= MAX_WAKE_BYTES);
    assert_eq!(
        dispatch
            .hint
            .events
            .iter()
            .map(|e| e.message_id)
            .collect::<Vec<_>>(),
        messages[..20]
    );
    f.accept_native_inputs(
        &b,
        &(dispatch
            .hint
            .events
            .iter()
            .map(|e| e.delivery_id)
            .collect::<Vec<_>>()),
    )
    .await;
    let next = f.app.prepare_wake(&b).await.unwrap().unwrap();
    assert_eq!(next.hint.events.len(), 5);
    assert_eq!(next.hint.events[0].message_id, messages[20]);
    assert_eq!(
        f.query(
            Some(&b),
            Query::Message {
                message_id: messages[0],
                receipts: PageRequest::default()
            }
        )
        .await["message"]["body"],
        body
    );
}

#[tokio::test]
async fn explicit_leave_cancels_claim_and_late_acceptance_cannot_revive_it() {
    let f = Fixture::new().await;
    let (a, b, bob) = actors(&f).await;
    f.post(&a, "@bob leaving").await;
    let dispatch = f.app.prepare_wake(&b).await.unwrap().unwrap();
    f.run(&b, Mutation::Leave).await;
    assert!(
        !f.app
            .finish_wake(&b, &dispatch, Ok(HostReceipt::default()))
            .await
            .unwrap()
    );
    assert_eq!(
        f.app.prepare_wake(&b).await.unwrap_err().code,
        ErrorCode::SessionRequired
    );
    assert_eq!(
        batch(&f, dispatch.hint.batch_id).await.state,
        BatchState::Cancelled
    );
    // A new physical managed launch receives a fresh connection identity;
    // the old launch/context is retained for stale-callback diagnosis.
    let mut next = b.clone();
    next.context_id = Uuid::new_v4();
    next.incarnation = Uuid::new_v4();
    assert_eq!(f.native_join(&next, "bob").await, bob);
    assert!(f.app.prepare_wake(&next).await.unwrap().is_none());
    assert!(
        !f.app
            .finish_wake(&b, &dispatch, Ok(HostReceipt::default()))
            .await
            .unwrap()
    );
    assert_eq!(f.count("read_receipts").await, 0);
}
