//! Explicit first reads remain atomic, scoped and independent from delivery history.
mod support;
use agentchat_core::{
    ErrorCode,
    application::{Mutation, MutationRequest},
    delivery::{HostReceipt, MessageProgress},
    model::{Context, DeliveryState as State, Receipt},
    ports::Storage,
    time::Clock,
};
use sea_orm::{
    ConnectionTrait,
    sea_query::{Alias, Expr, ExprTrait, Query},
};
use serde_json::json;
use std::collections::BTreeMap;
use support::{Fixture, id, native::Gate};
use uuid::Uuid;

async fn messages(f: &Fixture, author: &Context, count: usize) -> Vec<Uuid> {
    let mut ids = Vec::new();
    for i in 0..count {
        ids.push(id(
            &f.post(author, &format!("complete body {i}")).await.result,
            "message_id",
        ));
    }
    ids.sort_unstable();
    ids
}
fn read(ids: &[Uuid]) -> Mutation {
    Mutation::Read {
        message_ids: ids.to_vec(),
    }
}

#[tokio::test]
async fn two_hundred_reads_preserve_normalized_results_first_times_retries_and_agent_scope() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    let c = f.context();
    f.join(&a, "alice").await;
    let bob = f.join(&b, "bob").await;
    let cara = f.join(&c, "cara").await;
    let ids = messages(&f, &a, 200).await;
    f.run(&b, read(&ids[..10])).await;
    f.run(&c, read(&ids[10..11])).await;
    let first = f.clock.now();
    f.clock.advance(2);
    let second = f.clock.now();
    let events = f.count("events").await;
    let key = Uuid::new_v4();
    let mut reversed = ids.clone();
    reversed.reverse();
    let result = f.app.execute(key, &b, read(&reversed)).await.unwrap();
    assert_eq!(
        result.result,
        json!({"changed":ids[10..],"already_read":ids[..10]})
    );
    assert_eq!(f.count("events").await, events);
    let mut tx = f.store.begin().await.unwrap();
    let rows = tx.receipts(bob, &ids).await.unwrap();
    assert_eq!(rows.len(), 200);
    for r in rows {
        assert_eq!(
            r.first_read_at,
            if ids[..10].contains(&r.message_id) {
                first
            } else {
                second
            }
        );
    }
    assert_eq!(tx.receipts(cara, &ids).await.unwrap().len(), 1);
    tx.rollback().await.unwrap();
    assert_eq!(f.count("read_receipts").await, 201);
    assert!(
        f.run(&b, read(&[ids[0], ids[1], ids[1]]))
            .await
            .event_head
            .is_none()
    );
    assert_eq!(f.count("events").await, events);
    assert!(
        f.rows("deliveries")
            .await
            .iter()
            .all(|r| r.try_get::<String>("", "state").unwrap() == "pending"
                && r.try_get::<i32>("", "attempts").unwrap() == 0)
    );
    assert_eq!(f.count("wake_batches").await, 0);
    assert_eq!(
        f.app.execute(key, &c, read(&ids)).await.unwrap_err().code,
        ErrorCode::IdempotencyConflict
    );
    f.run(&b, Mutation::Leave).await;
    let retained = f.counts().await;
    let mut resume = b.clone();
    resume.context_id = Uuid::new_v4();
    resume.incarnation = Uuid::new_v4();
    let replay = f.app.execute(key, &resume, read(&ids)).await.unwrap();
    assert_eq!(replay.result, result.result);
    assert!(replay.event_head.is_none());
    assert_eq!(f.counts().await, retained);
    f.store.integrity_check().await.unwrap();
}

