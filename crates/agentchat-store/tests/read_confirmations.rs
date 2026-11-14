//! Processing deadlines, durable author-notice identity and bounded expiry pages.
mod support;
use agentchat_core::{
    application::{Application, Mutation},
    delivery::{MessageProgress, NativeDispatch},
    model::{Context, Delivery, DeliveryState, Phase},
    ports::{Storage, Transaction},
    read::{PageRequest, Query},
    time::Clock,
};
use agentchat_store::Store;
use std::{collections::BTreeSet, sync::Arc};
use support::{Fixture, id, native::Gate};
use uuid::Uuid;

async fn detail(app: &Application, message_id: Uuid) -> serde_json::Value {
    serde_json::to_value(
        app.query(
            None,
            Query::Message {
                message_id,
                receipts: PageRequest::default(),
            },
        )
        .await
        .unwrap(),
    )
    .unwrap()
}

async fn delivery(store: &Store, id: Uuid) -> Delivery {
    let mut tx = store.begin().await.unwrap();
    let row = tx.delivery(id).await.unwrap().unwrap();
    tx.rollback().await.unwrap();
    row
}

async fn delivery_set(tx: &mut dyn Transaction, ids: &[Uuid]) -> Vec<Delivery> {
    let mut rows = Vec::with_capacity(ids.len());
    for chunk in ids.chunks(200) {
        rows.extend(tx.deliveries(chunk).await.unwrap());
    }
    rows.sort_by_key(|row| row.id);
    rows
}

async fn drain_setup_notices(f: &Fixture, context: &Context) {
    // Run only before posting bodies: no mixed-FIFO claim is discarded or skipped.
    while let Some(dispatch) = f.app.prepare_native(context).await.unwrap() {
        assert!(matches!(dispatch, NativeDispatch::Notice(_)));
        assert!(
            f.app
                .finish_native(
                    context,
                    &dispatch,
                    Ok(MessageProgress::Accepted(Default::default()))
                )
                .await
                .unwrap()
        );
    }
}

