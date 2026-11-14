//! Persisted Dispatch aggregates stay complete across failures, retries and reopen.
mod support;

use agentchat_core::{
    application::Mutation,
    model::{
        Delivery, DeliveryHistory, DeliveryState, Dispatch, DispatchKind, DispatchNotice,
        DispatchPayload, EventKind, Message, NoticeKind,
    },
    ports::{IdempotencyRecord, Storage},
    time::{Clock, Timestamp},
};
use agentchat_store::Store;
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DbErr,
    sea_query::{Alias, Asterisk, Expr, ExprTrait, Order, Query},
};
use serde_json::json;
use support::{Fixture, id};
use uuid::Uuid;

async fn setup() -> (Fixture, Uuid, Uuid) {
    let f = Fixture::new().await;
    let author = f.join(&f.context(), "author").await;
    let mut tx = f.store.begin().await.unwrap();
    let channel = tx.channel("dev").await.unwrap().unwrap().id;
    tx.rollback().await.unwrap();
    (f, author, channel)
}

fn message(header: &Dispatch, author: Uuid) -> DispatchPayload {
    DispatchPayload::Message(Message {
        id: header.id,
        channel_id: header.channel_id.unwrap(),
        author_id: author,
        root_message_id: None,
        body: "immutable aggregate body".into(),
        created_at: header.created_at,
        creation_seq: header.creation_seq,
    })
}

fn notice(header: &Dispatch, actor: Uuid) -> DispatchPayload {
    DispatchPayload::Notice(DispatchNotice {
        dispatch_id: header.id,
        kind: NoticeKind::AgentJoined,
        data: json!({"agent_id": actor, "handle": "author"}),
    })
}

fn recipient(header: &Dispatch, actor: Uuid) -> Delivery {
    Delivery {
        id: Uuid::new_v4(),
        dispatch_id: header.id,
        recipient_id: actor,
        state: DeliveryState::Pending,
        wake_eligible: false,
        wake_batch_id: None,
        attempts: 0,
        history: DeliveryHistory::queued(header.created_at),
    }
}

async fn inventory(f: &Fixture) -> Vec<usize> {
    let mut counts = Vec::new();
    for table in [
        "dispatches",
        "dispatch_messages",
        "dispatch_notices",
        "deliveries",
        "delivery_state_transitions",
        "events",
        "idempotency",
    ] {
        counts.push(f.count(table).await);
    }
    counts
}

async fn history_snapshot(f: &Fixture) -> Vec<serde_json::Value> {
    f.inspect
        .query_all(
            &Query::select()
                .column(Asterisk)
                .from(Alias::new("delivery_state_transitions"))
                .order_by(Alias::new("delivery_id"), Order::Asc)
                .order_by(Alias::new("ordinal"), Order::Asc)
                .to_owned(),
        )
        .await
        .unwrap()
        .into_iter()
        .map(|row| {
            json!({
                "id": row.try_get::<String>("", "id").unwrap(),
                "delivery_id": row.try_get::<String>("", "delivery_id").unwrap(),
                "ordinal": row.try_get::<i64>("", "ordinal").unwrap(),
                "from_state": row.try_get::<Option<String>>("", "from_state").unwrap(),
                "to_state": row.try_get::<String>("", "to_state").unwrap(),
                "changed_at": row.try_get::<String>("", "changed_at").unwrap(),
                "deadline_at": row.try_get::<Option<String>>("", "deadline_at").unwrap(),
            })
        })
        .collect()
}

async fn reopen(f: &Fixture) -> Store {
    Store::open(
        f.root.path().join("agentchat.sqlite3"),
        f.store.identity().clone(),
    )
    .await
    .unwrap()
}

fn result_record(request_id: Uuid, dispatch: Uuid, at: Timestamp) -> IdempotencyRecord {
    IdempotencyRecord {
        request_id,
        actor_key: "dispatch-atomicity-contract".into(),
        operation: "post".into(),
        argument_hash: "contract-body".into(),
        result: json!({"message_id": dispatch}),
        committed_at: at,
    }
}

