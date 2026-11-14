//! Correlated clocks observe the real coordinator/port commit, never model work.
use super::*;

#[tokio::test]
async fn commit_dispatch_native_result_and_settlement_share_public_ids_and_local_clock() {
    let (running, host, mut receiver) = fixture().await;
    let (mut sender, sender_context, _) = start(&running, &host, "trace-sender").await;
    let (mut recipient, _, _) = start(&running, &host, "trace-receiver").await;
    let request = Uuid::new_v4();
    let body = "@trace-receiver PRIVATE_PIPELINE_BODY_MUST_NOT_BE_LOGGED";
    let posted = success(
        sender
            .call(
                Uuid::new_v4(),
                Call::Mutate {
                    request_id: request,
                    actor: agentchat_protocol::rpc::ActorReference::Native {
                        context_id: sender_context.context_id,
                    },
                    command: Mutation::Post {
                        channel: Some("dev".into()),
                        body: body.into(),
                    },
                },
            )
            .await
            .unwrap(),
    )
    .unwrap();
    let pending = receive(&mut receiver).await;
    let delivery = pending.dispatch.input.delivery_id;
    let message_id = pending.dispatch.input.message_id;
    assert_eq!(
        message_id.to_string(),
        posted["message_id"].as_str().unwrap()
    );
    let dispatch_seq = pending.dispatch.input.seq;
    let audit_seq: agentchat_core::sequence::Sequence =
        serde_json::from_value(posted["seq"].clone()).unwrap();
    assert!(dispatch_seq < audit_seq);
    let dispatch_cursor = serde_json::to_value(dispatch_seq).unwrap();
    // A held native operation has no application ACK; its own response is the
    // only release, independent of model processing, explicit read or reply.
    tokio::time::sleep(Duration::from_millis(40)).await;
    assert_eq!(
        support::runtime::health(&mut recipient).await["statistics"]["read_receipts"],
        0
    );
    accept(pending);
    let status = cursor(&mut recipient, &dispatch_cursor).await;
    assert!(
        status["statistics"]
            .get("acknowledged_deliveries")
            .is_none()
    );
    assert_eq!(status["statistics"]["read_receipts"], 1);
    support::runtime::assert_automatic_receipts(&mut recipient, message_id, 1).await;
    let path = running.project.data_dir().join("logs/runtime.jsonl");
    // Running owns the temporary project and removes it on stop. Observe the
    // bounded writer while that owner is alive, without producing more RPCs.
    let mut text = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let text = std::fs::read_to_string(&path).unwrap();
            if text
                .lines()
                .filter_map(|s| serde_json::from_str::<Value>(s).ok())
                .any(|r| {
                    r["event"]["delivery_id"] == delivery.to_string()
                        && r["event"]["phase"] == "acceptance_committed"
                })
            {
                break text;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    running.stop().await;
    let complete = text.rfind('\n').map_or(0, |i| i + 1);
    text.truncate(complete);
    assert!(!text.contains(body));
    let rows = text
        .lines()
        .map(|s| serde_json::from_str::<Value>(s).unwrap())
        .collect::<Vec<_>>();
    let commit = rows
        .iter()
        .find(|r| {
            r["event"]["kind"] == "mutation_committed"
                && r["event"]["request_id"] == request.to_string()
        })
        .unwrap();
    assert_eq!(commit["event"]["seq"], posted["seq"]);
    assert!(rows.iter().any(|r| r["event"]["kind"] == "rpc_timing"
        && r["event"]["request_id"] == request.to_string()
        && r["event"]["queue_us"].is_u64()
        && r["event"]["execution_us"].is_u64()));
    let mut last = commit["runtime_elapsed_us"].as_u64().unwrap();
    for phase in ["dispatch_started", "native_result", "acceptance_committed"] {
        let row = rows
            .iter()
            .find(|r| {
                r["event"]["kind"] == "native_input"
                    && r["event"]["delivery_id"] == delivery.to_string()
                    && r["event"]["phase"] == phase
            })
            .unwrap();
        assert_eq!(row["instance_id"], commit["instance_id"]);
        assert_eq!(row["event"]["seq"], dispatch_cursor);
        assert_eq!(row["event"]["receipt_only"], false);
        let observed = row["runtime_elapsed_us"].as_u64().unwrap();
        assert!(observed >= last);
        last = observed;
        if phase == "native_result" {
            assert_eq!(row["event"]["outcome"], "accepted");
            assert!(row["event"]["duration_us"].as_u64().unwrap() >= 20_000);
        }
    }
}