#[tokio::test]
async fn explicit_reads_preserve_processing_deadlines_and_all_terminal_history() {
    let f = Fixture::new().await;
    let a = f.context();
    f.join(&a, "alice").await;
    let b = f.context();
    let c = f.context();
    let gate = Gate::new(&f);
    gate.bind(&b);
    gate.bind(&c);
    let app = gate.app(&f);
    let bob = id(
        &app.start_launch(&b, Some("bob")).await.unwrap().result,
        "agent_id",
    );
    let cara = id(
        &app.start_launch(&c, Some("cara")).await.unwrap().result,
        "agent_id",
    );
    // Both conversations consume their setup notices before any body is queued.
    for context in [&b, &c] {
        while let Some(notice) = app.prepare_native(context).await.unwrap() {
            assert!(matches!(
                notice,
                agentchat_core::delivery::NativeDispatch::Notice(_)
            ));
            app.finish_native(
                context,
                &notice,
                Ok(MessageProgress::Accepted(HostReceipt::default())),
            )
            .await
            .unwrap();
        }
    }
    app.initialize_with_settings(
        None,
        agentchat_core::project::ReceiptSettingsUpdate {
            auto_read: Some(false),
            delivery_confirmation_timeout_seconds: Some(5),
        },
    )
    .await
    .unwrap();
    let mut ids = Vec::new();
    for i in 0..5 {
        ids.push(id(
            &f.post(&a, &format!("@bob @cara unchanged body {i}"))
                .await
                .result,
            "message_id",
        ));
    }
    app.execute(Uuid::new_v4(), &b, read(&ids[4..5]))
        .await
        .unwrap();
    let first_read = f.clock.now();
    let original = app.prepare_message(&b).await.unwrap().unwrap();
    app.finish_message_progress(&b, &original, Ok(MessageProgress::AwaitingAcceptance))
        .await
        .unwrap();
    let timely = app.prepare_message(&c).await.unwrap().unwrap();
    app.finish_message(&c, &timely, Ok(HostReceipt::default()))
        .await
        .unwrap();
    let late = app.prepare_message(&c).await.unwrap().unwrap();
    let mut delivery_ids = Vec::new();
    for message in &ids {
        for recipient in [bob, cara] {
            delivery_ids.push(f.delivery(*message, recipient).await);
        }
    }
    let mut tx = f.store.begin().await.unwrap();
    let before = tx
        .deliveries(&delivery_ids)
        .await
        .unwrap()
        .into_iter()
        .map(|delivery| (delivery.id, delivery))
        .collect::<BTreeMap<_, _>>();
    tx.rollback().await.unwrap();
    app.execute(Uuid::new_v4(), &b, read(&[ids[0], ids[2]]))
        .await
        .unwrap();
    let mut tx = f.store.begin().await.unwrap();
    for delivery in tx.deliveries(&delivery_ids).await.unwrap() {
        assert_eq!(delivery, before[&delivery.id]);
    }
    tx.rollback().await.unwrap();
    f.clock.advance(6);
    assert_eq!(
        app.maintain().await.unwrap().delivery_confirmation_timeouts,
        2
    );
    app.finish_message(&c, &late, Ok(HostReceipt::default()))
        .await
        .unwrap();
    app.prepare_message(&c).await.unwrap().unwrap();
    app.execute(Uuid::new_v4(), &c, Mutation::Leave)
        .await
        .unwrap();
    let mut tx = f.store.begin().await.unwrap();
    let before = tx
        .deliveries(&delivery_ids)
        .await
        .unwrap()
        .into_iter()
        .map(|delivery| (delivery.id, delivery))
        .collect::<BTreeMap<_, _>>();
    tx.rollback().await.unwrap();
    let states = before
        .values()
        .map(|delivery| delivery.state)
        .collect::<Vec<_>>();
    for state in [
        State::Pending,
        State::Delivered,
        State::FailedTimeout,
        State::DeliveredLate,
        State::FailedLeft,
    ] {
        assert!(states.contains(&state));
    }
    app.execute(Uuid::new_v4(), &b, read(&ids)).await.unwrap();
    let mut tx = f.store.begin().await.unwrap();
    for delivery in tx.deliveries(&delivery_ids).await.unwrap() {
        assert_eq!(delivery, before[&delivery.id]);
    }
    assert_eq!(
        tx.receipt(ids[4], bob)
            .await
            .unwrap()
            .unwrap()
            .first_read_at,
        first_read
    );
    assert_eq!(tx.receipts(bob, &ids).await.unwrap().len(), 5);
    assert!(tx.receipts(cara, &ids).await.unwrap().is_empty());
    tx.rollback().await.unwrap();
    assert_eq!(
        app.maintain().await.unwrap().delivery_confirmation_timeouts,
        0
    );
    f.store.integrity_check().await.unwrap();
}

