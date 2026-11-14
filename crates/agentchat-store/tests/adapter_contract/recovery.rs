//! Original results survive restart; user bookkeeping cannot release the head.
use super::*;
use agentchat_core::{application::Application, delivery::HostReceipt};
use std::sync::Arc;

async fn restart<M: Mechanism + 'static>(label: HostKind) {
    let h = configured_harness::<M>(label).await;
    let message = h.send("@receiver original before unknown result").await;
    h.send("passive successor").await;
    let original = h.app.prepare_native(&h.receiver).await.unwrap().unwrap();
    *h.adapter.outcome.lock().unwrap() = Some(Err(Error::new(
        ErrorCode::IoError,
        "result lost after possible entry",
    )));
    h.app
        .finish_native(&h.receiver, &original, h.dispatch(&original).await)
        .await
        .unwrap();
    let before = h.stream().await;
    h.app
        .execute(
            Uuid::new_v4(),
            &h.receiver,
            Mutation::Read {
                message_ids: vec![message],
            },
        )
        .await
        .unwrap();
    h.send("@receiver sender repost is independent work").await;
    assert_eq!(h.stream().await, before);

    let reopened = Arc::new(
        agentchat_store::Store::open(
            h.f.root.path().join("agentchat.sqlite3"),
            h.f.store.identity().clone(),
        )
        .await
        .unwrap(),
    );
    let app = Application::new(reopened, h.f.clock.clone(), h.adapter.clone());
    app.invalidate_connections().await.unwrap();
    let mut connection = h.receiver.clone();
    connection.context_id = Uuid::new_v4();
    connection.incarnation = Uuid::new_v4();
    h.adapter
        .bindings
        .lock()
        .unwrap()
        .insert(connection.context_id, h.identity.clone());
    app.attach(&connection).await.unwrap().unwrap();
    h.f.clock.advance(6);
    h.adapter.state.lock().unwrap().phase = Phase::ApprovalBlocked;
    if h.adapter.mechanism.capabilities().recovery == InputRecovery::OriginalReceipt {
        let poll = app.prepare_native(&connection).await.unwrap().unwrap();
        assert!(poll.awaiting_acceptance());
        assert_eq!(poll.token(), original.token());
        assert_eq!(poll.attempt(), 1);
        let waiting = h.dispatch_in(&connection, &poll).await;
        assert!(matches!(waiting, Ok(MessageProgress::AwaitingAcceptance)));
        app.finish_native(&connection, &poll, waiting)
            .await
            .unwrap();
        assert_eq!(h.stream().await.settled_seq, before.settled_seq);
        h.adapter.receipts.lock().unwrap().insert(
            original.token(),
            MessageProgress::Accepted(HostReceipt::default()),
        );
        h.f.clock.advance(2);
        let poll = app.prepare_native(&connection).await.unwrap().unwrap();
        app.finish_native(&connection, &poll, h.dispatch_in(&connection, &poll).await)
            .await
            .unwrap();
    } else {
        assert!(app.prepare_native(&connection).await.unwrap().is_none());
        assert_eq!(h.stream().await, before);
        app.finish_native(
            &h.receiver,
            &original,
            Ok(MessageProgress::Accepted(HostReceipt::default())),
        )
        .await
        .unwrap();
    }
    let accepted = h.stream().await;
    assert_eq!(accepted.settled_seq, original.seq());
    assert!(accepted.head.is_none());
    h.f.assert_read_kinds(1, 1).await;
    assert!(app.prepare_native(&connection).await.unwrap().is_none());
    h.adapter.state.lock().unwrap().phase = Phase::Idle;
    let successor = app.prepare_native(&connection).await.unwrap().unwrap();
    assert!(successor.seq() > original.seq());
    assert_eq!(
        h.adapter
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, poll, _)| !poll)
            .count(),
        1
    );
    assert!(
        !app.finish_native(
            &h.receiver,
            &original,
            Ok(MessageProgress::Accepted(HostReceipt::default()))
        )
        .await
        .unwrap()
    );
}

#[tokio::test]
async fn all_mechanisms_preserve_original_uncertainty_across_store_reopen_and_bookkeeping() {
    for label in [HostKind::Codex, HostKind::Claude] {
        restart::<Direct>(label).await;
        restart::<Journal>(label).await;
        restart::<TurnBoundJournal>(label).await;
    }
}