#[tokio::test]
async fn both_kinds_allow_zero_recipients_and_only_message_has_public_content_identity() {
    let (f, author, channel) = setup().await;
    let before = inventory(&f).await;
    let mut tx = f.store.begin().await.unwrap();
    let root = tx
        .allocate_dispatch(DispatchKind::Message, Some(channel), f.clock.now())
        .await
        .unwrap();
    let payload = message(&root, author);
    tx.publish_dispatch(&root, &payload, &[]).await.unwrap();
    let control = tx
        .allocate_dispatch(DispatchKind::Notice, None, f.clock.now())
        .await
        .unwrap();
    tx.publish_dispatch(&control, &notice(&control, author), &[])
        .await
        .unwrap();
    assert!(root.creation_seq < control.creation_seq);
    tx.commit().await.unwrap();

    let reopened = reopen(&f).await;
    let mut tx = reopened.begin().await.unwrap();
    assert_eq!(tx.dispatch(root.id).await.unwrap(), Some(root.clone()));
    assert_eq!(
        tx.dispatch(control.id).await.unwrap(),
        Some(control.clone())
    );
    let stored = tx.message(root.id).await.unwrap().unwrap();
    assert_eq!(stored.id, root.id);
    assert_eq!(DispatchPayload::Message(stored), payload);
    assert!(tx.message(control.id).await.unwrap().is_none());
    assert_eq!(tx.messages(&[root.id, control.id]).await.unwrap().len(), 1);
    tx.rollback().await.unwrap();
    reopened.close().await.unwrap();
    let after = inventory(&f).await;
    assert_eq!(after[0], before[0] + 2);
    assert_eq!(after[1], before[1] + 1);
    assert_eq!(after[2], before[2] + 1);
    assert_eq!(&after[3..], &before[3..]);
}

