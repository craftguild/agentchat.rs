//! Original uncertainty remains fenced across expiry, reads and fresh-format refusal.
use super::*;
use agentchat_core::delivery::NativeDispatch;
use serde_json::{Value, json};

async fn setup() -> (
    Fixture,
    Arc<Gate>,
    Application,
    Context,
    Context,
    LaunchIdentity,
) {
    let result = super::setup().await;
    *result.1.recovery.lock().unwrap() = agentchat_core::delivery::InputRecovery::RetainUncertainty;
    result
}

fn unknown() -> Result<MessageProgress> {
    Err(Error::new(
        ErrorCode::RuntimeUnavailable,
        "Synthetic result loss after possible native acceptance.",
    ))
}

#[tokio::test]
async fn orphaned_original_claim_survives_backup_restart_and_reconnect_without_replay() {
    let (f, gate, app, sender, context, identity) = setup().await;
    f.post(&sender, "@receiver possibly accepted before runtime loss")
        .await;
    f.post(&sender, "passive successor").await;
    let original = app.prepare_message(&context).await.unwrap().unwrap();
    let pending = stream(&f, &identity).await;
    assert!(pending.head.as_ref().unwrap().submission_pending);
    let mut replacement = pending.clone();
    let head = replacement.head.as_mut().unwrap();
    head.attempts += 1;
    head.token = Uuid::new_v4();
    let mut writer = f.store.begin().await.unwrap();
    assert_eq!(
        writer
            .save_message_stream(&replacement)
            .await
            .unwrap_err()
            .code,
        ErrorCode::IntegrityError
    );
    writer.rollback().await.unwrap();
    let backup = f.root.path().join("uncertain.sqlite3");
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
        pending
    );

    // No completion callback exists at this crash cut. Even if the process died
    // before the actual send, elapsed time cannot manufacture non-acceptance.
    f.clock.advance(30);
    let store = Arc::new(
        agentchat_store::Store::open(
            f.root.path().join("agentchat.sqlite3"),
            f.store.identity().clone(),
        )
        .await
        .unwrap(),
    );
    let app = Application::new(store, f.clock.clone(), gate.clone());
    app.invalidate_connections().await.unwrap();
    let mut next = context.clone();
    next.context_id = Uuid::new_v4();
    next.incarnation = Uuid::new_v4();
    gate.reconnect(&next, &identity);
    app.attach(&next).await.unwrap().unwrap();
    assert!(app.prepare_message(&next).await.unwrap().is_none());
    assert!(app.prepare_native(&next).await.unwrap().is_none());
    assert_eq!(stream(&f, &identity).await, pending);
    assert_eq!(
        app.query(Some(&next), Query::Whoami)
            .await
            .map(|value| serde_json::to_value(value).unwrap())
            .unwrap()["native_delivery"]["state"],
        "acceptance_unknown"
    );
    assert!(
        !app.finish_message_progress(&context, &original, unknown())
            .await
            .unwrap()
    );

    // A positive result for the unchanged original is still a valid receipt,
    // including across reconnect. This path emits no replacement native input.
    accept(&app, &context, &original).await;
    let successor = app.prepare_message(&next).await.unwrap().unwrap();
    assert_eq!(successor.input.body, "passive successor");
    assert_eq!(successor.attempt, 1);
    assert_eq!(f.count("read_receipts").await, 0);
}

