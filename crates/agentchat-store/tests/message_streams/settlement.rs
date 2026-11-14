//! Complete native acceptance settles FIFO independently of explicit reads.
use super::*;
use agentchat_core::delivery::NativeDispatch;

async fn delivery(f: &Fixture, id: Uuid) -> agentchat_core::model::Delivery {
    let mut tx = f.store.begin().await.unwrap();
    let row = tx.delivery(id).await.unwrap().unwrap();
    tx.rollback().await.unwrap();
    row
}
async fn session(f: &Fixture, context: &Context) -> agentchat_core::model::Session {
    let mut snapshot = f.store.snapshot().await.unwrap();
    let actor = snapshot
        .conversation(context.host, &context.host_session_id)
        .await
        .unwrap()
        .unwrap();
    snapshot.session(actor.id).await.unwrap().unwrap()
}

#[tokio::test]
async fn addressed_and_passive_acceptance_settles_without_read_or_broadcast() {
    for body in ["@receiver complete addressed body", "complete passive body"] {
        let (f, _gate, app, sender, context, identity) = setup().await;
        f.post(&sender, body).await;
        let original = app.prepare_message(&context).await.unwrap().unwrap();
        let before_events = f.count("events").await;
        let before_session = session(&f, &context).await;
        let before = delivery(&f, original.input.delivery_id).await;
        assert_eq!(before.state, DeliveryState::Processing);
        assert!(before.history.delivered_at.is_none());
        f.clock.advance(1);
        accept(&app, &context, &original).await;
        let accepted = delivery(&f, original.input.delivery_id).await;
        assert_eq!(accepted.state, DeliveryState::Delivered);
        assert!(accepted.history.delivered_at.is_some());
        assert_eq!(stream(&f, &identity).await.settled_seq, original.input.seq);
        assert_eq!(f.count("read_receipts").await, 0);
        assert_eq!(f.count("events").await, before_events);
        assert_eq!(session(&f, &context).await, before_session);

        f.clock.advance(1);
        assert_eq!(delivery(&f, original.input.delivery_id).await, accepted);
        assert_eq!(session(&f, &context).await, before_session);
        assert!(
            !app.finish_message(&context, &original, Ok(HostReceipt::default()))
                .await
                .unwrap()
        );
        assert_eq!(f.count("events").await, before_events);
    }
}

#[tokio::test]
async fn explicit_read_preserves_its_first_time_without_completing_the_native_head() {
    let (f, _gate, app, sender, context, identity) = setup().await;
    f.post(&sender, "@receiver pending original").await;
    let original = app.prepare_message(&context).await.unwrap().unwrap();
    let before = stream(&f, &identity).await;
    f.clock.advance(1);
    command(
        &app,
        &context,
        Mutation::Read {
            message_ids: vec![original.input.message_id],
        },
    )
    .await;
    let read_time = f.rows("read_receipts").await[0]
        .try_get::<String>("", "first_read_at")
        .unwrap();
    assert_eq!(stream(&f, &identity).await, before);
    let pending = delivery(&f, original.input.delivery_id).await;
    assert_eq!(pending.state, DeliveryState::Processing);
    assert!(pending.history.delivered_at.is_none());
    f.clock.advance(1);
    accept(&app, &context, &original).await;
    let completed = delivery(&f, original.input.delivery_id).await;
    assert_eq!(completed.state, DeliveryState::Delivered);
    assert_eq!(
        completed.history.processing_started_at,
        pending.history.processing_started_at
    );
    assert_eq!(completed.history.deadline_at, pending.history.deadline_at);
    assert_eq!(stream(&f, &identity).await.settled_seq, original.input.seq);
    assert_eq!(
        f.rows("read_receipts").await[0]
            .try_get::<String>("", "first_read_at")
            .unwrap(),
        read_time
    );
    assert_eq!(f.count("read_receipts").await, 1);
}

#[tokio::test]
async fn publication_refusal_and_unknown_result_cannot_complete_or_advance() {
    for outcome in [
        Ok(MessageProgress::AwaitingAcceptance),
        failed(),
        Err(Error::new(
            ErrorCode::RuntimeUnavailable,
            "Native result unknown.",
        )),
    ] {
        let (f, _gate, app, sender, context, identity) = setup().await;
        f.post(&sender, "@receiver receipt required").await;
        let original = app.prepare_message(&context).await.unwrap().unwrap();
        let before = stream(&f, &identity).await;
        assert!(
            app.finish_message_progress(&context, &original, outcome)
                .await
                .unwrap()
        );
        let current = stream(&f, &identity).await;
        assert_eq!(current.settled_seq, before.settled_seq);
        assert_eq!(current.head.unwrap().token, original.token);
        let row = delivery(&f, original.input.delivery_id).await;
        assert_eq!(row.state, DeliveryState::Processing);
        assert!(row.history.delivered_at.is_none());
        assert_eq!(f.count("read_receipts").await, 0);
    }
}

#[tokio::test]
async fn typed_notice_acceptance_uses_the_same_settlement_commit() {
    let (f, _gate, app, sender, context, identity) = setup().await;
    f.run(
        &sender,
        Mutation::Reserve {
            patterns: vec!["src/native-settlement/**".into()],
            ttl_seconds: None,
            reason: None,
        },
    )
    .await;
    let original = app.prepare_native(&context).await.unwrap().unwrap();
    assert!(matches!(original, NativeDispatch::Notice(_)));
    let events = f.count("events").await;
    assert!(
        app.finish_native(
            &context,
            &original,
            Ok(MessageProgress::Accepted(HostReceipt::default()))
        )
        .await
        .unwrap()
    );
    let row = delivery(&f, original.delivery_id()).await;
    assert_eq!(row.state, DeliveryState::Delivered);
    assert!(row.history.delivered_at.is_some());
    assert_eq!(stream(&f, &identity).await.settled_seq, original.seq());
    assert_eq!(f.count("events").await, events);
    assert_eq!(f.count("read_receipts").await, 0);
}
