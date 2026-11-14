//! Disposable stdin-controlled data fixture; the product HTTP surface stays read-only.
mod support;

use agentchat_web::Viewer;
use serde::Deserialize;
use serde_json::json;
use std::sync::{Arc, atomic::Ordering};
use support::reader::{Fixture, Reader};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    sync::oneshot,
};
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
enum Control {
    Reset,
    Root {
        body: String,
        count: Option<usize>,
        channel: Option<String>,
    },
    Reply {
        root: Uuid,
        body: String,
        count: Option<usize>,
    },
    Channel {
        name: String,
    },
    Restore,
    Fail {
        enabled: bool,
    },
    Status,
    Stop,
}
fn seed(fixture: &Fixture) {
    fixture.reset();
    for n in 0..120 {
        fixture.append(format!("Root {n:03}\n\nHello **AgentChat**."));
    }
    let root = fixture
        .rows
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .1
        .message
        .message
        .id;
    for n in 0..75 {
        fixture.reply(root, format!("Reply {n:03}"));
    }
}

#[tokio::test]
#[ignore = "Owned by the explicit Playwright browser harness; no production control endpoint"]
async fn browser_provider() {
    assert_eq!(
        std::env::var("AGENTCHAT_WEB_BROWSER_PROVIDER").as_deref(),
        Ok("1")
    );
    let fixture = Fixture::new();
    seed(&fixture);
    let viewer = Viewer::bind_with_context(
        Arc::new(Reader(fixture.clone())),
        agentchat_web::ViewerContext {
            directory_name: Some("viewer-fixture".into()),
        },
    )
    .await
    .unwrap();
    println!("AGENTCHAT_BROWSER_URL={}", viewer.url());
    let (stop, receive) = oneshot::channel();
    let task = tokio::spawn(viewer.serve(async {
        let _ = receive.await;
    }));
    let mut input = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = input.next_line().await.unwrap() {
        assert!(line.len() <= 1024 * 1024);
        let command: Control = serde_json::from_str(&line).unwrap();
        match command {
            Control::Stop => break,
            Control::Reset => seed(&fixture),
            Control::Root {
                body,
                count,
                channel,
            } => {
                for _ in 0..count.unwrap_or(1).min(100) {
                    fixture.post(channel.as_deref().unwrap_or("dev"), None, body.clone());
                }
            }
            Control::Reply { root, body, count } => {
                for _ in 0..count.unwrap_or(1).min(100) {
                    fixture.reply(root, body.clone());
                }
            }
            Control::Channel { name } => fixture.add_channel(name),
            Control::Restore => fixture.restore(),
            Control::Fail { enabled } => {
                fixture.fail_source.store(enabled, Ordering::SeqCst);
                fixture
                    .hints
                    .send_replace(fixture.current().seq.get() as usize);
            }
            Control::Status => {}
        }
        println!(
            "AGENTCHAT_BROWSER_CONTROL={}",
            json!({"head":fixture.current(),"starts":fixture.starts.load(Ordering::SeqCst),"drops":fixture.drops.load(Ordering::SeqCst)})
        );
    }
    let _ = stop.send(());
    task.await.unwrap().unwrap();
}
