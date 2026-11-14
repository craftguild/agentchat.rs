//! Product-generated ABI8 Query/SSE against the supported managed public service.
mod support;

use agentchat_core::read::Checkpoint;
use chigiri::{ShutdownOptions, ShutdownOutcome};
use serde_json::{Value, json};
use std::{sync::atomic::Ordering, time::Duration};
use support::{reader::Fixture, server::Server, wire::Http};

const CHECKPOINT: &str = "projectId seq eventId";
const MESSAGE: &str = "id channelId channel author { id kind handle } rootId threadId creationSeq createdAt body bodyHtml renderFallback replyCount";

fn checkpoint(cp: Checkpoint) -> Value {
    json!({"projectId":cp.project_id,"seq":cp.seq,"eventId":cp.event_id})
}
fn subscription() -> String {
    format!(
        "subscription($after:CheckpointInput!) {{ changes(after:$after) {{ after {{ {CHECKPOINT} }} scan {{ {CHECKPOINT} }} head {{ {CHECKPOINT} }} hasMore items {{ eventId seq kind channel {{ id name creationSeq }} message {{ {MESSAGE} }} }} }} }}"
    )
}
async fn released(fixture: &Fixture, count: usize) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while fixture.drops.load(Ordering::SeqCst) < count {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn published_contract_fixture_uses_the_actual_generated_query_projections() {
    let fixture = Fixture::new();
    fixture.append("Hello **AgentChat**.".into());
    let server = Server::open(fixture.clone(), Some(ShutdownOptions::graceful())).await;
    let query = format!(
        "query($after:CheckpointInput!) {{ project {{ id checkpoint {{ {CHECKPOINT} }} }} channels(state:ALL) {{ items {{ id name creationSeq lastConversationAt archiveAt archived mandatory }} nextCursor scanCursor head asOf }} roots(channel:\"dev\") {{ snapshot {{ {CHECKPOINT} }} nextBefore items {{ {MESSAGE} }} }} changes(after:$after) {{ after {{ {CHECKPOINT} }} scan {{ {CHECKPOINT} }} head {{ {CHECKPOINT} }} hasMore items {{ eventId seq kind channel {{ id name creationSeq }} message {{ {MESSAGE} }} }} }} }}"
    );
    let response = Http::open(
        server.address,
        &query,
        json!({"after":checkpoint(fixture.cp(0))}),
        false,
    )
    .await
    .json()
    .await;
    assert!(response.get("errors").is_none(), "{response}");
    assert_eq!(
        response["data"]["roots"]["items"][0]["body"],
        "Hello **AgentChat**."
    );
    assert_eq!(response["data"]["project"]["checkpoint"]["seq"], "3");
    assert_eq!(
        response["data"]["changes"]["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let expected: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../web/fixtures/contract-v1.json"
    )))
    .unwrap();
    assert_eq!(response["data"], expected);
    assert_eq!(server.finish(true).await, Some(ShutdownOutcome::Drained));
}

#[tokio::test]
async fn actual_managed_query_and_sse_preserve_maximum_bodies_and_drain_render_continuations() {
    let fixture = Fixture::new();
    for index in 0..65 {
        fixture.append(format!("record {index}"));
    }
    let originals = [
        "\0".repeat(65_536),
        "💜".repeat(16_384),
        "&".repeat(65_536),
        "# <&>\n".repeat(9000),
    ];
    for body in &originals {
        fixture.append(body.clone());
    }
    let server = Server::open(
        fixture.clone(),
        Some(ShutdownOptions::graceful_for(Duration::from_secs(2)).unwrap()),
    )
    .await;
    let query = format!(
        "query($before:String,$snapshot:CheckpointInput) {{ roots(channel:\"dev\",before:$before,snapshot:$snapshot) {{ snapshot {{ {CHECKPOINT} }} nextBefore items {{ {MESSAGE} }} }} }}"
    );
    let mut before = Value::Null;
    let mut snapshot = Value::Null;
    let mut collected = Vec::new();
    loop {
        let response = Http::open(
            server.address,
            &query,
            json!({"before":before,"snapshot":snapshot}),
            false,
        )
        .await
        .json()
        .await;
        assert!(response.get("errors").is_none(), "{response}");
        let page = &response["data"]["roots"];
        if snapshot.is_null() {
            snapshot = page["snapshot"].clone();
        } else {
            assert_eq!(snapshot, page["snapshot"]);
        }
        before = page["nextBefore"].clone();
        let items = page["items"].as_array().unwrap();
        collected.splice(0..0, items.iter().cloned());
        if before.is_null() {
            break;
        }
    }
    assert_eq!(collected.len(), 69);
    for (row, original) in collected.iter().skip(65).zip(&originals) {
        assert_eq!(row["body"].as_str().unwrap(), original);
        assert!(row["replyCount"].is_string());
        assert!(row["bodyHtml"].as_str().unwrap().len() < 512 * 1024);
    }
    assert_eq!(collected[68]["renderFallback"], true);

    let mut progress = fixture.cp(0);
    let mut stream = Http::open(
        server.address,
        &subscription(),
        json!({"after":checkpoint(progress)}),
        true,
    )
    .await;
    assert_eq!(stream.response.status(), 200);
    assert!(
        stream.response.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );
    // Committed while replay/setup is active: either captured by catch-up or followed.
    fixture.append("setup overlap".into());
    let target = fixture.current();
    let mut ids = Vec::new();
    let mut bodies = Vec::new();
    while progress.seq < target.seq {
        let (event, payload) = tokio::time::timeout(Duration::from_secs(10), stream.event())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(event, "next");
        let payload = payload.unwrap();
        assert!(serde_json::to_vec(&payload).unwrap().len() < 1024 * 1024);
        assert!(payload.get("errors").is_none(), "{payload}");
        let batch = &payload["data"]["changes"];
        assert_eq!(batch["after"], checkpoint(progress));
        for change in batch["items"].as_array().unwrap() {
            let seq = change["seq"].as_str().unwrap().parse::<i64>().unwrap();
            assert!(!ids.contains(&seq));
            ids.push(seq);
            if change["kind"] == "MESSAGE_CREATED" {
                bodies.push(change["message"]["body"].as_str().unwrap().to_owned());
            }
        }
        progress = fixture.cp(batch["scan"]["seq"].as_str().unwrap().parse().unwrap());
    }
    assert_eq!(ids.len(), 71);
    assert_eq!(bodies.len(), 70);
    assert_eq!(&bodies[65..69], &originals);
    assert_eq!(bodies[69], "setup overlap");
    drop(stream);
    released(&fixture, 1).await;
    assert_eq!(server.finish(true).await, Some(ShutdownOutcome::Drained));
}