#[tokio::test]
async fn typed_audit_publication_and_replay_never_manufacture_native_work() {
    let (f, actor, _) = setup().await;
    let before = inventory(&f).await;
    let mut tx = f.store.begin().await.unwrap();
    let mut occurrence = tx
        .allocate_event(EventKind::AgentLeft, f.clock.now())
        .await
        .unwrap();
    occurrence.actor_id = Some(actor);
    occurrence.data = json!({"agent_id": actor, "handle": "author"});
    tx.publish_event(&occurrence).await.unwrap();
    tx.commit().await.unwrap();
    let mut after = before.clone();
    after[5] += 1;
    assert_eq!(inventory(&f).await, after);

    let reopened = reopen(&f).await;
    let mut tx = reopened.begin().await.unwrap();
    assert_eq!(
        tx.event(occurrence.event_id).await.unwrap(),
        Some(occurrence.clone())
    );
    assert!(tx.publish_event(&occurrence).await.is_err());
    tx.commit().await.unwrap();
    assert_eq!(inventory(&f).await, after);
    let mut tx = reopened.begin().await.unwrap();
    let header = tx
        .allocate_dispatch(DispatchKind::Notice, None, f.clock.now())
        .await
        .unwrap();
    let delivery = recipient(&header, actor);
    tx.publish_dispatch(
        &header,
        &notice(&header, actor),
        std::slice::from_ref(&delivery),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(f.count("deliveries").await, before[3] + 1);
    assert_eq!(f.count("delivery_state_transitions").await, before[4] + 1);
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn abandoned_header_rolls_back_valid_sibling_payload_and_success_receipt() {
    let (f, author, channel) = setup().await;
    let before = inventory(&f).await;
    let request = Uuid::new_v4();
    let mut tx = f.store.begin().await.unwrap();
    let valid = tx
        .allocate_dispatch(DispatchKind::Message, Some(channel), f.clock.now())
        .await
        .unwrap();
    tx.publish_dispatch(&valid, &message(&valid, author), &[])
        .await
        .unwrap();
    tx.save_idempotency(&result_record(request, valid.id, f.clock.now()))
        .await
        .unwrap();
    let abandoned = tx
        .allocate_dispatch(DispatchKind::Notice, None, f.clock.now())
        .await
        .unwrap();
    assert!(tx.commit().await.is_err());

    let reopened = reopen(&f).await;
    let mut tx = reopened.begin().await.unwrap();
    for id in [valid.id, abandoned.id] {
        assert!(tx.dispatch(id).await.unwrap().is_none());
        assert!(tx.message(id).await.unwrap().is_none());
    }
    assert!(tx.idempotency(request).await.unwrap().is_none());
    tx.rollback().await.unwrap();
    reopened.close().await.unwrap();
    assert_eq!(inventory(&f).await, before);
}

#[tokio::test]
async fn mismatched_payload_and_recipient_ownership_cannot_be_committed_after_error() {
    let (f, author, channel) = setup().await;
    let before = inventory(&f).await;
    for failure in [
        "kind",
        "message_id",
        "recipient_parent",
        "recipient_duplicate",
    ] {
        let mut tx = f.store.begin().await.unwrap();
        let header = tx
            .allocate_dispatch(DispatchKind::Message, Some(channel), f.clock.now())
            .await
            .unwrap();
        let mut payload = message(&header, author);
        let mut deliveries = vec![recipient(&header, author)];
        match failure {
            "kind" => payload = notice(&header, author),
            "message_id" => {
                let DispatchPayload::Message(value) = &mut payload else {
                    unreachable!()
                };
                value.id = Uuid::new_v4();
            }
            "recipient_parent" => deliveries[0].dispatch_id = Uuid::new_v4(),
            "recipient_duplicate" => deliveries.push(recipient(&header, author)),
            _ => unreachable!(),
        }
        assert!(
            tx.publish_dispatch(&header, &payload, &deliveries)
                .await
                .is_err(),
            "{failure}"
        );
        assert!(tx.commit().await.is_err(), "{failure}");
        let reopened = reopen(&f).await;
        let mut tx = reopened.begin().await.unwrap();
        assert!(tx.dispatch(header.id).await.unwrap().is_none(), "{failure}");
        for delivery in deliveries {
            assert!(
                tx.delivery(delivery.id).await.unwrap().is_none(),
                "{failure}"
            );
        }
        tx.rollback().await.unwrap();
        reopened.close().await.unwrap();
        assert_eq!(inventory(&f).await, before, "{failure}");
    }
}

async fn fanout_actors(f: &Fixture) -> Vec<Uuid> {
    let mut actors = Vec::new();
    for index in 0..17 {
        actors.push(f.join(&f.context(), &format!("recipient-{index}")).await);
    }
    actors
}

#[tokio::test]
async fn later_fanout_fk_failure_then_ignored_error_cannot_commit_partial_aggregate() {
    let (f, author, channel) = setup().await;
    let actors = fanout_actors(&f).await;
    let before = inventory(&f).await;
    let request = Uuid::new_v4();
    let mut tx = f.store.begin().await.unwrap();
    let header = tx
        .allocate_dispatch(DispatchKind::Message, Some(channel), f.clock.now())
        .await
        .unwrap();
    let mut deliveries = actors
        .iter()
        .map(|id| recipient(&header, *id))
        .collect::<Vec<_>>();
    // Recipient 17 fails after the first sixteen recipients and initial rows
    // have crossed the implementation's insert-chunk boundary.
    deliveries[16].recipient_id = Uuid::new_v4();
    assert!(
        tx.publish_dispatch(&header, &message(&header, author), &deliveries)
            .await
            .is_err()
    );
    tx.save_idempotency(&result_record(request, header.id, f.clock.now()))
        .await
        .unwrap();
    assert!(tx.commit().await.is_err());
    let reopened = reopen(&f).await;
    let mut tx = reopened.begin().await.unwrap();
    assert!(tx.dispatch(header.id).await.unwrap().is_none());
    assert!(tx.message(header.id).await.unwrap().is_none());
    assert!(tx.idempotency(request).await.unwrap().is_none());
    for value in deliveries {
        assert!(tx.delivery(value.id).await.unwrap().is_none());
    }
    tx.rollback().await.unwrap();
    reopened.close().await.unwrap();
    assert_eq!(inventory(&f).await, before);
}

#[tokio::test]
async fn failed_fanout_can_retry_same_header_without_leftover_subtype_or_initial_history() {
    let (f, author, channel) = setup().await;
    let actors = fanout_actors(&f).await;
    let before = inventory(&f).await;
    let mut tx = f.store.begin().await.unwrap();
    let header = tx
        .allocate_dispatch(DispatchKind::Message, Some(channel), f.clock.now())
        .await
        .unwrap();
    let payload = message(&header, author);
    let deliveries = actors
        .iter()
        .map(|id| recipient(&header, *id))
        .collect::<Vec<_>>();
    let mut invalid = deliveries.clone();
    invalid[16].recipient_id = Uuid::new_v4();
    assert!(
        tx.publish_dispatch(&header, &payload, &invalid)
            .await
            .is_err()
    );
    tx.publish_dispatch(&header, &payload, &deliveries)
        .await
        .unwrap();
    assert!(
        tx.publish_dispatch(&header, &payload, &deliveries)
            .await
            .is_err()
    );
    tx.commit().await.unwrap();
    let reopened = reopen(&f).await;
    let mut tx = reopened.begin().await.unwrap();
    assert_eq!(tx.dispatch(header.id).await.unwrap(), Some(header));
    for value in deliveries {
        assert_eq!(tx.delivery(value.id).await.unwrap(), Some(value));
    }
    tx.rollback().await.unwrap();
    reopened.integrity_check().await.unwrap();
    reopened.close().await.unwrap();
    let after = inventory(&f).await;
    assert_eq!(after[0], before[0] + 1);
    assert_eq!(after[1], before[1] + 1);
    assert_eq!(after[2], before[2]);
    assert_eq!(after[3], before[3] + 17);
    assert_eq!(after[4], before[4] + 17);
    assert_eq!(&after[5..], &before[5..]);
}

#[tokio::test]
async fn post_request_replay_preserves_public_message_id_header_and_initial_history() {
    let f = Fixture::new().await;
    let sender = f.context();
    f.join(&sender, "sender").await;
    let receiver = f.context();
    f.native_join(&receiver, "receiver").await;
    let request = Uuid::new_v4();
    let command = Mutation::Post {
        channel: Some("dev".into()),
        body: "one request".into(),
    };
    let original = f
        .app
        .execute(request, &sender, command.clone())
        .await
        .unwrap();
    let message_id = id(&original.result, "message_id");
    let before = inventory(&f).await;
    let history = history_snapshot(&f).await;
    f.clock.advance(30);
    let replay = f.app.execute(request, &sender, command).await.unwrap();
    assert_eq!(replay.result, original.result);
    assert!(replay.event_head.is_none());
    assert_eq!(inventory(&f).await, before);
    assert_eq!(history_snapshot(&f).await, history);
    let reopened = reopen(&f).await;
    let mut tx = reopened.begin().await.unwrap();
    let header = tx.dispatch(message_id).await.unwrap().unwrap();
    assert_eq!(header.kind, DispatchKind::Message);
    assert_eq!(tx.message(message_id).await.unwrap().unwrap().id, header.id);
    assert_eq!(
        tx.idempotency(request).await.unwrap().unwrap().result,
        original.result
    );
    tx.rollback().await.unwrap();
    reopened.close().await.unwrap();
}

async fn insert_delivery(db: &DatabaseConnection, delivery: &Delivery) -> Result<(), DbErr> {
    db.execute(
        &Query::insert()
            .into_table(Alias::new("deliveries"))
            .columns(
                [
                    "id",
                    "dispatch_id",
                    "recipient_id",
                    "state",
                    "state_version",
                    "wake_eligible",
                    "wake_batch_id",
                    "attempts",
                    "queued_at",
                ]
                .map(Alias::new),
            )
            .values_panic([
                delivery.id.to_string().into(),
                delivery.dispatch_id.to_string().into(),
                delivery.recipient_id.to_string().into(),
                "pending".into(),
                1_i64.into(),
                false.into(),
                Option::<String>::None.into(),
                0.into(),
                delivery.history.queued_at.to_string().into(),
            ])
            .to_owned(),
    )
    .await
    .map(|_| ())
}

#[tokio::test]
async fn required_parent_fks_and_recipient_uniqueness_are_database_constraints() {
    let (f, author, channel) = setup().await;
    let mut tx = f.store.begin().await.unwrap();
    let header = tx
        .allocate_dispatch(DispatchKind::Message, Some(channel), f.clock.now())
        .await
        .unwrap();
    let delivery = recipient(&header, author);
    tx.publish_dispatch(
        &header,
        &message(&header, author),
        std::slice::from_ref(&delivery),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let before = inventory(&f).await;
    let mut duplicate = delivery.clone();
    duplicate.id = Uuid::new_v4();
    assert!(insert_delivery(&f.inspect, &duplicate).await.is_err());
    duplicate.dispatch_id = Uuid::new_v4();
    assert!(insert_delivery(&f.inspect, &duplicate).await.is_err());
    assert!(
        f.inspect
            .execute(
                &Query::insert()
                    .into_table(Alias::new("dispatch_notices"))
                    .columns(["dispatch_id", "kind", "data"].map(Alias::new))
                    .values_panic([
                        Uuid::new_v4().to_string().into(),
                        "agent.joined".into(),
                        json!({"agent_id":author,"handle":"author"}).into()
                    ])
                    .to_owned()
            )
            .await
            .is_err()
    );
    assert!(
        f.inspect
            .execute(
                &Query::update()
                    .table(Alias::new("deliveries"))
                    .value(Alias::new("dispatch_id"), Option::<String>::None)
                    .and_where(Expr::col(Alias::new("id")).eq(delivery.id.to_string()))
                    .to_owned()
            )
            .await
            .is_err()
    );
    assert_eq!(inventory(&f).await, before);
    f.store.integrity_check().await.unwrap();
}

#[tokio::test]
async fn notice_is_not_a_reply_root_and_cannot_gain_a_second_subtype() {
    let (f, author, channel) = setup().await;
    let mut tx = f.store.begin().await.unwrap();
    let control = tx
        .allocate_dispatch(DispatchKind::Notice, None, f.clock.now())
        .await
        .unwrap();
    tx.publish_dispatch(&control, &notice(&control, author), &[])
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let before = inventory(&f).await;
    let mut tx = f.store.begin().await.unwrap();
    let reply = tx
        .allocate_dispatch(DispatchKind::Message, Some(channel), f.clock.now())
        .await
        .unwrap();
    let mut payload = message(&reply, author);
    let DispatchPayload::Message(value) = &mut payload else {
        unreachable!()
    };
    value.root_message_id = Some(control.id);
    assert!(tx.publish_dispatch(&reply, &payload, &[]).await.is_err());
    assert!(tx.commit().await.is_err());
    let mut tx = f.store.begin().await.unwrap();
    assert!(
        tx.publish_dispatch(
            &control,
            &message(
                &Dispatch {
                    channel_id: Some(channel),
                    ..control.clone()
                },
                author
            ),
            &[]
        )
        .await
        .is_err()
    );
    tx.commit().await.unwrap();
    assert_eq!(inventory(&f).await, before);
}

#[tokio::test]
async fn missing_or_second_persisted_subtype_is_refused_as_an_incomplete_aggregate() {
    for corruption in ["missing_message", "second_notice"] {
        let (f, author, channel) = setup().await;
        let mut tx = f.store.begin().await.unwrap();
        let header = tx
            .allocate_dispatch(DispatchKind::Message, Some(channel), f.clock.now())
            .await
            .unwrap();
        tx.publish_dispatch(&header, &message(&header, author), &[])
            .await
            .unwrap();
        tx.commit().await.unwrap();
        if corruption == "missing_message" {
            let deleted = f
                .inspect
                .execute(
                    &Query::delete()
                        .from_table(Alias::new("dispatch_messages"))
                        .and_where(Expr::col(Alias::new("dispatch_id")).eq(header.id.to_string()))
                        .to_owned(),
                )
                .await
                .unwrap();
            assert_eq!(deleted.rows_affected(), 1);
        } else {
            f.inspect
                .execute(
                    &Query::insert()
                        .into_table(Alias::new("dispatch_notices"))
                        .columns(["dispatch_id", "kind", "data"].map(Alias::new))
                        .values_panic([
                            header.id.to_string().into(),
                            "agent.joined".into(),
                            json!({"agent_id":author,"handle":"author"}).into(),
                        ])
                        .to_owned(),
                )
                .await
                .unwrap();
        }
        // Raw corruption is confined to this disposable fixture. The ordinary
        // Store may not expose or certify the resulting incomplete aggregate.
        let reopened = reopen(&f).await;
        let mut tx = reopened.begin().await.unwrap();
        assert!(tx.dispatch(header.id).await.is_err(), "{corruption}");
        assert!(tx.commit().await.is_err(), "{corruption}");
        reopened.close().await.unwrap();
    }
}
