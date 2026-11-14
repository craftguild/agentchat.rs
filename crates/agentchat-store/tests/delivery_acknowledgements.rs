//! ACK-free delivery storage retains bounded reads and independent explicit reads.
mod support;
use agentchat_core::{
    application::{Mutation, MutationRequest},
    model::DeliveryState,
    ports::Storage,
    time::Clock,
};
use support::{Fixture, id};
use uuid::Uuid;

async fn posts(f: &Fixture, context: &agentchat_core::model::Context, count: usize) -> Vec<Uuid> {
    let mut ids = Vec::new();
    for start in (0..count).step_by(8) {
        let requests = (start..(start + 8).min(count))
            .map(|i| MutationRequest {
                request_id: Uuid::new_v4(),
                context: context.clone(),
                command: Mutation::Post {
                    channel: Some("dev".into()),
                    body: format!("@bob retained body {i}"),
                },
            })
            .collect();
        for result in f.app.execute_group(requests).await.unwrap() {
            ids.push(id(&result.unwrap().result, "message_id"));
        }
    }
    ids
}

#[tokio::test]
async fn two_hundred_explicit_reads_preserve_original_pending_deliveries_and_retry_identity() {
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    f.join(&a, "alice").await;
    let bob = f.join(&b, "bob").await;
    let messages = posts(&f, &a, 200).await;
    let mut ids = Vec::new();
    for message in &messages {
        ids.push(f.delivery(*message, bob).await);
    }
    let mut tx = f.store.begin().await.unwrap();
    let before = tx.deliveries(&ids).await.unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(before.len(), 200);
    assert!(
        before
            .iter()
            .all(|d| d.state == DeliveryState::Pending && d.history.deadline_at.is_none())
    );
    let key = Uuid::new_v4();
    let command = Mutation::Read {
        message_ids: messages,
    };
    let first = f.app.execute(key, &b, command.clone()).await.unwrap();
    assert_eq!(
        f.app.execute(key, &b, command).await.unwrap().result,
        first.result
    );
    let mut tx = f.store.begin().await.unwrap();
    assert_eq!(tx.deliveries(&ids).await.unwrap(), before);
    tx.rollback().await.unwrap();
    assert_eq!(f.count("read_receipts").await, 200);
    for operation in ["ack", "delivery_ack", "acknowledge"] {
        assert!(
            serde_json::from_value::<Mutation>(
                serde_json::json!({"operation":operation,"delivery_ids":ids})
            )
            .is_err()
        );
    }
}

#[tokio::test]
async fn invalid_lifecycle_write_rolls_back_a_provisional_valid_prefix_without_fabricated_receipt()
{
    let f = Fixture::new().await;
    let a = f.context();
    let b = f.context();
    f.join(&a, "alice").await;
    let bob = f.join(&b, "bob").await;
    let messages = posts(&f, &a, 2).await;
    let first = f.delivery(messages[0], bob).await;
    let second = f.delivery(messages[1], bob).await;
    let mut tx = f.store.begin().await.unwrap();
    let mut valid = tx.delivery(first).await.unwrap().unwrap();
    valid.start_processing(f.clock.now(), 10).unwrap();
    tx.save_delivery(&valid).await.unwrap();
    let mut invalid = tx.delivery(second).await.unwrap().unwrap();
    invalid.state = DeliveryState::Delivered;
    assert!(tx.save_delivery(&invalid).await.is_err());
    tx.rollback().await.unwrap();
    let mut tx = f.store.begin().await.unwrap();
    assert_eq!(
        tx.delivery(first).await.unwrap().unwrap().state,
        DeliveryState::Pending
    );
    assert_eq!(
        tx.delivery(second).await.unwrap().unwrap().state,
        DeliveryState::Pending
    );
    tx.rollback().await.unwrap();
    assert_eq!(f.count("read_receipts").await, 0);
}

#[tokio::test]
async fn storage_delivery_set_queries_keep_the_existing_distinct_two_hundred_bound() {
    let f = Fixture::new().await;
    let mut tx = f.store.begin().await.unwrap();
    assert!(tx.deliveries(&[]).await.unwrap().is_empty());
    let missing = Uuid::new_v4();
    assert!(tx.deliveries(&[missing]).await.unwrap().is_empty());
    assert!(tx.deliveries(&[missing, missing]).await.is_err());
    assert!(
        tx.deliveries(&(0..201).map(|_| Uuid::new_v4()).collect::<Vec<_>>())
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
}
