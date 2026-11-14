//! Real Store frozen read policy, independent read kinds and ACK-free settlement.
mod support;
use agentchat_core::{
    application::{Application, Mutation},
    delivery::{HostReceipt, MessageProgress},
    model::{Context, DeliveryState, ReadKind},
    ports::Storage,
    project::ReceiptSettingsUpdate,
    read::{MessageDetail, PageRequest, Query, QueryResult},
    time::Clock,
};
use support::{Fixture, id, native::Gate};
use uuid::Uuid;

async fn detail(app: &Application, message_id: Uuid) -> MessageDetail {
    match app
        .query(
            None,
            Query::Message {
                message_id,
                receipts: PageRequest::default(),
            },
        )
        .await
        .unwrap()
    {
        QueryResult::Message(detail) => detail,
        _ => panic!("message result"),
    }
}
async fn setup(read: bool) -> (Fixture, std::sync::Arc<Gate>, Application, Context, Context) {
    let f = Fixture::new().await;
    let sender = f.context();
    f.join(&sender, "sender").await;
    let receiver = f.context();
    let gate = Gate::new(&f);
    gate.bind(&receiver);
    let app = gate.app(&f);
    app.start_launch(&receiver, Some("receiver")).await.unwrap();
    app.initialize_with_settings(
        None,
        ReceiptSettingsUpdate {
            auto_read: Some(read),
            delivery_confirmation_timeout_seconds: Some(10),
        },
    )
    .await
    .unwrap();
    (f, gate, app, sender, receiver)
}

#[tokio::test]
async fn both_read_policies_freeze_at_claim_and_acceptance_releases_before_explicit_read() {
    for read in [false, true] {
        let (f, _gate, app, sender, receiver) = setup(read).await;
        let first = id(
            &f.post(&sender, "@receiver complete first body")
                .await
                .result,
            "message_id",
        );
        let second = id(
            &f.post(&sender, "passive successor").await.result,
            "message_id",
        );
        let dispatch = app.prepare_message(&receiver).await.unwrap().unwrap();
        assert_eq!(dispatch.input.message_id, first);
        assert_eq!(dispatch.input.auto_read, read);
        let original = detail(&app, first).await.receipts.items[0]
            .delivery
            .clone()
            .unwrap();
        // Configuration cannot reinterpret original input bytes or first deadline.
        app.initialize_with_settings(
            None,
            ReceiptSettingsUpdate {
                auto_read: Some(!read),
                delivery_confirmation_timeout_seconds: Some(100),
            },
        )
        .await
        .unwrap();
        let events = f.count("events").await;
        app.finish_message_progress(
            &receiver,
            &dispatch,
            Ok(MessageProgress::AwaitingAcceptance),
        )
        .await
        .unwrap();
        f.clock.advance(1);
        let poll = app.prepare_message(&receiver).await.unwrap().unwrap();
        assert_eq!(poll.input, dispatch.input);
        assert_eq!(poll.token, dispatch.token);
        assert_eq!(poll.attempt, 1);
        assert!(
            app.finish_message(&receiver, &poll, Ok(HostReceipt::default()))
                .await
                .unwrap()
        );
        assert_eq!(f.count("events").await, events);
        let completed = detail(&app, first).await;
        let row = &completed.receipts.items[0];
        let delivery = row.delivery.as_ref().unwrap();
        assert_eq!(delivery.state, DeliveryState::Delivered);
        assert_eq!(delivery.history.delivered_at, Some(f.clock.now()));
        assert_eq!(
            delivery.history.processing_started_at,
            original.history.processing_started_at
        );
        assert_eq!(delivery.history.deadline_at, original.history.deadline_at);
        assert_eq!(row.reads.len(), usize::from(read));
        if read {
            assert_eq!(row.reads[0].kind, ReadKind::Automatic);
        }
        let completed_history = delivery.history.clone();
        // This succeeds even with automatic reads disabled and before explicit Read.
        let next = app.prepare_message(&receiver).await.unwrap().unwrap();
        assert_eq!(next.input.message_id, second);
        assert_eq!(next.input.auto_read, !read);
        assert!(
            !app.finish_message(&receiver, &poll, Ok(HostReceipt::default()))
                .await
                .unwrap()
        );
        f.clock.advance(1);
        let request = Uuid::new_v4();
        let mutation = Mutation::Read {
            message_ids: vec![first],
        };
        let explicit = app
            .execute(request, &receiver, mutation.clone())
            .await
            .unwrap();
        assert_eq!(explicit.result["changed"], serde_json::json!([first]));
        assert_eq!(
            app.execute(request, &receiver, mutation.clone())
                .await
                .unwrap()
                .result,
            explicit.result
        );
        let first_read = detail(&app, first).await;
        let first_read = first_read.receipts.items[0].reads.last().unwrap().at;
        f.clock.advance(1);
        let repeated = app
            .execute(Uuid::new_v4(), &receiver, mutation)
            .await
            .unwrap();
        assert_eq!(repeated.result["already_read"], serde_json::json!([first]));
        let after = detail(&app, first).await;
        let row = &after.receipts.items[0];
        assert_eq!(row.reads.len(), if read { 2 } else { 1 });
        assert_eq!(row.reads.last().unwrap().kind, ReadKind::Explicit);
        assert_eq!(row.reads.last().unwrap().at, first_read);
        assert_eq!(row.delivery.as_ref().unwrap().history, completed_history);
        assert_eq!(f.count("events").await, events);
        // Reading the predecessor cannot settle the still-unaccepted successor.
        let binding = app
            .native_binding(&receiver)
            .await
            .unwrap()
            .native_delivery
            .unwrap();
        assert_eq!(binding.head.unwrap().delivery_id, next.input.delivery_id);
        f.store.integrity_check().await.unwrap();
    }
}