#[tokio::test]
async fn set_ports_reject_invalid_bounds_and_conflicts_without_partial_first_reads() {
    let f = Fixture::new().await;
    let a = f.context();
    let actor = f.join(&a, "alice").await;
    let ids = messages(&f, &a, 3).await;
    let mut tx = f.store.begin().await.unwrap();
    assert!(tx.messages(&[]).await.unwrap().is_empty());
    assert!(tx.receipts(actor, &[]).await.unwrap().is_empty());
    tx.insert_receipts(&[]).await.unwrap();
    for invalid in [
        vec![ids[0], ids[0]],
        (0..201).map(|_| Uuid::new_v4()).collect(),
    ] {
        assert_eq!(
            tx.messages(&invalid).await.unwrap_err().code,
            ErrorCode::IntegrityError
        );
        assert_eq!(
            tx.receipts(actor, &invalid).await.unwrap_err().code,
            ErrorCode::IntegrityError
        );
    }
    let receipt = |message_id| Receipt {
        message_id,
        actor_id: actor,
        kind: agentchat_core::model::ReadKind::Explicit,
        first_read_at: f.clock.now(),
    };
    let r = receipt(ids[0]);
    assert_eq!(
        tx.insert_receipts(&[r.clone(), r.clone()])
            .await
            .unwrap_err()
            .code,
        ErrorCode::IntegrityError
    );
    assert_eq!(
        tx.insert_receipts(
            &(0..201)
                .map(|_| receipt(Uuid::new_v4()))
                .collect::<Vec<_>>()
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::IntegrityError
    );
    tx.insert_receipts(std::slice::from_ref(&r)).await.unwrap();
    tx.commit().await.unwrap();
    for bad in [ids[0], Uuid::new_v4()] {
        let mut tx = f.store.begin().await.unwrap();
        assert_eq!(
            tx.insert_receipts(&[receipt(ids[1]), receipt(ids[2]), receipt(bad)])
                .await
                .unwrap_err()
                .code,
            ErrorCode::StorageError
        );
        // SQLite's one-statement failure must not persist the valid prefix,
        // even if a low-level caller explicitly commits after seeing the error.
        tx.commit().await.unwrap();
        let mut tx = f.store.begin().await.unwrap();
        assert_eq!(tx.receipts(actor, &ids).await.unwrap(), vec![r.clone()]);
        tx.rollback().await.unwrap();
    }
    f.store.integrity_check().await.unwrap();
}

#[tokio::test]
async fn missing_and_oversized_inputs_preserve_the_complete_mutation_boundary() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    f.join(&a, "alice").await;
    f.join(&b, "bob").await;
    let ids = messages(&f, &a, 3).await;
    let before = f.counts().await;
    for (command, code) in [
        (read(&[ids[0], Uuid::new_v4(), ids[1]]), ErrorCode::NotFound),
        (read(&[]), ErrorCode::InvalidArgument),
        (read(&vec![ids[0]; 201]), ErrorCode::InvalidArgument),
    ] {
        assert_eq!(
            f.app
                .execute(Uuid::new_v4(), &b, command)
                .await
                .unwrap_err()
                .code,
            code
        );
        assert_eq!(f.counts().await, before);
    }
}

