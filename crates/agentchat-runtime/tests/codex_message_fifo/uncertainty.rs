//! Ordinary scheduler errors/timeouts retain the original unknown request.
use super::*;

async fn stopped_after_unknown(explicit_error: bool) {
    let (running, host, mut receiver) = fixture().await;
    let (mut sender, sender_context, _) = start(&running, &host, "sender").await;
    let (mut recipient, recipient_context, _) = start(&running, &host, "receiver").await;
    post(
        &mut sender,
        &sender_context,
        "@receiver unresolved original",
    )
    .await;
    post(&mut sender, &sender_context, "passive successor").await;
    let original = receive(&mut receiver).await;
    let first = original.dispatch.clone();
    let result = if explicit_error {
        original
            .result
            .send(Err(Error::new(
                ErrorCode::RuntimeUnavailable,
                "Synthetic lost native reply.",
            )))
            .unwrap();
        None
    } else {
        // Keep the actual port future pending beyond the production submission
        // budget. A timeout drops it; no synthetic negative receipt is supplied.
        Some(original.result)
    };
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let state = support::runtime::whoami(&mut recipient).await;
            if state["native_delivery"]["state"] == "acceptance_unknown" {
                assert_eq!(
                    state["native_delivery"]["head"]["seq"],
                    serde_json::to_value(first.input.seq).unwrap()
                );
                assert_eq!(state["native_delivery"]["head"]["attempts"], 1);
                assert!(state["statistics"].get("acknowledged_deliveries").is_none());
                assert_eq!(state["statistics"]["read_receipts"], 0);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    if let Some(result) = result {
        assert!(
            result.is_closed(),
            "timed-out port future must be cancelled"
        );
    }
    running.clock.advance(6);
    success(
        recipient
            .call(
                Uuid::new_v4(),
                Call::Heartbeat {
                    context_id: recipient_context.context_id,
                },
            )
            .await
            .unwrap(),
    )
    .unwrap();
    post(
        &mut sender,
        &sender_context,
        "@receiver sender chose to repost",
    )
    .await;
    assert!(
        tokio::time::timeout(Duration::from_millis(200), receiver.recv())
            .await
            .is_err()
    );
    let state = support::runtime::whoami(&mut recipient).await;
    let head = &state["native_delivery"]["head"];
    assert_eq!(head["seq"], serde_json::to_value(first.input.seq).unwrap());
    assert_eq!(head["attempts"], 1);
    assert_eq!(head["submission_pending"], true);
    assert!(
        state["native_delivery"]["settled_seq"] != serde_json::to_value(first.input.seq).unwrap()
    );
    running.stop().await;
}

#[tokio::test]
async fn ambiguous_result_does_not_retry_or_release_successor_after_the_retry_deadline() {
    stopped_after_unknown(true).await;
}

#[tokio::test]
async fn real_submission_timeout_does_not_retry_or_release_a_later_sender_repost() {
    stopped_after_unknown(false).await;
}
