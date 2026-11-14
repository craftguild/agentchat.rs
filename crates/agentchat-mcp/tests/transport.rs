//! Wire allocation limits and backpressure are tested with real SDK dispatch.
mod support;
use agentchat_protocol::framing::{self, FramedReader};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use support::*;
use tokio::io::{AsyncWriteExt, BufReader};

#[tokio::test]
async fn oversized_and_truncated_initial_frames_are_refused_without_invocation() {
    for frame in [
        vec![b'x'; framing::MAX_FRAME_BYTES + 1],
        b"{\"jsonrpc\":".to_vec(),
    ] {
        let backend = Arc::new(Fake::default());
        let server = agentchat_mcp::Server::new(backend.clone());
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            agentchat_mcp::serve(server, std::io::Cursor::new(frame), tokio::io::sink()),
        )
        .await
        .unwrap();
        assert!(result.is_err());
        assert!(backend.calls.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn slow_stdout_bounds_pending_requests_and_closes_within_deadline() {
    let backend = Arc::new(Fake::default());
    *backend.result.lock().unwrap() = Some(json!({"items":[{"body":"x".repeat(65_536)}]}));
    let (client, socket) = tokio::io::duplex(8192);
    let (reader, writer) = tokio::io::split(socket);
    let server = tokio::spawn(agentchat_mcp::serve(
        agentchat_mcp::Server::new(backend.clone()),
        reader,
        writer,
    ));
    let (reader, mut writer) = tokio::io::split(client);
    let mut reader = FramedReader::new(BufReader::new(reader));
    framing::write(&mut writer, &json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{
        "protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"slow-test","version":"1"}
    }})).await.unwrap();
    let initialized: Value = reader.read().await.unwrap().unwrap();
    assert!(initialized.get("result").is_some());
    framing::write(
        &mut writer,
        &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
    )
    .await
    .unwrap();
    let sender = tokio::spawn(async move {
        for id in 1..=100 {
            if framing::write(
                &mut writer,
                &json!({"jsonrpc":"2.0","id":id,"method":"tools/call",
                "params":{"name":"agentchat_history","arguments":{"project_path":"/explicit"}}}),
            )
            .await
            .is_err()
            {
                break;
            }
        }
        let _ = writer.shutdown().await;
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while backend.calls.lock().unwrap().len() < 32 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(backend.calls.lock().unwrap().len(), 32);
    // Keep stdout unread. A timed-out send wakes a receive blocked on its
    // exhausted request credits; neither half can keep the SDK alive forever.
    tokio::time::timeout(Duration::from_secs(8), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    sender.await.unwrap();
    assert_eq!(backend.calls.lock().unwrap().len(), 32);
}