#[tokio::test]
async fn exact_original_refusal_after_reconnect_releases_only_its_retry_fence() {
    {
        let (f, gate, app, sender, context, identity) = setup().await;
        f.post(&sender, "@receiver definitely refused before reconnect")
            .await;
        let original = app.prepare_message(&context).await.unwrap().unwrap();
        let settled_seq = stream(&f, &identity).await.settled_seq;
        let mut next = context.clone();
        next.context_id = Uuid::new_v4();
        next.incarnation = Uuid::new_v4();
        gate.reconnect(&next, &identity);
        app.invalidate_connections().await.unwrap();
        app.attach(&next).await.unwrap().unwrap();

        assert!(
            app.finish_message_progress(&context, &original, failed())
                .await
                .unwrap()
        );
        let resolved = stream(&f, &identity).await;
        assert_eq!(resolved.settled_seq, settled_seq);
        assert!(!resolved.head.as_ref().unwrap().submission_pending);
        f.clock.advance(6);
        let retry = app.prepare_message(&next).await.unwrap().unwrap();
        assert_eq!(retry.input, original.input);
        assert_eq!(retry.attempt, 2);
        assert_ne!(retry.token, original.token);
    }

    {
        let (f, gate, app, sender, context, identity) = setup().await;
        f.run(
            &sender,
            Mutation::Reserve {
                patterns: vec!["src/reconnect/**".into()],
                ttl_seconds: None,
                reason: None,
            },
        )
        .await;
        let original = app.prepare_native(&context).await.unwrap().unwrap();
        assert!(matches!(original, NativeDispatch::Notice(_)));
        let settled_seq = stream(&f, &identity).await.settled_seq;
        let mut next = context.clone();
        next.context_id = Uuid::new_v4();
        next.incarnation = Uuid::new_v4();
        gate.reconnect(&next, &identity);
        app.invalidate_connections().await.unwrap();
        app.attach(&next).await.unwrap().unwrap();

        assert!(
            app.finish_native(&context, &original, failed())
                .await
                .unwrap()
        );
        let resolved = stream(&f, &identity).await;
        assert_eq!(resolved.settled_seq, settled_seq);
        assert!(!resolved.head.as_ref().unwrap().submission_pending);
        f.clock.advance(6);
        let retry = app.prepare_native(&next).await.unwrap().unwrap();
        assert_eq!(retry.input(), original.input());
        assert_eq!(retry.attempt(), 2);
        assert_ne!(retry.token(), original.token());
    }
}

#[tokio::test]
async fn captured_delivery_deadlines_notify_unknown_acceptance_and_repost_cannot_unblock() {
    for timeout in [None, Some(10)] {
        let (f, _gate, app, sender, context, identity) = setup().await;
        f.timeout(timeout.unwrap_or(60)).await;
        let posted = f
            .run(
                &sender,
                Mutation::Post {
                    channel: Some("dev".into()),
                    body: "@receiver original request".into(),
                },
            )
            .await
            .result;
        let original = app.prepare_message(&context).await.unwrap().unwrap();
        assert_eq!(original.input.message_id, id(&posted, "message_id"));
        app.finish_message_progress(&context, &original, unknown())
            .await
            .unwrap();
        let pending = stream(&f, &identity).await;
        f.timeout(60).await;
        f.post(&sender, "@receiver sender decides to repost").await;
        assert!(
            serde_json::from_value::<Mutation>(json!({
                "operation":"ack","delivery_ids":[original.input.delivery_id]
            }))
            .is_err()
        );
        f.clock.advance(11);
        let maintenance = app.maintain().await.unwrap();
        assert_eq!(
            maintenance.delivery_confirmation_timeouts,
            usize::from(timeout.is_some())
        );
        app.maintain().await.unwrap();
        assert!(app.prepare_native(&context).await.unwrap().is_none());
        assert!(app.prepare_message(&context).await.unwrap().is_none());
        assert_eq!(stream(&f, &identity).await, pending);
        assert_eq!(f.count("read_receipts").await, 0);
        let events = f.rows("events").await;
        let notices: Vec<_> = events
            .iter()
            .filter(|e| {
                e.try_get::<String>("", "kind").unwrap() == "message.delivery_confirmation_timeout"
            })
            .collect();
        assert_eq!(notices.len(), usize::from(timeout.is_some()));
        if let Some(notice) = notices.first() {
            let mut tx = f.store.begin().await.unwrap();
            let notice_id = tx
                .delivery_timeout_notice(original.input.message_id)
                .await
                .unwrap()
                .unwrap()
                .to_string();
            tx.rollback().await.unwrap();
            let data: Value =
                serde_json::from_str(&notice.try_get::<String>("", "data").unwrap()).unwrap();
            assert_eq!(data["message_id"], json!(original.input.message_id));
            assert_eq!(data["expired_count"], 1);
            assert_eq!(
                data["sample_recipients"][0]["delivery_id"],
                json!(original.input.delivery_id)
            );
            assert_eq!(data["sample_recipients"][0]["state"], "failed_timeout");
            assert!(data["sample_recipients"][0]["deadline_at"].is_string());
            assert!(data["sample_recipients"][0]["timed_out_at"].is_string());
            let status = app
                .query(Some(&context), Query::Whoami)
                .await
                .map(|value| serde_json::to_value(value).unwrap())
                .unwrap();
            assert_eq!(status["native_delivery"]["state"], "acceptance_unknown");
            assert!(
                data["preview"]
                    .as_str()
                    .unwrap()
                    .starts_with("Delivery was not confirmed")
            );
            assert!(!data["preview"].as_str().unwrap().contains("not delivered"));
            let author = data["author_id"].as_str().unwrap();
            for delivery in f.rows("deliveries").await {
                if delivery.try_get::<String>("", "dispatch_id").unwrap() == notice_id {
                    assert_eq!(
                        delivery.try_get::<bool>("", "wake_eligible").unwrap(),
                        delivery.try_get::<String>("", "recipient_id").unwrap() == author
                    );
                }
            }
        }
        f.store.integrity_check().await.unwrap();
    }
}

