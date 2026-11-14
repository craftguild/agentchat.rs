//! Claude original-result uncertainty survives crash/reconnect; old formats refuse.
use super::*;
use agentchat_core::model::HostKind;

async fn setup_claude() -> (
    Fixture,
    Arc<Gate>,
    Application,
    Context,
    Context,
    LaunchIdentity,
) {
    let (f, gate, app, sender, _, _) = setup().await;
    let mut context = f.context();
    context.host = HostKind::Claude;
    let identity = gate.bind(&context);
    app.start_launch(&context, Some("claude")).await.unwrap();
    (f, gate, app, sender, context, identity)
}
fn unknown() -> Result<MessageProgress> {
    Err(Error::new(
        ErrorCode::IoError,
        "Synthetic native result lost after possible storage.",
    ))
}
#[tokio::test]
async fn first_claude_claim_is_fenced_before_io_and_only_original_receipt_survives_restart() {
    let (f, gate, app, sender, context, identity) = setup_claude().await;
    f.post(&sender, "@claude original before runtime loss")
        .await;
    f.post(&sender, "passive successor").await;
    let original = app.prepare_message(&context).await.unwrap().unwrap();
    assert!(
        !original.awaiting_acceptance,
        "only the original caller may publish"
    );
    let claimed = stream(&f, &identity).await;
    let head = claimed.head.as_ref().unwrap();
    assert!(head.awaiting_acceptance && !head.submission_pending);
    let backup = f.root.path().join("unknown.sqlite3");
    f.store.backup(&backup).await.unwrap();
    let copy = agentchat_store::Store::open(&backup, f.store.identity().clone())
        .await
        .unwrap();
    assert_eq!(
        copy.snapshot()
            .await
            .unwrap()
            .message_stream(identity.generation)
            .await
            .unwrap()
            .unwrap(),
        claimed
    );
    let reopened = Arc::new(
        agentchat_store::Store::open(
            f.root.path().join("agentchat.sqlite3"),
            f.store.identity().clone(),
        )
        .await
        .unwrap(),
    );
    let app = Application::new(reopened, f.clock.clone(), gate.clone());
    app.invalidate_connections().await.unwrap();
    let mut next = context.clone();
    next.context_id = Uuid::new_v4();
    next.incarnation = Uuid::new_v4();
    gate.reconnect(&next, &identity);
    app.attach(&next).await.unwrap().unwrap();
    f.clock.advance(6);
    for _ in 0..3 {
        let poll = app.prepare_message(&next).await.unwrap().unwrap();
        assert!(poll.awaiting_acceptance);
        assert_eq!(poll.token, original.token);
        assert_eq!(poll.input, original.input);
        assert_eq!(poll.attempt, 1);
        assert!(
            app.finish_message_progress(&next, &poll, unknown())
                .await
                .unwrap()
        );
        let state = stream(&f, &identity).await;
        assert_eq!(state.settled_seq, claimed.settled_seq);
        assert!(state.head.as_ref().unwrap().awaiting_acceptance);
        assert!(state.head.as_ref().unwrap().failure_code.is_none());
        assert_eq!(state.head.as_ref().unwrap().token, original.token);
        f.clock.advance(6);
    }
    assert_eq!(f.count("read_receipts").await, 0);
    accept(&app, &context, &original).await;
    let next = app.prepare_message(&next).await.unwrap().unwrap();
    assert_eq!(next.input.body, "passive successor");
    assert_eq!(next.attempt, 1);
    assert!(
        app.health()
            .await
            .map(|value| serde_json::to_value(value).unwrap())
            .unwrap()["statistics"]
            .get("acknowledged_deliveries")
            .is_none()
    );
    let mut tx = f.store.begin().await.unwrap();
    let row = tx
        .delivery(original.input.delivery_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.state, DeliveryState::Delivered);
    assert!(row.history.delivered_at.is_some());
    tx.rollback().await.unwrap();
    f.store.integrity_check().await.unwrap();
}
#[tokio::test]
async fn exact_claude_refusal_after_reconnect_grants_only_a_same_body_retry() {
    let (f, gate, app, sender, context, identity) = setup_claude().await;
    f.post(&sender, "@claude definitely not entered").await;
    f.post(&sender, "successor cannot overtake").await;
    let first = app.prepare_message(&context).await.unwrap().unwrap();
    let before = stream(&f, &identity).await;
    let mut next = context.clone();
    next.context_id = Uuid::new_v4();
    next.incarnation = Uuid::new_v4();
    gate.reconnect(&next, &identity);
    app.invalidate_connections().await.unwrap();
    app.attach(&next).await.unwrap().unwrap();
    assert!(
        app.finish_message_progress(&context, &first, failed())
            .await
            .unwrap()
    );
    let refused = stream(&f, &identity).await;
    assert_eq!(refused.settled_seq, before.settled_seq);
    assert!(!refused.head.as_ref().unwrap().awaiting_acceptance);
    f.clock.advance(6);
    let retry = app.prepare_message(&next).await.unwrap().unwrap();
    assert!(!retry.awaiting_acceptance);
    assert_eq!(retry.input, first.input);
    assert_eq!(retry.attempt, 2);
    assert_ne!(retry.token, first.token);
}
#[tokio::test]
async fn released_v7_namespace_is_refused_before_reclassifying_original_results() {
    old_namespace_is_refused(7).await;
}

#[tokio::test]
async fn claude_typed_notice_original_claim_remains_read_only_after_lost_completion() {
    use agentchat_core::delivery::NativeDispatch;
    let (f, _gate, app, sender, context, identity) = setup_claude().await;
    f.run(
        &sender,
        Mutation::Reserve {
            patterns: vec!["src/claude/**".into()],
            ttl_seconds: None,
            reason: None,
        },
    )
    .await;
    let original = app.prepare_native(&context).await.unwrap().unwrap();
    assert!(matches!(original, NativeDispatch::Notice(_)));
    assert!(!original.awaiting_acceptance());
    let claimed = stream(&f, &identity).await;
    assert!(claimed.head.as_ref().unwrap().awaiting_acceptance);
    f.post(&sender, "@claude later message cannot overtake notice")
        .await;
    f.clock.advance(6);
    let poll = app.prepare_native(&context).await.unwrap().unwrap();
    assert!(poll.awaiting_acceptance());
    assert_eq!(poll.token(), original.token());
    assert_eq!(poll.input(), original.input());
    assert!(app.finish_native(&context, &poll, unknown()).await.unwrap());
    assert_eq!(
        stream(&f, &identity).await.head.as_ref().unwrap().token,
        original.token()
    );
    assert!(
        app.finish_native(
            &context,
            &original,
            Ok(MessageProgress::Accepted(HostReceipt::default()))
        )
        .await
        .unwrap()
    );
    let next = app.prepare_native(&context).await.unwrap().unwrap();
    assert!(matches!(next, NativeDispatch::Message(_)));
    assert_ne!(next.token(), original.token());
}
