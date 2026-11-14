//! Executable pipe shutdown must not depend on the host closing its descriptors.
mod support;

use agentchat_protocol::framing::{self, FramedReader};
use serde_json::{Value, json};
use std::{process::Stdio, time::Duration};
use support::Fixture;
use tokio::{
    io::{AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout},
};

fn spawn(f: &Fixture) -> Child {
    f.command(&["mcp"]).stdin(Stdio::piped()).spawn().unwrap()
}

async fn initialize(child: &mut Child) -> (ChildStdin, FramedReader<BufReader<ChildStdout>>) {
    let mut input = child.stdin.take().unwrap();
    let mut output = FramedReader::new(BufReader::new(child.stdout.take().unwrap()));
    framing::write(&mut input, &json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{
        "protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"shutdown-test","version":"1"}
    }})).await.unwrap();
    let initialized: Value = tokio::time::timeout(Duration::from_secs(5), output.read())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(initialized.get("result").is_some(), "{initialized}");
    framing::write(
        &mut input,
        &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
    )
    .await
    .unwrap();
    (input, output)
}

#[tokio::test]
async fn sigint_exits_while_the_host_keeps_initialized_stdin_open() {
    let f = Fixture::new();
    let mut child = spawn(&f);
    let (mut input, mut output) = initialize(&mut child).await;
    framing::write(
        &mut input,
        &json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}),
    )
    .await
    .unwrap();
    let tools: Value = tokio::time::timeout(Duration::from_secs(5), output.read())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(tools["result"]["tools"].as_array().unwrap().len(), 23);
    assert!(
        tokio::process::Command::new("kill")
            .args(["-INT", &child.id().unwrap().to_string()])
            .status()
            .await
            .unwrap()
            .success()
    );
    let status = tokio::time::timeout(Duration::from_secs(3), child.wait())
        .await
        .expect("SIGINT shutdown waited for host EOF")
        .unwrap();
    assert!(status.success(), "{status}");
    drop((input, output));
    assert!(!f.manager.home().exists());
}

#[tokio::test]
async fn partial_initialization_times_out_with_the_host_still_holding_stdin() {
    let f = Fixture::new();
    let mut child = spawn(&f);
    let mut input = child.stdin.take().unwrap();
    input.write_all(b"{").await.unwrap();
    let output = tokio::time::timeout(Duration::from_secs(8), child.wait_with_output())
        .await
        .expect("initialization deadline waited for host EOF")
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("initialization"));
    drop(input);
    assert!(!f.manager.home().exists());
}

#[tokio::test]
async fn oversized_frame_exits_without_needing_the_host_to_close_its_writer() {
    let f = Fixture::new();
    let mut child = spawn(&f);
    let (mut input, output) = initialize(&mut child).await;
    let _ = input
        .write_all(&vec![b'x'; framing::MAX_FRAME_BYTES + 1])
        .await;
    let status = tokio::time::timeout(Duration::from_secs(3), child.wait())
        .await
        .expect("oversized frame shutdown waited for host EOF")
        .unwrap();
    assert!(status.success(), "{status}");
    drop((input, output));
    assert!(!f.manager.home().exists());
}

#[tokio::test]
async fn unread_stdout_reaches_its_deadline_and_exits_with_both_host_pipes_open() {
    let f = Fixture::new();
    let mut child = spawn(&f);
    let (mut input, output) = initialize(&mut child).await;
    // Tool schemas exceed a pipe's capacity. Keep its read end open without
    // consuming any responses, so this exercises backpressure, not BrokenPipe.
    for id in 1..=32 {
        framing::write(
            &mut input,
            &json!({"jsonrpc":"2.0","id":id,"method":"tools/list","params":{}}),
        )
        .await
        .unwrap();
    }
    let status = tokio::time::timeout(Duration::from_secs(8), child.wait())
        .await
        .expect("write deadline left a blocking stdout worker alive")
        .unwrap();
    assert!(status.success(), "{status}");
    drop((input, output));
    assert!(!f.manager.home().exists());
}