#[tokio::test]
async fn deadlines_capture_processing_settings_and_explicit_reads_cannot_change_them() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    f.join(&a, "author").await;
    let recipient = f.native_join(&b, "recipient").await;
    f.timeout(2).await;
    let first = id(&f.post(&a, "first").await.result, "message_id");
    let second = id(
        &f.run(
            &a,
            Mutation::Reply {
                root_message_id: first,
                body: "second".into(),
                read_message_ids: Vec::new(),
            },
        )
        .await
        .result,
        "message_id",
    );
    let first_id = f.delivery(first, recipient).await;
    let second_id = f.delivery(second, recipient).await;
    let queued = delivery(&f.store, first_id).await;
    f.timeout(10).await;
    f.clock.advance(5);
    assert_eq!(
        f.app
            .maintain()
            .await
            .unwrap()
            .delivery_confirmation_timeouts,
        0
    );
    assert_eq!(delivery(&f.store, first_id).await, queued);
    let original_read = f
        .run(
            &b,
            Mutation::Read {
                message_ids: vec![first, second],
            },
        )
        .await;
    assert_eq!(original_read.result["changed"].as_array().unwrap().len(), 2);
    assert_eq!(delivery(&f.store, first_id).await, queued);
    let dispatch = f.app.prepare_message(&b).await.unwrap().unwrap();
    assert_eq!(dispatch.input.message_id, first);
    let started = delivery(&f.store, first_id).await;
    assert_eq!(started.state, DeliveryState::Processing);
    assert_eq!(started.history.processing_started_at, Some(f.clock.now()));
    assert_eq!(
        started.history.deadline_at,
        Some(f.clock.now().after_seconds(10).unwrap())
    );
    assert_eq!(started.history.queued_at, queued.history.queued_at);
    f.timeout(2).await;
    f.clock.advance(10);
    assert_eq!(
        f.app
            .maintain()
            .await
            .unwrap()
            .delivery_confirmation_timeouts,
        0
    );
    assert_eq!(delivery(&f.store, first_id).await, started);
    f.clock.advance(1);
    assert_eq!(
        f.app
            .maintain()
            .await
            .unwrap()
            .delivery_confirmation_timeouts,
        1
    );
    let expired = delivery(&f.store, first_id).await;
    assert_eq!(expired.state, DeliveryState::FailedTimeout);
    assert_eq!(expired.history.deadline_at, started.history.deadline_at);
    assert_eq!(expired.history.timed_out_at, Some(f.clock.now()));
    assert_eq!(
        delivery(&f.store, second_id).await.state,
        DeliveryState::Pending
    );
    assert!(
        delivery(&f.store, second_id)
            .await
            .history
            .deadline_at
            .is_none()
    );
    // Only this exact original complete-input result releases the successor.
    assert!(
        f.app
            .finish_message(&b, &dispatch, Ok(Default::default()))
            .await
            .unwrap()
    );
    assert_eq!(
        delivery(&f.store, first_id).await.state,
        DeliveryState::DeliveredLate
    );
    let next = f.app.prepare_message(&b).await.unwrap().unwrap();
    assert_eq!(next.input.message_id, second);
    let second_started = delivery(&f.store, second_id).await;
    assert_eq!(
        second_started.history.processing_started_at,
        Some(f.clock.now())
    );
    assert_eq!(
        second_started.history.deadline_at,
        Some(f.clock.now().after_seconds(2).unwrap())
    );
    f.timeout(100).await;
    f.clock.advance(2);
    assert_eq!(
        f.app
            .maintain()
            .await
            .unwrap()
            .delivery_confirmation_timeouts,
        0
    );
    f.clock.advance(1);
    assert_eq!(
        f.app
            .maintain()
            .await
            .unwrap()
            .delivery_confirmation_timeouts,
        1
    );
    let before_read = delivery(&f.store, second_id).await;
    let repeated = f
        .run(
            &b,
            Mutation::Read {
                message_ids: vec![first, second],
            },
        )
        .await;
    assert_eq!(repeated.result["changed"], serde_json::json!([]));
    assert_eq!(delivery(&f.store, second_id).await, before_read);
    assert_eq!(before_read.state, DeliveryState::FailedTimeout);
    assert_eq!(
        before_read.history.deadline_at,
        second_started.history.deadline_at
    );
    assert!(before_read.history.delivered_at.is_none());
    assert_eq!(
        detail(&f.app, second).await["receipts"]["items"][0]["reads"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    f.assert_read_kinds(0, 2).await;
}

#[tokio::test]
async fn offline_author_restart_backup_and_paused_resume_preserve_timeout_identity() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    let gate = Gate::new(&f);
    let author_identity = gate.bind(&a);
    gate.bind(&b);
    let app = gate.app(&f);
    app.start_launch(&a, Some("alice")).await.unwrap();
    let recipient = id(
        &app.start_launch(&b, Some("bob")).await.unwrap().result,
        "agent_id",
    );
    f.timeout(2).await;
    let key = Uuid::new_v4();
    let command = Mutation::Post {
        channel: Some("dev".into()),
        body: "@bob durable".into(),
    };
    let posted = app.execute(key, &a, command.clone()).await.unwrap();
    let message = id(&posted.result, "message_id");
    let delivery_id = f.delivery(message, recipient).await;
    let original = app.prepare_native(&b).await.unwrap().unwrap();
    assert!(matches!(original, NativeDispatch::Message(_)));
    assert_eq!(original.delivery_id(), delivery_id);
    app.finish_native(&b, &original, Ok(MessageProgress::AwaitingAcceptance))
        .await
        .unwrap();
    let started = delivery(&f.store, delivery_id).await;
    app.detach(&a).await.unwrap();
    app.detach(&b).await.unwrap();
    f.clock.advance(3);
    let destination = f.root.path().join("restart.sqlite3");
    f.store.backup(&destination).await.unwrap();
    let restored = Arc::new(
        Store::open(&destination, f.store.identity().clone())
            .await
            .unwrap(),
    );
    let app = Application::new(restored.clone(), f.clock.clone(), gate.clone());
    app.invalidate_connections().await.unwrap();
    assert_eq!(
        app.maintain().await.unwrap().delivery_confirmation_timeouts,
        1
    );
    assert!(app.prepare_wake(&a).await.unwrap().is_none());
    let expired = delivery(&restored, delivery_id).await;
    assert_eq!(expired.state, DeliveryState::FailedTimeout);
    assert_eq!(
        expired.history.processing_started_at,
        started.history.processing_started_at
    );
    assert_eq!(expired.history.deadline_at, started.history.deadline_at);
    assert_eq!(expired.history.timed_out_at, Some(f.clock.now()));
    assert!(expired.history.delivered_at.is_none());
    let first = detail(&app, message).await;
    let mut tx = restored.begin().await.unwrap();
    let notice_id = tx.delivery_timeout_notice(message).await.unwrap().unwrap();
    let inspect = support::upgrade::inspect(&destination).await;
    let notice = notice_payload(&inspect, notice_id).await;
    assert_eq!(notice.data["expired_count"], 1);
    let head = tx
        .message_stream(original.generation())
        .await
        .unwrap()
        .unwrap()
        .head
        .unwrap();
    assert_eq!(head.token, original.token());
    assert_eq!(head.delivery_id, delivery_id);
    assert!(head.awaiting_acceptance);
    tx.rollback().await.unwrap();
    let mut resumed = a.clone();
    resumed.context_id = Uuid::new_v4();
    resumed.incarnation = Uuid::new_v4();
    gate.reconnect(&resumed, &author_identity);
    f.host.phase(&resumed, Phase::Paused);
    app.attach(&resumed).await.unwrap();
    assert!(app.prepare_wake(&resumed).await.unwrap().is_none());
    f.host.phase(&resumed, Phase::ApprovalBlocked);
    app.heartbeat(&resumed).await.unwrap();
    assert!(app.prepare_wake(&resumed).await.unwrap().is_none());
    f.host.phase(&resumed, Phase::Idle);
    app.heartbeat(&resumed).await.unwrap();
    let dispatch = app.prepare_wake(&resumed).await.unwrap().unwrap();
    assert_eq!(dispatch.hint.events.len(), 1);
    assert_eq!(
        dispatch.hint.events[0].kind,
        "message.delivery_confirmation_timeout"
    );
    assert_eq!(dispatch.hint.events[0].dispatch_id, notice_id);
    assert_eq!(
        app.execute(key, &resumed, command).await.unwrap().result,
        posted.result
    );
    let second = f.root.path().join("terminal.sqlite3");
    restored.backup(&second).await.unwrap();
    let terminal = Arc::new(
        Store::open(second, f.store.identity().clone())
            .await
            .unwrap(),
    );
    let again = Application::new(terminal.clone(), f.clock.clone(), gate);
    assert_eq!(
        again
            .maintain()
            .await
            .unwrap()
            .delivery_confirmation_timeouts,
        0
    );
    assert_eq!(detail(&again, message).await, first);
    let mut tx = terminal.begin().await.unwrap();
    assert_eq!(
        tx.delivery_timeout_notice(message).await.unwrap(),
        Some(notice_id)
    );
    assert_eq!(notice_payload(&inspect, notice_id).await.data, notice.data);
    assert_eq!(tx.delivery(delivery_id).await.unwrap().unwrap(), expired);
    tx.rollback().await.unwrap();
    terminal.integrity_check().await.unwrap();
}

