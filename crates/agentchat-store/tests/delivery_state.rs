//! Real transactions cover processing deadlines, Unknown FIFO and late settlement.
mod support;
use agentchat_core::{
    Error, ErrorCode,
    application::Application,
    delivery::{HostReceipt, MessageProgress},
    model::{Context, DeliveryState as State},
    ports::Storage,
    project::ReceiptSettingsUpdate,
    read::{MessageDetail, PageRequest, Query, QueryResult},
    time::Clock,
};
use support::{Fixture, id, native::Gate};
use uuid::Uuid;

async fn setup() -> (Fixture, std::sync::Arc<Gate>, Application, Context, Context) {
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
            auto_read: Some(true),
            delivery_confirmation_timeout_seconds: Some(10),
        },
    )
    .await
    .unwrap();
    (f, gate, app, sender, receiver)
}

async fn detail(app: &Application, message: Uuid) -> MessageDetail {
    match app
        .query(
            None,
            Query::Message {
                message_id: message,
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

#[tokio::test]
async fn pending_time_is_free_and_deadline_is_inclusive_before_atomic_successor_release() {
    let (f, _gate, app, sender, receiver) = setup().await;
    let first = id(&f.post(&sender, "first").await.result, "message_id");
    let second = id(&f.post(&sender, "second").await.result, "message_id");
    let queued = detail(&app, first).await;
    let delivery = queued.receipts.items[0].delivery.as_ref().unwrap();
    assert_eq!(delivery.state, State::Pending);
    assert_eq!(
        delivery.history.queued_at,
        queued.message.message.created_at
    );
    assert_eq!(delivery.history.deadline_at, None);
    f.clock.advance(20);
    app.maintain().await.unwrap();
    assert_eq!(
        detail(&app, first).await.receipts.items[0]
            .delivery
            .as_ref()
            .unwrap()
            .state,
        State::Pending
    );
    let dispatch = app.prepare_message(&receiver).await.unwrap().unwrap();
    let started = detail(&app, first).await;
    let history = &started.receipts.items[0].delivery.as_ref().unwrap().history;
    assert_eq!(history.processing_started_at, Some(f.clock.now()));
    assert_eq!(
        history.deadline_at,
        Some(f.clock.now().after_seconds(10).unwrap())
    );
    f.clock.advance(10);
    app.maintain().await.unwrap();
    assert_eq!(
        detail(&app, first).await.receipts.items[0]
            .delivery
            .as_ref()
            .unwrap()
            .state,
        State::Processing
    );
    app.finish_message(&receiver, &dispatch, Ok(HostReceipt::default()))
        .await
        .unwrap();
    let completed = detail(&app, first).await;
    let delivery = completed.receipts.items[0].delivery.as_ref().unwrap();
    assert_eq!(delivery.state, State::Delivered);
    assert_eq!(delivery.history.delivered_at, Some(f.clock.now()));
    assert_eq!(delivery.history.timed_out_at, None);
    let successor = app.prepare_message(&receiver).await.unwrap().unwrap();
    assert_eq!(successor.input.message_id, second);
    assert_eq!(
        detail(&app, second).await.receipts.items[0]
            .delivery
            .as_ref()
            .unwrap()
            .history
            .processing_started_at,
        Some(f.clock.now())
    );
    assert!(
        !app.finish_message(&receiver, &dispatch, Ok(HostReceipt::default()))
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn timeout_retains_original_token_without_resubmission_and_late_acceptance_releases_fifo() {
    let (f, _gate, app, sender, receiver) = setup().await;
    let first = id(
        &f.post(&sender, "unknown original").await.result,
        "message_id",
    );
    let second = id(&f.post(&sender, "successor").await.result, "message_id");
    let original = app.prepare_message(&receiver).await.unwrap().unwrap();
    app.finish_message_progress(
        &receiver,
        &original,
        Ok(MessageProgress::AwaitingAcceptance),
    )
    .await
    .unwrap();
    f.clock.advance(11);
    app.maintain().await.unwrap();
    let expired = detail(&app, first).await;
    let delivery = expired.receipts.items[0].delivery.as_ref().unwrap();
    assert_eq!(delivery.state, State::FailedTimeout);
    let timeout_at = delivery.history.timed_out_at.unwrap();
    assert_eq!(f.count("delivery_timeout_notices").await, 1);
    let poll = app.prepare_message(&receiver).await.unwrap().unwrap();
    assert!(poll.awaiting_acceptance);
    assert_eq!(poll.token, original.token);
    assert_eq!(poll.attempt, 1);
    assert_eq!(poll.input, original.input);
    f.clock.advance(1);
    app.finish_message(&receiver, &poll, Ok(HostReceipt::default()))
        .await
        .unwrap();
    let late = detail(&app, first).await;
    let delivery = late.receipts.items[0].delivery.as_ref().unwrap();
    assert_eq!(delivery.state, State::DeliveredLate);
    assert_eq!(delivery.history.timed_out_at, Some(timeout_at));
    assert_eq!(delivery.history.delivered_at, Some(f.clock.now()));
    assert_eq!(late.receipts.items[0].reads.len(), 1);
    assert_eq!(f.count("delivery_timeout_notices").await, 1);
    assert_eq!(
        app.prepare_message(&receiver)
            .await
            .unwrap()
            .unwrap()
            .input
            .message_id,
        second
    );
}

#[tokio::test]
async fn definite_original_refusal_after_expiry_settles_without_success_or_retry() {
    let (f, _gate, app, sender, receiver) = setup().await;
    let first = id(&f.post(&sender, "not accepted").await.result, "message_id");
    let second = id(&f.post(&sender, "next eligible").await.result, "message_id");
    let original = app.prepare_message(&receiver).await.unwrap().unwrap();
    f.clock.advance(11);
    app.finish_message_progress(
        &receiver,
        &original,
        Ok(MessageProgress::NotAccepted(Error::new(
            ErrorCode::RuntimeUnavailable,
            "Verified synthetic refusal",
        ))),
    )
    .await
    .unwrap();
    let d = detail(&app, first).await;
    let delivery = d.receipts.items[0].delivery.as_ref().unwrap();
    assert_eq!(delivery.state, State::FailedTimeout);
    assert_eq!(delivery.history.delivered_at, None);
    assert!(d.receipts.items[0].reads.is_empty());
    assert_eq!(
        app.prepare_message(&receiver)
            .await
            .unwrap()
            .unwrap()
            .input
            .message_id,
        second
    );
    assert!(
        !app.finish_message(&receiver, &original, Ok(HostReceipt::default()))
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn settings_edits_retries_and_reopen_preserve_first_deadline_and_frozen_read_policy() {
    let (f, _gate, app, sender, receiver) = setup().await;
    let message = id(
        &f.post(&sender, "retry original").await.result,
        "message_id",
    );
    let original = app.prepare_message(&receiver).await.unwrap().unwrap();
    let first = detail(&app, message).await.receipts.items[0]
        .delivery
        .clone()
        .unwrap();
    app.initialize_with_settings(
        None,
        ReceiptSettingsUpdate {
            auto_read: Some(false),
            delivery_confirmation_timeout_seconds: Some(1),
        },
    )
    .await
    .unwrap();
    app.finish_message_progress(
        &receiver,
        &original,
        Ok(MessageProgress::NotAccepted(Error::new(
            ErrorCode::RuntimeUnavailable,
            "Verified synthetic non-entry",
        ))),
    )
    .await
    .unwrap();
    f.clock.advance(5);
    let retry = app.prepare_message(&receiver).await.unwrap().unwrap();
    assert_eq!(retry.attempt, 2);
    assert_ne!(retry.token, original.token);
    assert_eq!(retry.input, original.input);
    let current = detail(&app, message).await.receipts.items[0]
        .delivery
        .clone()
        .unwrap();
    assert_eq!(current.history, first.history);
    let reopened = agentchat_store::Store::open(
        f.root.path().join("agentchat.sqlite3"),
        f.store.identity().clone(),
    )
    .await
    .unwrap();
    let mut tx = reopened.begin().await.unwrap();
    let retained = tx.delivery(current.delivery_id).await.unwrap().unwrap();
    assert_eq!(retained.history, first.history);
    let mut forged = retained.clone();
    forged.history.deadline_at = Some(f.clock.now().after_seconds(50).unwrap());
    assert!(tx.save_delivery(&forged).await.is_err());
    tx.rollback().await.unwrap();
    reopened.close().await.unwrap();
    app.finish_message(&receiver, &retry, Ok(HostReceipt::default()))
        .await
        .unwrap();
    assert_eq!(detail(&app, message).await.receipts.items[0].reads.len(), 1);
}

#[tokio::test]
async fn post_replay_retains_its_original_recipients_and_successful_result() {
    use agentchat_core::application::Mutation;
    let (f, _gate, app, sender, _receiver) = setup().await;
    let request = Uuid::new_v4();
    let command = Mutation::Post {
        channel: Some("dev".into()),
        body: "original receiver set".into(),
    };
    let original = f
        .app
        .execute(request, &sender, command.clone())
        .await
        .unwrap();
    assert_eq!(original.result["recipient_count"], 1);
    assert!(original.result.get("delivery_confirmation").is_none());
    let message = id(&original.result, "message_id");
    let newcomer = f.context();
    f.join(&newcomer, "later").await;
    let counts = f.counts().await;
    let replay = f.app.execute(request, &sender, command).await.unwrap();
    assert_eq!(replay.result, original.result);
    assert_eq!(f.counts().await, counts);
    let recipients = detail(&app, message).await.receipts.items;
    assert_eq!(recipients.len(), 1);
    assert_eq!(
        recipients[0].delivery.as_ref().unwrap().state,
        State::Pending
    );
}

#[tokio::test]
async fn recipient_failure_rolls_back_staged_message_event_rows_and_successful_request() {
    use agentchat_core::{
        model::{Delivery, DeliveryHistory, DispatchKind, DispatchPayload, EventKind, Message},
        ports::IdempotencyRecord,
    };
    let (f, _gate, _app, sender, _receiver) = setup().await;
    // Cross a bounded insertion chunk so a later foreign-key failure cannot
    // publish the earlier provisional recipient rows or successful request.
    for n in 0..16 {
        f.join(&f.context(), &format!("recipient{n}")).await;
    }
    let before = f.counts().await;
    let request = Uuid::new_v4();
    let now = f.clock.now();
    let mut tx = f.store.begin().await.unwrap();
    let author = tx
        .conversation(sender.host, &sender.host_session_id)
        .await
        .unwrap()
        .unwrap()
        .id;
    let channel = tx.channel("dev").await.unwrap().unwrap().id;
    let header = tx
        .allocate_dispatch(DispatchKind::Message, Some(channel), now)
        .await
        .unwrap();
    let message_id = header.id;
    let mut event = tx
        .allocate_event(EventKind::MessageCreated, now)
        .await
        .unwrap();
    let payload = DispatchPayload::Message(Message {
        id: message_id,
        channel_id: channel,
        author_id: author,
        root_message_id: None,
        body: "atomic staged body".into(),
        created_at: now,
        creation_seq: header.creation_seq,
    });
    tx.save_idempotency(&IdempotencyRecord {
        request_id: request,
        actor_key: "staged-test-actor".into(),
        operation: "post".into(),
        argument_hash: "staged-test-hash".into(),
        result: serde_json::json!({"message_id":message_id}),
        committed_at: now,
    })
    .await
    .unwrap();
    event.actor_id = Some(author);
    event.channel_id = Some(channel);
    event.message_id = Some(message_id);
    let mut recipients = tx
        .joined_agents()
        .await
        .unwrap()
        .into_iter()
        .filter(|actor| actor.id != author)
        .take(16)
        .map(|actor| actor.id)
        .collect::<Vec<_>>();
    assert_eq!(recipients.len(), 16);
    recipients.push(Uuid::new_v4());
    let deliveries = recipients
        .into_iter()
        .map(|recipient_id| Delivery {
            id: Uuid::new_v4(),
            dispatch_id: header.id,
            recipient_id,
            state: State::Pending,
            wake_eligible: false,
            wake_batch_id: None,
            attempts: 0,
            history: DeliveryHistory::queued(now),
        })
        .collect::<Vec<_>>();
    assert_eq!(event.message_id, Some(message_id));
    assert!(
        tx.publish_dispatch(&header, &payload, &deliveries)
            .await
            .is_err()
    );
    assert!(tx.commit().await.is_err());
    assert_eq!(f.counts().await, before);
    let mut tx = f.store.begin().await.unwrap();
    assert!(tx.idempotency(request).await.unwrap().is_none());
    assert!(tx.message(message_id).await.unwrap().is_none());
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn body_only_selection_cannot_overtake_an_earlier_typed_notice() {
    use agentchat_core::{application::Mutation, delivery::NativeDispatch};
    let (f, _gate, app, sender, receiver) = setup().await;
    f.run(
        &sender,
        Mutation::Reserve {
            patterns: vec!["src/fifo/**".into()],
            ttl_seconds: None,
            reason: None,
        },
    )
    .await;
    let message = id(
        &f.post(&sender, "body after typed notice").await.result,
        "message_id",
    );
    assert!(app.prepare_message(&receiver).await.unwrap().is_none());
    assert_eq!(
        detail(&app, message).await.receipts.items[0]
            .delivery
            .as_ref()
            .unwrap()
            .state,
        State::Pending
    );
    let first = app.prepare_native(&receiver).await.unwrap().unwrap();
    assert!(matches!(first, NativeDispatch::Notice(_)));
    app.finish_native(
        &receiver,
        &first,
        Ok(MessageProgress::Accepted(HostReceipt::default())),
    )
    .await
    .unwrap();
    let body = app.prepare_message(&receiver).await.unwrap().unwrap();
    assert_eq!(body.input.message_id, message);
    assert!(body.input.seq > first.seq());
    assert!(
        detail(&app, message).await.receipts.items[0]
            .reads
            .is_empty()
    );
}

#[tokio::test]
async fn terminal_departure_history_rejects_later_claim_facts_without_changing_the_stored_row() {
    use agentchat_core::application::Mutation;
    for processing in [false, true] {
        let (f, _gate, app, sender, receiver) = setup().await;
        let message = id(
            &f.post(&sender, "terminal history boundary").await.result,
            "message_id",
        );
        let delivery_id = detail(&app, message).await.receipts.items[0]
            .delivery
            .as_ref()
            .unwrap()
            .delivery_id;
        if processing {
            let claim = app.prepare_message(&receiver).await.unwrap().unwrap();
            assert_eq!(claim.input.delivery_id, delivery_id);
        }
        f.clock.advance(2);
        app.execute(Uuid::new_v4(), &receiver, Mutation::Leave)
            .await
            .unwrap();
        let mut tx = f.store.begin().await.unwrap();
        let original = tx.delivery(delivery_id).await.unwrap().unwrap();
        assert_eq!(original.state, State::FailedLeft);
        assert_eq!(original.history.left_at, Some(f.clock.now()));
        assert_eq!(original.history.processing_started_at.is_some(), processing);
        assert_eq!(original.history.deadline_at.is_some(), processing);
        assert!(original.history.delivered_at.is_none());
        assert!(original.history.timed_out_at.is_none());
        // Saving the same terminal row remains a legitimate idempotent update.
        tx.save_delivery(&original).await.unwrap();
        tx.commit().await.unwrap();
        let mut forged = original.clone();
        forged.history.processing_started_at =
            Some(original.history.queued_at.after_seconds(1).unwrap());
        forged.history.deadline_at = Some(original.history.queued_at.after_seconds(11).unwrap());
        forged.history.validate(State::FailedLeft).unwrap();
        let mut tx = f.store.begin().await.unwrap();
        assert_eq!(
            tx.save_delivery(&forged).await.unwrap_err().code,
            ErrorCode::IntegrityError
        );
        assert_eq!(tx.delivery(delivery_id).await.unwrap().unwrap(), original);
        // Commit after the rejected save so a rollback cannot mask an update.
        tx.commit().await.unwrap();
        let reopened = agentchat_store::Store::open(
            f.root.path().join("agentchat.sqlite3"),
            f.store.identity().clone(),
        )
        .await
        .unwrap();
        let mut tx = reopened.begin().await.unwrap();
        assert_eq!(tx.delivery(delivery_id).await.unwrap().unwrap(), original);
        tx.rollback().await.unwrap();
        reopened.integrity_check().await.unwrap();
        reopened.close().await.unwrap();
    }
}