async fn corrupt(f: &Fixture, table: &str, column: &str, message: Uuid, value: &str) {
    let key = if table == "dispatches" {
        "id"
    } else {
        "message_id"
    };
    assert_eq!(
        f.inspect
            .execute(
                &Query::update()
                    .table(Alias::new(table))
                    .value(Alias::new(column), value)
                    .and_where(Expr::col(Alias::new(key)).eq(message.to_string()))
                    .to_owned()
            )
            .await
            .unwrap()
            .rows_affected(),
        1
    );
}

#[tokio::test]
async fn malformed_messages_or_receipts_cannot_commit_a_valid_prefix_or_new_event() {
    for table in ["dispatches", "read_receipts"] {
        let f = Fixture::new().await;
        let a = f.context();
        let b = f.context();
        f.join(&a, "alice").await;
        f.join(&b, "bob").await;
        let watched = id(
            &f.run(
                &a,
                Mutation::Post {
                    channel: Some("dev".into()),
                    body: "@bob guarded deadline".into(),
                },
            )
            .await
            .result,
            "message_id",
        );
        let other = id(&f.post(&a, "other unread body").await.result, "message_id");
        if table == "read_receipts" {
            f.run(&b, read(&[watched])).await;
        }
        let column = match table {
            "dispatches" => "created_at",
            "read_receipts" => "first_read_at",
            _ => "deadline",
        };
        let original = f.clock.now();
        corrupt(&f, table, column, watched, "not-a-timestamp").await;
        let before = f.counts().await;
        let key = Uuid::new_v4();
        assert_eq!(
            f.app
                .execute(key, &b, read(&[watched, other]))
                .await
                .unwrap_err()
                .code,
            ErrorCode::IntegrityError
        );
        assert_eq!(f.counts().await, before);
        let mut tx = f.store.begin().await.unwrap();
        assert!(tx.idempotency(key).await.unwrap().is_none());
        tx.rollback().await.unwrap();
        corrupt(&f, table, column, watched, &original.to_string()).await;
        f.app
            .execute(key, &b, read(&[watched, other]))
            .await
            .unwrap();
        f.store.integrity_check().await.unwrap();
    }
}

#[tokio::test]
async fn concurrent_reads_and_group_rollback_keep_one_first_transition_per_agent() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    let c = f.context();
    f.join(&a, "alice").await;
    let bob = f.join(&b, "bob").await;
    let cara = f.join(&c, "cara").await;
    let ids = messages(&f, &a, 20).await;
    let events = f.count("events").await;
    let (one, two) = tokio::join!(
        f.app.execute(Uuid::new_v4(), &b, read(&ids)),
        f.app.execute(Uuid::new_v4(), &b, read(&ids))
    );
    let one = one.unwrap();
    let two = two.unwrap();
    assert!(one.event_head.is_none() && two.event_head.is_none());
    assert_eq!(
        one.result["changed"].as_array().unwrap().len()
            + two.result["changed"].as_array().unwrap().len(),
        20
    );
    assert_eq!(f.count("events").await, events);
    let group = f
        .app
        .execute_group(vec![
            MutationRequest {
                request_id: Uuid::new_v4(),
                context: c.clone(),
                command: read(&ids),
            },
            MutationRequest {
                request_id: Uuid::new_v4(),
                context: c.clone(),
                command: read(&[ids[0], Uuid::new_v4()]),
            },
            MutationRequest {
                request_id: Uuid::new_v4(),
                context: b.clone(),
                command: read(&ids),
            },
        ])
        .await
        .unwrap();
    assert!(group[0].is_ok());
    assert_eq!(group[1].as_ref().unwrap_err().code, ErrorCode::NotFound);
    assert!(group[2].as_ref().unwrap().event_head.is_none());
    let mut tx = f.store.begin().await.unwrap();
    assert_eq!(tx.receipts(bob, &ids).await.unwrap().len(), 20);
    assert_eq!(tx.receipts(cara, &ids).await.unwrap().len(), 20);
    tx.rollback().await.unwrap();
    assert_eq!(f.count("events").await, events);
    f.store.integrity_check().await.unwrap();
}
