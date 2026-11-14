//! Actual HTTP projection of canonical delivery facts; reads remain independent.
mod support;

use agentchat_core::{
    model::{ActorKind, DeliveryHistory, DeliveryState, ReadKind},
    read::{IdentityLabel, ReceiptDelivery, ReceiptRead, RecipientReceipt},
};
use serde_json::json;
use support::{reader::Fixture, server::Server, wire::Http};
use uuid::Uuid;

#[tokio::test]
async fn all_six_states_preserve_timestamp_nullability_and_historical_reads() {
    use DeliveryState::*;
    let fixture = Fixture::new();
    fixture.append("canonical immutable body".into());
    let message_id = fixture.rows.lock().unwrap()[0].1.message.message.id;
    let queued = fixture.rows.lock().unwrap()[0].1.message.message.created_at;
    let start = queued.after_seconds(3600).unwrap();
    let deadline = start.after_seconds(3600).unwrap();
    let expired = deadline.after_seconds(1).unwrap();
    let states = [
        Pending,
        Processing,
        Delivered,
        FailedTimeout,
        FailedLeft,
        DeliveredLate,
    ];
    let names = [
        "PENDING",
        "PROCESSING",
        "DELIVERED",
        "FAILED_TIMEOUT",
        "FAILED_LEFT",
        "DELIVERED_LATE",
    ];
    let mut histories = Vec::new();
    for (index, state) in states.into_iter().enumerate() {
        let mut history = DeliveryHistory::queued(queued);
        if !matches!(state, Pending | FailedLeft) {
            history.processing_started_at = Some(start);
            history.deadline_at = Some(deadline);
        }
        if matches!(state, FailedTimeout | DeliveredLate) {
            history.timed_out_at = Some(expired);
        }
        if state == Delivered {
            history.delivered_at = Some(deadline);
        }
        if state == DeliveredLate {
            history.delivered_at = Some(expired);
        }
        if state == FailedLeft {
            history.left_at = Some(start);
        }
        history.validate(state).unwrap();
        histories.push(history.clone());
        fixture.receipt_rows.lock().unwrap().push(RecipientReceipt {
            recipient: IdentityLabel {
                actor_id: Uuid::from_u128(100 + index as u128),
                kind: ActorKind::Agent,
                handle: format!("recipient-{index}"),
            },
            delivery: Some(ReceiptDelivery {
                delivery_id: Uuid::from_u128(200 + index as u128),
                state,
                history,
            }),
            // Explicit reading of a pending obligation never fabricates delivery.
            reads: vec![ReceiptRead {
                kind: ReadKind::Explicit,
                at: queued,
            }],
        });
    }
    fixture.receipt_rows.lock().unwrap().push(RecipientReceipt {
        recipient: IdentityLabel {
            actor_id: Uuid::from_u128(9),
            kind: ActorKind::User,
            handle: "user".into(),
        },
        delivery: None,
        reads: vec![ReceiptRead {
            kind: ReadKind::Explicit,
            at: queued,
        }],
    });
    let server = Server::open(fixture.clone(), Some(chigiri::ShutdownOptions::graceful())).await;
    let query = r#"query($id:ID!){message(messageId:$id,limit:200){message{id body createdAt}
      receipts{items{recipient{id kind} delivery{deliveryId state queuedAt processingStartedAt deadlineAt timedOutAt deliveredAt leftAt} reads{kind at}}nextCursor scanCursor head}}}"#;
    let result = Http::open(server.address, query, json!({"id":message_id}), false)
        .await
        .json()
        .await;
    assert!(result.get("errors").is_none(), "{result}");
    assert_eq!(
        result["data"]["message"]["message"]["body"],
        "canonical immutable body"
    );
    let rows = result["data"]["message"]["receipts"]["items"]
        .as_array()
        .unwrap();
    assert_eq!(rows.len(), 7);
    for (index, history) in histories.into_iter().enumerate() {
        let delivery = &rows[index]["delivery"];
        assert_eq!(delivery["state"], names[index]);
        assert_eq!(delivery["queuedAt"], history.queued_at.to_string());
        for (field, value) in [
            ("processingStartedAt", history.processing_started_at),
            ("deadlineAt", history.deadline_at),
            ("timedOutAt", history.timed_out_at),
            ("deliveredAt", history.delivered_at),
            ("leftAt", history.left_at),
        ] {
            assert_eq!(
                delivery[field],
                json!(value.map(|at| at.to_string())),
                "{field}"
            );
        }
        assert_eq!(rows[index]["reads"][0]["kind"], "EXPLICIT");
    }
    assert!(rows[6]["delivery"].is_null());
    assert_eq!(rows[6]["recipient"]["kind"], "USER");
    for field in ["acceptedAt", "acknowledgedAt", "confirmation { state }"] {
        let query = format!(
            "query($id:ID!){{message(messageId:$id){{receipts{{items{{delivery{{{field}}}}}}}}}}}"
        );
        let rejected = Http::open(server.address, &query, json!({"id":message_id}), false)
            .await
            .json_status(axum::http::StatusCode::UNPROCESSABLE_ENTITY)
            .await;
        assert!(
            rejected.get("errors").is_some(),
            "obsolete field accepted: {rejected}"
        );
    }
    assert_eq!(fixture.starts.load(std::sync::atomic::Ordering::SeqCst), 0);
    server.finish(true).await;
}
