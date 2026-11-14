//! Native refusal, controls, loss and absent mechanisms use common policy.
use super::*;
use agentchat_core::model::LaunchOwnerState;

async fn refusal<M: Mechanism + 'static>(label: HostKind) {
    let h = Harness::<M>::new(label).await;
    h.send("@receiver explicit refusal stays at the FIFO head")
        .await;
    h.send("later passive body").await;
    let mut original = None;
    for attempt in 1..=3 {
        let dispatch = h.app.prepare_native(&h.receiver).await.unwrap().unwrap();
        assert_eq!(dispatch.attempt(), attempt);
        if let Some(first) = &original {
            let first: &agentchat_core::delivery::NativeDispatch = first;
            assert_eq!(dispatch.delivery_id(), first.delivery_id());
            assert_ne!(dispatch.token(), first.token());
            assert!(
                !h.app
                    .finish_native(
                        &h.receiver,
                        first,
                        Ok(MessageProgress::Accepted(Default::default()))
                    )
                    .await
                    .unwrap()
            );
        } else {
            original = Some(dispatch.clone());
        }
        *h.adapter.outcome.lock().unwrap() = Some(Ok(MessageProgress::NotAccepted(Error::new(
            ErrorCode::Busy,
            "explicit native refusal",
        ))));
        h.app
            .finish_native(&h.receiver, &dispatch, h.dispatch(&dispatch).await)
            .await
            .unwrap();
        assert!(h.app.prepare_native(&h.receiver).await.unwrap().is_none());
        h.f.clock.advance(6);
    }
    assert!(h.app.prepare_native(&h.receiver).await.unwrap().is_none());
    assert_eq!(h.stream().await.head.unwrap().attempts, 3);
    assert_eq!(h.adapter.calls.lock().unwrap().len(), 3);
    assert_eq!(h.f.count("read_receipts").await, 0);
}

#[tokio::test]
async fn every_adapter_uses_one_bounded_refusal_budget_and_stale_token_fence() {
    for label in [HostKind::Codex, HostKind::Claude] {
        refusal::<Direct>(label).await;
        refusal::<Journal>(label).await;
        refusal::<TurnBoundJournal>(label).await;
    }
}

async fn loss<M: Mechanism + 'static>(label: HostKind, root: bool) {
    let h = Harness::<M>::new(label).await;
    h.send("@receiver control-fenced body").await;
    let before = h.stream().await;
    for phase in [Phase::Paused, Phase::ApprovalBlocked, Phase::Disconnected] {
        h.adapter.state.lock().unwrap().phase = phase;
        assert!(h.app.prepare_native(&h.receiver).await.unwrap().is_none());
        assert_eq!(h.stream().await, before);
    }
    h.adapter.state.lock().unwrap().phase = Phase::Idle;
    *h.adapter.owner.lock().unwrap() = LaunchOwnerState::Unknown;
    assert!(h.app.prepare_native(&h.receiver).await.unwrap().is_none());
    h.app.reconcile_launch(h.identity.generation).await.unwrap();
    assert_eq!(h.stream().await, before);
    *h.adapter.owner.lock().unwrap() = LaunchOwnerState::Current;
    let original = h.app.prepare_native(&h.receiver).await.unwrap().unwrap();
    let pending = h.stream().await;
    *h.adapter.owner.lock().unwrap() = LaunchOwnerState::Unknown;
    assert!(
        !h.app
            .finish_native(
                &h.receiver,
                &original,
                Ok(MessageProgress::Accepted(Default::default()))
            )
            .await
            .unwrap()
    );
    assert_eq!(h.stream().await, pending);
    if root {
        *h.adapter.owner.lock().unwrap() = LaunchOwnerState::Current;
        *h.adapter.root_changed.lock().unwrap() = true;
    } else {
        *h.adapter.owner.lock().unwrap() = LaunchOwnerState::Gone;
    }
    h.app.reconcile_launch(h.identity.generation).await.unwrap();
    let retired = h.stream().await;
    assert!(retired.cancelled);
    assert_eq!(retired.settled_seq, before.settled_seq);
    assert_eq!(retired.head.unwrap().token, original.token());
    assert!(
        !h.app
            .finish_native(
                &h.receiver,
                &original,
                Ok(MessageProgress::Accepted(Default::default()))
            )
            .await
            .unwrap()
    );
    assert_eq!(h.f.count("dispatch_messages").await, 1);
    assert_eq!(h.f.count("read_receipts").await, 0);
    h.send("inactive interval is never collected").await;
    *h.adapter.owner.lock().unwrap() = LaunchOwnerState::Current;
    *h.adapter.root_changed.lock().unwrap() = false;
    let mut fresh = h.receiver.clone();
    fresh.context_id = Uuid::new_v4();
    fresh.incarnation = Uuid::new_v4();
    h.adapter.bind(&fresh);
    h.app.start_launch(&fresh, Some("receiver")).await.unwrap();
    assert!(h.app.prepare_native(&fresh).await.unwrap().is_none());
}

#[tokio::test]
async fn every_adapter_suspends_on_unknown_and_retires_only_on_positive_owner_or_root_loss() {
    for label in [HostKind::Codex, HostKind::Claude] {
        for root in [false, true] {
            loss::<Direct>(label, root).await;
            loss::<Journal>(label, root).await;
            loss::<TurnBoundJournal>(label, root).await;
        }
    }
}

#[tokio::test]
async fn missing_passive_mechanism_cannot_skip_to_a_later_addressed_body() {
    let h = Harness::<TurnBoundJournal>::new(HostKind::Claude).await;
    h.send("passive head requires its own mechanism").await;
    h.send("@receiver addressed successor").await;
    let before = h.stream().await;
    h.adapter.capabilities.lock().unwrap().passive = false;
    assert_eq!(
        h.app.prepare_native(&h.receiver).await.unwrap_err().code,
        ErrorCode::UnsupportedProtocol
    );
    assert_eq!(h.stream().await, before);
    assert!(h.adapter.calls.lock().unwrap().is_empty());
    h.adapter.capabilities.lock().unwrap().passive = true;
    let head = h.app.prepare_native(&h.receiver).await.unwrap().unwrap();
    assert!(!head.input().addressed());
}

#[tokio::test]
async fn publication_or_direct_store_edit_cannot_change_original_recovery_meaning() {
    let h = Harness::<Direct>::new(HostKind::Claude).await;
    h.send("@receiver original direct-result attempt").await;
    let dispatch = h.app.prepare_native(&h.receiver).await.unwrap().unwrap();
    let before = h.stream().await;
    assert_eq!(
        h.app
            .finish_native(
                &h.receiver,
                &dispatch,
                Ok(MessageProgress::AwaitingAcceptance)
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::UnsupportedProtocol
    );
    assert_eq!(h.stream().await, before);
    let mut changed = before.clone();
    let head = changed.head.as_mut().unwrap();
    head.submission_pending = false;
    head.awaiting_acceptance = true;
    let mut writer = h.f.store.begin().await.unwrap();
    assert_eq!(
        writer.save_message_stream(&changed).await.unwrap_err().code,
        ErrorCode::IntegrityError
    );
    writer.rollback().await.unwrap();
    assert_eq!(h.stream().await, before);
}