#[tokio::test]
async fn uncertain_typed_notice_blocks_later_messages_and_only_original_receipt_settles() {
    let (f, _gate, app, sender, context, identity) = setup().await;
    f.run(
        &sender,
        Mutation::Reserve {
            patterns: vec!["src/**".into()],
            ttl_seconds: None,
            reason: None,
        },
    )
    .await;
    let original = app.prepare_native(&context).await.unwrap().unwrap();
    assert!(matches!(original, NativeDispatch::Notice(_)));
    app.finish_native(&context, &original, unknown())
        .await
        .unwrap();
    f.post(&sender, "@receiver later addressed body").await;
    f.clock.advance(30);
    assert!(app.prepare_native(&context).await.unwrap().is_none());
    let retained = stream(&f, &identity).await;
    assert_eq!(retained.head.as_ref().unwrap().token, original.token());
    assert_eq!(retained.head.as_ref().unwrap().attempts, 1);
    let mut forged = original.clone();
    match &mut forged {
        NativeDispatch::Notice(n) => n.token = Uuid::new_v4(),
        _ => unreachable!(),
    }
    assert!(
        !app.finish_native(
            &context,
            &forged,
            Ok(MessageProgress::Accepted(HostReceipt::default()))
        )
        .await
        .unwrap()
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
    let NativeDispatch::Message(next) = next else {
        panic!("message successor expected")
    };
    assert_eq!(next.input.body, "@receiver later addressed body");
}

#[tokio::test]
async fn released_v6_namespace_is_refused_before_interpreting_native_heads() {
    old_namespace_is_refused(6).await;
}

#[test]
fn old_frame_revisions_are_not_deserialized_as_current_policy() {
    use agentchat_core::project::NativeFrameRevision;
    assert_eq!(
        NativeFrameRevision::from_code(4).unwrap(),
        NativeFrameRevision::DispatchV4
    );
    for code in [0, 1, 2, 3] {
        assert_eq!(
            NativeFrameRevision::from_code(code).unwrap_err().code,
            ErrorCode::UnsupportedProtocol
        );
    }
    for revision in ["legacy_v1", "receipt_policy_v2"] {
        assert!(serde_json::from_value::<NativeFrameRevision>(json!(revision)).is_err());
    }
}

#[tokio::test]
async fn explicit_read_cannot_complete_or_settle_unknown_native_acceptance() {
    let (f, _gate, app, sender, context, identity) = setup().await;
    f.timeout(10).await;
    let post = f
        .run(
            &sender,
            Mutation::Post {
                channel: Some("dev".into()),
                body: "@receiver read confirmation is independent".into(),
            },
        )
        .await
        .result;
    let original = app.prepare_message(&context).await.unwrap().unwrap();
    app.finish_message_progress(&context, &original, unknown())
        .await
        .unwrap();
    let pending = stream(&f, &identity).await;
    command(
        &app,
        &context,
        Mutation::Read {
            message_ids: vec![id(&post, "message_id")],
        },
    )
    .await;
    f.clock.advance(11);
    assert_eq!(
        app.maintain().await.unwrap().delivery_confirmation_timeouts,
        1
    );
    assert_eq!(f.count("read_receipts").await, 1);
    assert_eq!(stream(&f, &identity).await, pending);
    assert!(app.prepare_native(&context).await.unwrap().is_none());
}
