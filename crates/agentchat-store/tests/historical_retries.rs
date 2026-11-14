//! Removed ACK spellings cannot replay old successes as new-format operations.
mod support;
use agentchat_core::{
    application::Mutation,
    ports::{IdempotencyRecord, Storage},
    time::Clock,
};
use sha2::{Digest, Sha256};
use support::{Fixture, id};
use uuid::Uuid;

#[tokio::test]
async fn legacy_ack_success_is_not_reinterpreted_as_a_new_operation_or_actor() {
    let f = Fixture::new().await;
    let sender = f.context();
    let receiver = f.context();
    f.join(&sender, "sender").await;
    let actor = f.join(&receiver, "receiver").await;
    let posted = f.post(&sender, "retained original").await;
    let message = id(&posted.result, "message_id");
    let delivery = f.delivery(message, actor).await;
    let completed = serde_json::json!({"acknowledged":[delivery]});
    let request = Uuid::new_v4();
    // A retained incompatible success is not a compatibility adapter: its
    // original operation and argument bytes cannot become a new Read request.
    let original = format!(r#"{{"operation":"delivery_ack","delivery_ids":["{delivery}"]}}"#);
    let hash = Sha256::digest(original.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let mut tx = f.store.begin().await.unwrap();
    tx.save_idempotency(&IdempotencyRecord {
        request_id: request,
        actor_key: serde_json::to_string(&(receiver.host, &receiver.host_session_id)).unwrap(),
        operation: "delivery_ack".into(),
        argument_hash: hash,
        result: completed.clone(),
        committed_at: f.clock.now(),
    })
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let before = f.counts().await;
    f.clock.advance(2);
    for context in [&receiver, &sender] {
        assert_eq!(
            f.app
                .execute(
                    request,
                    context,
                    Mutation::Read {
                        message_ids: vec![message]
                    }
                )
                .await
                .unwrap_err()
                .code,
            agentchat_core::ErrorCode::IdempotencyConflict
        );
        assert_eq!(f.counts().await, before);
    }
    let mut tx = f.store.begin().await.unwrap();
    let retained = tx.idempotency(request).await.unwrap().unwrap();
    assert_eq!(retained.operation, "delivery_ack");
    assert_eq!(retained.result, completed);
    tx.rollback().await.unwrap();
    for operation in ["ack", "delivery_ack"] {
        assert!(
            serde_json::from_value::<Mutation>(serde_json::json!({
                "operation":operation,"delivery_ids":[delivery]
            }))
            .is_err()
        );
    }
}