async fn cancelled<M: Mechanism + 'static>(label: HostKind) {
    use std::{sync::atomic::Ordering, time::Duration};
    let h = configured_harness::<M>(label).await;
    h.send("@receiver cancellation after native entry may have succeeded")
        .await;
    h.send("passive successor cannot overtake").await;
    let original = h.app.prepare_native(&h.receiver).await.unwrap().unwrap();
    let before = h.stream().await;
    h.adapter.hold_result.store(true, Ordering::SeqCst);
    assert!(
        tokio::time::timeout(Duration::from_millis(10), h.dispatch(&original))
            .await
            .is_err()
    );
    h.adapter.hold_result.store(false, Ordering::SeqCst);
    h.f.clock.advance(6);
    if h.adapter.mechanism.capabilities().recovery == InputRecovery::OriginalReceipt {
        let poll = h.app.prepare_native(&h.receiver).await.unwrap().unwrap();
        assert!(poll.awaiting_acceptance());
        assert_eq!(poll.token(), original.token());
        assert!(matches!(
            h.dispatch(&poll).await.unwrap(),
            MessageProgress::AwaitingAcceptance
        ));
    } else {
        assert!(h.app.prepare_native(&h.receiver).await.unwrap().is_none());
    }
    assert_eq!(h.stream().await, before);
    assert_eq!(
        h.adapter
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, poll, _)| !poll)
            .count(),
        1
    );
    h.f.assert_read_kinds(0, 0).await;
    assert!(
        h.app
            .health()
            .await
            .map(|value| serde_json::to_value(value).unwrap())
            .unwrap()["statistics"]
            .get("acknowledged_deliveries")
            .is_none()
    );
    let mut tx = h.f.store.begin().await.unwrap();
    let row = tx.delivery(original.delivery_id()).await.unwrap().unwrap();
    assert_eq!(row.state, DeliveryState::Processing);
    assert!(row.history.delivered_at.is_none());
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn actual_port_future_timeout_preserves_the_committed_original_for_all_mechanisms() {
    for label in [HostKind::Codex, HostKind::Claude] {
        cancelled::<Direct>(label).await;
        cancelled::<Journal>(label).await;
        cancelled::<TurnBoundJournal>(label).await;
    }
}

async fn publication<M: Mechanism + 'static>(label: HostKind) {
    let h = configured_harness::<M>(label).await;
    h.send("@receiver locally published complete body").await;
    h.send("passive successor").await;
    let first = h.app.prepare_native(&h.receiver).await.unwrap().unwrap();
    *h.adapter.outcome.lock().unwrap() = Some(Ok(MessageProgress::AwaitingAcceptance));
    h.app
        .finish_native(&h.receiver, &first, h.dispatch(&first).await)
        .await
        .unwrap();
    let waiting = h.stream().await;
    assert_eq!(waiting.head.as_ref().unwrap().token, first.token());
    assert!(h.app.prepare_native(&h.receiver).await.unwrap().is_none());
    assert!(
        h.app
            .health()
            .await
            .map(|value| serde_json::to_value(value).unwrap())
            .unwrap()["statistics"]
            .get("acknowledged_deliveries")
            .is_none()
    );
    let mut tx = h.f.store.begin().await.unwrap();
    let row = tx.delivery(first.delivery_id()).await.unwrap().unwrap();
    assert_eq!(row.state, DeliveryState::Processing);
    assert!(row.history.delivered_at.is_none());
    tx.rollback().await.unwrap();
    h.f.clock.advance(20);
    let poll = h.app.prepare_native(&h.receiver).await.unwrap().unwrap();
    assert!(poll.awaiting_acceptance());
    assert_eq!(poll.token(), first.token());
    assert_eq!(poll.attempt(), 1);
    h.adapter.receipts.lock().unwrap().insert(
        first.token(),
        MessageProgress::Accepted(HostReceipt::default()),
    );
    h.app
        .finish_native(&h.receiver, &poll, h.dispatch(&poll).await)
        .await
        .unwrap();
    assert!(h.stream().await.head.is_none());
    assert_eq!(h.stream().await.settled_seq, first.seq());
    assert_eq!(
        h.adapter
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, poll, _)| !poll)
            .count(),
        1
    );
    h.f.assert_read_kinds(1, 0).await;
}

#[tokio::test]
async fn both_journals_wait_for_native_acceptance_after_local_publication_without_resubmitting() {
    for label in [HostKind::Codex, HostKind::Claude] {
        publication::<Journal>(label).await;
        publication::<TurnBoundJournal>(label).await;
    }
}

#[tokio::test]
async fn a_head_created_during_observation_is_checked_against_the_captured_mechanism() {
    use std::{sync::atomic::Ordering, time::Duration};
    for first in [
        InputRecovery::OriginalReceipt,
        InputRecovery::RetainUncertainty,
    ] {
        for message_only in [false, true] {
            let h = configured_harness::<Journal>(HostKind::Codex).await;
            h.send("@receiver one immutable original head").await;
            h.adapter.capabilities.lock().unwrap().recovery = first;
            h.adapter.hold_verification.store(true, Ordering::SeqCst);
            let app = Application::new(h.f.store.clone(), h.f.clock.clone(), h.adapter.clone());
            let context = h.receiver.clone();
            let earlier = tokio::spawn(async move {
                if message_only {
                    app.prepare_message(&context).await.map(|_| ())
                } else {
                    app.prepare_native(&context).await.map(|_| ())
                }
            });
            tokio::time::timeout(
                Duration::from_secs(5),
                h.adapter.verification_entered.notified(),
            )
            .await
            .unwrap();
            h.adapter.capabilities.lock().unwrap().recovery = match first {
                InputRecovery::OriginalReceipt => InputRecovery::RetainUncertainty,
                InputRecovery::RetainUncertainty => InputRecovery::OriginalReceipt,
            };
            let original = h.app.prepare_native(&h.receiver).await.unwrap().unwrap();
            let before = h.stream().await;
            h.f.clock.advance(6);
            h.adapter.verification_release.notify_one();
            assert_eq!(
                earlier.await.unwrap().unwrap_err().code,
                ErrorCode::UnsupportedProtocol
            );
            assert_eq!(h.stream().await, before);
            assert_eq!(h.stream().await.head.unwrap().token, original.token());
            assert!(h.adapter.calls.lock().unwrap().is_empty());
        }
    }
}