#[tokio::test]
async fn canonical_message_receipts_and_mixed_membership_keep_typed_actor_labels() {
    use agentchat_core::{
        model::{Actor, ActorIdentity, ActorKind, DeliveryHistory, DeliveryState, ReadKind},
        read::{ActorView, IdentityLabel, ReceiptDelivery, ReceiptRead, RecipientReceipt},
        sequence::Sequence,
        time::Timestamp,
    };
    use uuid::Uuid;
    let fixture = Fixture::new();
    fixture.append("full body 日本語".into());
    let message_id = fixture.rows.lock().unwrap()[0].1.message.message.id;
    let at = Timestamp::parse("2026-10-28T00:00:00.000Z").unwrap();
    let user_id = Uuid::from_u128(9);
    fixture.extra_members.lock().unwrap().push(ActorView {
        actor: Actor {
            actor_id: user_id,
            identity: ActorIdentity::User,
            handle: "user".into(),
            joined: true,
            first_join_at: at,
            join_seq: Sequence::new(1).unwrap(),
            leave_seq: None,
        },
        presence: None,
        connected: None,
        incarnation: None,
    });
    fixture.receipt_rows.lock().unwrap().extend([
        RecipientReceipt {
            recipient: IdentityLabel {
                actor_id: user_id,
                kind: ActorKind::User,
                handle: "user".into(),
            },
            delivery: None,
            reads: vec![ReceiptRead {
                kind: ReadKind::Explicit,
                at,
            }],
        },
        RecipientReceipt {
            recipient: IdentityLabel {
                actor_id: Uuid::from_u128(3),
                kind: ActorKind::Agent,
                handle: "alice".into(),
            },
            delivery: Some(ReceiptDelivery {
                delivery_id: Uuid::from_u128(40),
                state: DeliveryState::Delivered,
                history: DeliveryHistory {
                    queued_at: at,
                    processing_started_at: Some(at),
                    deadline_at: Some(at.after_seconds(3600).unwrap()),
                    timed_out_at: None,
                    delivered_at: Some(at),
                    left_at: None,
                },
            }),
            reads: vec![
                ReceiptRead {
                    kind: ReadKind::Automatic,
                    at,
                },
                ReceiptRead {
                    kind: ReadKind::Explicit,
                    at,
                },
            ],
        },
    ]);
    let server = Server::open(fixture.clone(), Some(ShutdownOptions::graceful())).await;
    let query = r#"query($id:ID!,$after:String){
        agents { items { id kind host } }
        channelMembers(channel:"dev") { items { id kind host chatId } }
        message(messageId:$id,after:$after,limit:1) {
            message { id body author { id kind handle } }
            receipts { items { recipient { id kind handle } delivery { deliveryId state queuedAt processingStartedAt deadlineAt timedOutAt deliveredAt leftAt } reads { kind at } } nextCursor scanCursor head }
        }
    }"#;
    let first = Http::open(
        server.address,
        query,
        json!({"id":message_id,"after":null}),
        false,
    )
    .await
    .json()
    .await;
    assert!(first.get("errors").is_none(), "{first}");
    let data = &first["data"];
    assert!(
        data["agents"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["kind"] == "AGENT")
    );
    let user = data["channelMembers"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["kind"] == "USER")
        .unwrap();
    assert!(user["host"].is_null() && user["chatId"].is_null());
    assert_eq!(data["message"]["message"]["body"], "full body 日本語");
    let receipt = &data["message"]["receipts"]["items"][0];
    assert_eq!(receipt["recipient"]["kind"], "USER");
    assert!(receipt["delivery"].is_null());
    assert_eq!(receipt["reads"][0]["kind"], "EXPLICIT");
    assert!(data["message"].get("readers").is_none());
    let second = Http::open(
        server.address,
        query,
        json!({"id":message_id,"after":data["message"]["receipts"]["nextCursor"]}),
        false,
    )
    .await
    .json()
    .await;
    assert!(second.get("errors").is_none(), "{second}");
    let native = &second["data"]["message"]["receipts"]["items"][0];
    assert_eq!(native["reads"][0]["kind"], "AUTOMATIC");
    assert_eq!(native["reads"][1]["kind"], "EXPLICIT");
    assert_eq!(native["delivery"]["state"], "DELIVERED");
    assert_eq!(native["delivery"]["queuedAt"], at.to_string());
    assert_eq!(native["delivery"]["deliveredAt"], at.to_string());
    assert!(native["delivery"]["timedOutAt"].is_null());
    assert!(native["delivery"].get("confirmation").is_none());
    assert_eq!(server.finish(true).await, Some(ShutdownOutcome::Drained));
}