#[tokio::test]
async fn explicit_read_does_not_settle_unknown_and_late_reconnect_preserves_both_first_kinds() {
    let (f, gate, app, sender, receiver) = setup(true).await;
    let message = id(&f.post(&sender, "whole body").await.result, "message_id");
    let successor = id(&f.post(&sender, "successor").await.result, "message_id");
    let first = app.prepare_message(&receiver).await.unwrap().unwrap();
    app.execute(
        Uuid::new_v4(),
        &receiver,
        Mutation::Read {
            message_ids: vec![message],
        },
    )
    .await
    .unwrap();
    let before = detail(&app, message).await;
    let row = &before.receipts.items[0];
    assert_eq!(
        row.delivery.as_ref().unwrap().state,
        DeliveryState::Processing
    );
    assert!(
        row.delivery
            .as_ref()
            .unwrap()
            .history
            .delivered_at
            .is_none()
    );
    assert_eq!(row.reads[0].kind, ReadKind::Explicit);
    let explicit_at = row.reads[0].at;
    assert!(app.prepare_message(&receiver).await.unwrap().is_none());
    app.finish_message_progress(&receiver, &first, Ok(MessageProgress::AwaitingAcceptance))
        .await
        .unwrap();
    let identity = f
        .store
        .snapshot()
        .await
        .unwrap()
        .latest_launch(row.recipient.actor_id)
        .await
        .unwrap()
        .unwrap()
        .identity;
    let mut fresh = receiver.clone();
    fresh.context_id = Uuid::new_v4();
    fresh.incarnation = Uuid::new_v4();
    gate.reconnect(&fresh, &identity);
    app.attach(&fresh).await.unwrap();
    f.clock.advance(11);
    app.maintain().await.unwrap();
    let expired = detail(&app, message).await;
    let timeout_at = expired.receipts.items[0]
        .delivery
        .as_ref()
        .unwrap()
        .history
        .timed_out_at;
    assert_eq!(
        expired.receipts.items[0].delivery.as_ref().unwrap().state,
        DeliveryState::FailedTimeout
    );
    let poll = app.prepare_message(&fresh).await.unwrap().unwrap();
    assert_eq!(poll.token, first.token);
    assert_eq!(poll.attempt, 1);
    assert_eq!(poll.input, first.input);
    assert!(poll.awaiting_acceptance);
    f.clock.advance(1);
    app.finish_message(&fresh, &poll, Ok(HostReceipt::default()))
        .await
        .unwrap();
    let after = detail(&app, message).await;
    let row = &after.receipts.items[0];
    let delivery = row.delivery.as_ref().unwrap();
    assert_eq!(delivery.state, DeliveryState::DeliveredLate);
    assert_eq!(delivery.history.timed_out_at, timeout_at);
    assert_eq!(delivery.history.delivered_at, Some(f.clock.now()));
    assert_eq!(
        row.reads.iter().map(|r| r.kind).collect::<Vec<_>>(),
        vec![ReadKind::Automatic, ReadKind::Explicit]
    );
    assert_eq!(row.reads[0].at, f.clock.now());
    assert_eq!(row.reads[1].at, explicit_at);
    assert_eq!(
        app.prepare_message(&fresh)
            .await
            .unwrap()
            .unwrap()
            .input
            .message_id,
        successor
    );
    assert!(
        !app.finish_message(&fresh, &poll, Ok(HostReceipt::default()))
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn typed_notice_acceptance_settles_but_never_marks_its_referenced_message_read() {
    let (f, _gate, app, sender, receiver) = setup(true).await;
    let message = id(&f.post(&sender, "waiting body").await.result, "message_id");
    let body = app.prepare_native(&receiver).await.unwrap().unwrap();
    app.finish_native(
        &receiver,
        &body,
        Ok(MessageProgress::Accepted(HostReceipt::default())),
    )
    .await
    .unwrap();
    f.join(&f.context(), "newcomer").await;
    let notice = app.prepare_native(&receiver).await.unwrap().unwrap();
    assert!(matches!(
        notice,
        agentchat_core::delivery::NativeDispatch::Notice(_)
    ));
    let reads = f.count("read_receipts").await;
    let events = f.count("events").await;
    app.finish_native(
        &receiver,
        &notice,
        Ok(MessageProgress::Accepted(HostReceipt::default())),
    )
    .await
    .unwrap();
    assert_eq!(f.count("read_receipts").await, reads);
    assert_eq!(f.count("events").await, events);
    assert_eq!(detail(&app, message).await.receipts.items[0].reads.len(), 1);
    assert!(
        !app.finish_native(
            &receiver,
            &notice,
            Ok(MessageProgress::Accepted(HostReceipt::default()))
        )
        .await
        .unwrap()
    );
    // One Join can publish multiple typed notices; each requires its own acceptance.
    while let Some(next) = app.prepare_native(&receiver).await.unwrap() {
        assert!(matches!(
            next,
            agentchat_core::delivery::NativeDispatch::Notice(_)
        ));
        app.finish_native(
            &receiver,
            &next,
            Ok(MessageProgress::Accepted(HostReceipt::default())),
        )
        .await
        .unwrap();
    }
    assert_eq!(f.count("read_receipts").await, reads);
}
