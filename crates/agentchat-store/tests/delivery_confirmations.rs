//! Actual processing deadlines, bounded author notices, scoped departure and users.
mod support;
use agentchat_core::{
    application::{Application, Mutation},
    delivery::{HostReceipt, MessageProgress, NativeDispatch},
    model::{Context, DeliveryState as State, OperationActor, ReadKind},
    project::ReceiptSettingsUpdate,
    read::{MessageDetail, PageRequest, Query, QueryResult},
    time::Clock,
};
use sea_orm::{
    ConnectionTrait,
    sea_query::{Alias, Expr, ExprTrait, Query as Sql},
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
async fn setup() -> (Fixture, Application, Context, Context) {
    let f = Fixture::new().await;
    let sender = f.context();
    f.join(&sender, "author").await;
    let receiver = f.context();
    let gate = Gate::new(&f);
    gate.bind(&receiver);
    let app = gate.app(&f);
    app.start_launch(&receiver, Some("receiver")).await.unwrap();
    app.initialize_with_settings(
        None,
        ReceiptSettingsUpdate {
            auto_read: Some(false),
            delivery_confirmation_timeout_seconds: Some(10),
        },
    )
    .await
    .unwrap();
    (f, app, sender, receiver)
}
async fn notifications(f: &Fixture) -> Vec<sea_orm::QueryResult> {
    f.rows("events")
        .await
        .into_iter()
        .filter(|r| {
            r.try_get::<String>("", "kind").unwrap() == "message.delivery_confirmation_timeout"
        })
        .collect()
}
async fn event_targets(f: &Fixture, event: String) -> Vec<sea_orm::QueryResult> {
    let original = f
        .rows("events")
        .await
        .into_iter()
        .find(|r| r.try_get::<String>("", "event_id").unwrap() == event)
        .unwrap()
        .try_get::<Option<String>>("", "message_id")
        .unwrap()
        .unwrap();
    let notice = f
        .rows("delivery_timeout_notices")
        .await
        .into_iter()
        .find(|r| r.try_get::<String>("", "message_id").unwrap() == original)
        .unwrap()
        .try_get::<String>("", "notice_dispatch_id")
        .unwrap();
    f.inspect
        .query_all(
            &Sql::select()
                .column(Alias::new("recipient_id"))
                .from(Alias::new("deliveries"))
                .and_where(Expr::col(Alias::new("dispatch_id")).eq(notice))
                .to_owned(),
        )
        .await
        .unwrap()
}
async fn drain_notices(app: &Application, context: &Context) {
    while let Some(dispatch) = app.prepare_native(context).await.unwrap() {
        assert!(matches!(dispatch, NativeDispatch::Notice(_)));
        app.finish_native(
            context,
            &dispatch,
            Ok(MessageProgress::Accepted(HostReceipt::default())),
        )
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn inclusive_deadline_delivers_and_racing_late_acceptance_preserves_one_timeout() {
    let (f, app, sender, receiver) = setup().await;
    let first = id(
        &f.post(&sender, "passive target without mention")
            .await
            .result,
        "message_id",
    );
    let dispatch = app.prepare_message(&receiver).await.unwrap().unwrap();
    f.clock.advance(10);
    assert_eq!(
        app.maintain().await.unwrap().delivery_confirmation_timeouts,
        0
    );
    app.finish_message(&receiver, &dispatch, Ok(HostReceipt::default()))
        .await
        .unwrap();
    let d = detail(&app, first).await;
    let delivery = d.receipts.items[0].delivery.as_ref().unwrap();
    assert_eq!(delivery.state, State::Delivered);
    assert_eq!(delivery.history.delivered_at, delivery.history.deadline_at);
    assert!(delivery.history.timed_out_at.is_none());
    assert!(d.receipts.items[0].reads.is_empty());
    let second = id(
        &f.post(&sender, "late full acceptance").await.result,
        "message_id",
    );
    let dispatch = app.prepare_message(&receiver).await.unwrap().unwrap();
    f.clock.advance(11);
    let (maintenance, completion) = tokio::join!(
        app.maintain(),
        app.finish_message(&receiver, &dispatch, Ok(HostReceipt::default()))
    );
    // Either writer may discover expiry first; both must compose to one notice.
    assert!(maintenance.unwrap().delivery_confirmation_timeouts <= 1);
    assert!(completion.unwrap());
    let d = detail(&app, second).await;
    let delivery = d.receipts.items[0].delivery.as_ref().unwrap();
    assert_eq!(delivery.state, State::DeliveredLate);
    assert_eq!(delivery.history.delivered_at, Some(f.clock.now()));
    assert!(delivery.history.timed_out_at.is_some());
    assert_eq!(notifications(&f).await.len(), 1);
    assert_eq!(f.count("delivery_timeout_notices").await, 1);
    assert_eq!(
        app.maintain().await.unwrap().delivery_confirmation_timeouts,
        0
    );
    f.store.integrity_check().await.unwrap();
}

#[tokio::test]
async fn twenty_five_expired_passive_targets_coalesce_without_expiring_an_unstarted_recipient() {
    let f = Fixture::new().await;
    let author = f.context();
    let author_id = f.join(&author, "author").await;
    let gate = Gate::new(&f);
    let app = gate.app(&f);
    let mut contexts = Vec::new();
    for n in 0..26 {
        let context = f.context();
        gate.bind(&context);
        app.start_launch(&context, Some(&format!("recipient{n}")))
            .await
            .unwrap();
        contexts.push(context);
    }
    for context in &contexts {
        drain_notices(&app, context).await;
    }
    f.timeout(1).await;
    let posted = f.post(&author, "no explicit recipients").await;
    assert_eq!(posted.result["recipient_count"], 26);
    let message = id(&posted.result, "message_id");
    f.join(&f.context(), "latejoiner").await;
    for context in &contexts[..25] {
        assert_eq!(
            app.prepare_message(context)
                .await
                .unwrap()
                .unwrap()
                .input
                .message_id,
            message
        );
    }
    f.clock.advance(2);
    assert_eq!(
        app.maintain().await.unwrap().delivery_confirmation_timeouts,
        1
    );
    let notices = notifications(&f).await;
    assert_eq!(notices.len(), 1);
    let data: serde_json::Value = notices[0].try_get("", "data").unwrap();
    assert_eq!(data["expired_count"], 25);
    assert_eq!(data["sample_recipients"].as_array().unwrap().len(), 20);
    assert!(
        data["sample_recipients"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["state"] == "failed_timeout"
                && row["deadline_at"].as_str().is_some()
                && row["timed_out_at"].as_str().is_some())
    );
    let targets = event_targets(&f, notices[0].try_get("", "event_id").unwrap()).await;
    assert_eq!(targets.len(), 1);
    assert_eq!(
        targets[0].try_get::<String>("", "recipient_id").unwrap(),
        author_id.to_string()
    );
    let d = detail(&app, message).await;
    assert_eq!(d.receipts.items.len(), 26);
    assert_eq!(
        d.receipts
            .items
            .iter()
            .filter(|row| row.delivery.as_ref().unwrap().state == State::FailedTimeout)
            .count(),
        25
    );
    let pending = d
        .receipts
        .items
        .iter()
        .find(|row| row.delivery.as_ref().unwrap().state == State::Pending)
        .unwrap();
    assert!(
        pending
            .delivery
            .as_ref()
            .unwrap()
            .history
            .deadline_at
            .is_none()
    );
    assert_eq!(
        app.prepare_message(&contexts[25])
            .await
            .unwrap()
            .unwrap()
            .input
            .message_id,
        message
    );
    f.clock.advance(2);
    assert_eq!(
        app.maintain().await.unwrap().delivery_confirmation_timeouts,
        0
    );
    assert_eq!(notifications(&f).await.len(), 1);
    assert_eq!(f.count("delivery_timeout_notices").await, 1);
    assert!(
        detail(&app, message)
            .await
            .receipts
            .items
            .iter()
            .all(|row| row.delivery.as_ref().unwrap().state == State::FailedTimeout)
    );
}

#[tokio::test]
async fn author_project_leave_cancels_notice_without_changing_delivery_or_independent_reads() {
    for rejoin in [false, true] {
        let (f, app, sender, receiver) = setup().await;
        let message = id(
            &f.post(&sender, "unaccepted original").await.result,
            "message_id",
        );
        app.execute(
            Uuid::new_v4(),
            &receiver,
            Mutation::Read {
                message_ids: vec![message],
            },
        )
        .await
        .unwrap();
        let queued = detail(&app, message).await;
        assert_eq!(
            queued.receipts.items[0].delivery.as_ref().unwrap().state,
            State::Pending
        );
        assert_eq!(queued.receipts.items[0].reads[0].kind, ReadKind::Explicit);
        let dispatch = app.prepare_message(&receiver).await.unwrap().unwrap();
        f.run(&sender, Mutation::Leave).await;
        if rejoin {
            f.join(&sender, "author").await;
        }
        assert_eq!(
            detail(&app, message).await.receipts.items[0]
                .delivery
                .as_ref()
                .unwrap()
                .state,
            State::Processing
        );
        f.clock.advance(11);
        assert_eq!(
            app.maintain().await.unwrap().delivery_confirmation_timeouts,
            0
        );
        let after = detail(&app, message).await;
        let delivery = after.receipts.items[0].delivery.as_ref().unwrap();
        assert_eq!(delivery.state, State::FailedTimeout);
        assert!(delivery.history.delivered_at.is_none());
        assert_eq!(
            after.receipts.items[0].reads,
            queued.receipts.items[0].reads
        );
        assert!(notifications(&f).await.is_empty());
        assert_eq!(f.count("delivery_timeout_notices").await, 0);
        // Cancellation is permanent for this original message, including late acceptance.
        app.finish_message(&receiver, &dispatch, Ok(HostReceipt::default()))
            .await
            .unwrap();
        let late = detail(&app, message).await;
        assert_eq!(
            late.receipts.items[0].delivery.as_ref().unwrap().state,
            State::DeliveredLate
        );
        assert_eq!(late.receipts.items[0].reads, queued.receipts.items[0].reads);
        assert!(notifications(&f).await.is_empty());
        assert_eq!(f.count("delivery_timeout_notices").await, 0);
    }
}

#[tokio::test]
async fn recipient_leave_fails_processing_and_pending_rows_and_refuses_obsolete_acceptance() {
    let (f, app, sender, receiver) = setup().await;
    let first = id(
        &f.post(&sender, "processing original").await.result,
        "message_id",
    );
    let second = id(
        &f.post(&sender, "unstarted successor").await.result,
        "message_id",
    );
    let dispatch = app.prepare_message(&receiver).await.unwrap().unwrap();
    app.execute(Uuid::new_v4(), &receiver, Mutation::Leave)
        .await
        .unwrap();
    for (message, started) in [(first, true), (second, false)] {
        let d = detail(&app, message).await;
        let delivery = d.receipts.items[0].delivery.as_ref().unwrap();
        assert_eq!(delivery.state, State::FailedLeft);
        assert_eq!(delivery.history.processing_started_at.is_some(), started);
        assert_eq!(delivery.history.left_at, Some(f.clock.now()));
        assert!(delivery.history.delivered_at.is_none());
        assert!(d.receipts.items[0].reads.is_empty());
    }
    assert!(
        !app.finish_message(&receiver, &dispatch, Ok(HostReceipt::default()))
            .await
            .unwrap()
    );
    f.clock.advance(11);
    assert_eq!(
        app.maintain().await.unwrap().delivery_confirmation_timeouts,
        0
    );
    assert!(notifications(&f).await.is_empty());
}

#[tokio::test]
async fn anonymous_author_timeout_is_audit_only_and_users_never_receive_native_targets() {
    let f = Fixture::new().await;
    let receiver = f.context();
    let gate = Gate::new(&f);
    gate.bind(&receiver);
    let app = gate.app(&f);
    app.start_launch(&receiver, Some("recipient"))
        .await
        .unwrap();
    app.execute_actor(
        Uuid::new_v4(),
        &OperationActor::User,
        Mutation::Join { name: None },
    )
    .await
    .unwrap();
    drain_notices(&app, &receiver).await;
    f.timeout(1).await;
    let posted = app
        .execute_actor(
            Uuid::new_v4(),
            &OperationActor::User,
            Mutation::Post {
                channel: None,
                body: "from user".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(posted.result["recipient_count"], 1);
    let message = id(&posted.result, "message_id");
    f.clock.advance(2);
    assert_eq!(
        app.maintain().await.unwrap().delivery_confirmation_timeouts,
        0
    );
    assert_eq!(
        app.prepare_message(&receiver)
            .await
            .unwrap()
            .unwrap()
            .input
            .message_id,
        message
    );
    f.clock.advance(2);
    assert_eq!(
        app.maintain().await.unwrap().delivery_confirmation_timeouts,
        1
    );
    let notices = notifications(&f).await;
    assert_eq!(notices.len(), 1);
    assert!(
        event_targets(&f, notices[0].try_get("", "event_id").unwrap())
            .await
            .is_empty()
    );
    assert_eq!(f.count("delivery_timeout_notices").await, 1);
}

#[tokio::test]
async fn anonymous_author_leave_then_rejoin_never_restores_its_cancelled_timeout_notice() {
    let f = Fixture::new().await;
    let receiver = f.context();
    let gate = Gate::new(&f);
    gate.bind(&receiver);
    let app = gate.app(&f);
    app.start_launch(&receiver, Some("recipient"))
        .await
        .unwrap();
    app.execute_actor(
        Uuid::new_v4(),
        &OperationActor::User,
        Mutation::Join { name: None },
    )
    .await
    .unwrap();
    drain_notices(&app, &receiver).await;
    f.timeout(1).await;
    let posted = app
        .execute_actor(
            Uuid::new_v4(),
            &OperationActor::User,
            Mutation::Post {
                channel: None,
                body: "from user before leave".into(),
            },
        )
        .await
        .unwrap();
    let message = id(&posted.result, "message_id");
    let dispatch = app.prepare_message(&receiver).await.unwrap().unwrap();
    app.execute_actor(Uuid::new_v4(), &OperationActor::User, Mutation::Leave)
        .await
        .unwrap();
    app.execute_actor(
        Uuid::new_v4(),
        &OperationActor::User,
        Mutation::Join { name: None },
    )
    .await
    .unwrap();
    f.clock.advance(2);
    assert_eq!(
        app.maintain().await.unwrap().delivery_confirmation_timeouts,
        0
    );
    assert_eq!(
        detail(&app, message).await.receipts.items[0]
            .delivery
            .as_ref()
            .unwrap()
            .state,
        State::FailedTimeout
    );
    app.finish_message(&receiver, &dispatch, Ok(HostReceipt::default()))
        .await
        .unwrap();
    assert_eq!(
        detail(&app, message).await.receipts.items[0]
            .delivery
            .as_ref()
            .unwrap()
            .state,
        State::DeliveredLate
    );
    assert!(notifications(&f).await.is_empty());
    assert_eq!(f.count("delivery_timeout_notices").await, 0);
}