#[tokio::test]
async fn source_setup_errors_disconnect_replay_and_public_shutdown_release_owned_sources() {
    for mode in [
        None,
        Some(ShutdownOptions::immediate()),
        Some(ShutdownOptions::graceful_for(Duration::from_secs(2)).unwrap()),
    ] {
        let fixture = Fixture::new();
        fixture.append("first".into());
        let server = Server::open(fixture.clone(), mode).await;
        let mut stream = Http::open(
            server.address,
            &subscription(),
            json!({"after":checkpoint(fixture.current())}),
            true,
        )
        .await;
        assert_eq!(stream.response.status(), 200);
        fixture.append("disconnect gap".into());
        let (_, payload) = tokio::time::timeout(Duration::from_secs(3), stream.event())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            payload.unwrap()["data"]["changes"]["items"][0]["message"]["body"],
            "disconnect gap"
        );
        let progress = fixture.current();
        drop(stream);
        released(&fixture, 1).await;
        fixture.append("reconnected".into());
        let mut stream = Http::open(
            server.address,
            &subscription(),
            json!({"after":checkpoint(progress)}),
            true,
        )
        .await;
        let (_, payload) = tokio::time::timeout(Duration::from_secs(3), stream.event())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            payload.unwrap()["data"]["changes"]["items"][0]["message"]["body"],
            "reconnected"
        );
        if mode.is_none() {
            // No-hook has no coordinated native/SDK drain promise. Close the
            // active physical consumer before releasing its remaining owners.
            drop(stream);
            released(&fixture, 2).await;
            server.finish(false).await;
            continue;
        }
        let outcome = server.finish(true).await;
        released(&fixture, 2).await;
        if let Some(mode) = mode {
            match mode.mode() {
                chigiri::ShutdownMode::Immediate => {
                    assert_eq!(outcome, Some(ShutdownOutcome::CancellationRequested))
                }
                chigiri::ShutdownMode::Graceful => {
                    assert_eq!(outcome, Some(ShutdownOutcome::Drained))
                }
            }
        }
        drop(stream);
    }

    let fixture = Fixture::new();
    let server = Server::open(fixture.clone(), Some(ShutdownOptions::graceful())).await;
    fixture.fail_setup.store(true, Ordering::SeqCst);
    let mut stream = Http::open(
        server.address,
        &subscription(),
        json!({"after":checkpoint(fixture.current())}),
        true,
    )
    .await;
    let (_, payload) = tokio::time::timeout(Duration::from_secs(3), stream.event())
        .await
        .unwrap()
        .unwrap();
    assert!(payload.unwrap().get("errors").is_some());
    drop(stream);
    assert_eq!(fixture.starts.load(Ordering::SeqCst), 0);
    fixture.fail_setup.store(false, Ordering::SeqCst);
    let mut stream = Http::open(
        server.address,
        &subscription(),
        json!({"after":checkpoint(fixture.current())}),
        true,
    )
    .await;
    fixture.fail_source.store(true, Ordering::SeqCst);
    fixture.hints.send_replace(0);
    let (_, payload) = tokio::time::timeout(Duration::from_secs(3), stream.event())
        .await
        .unwrap()
        .unwrap();
    assert!(payload.unwrap().get("errors").is_some());
    drop(stream);
    released(&fixture, 1).await;
    assert_eq!(server.finish(true).await, Some(ShutdownOutcome::Drained));
}