#[tokio::test]
async fn bounded_two_hundred_due_rows_coalesce_each_message_once_and_read_cannot_undo_expiry() {
    let f = Fixture::new().await;
    let a = f.context();
    f.join(&a, "author").await;
    let mut receivers = Vec::new();
    for index in 0..11 {
        let context = f.context();
        f.native_join(&context, &format!("recipient{index}")).await;
        receivers.push(context);
    }
    for context in &receivers {
        drain_setup_notices(&f, context).await;
    }
    f.timeout(1).await;
    let mentions = (0..11)
        .map(|i| format!("@recipient{i}"))
        .collect::<Vec<_>>()
        .join(" ");
    let mut messages = Vec::new();
    for index in 0..19 {
        messages.push(id(
            &f.post(&a, &format!("{mentions} request {index}"))
                .await
                .result,
            "message_id",
        ));
    }
    let started_at = f.clock.now();
    let mut claimed = BTreeSet::new();
    // A real hint batch starts each selected reference's processing budget.
    // It neither confirms complete body acceptance nor writes a read receipt.
    for context in &receivers {
        let dispatch = f.app.prepare_wake(context).await.unwrap().unwrap();
        assert_eq!(dispatch.hint.events.len(), 19);
        for event in dispatch.hint.events {
            assert_eq!(event.kind, "message.created");
            assert!(claimed.insert(event.delivery_id));
        }
    }
    assert_eq!(claimed.len(), 209);
    let ids = claimed.into_iter().collect::<Vec<_>>();
    let mut tx = f.store.begin().await.unwrap();
    for row in delivery_set(tx.as_mut(), &ids).await {
        assert_eq!(row.state, DeliveryState::Processing);
        assert_eq!(row.history.processing_started_at, Some(started_at));
        assert_eq!(
            row.history.deadline_at,
            Some(started_at.after_seconds(1).unwrap())
        );
        assert!(row.history.delivered_at.is_none());
    }
    tx.rollback().await.unwrap();
    f.assert_read_kinds(0, 0).await;
    f.clock.advance(1);
    let mut tx = f.store.begin().await.unwrap();
    assert!(tx.due_deliveries(f.clock.now()).await.unwrap().is_empty());
    tx.rollback().await.unwrap();
    f.clock.advance(1);
    let mut tx = f.store.begin().await.unwrap();
    assert_eq!(tx.due_deliveries(f.clock.now()).await.unwrap().len(), 200);
    tx.rollback().await.unwrap();
    assert_eq!(
        f.app
            .maintain()
            .await
            .unwrap()
            .delivery_confirmation_timeouts,
        19
    );
    let mut tx = f.store.begin().await.unwrap();
    assert_eq!(tx.due_deliveries(f.clock.now()).await.unwrap().len(), 9);
    let mut notifications = Vec::new();
    let mut expired_count = 0;
    for message in &messages {
        let event_id = tx.delivery_timeout_notice(*message).await.unwrap().unwrap();
        let event = notice_payload(&f.inspect, event_id).await;
        let count = tx.expired_recipients(*message).await.unwrap().count;
        assert_eq!(event.data["expired_count"].as_u64(), Some(count));
        assert!((2..=11).contains(&count));
        assert_eq!(
            event.data["sample_recipients"].as_array().unwrap().len(),
            count as usize
        );
        expired_count += count;
        notifications.push((*message, event_id, event.data));
    }
    assert_eq!(expired_count, 200);
    tx.rollback().await.unwrap();
    assert_eq!(
        f.app
            .maintain()
            .await
            .unwrap()
            .delivery_confirmation_timeouts,
        0
    );
    let mut tx = f.store.begin().await.unwrap();
    assert!(tx.due_deliveries(f.clock.now()).await.unwrap().is_empty());
    for (message, event_id, data) in &notifications {
        assert_eq!(tx.expired_recipients(*message).await.unwrap().count, 11);
        assert_eq!(
            tx.delivery_timeout_notice(*message).await.unwrap(),
            Some(*event_id)
        );
        assert_eq!(notice_payload(&f.inspect, *event_id).await.data, *data);
    }
    let before_read = delivery_set(tx.as_mut(), &ids).await;
    assert_eq!(before_read.len(), 209);
    assert!(
        before_read
            .iter()
            .all(|row| row.state == DeliveryState::FailedTimeout
                && row.history.delivered_at.is_none())
    );
    tx.rollback().await.unwrap();
    f.run(
        &receivers[0],
        Mutation::Read {
            message_ids: messages,
        },
    )
    .await;
    let mut tx = f.store.begin().await.unwrap();
    assert_eq!(delivery_set(tx.as_mut(), &ids).await, before_read);
    tx.rollback().await.unwrap();
    assert_eq!(f.count("delivery_timeout_notices").await, 19);
    assert_eq!(
        f.app
            .maintain()
            .await
            .unwrap()
            .delivery_confirmation_timeouts,
        0
    );
    f.assert_read_kinds(0, 19).await;
    f.store.integrity_check().await.unwrap();
}

async fn notice_payload(
    db: &sea_orm::DatabaseConnection,
    id: Uuid,
) -> agentchat_core::model::DispatchNotice {
    use sea_orm::{
        ConnectionTrait,
        sea_query::{Alias, Expr, ExprTrait, Query},
    };
    let row = db
        .query_one(
            &Query::select()
                .columns([Alias::new("kind"), Alias::new("data")])
                .from(Alias::new("dispatch_notices"))
                .and_where(Expr::col(Alias::new("dispatch_id")).eq(id.to_string()))
                .to_owned(),
        )
        .await
        .unwrap()
        .unwrap();
    agentchat_core::model::DispatchNotice {
        dispatch_id: id,
        kind: row.try_get::<String>("", "kind").unwrap().parse().unwrap(),
        data: row.try_get("", "data").unwrap(),
    }
}
