//! Bounded socket writers cannot block other operations or retain idle activity.

mod support;
use agentchat_core::{application::Mutation, read::Query};
use agentchat_protocol::{
    framing,
    rpc::{Call, Request},
};
use std::time::Duration;
use support::runtime::{Running, join, mutate, options};
use tokio::{
    io::{AsyncReadExt, BufReader},
    net::UnixStream,
};
use uuid::Uuid;

async fn checked_stream(
    r: &Running,
) -> (
    BufReader<tokio::net::unix::OwnedReadHalf>,
    tokio::net::unix::OwnedWriteHalf,
) {
    let stream = UnixStream::connect(&r.handle.paths().socket).await.unwrap();
    let (read, mut write) = stream.into_split();
    let mut read = BufReader::new(read);
    let hello = Request::new(
        Uuid::new_v4(),
        &Call::Handshake(agentchat_protocol::envelope::Handshake {
            version: 1,
            project_id: r.project.identity().id,
        }),
    )
    .unwrap();
    framing::write(&mut write, &hello).await.unwrap();
    let _: serde_json::Value = framing::read(&mut read).await.unwrap().unwrap();
    (read, write)
}

#[tokio::test]
async fn a_slow_finite_reader_times_out_without_blocking_shared_application_work() {
    let mut opts = options();
    opts.io_timeout = Duration::from_millis(200);
    let r = Running::new(opts).await;
    let context = r.context("slow-reader-author").await;
    let mut author = r.client().await;
    join(&mut author, &context, "alice").await;
    for _ in 0..12 {
        mutate(
            &mut author,
            &context,
            Mutation::Post {
                channel: Some("dev".into()),
                body: "x".repeat(65_536),
            },
        )
        .await;
    }
    let (mut read, mut write) = checked_stream(&r).await;
    let request = Request::new(
        Uuid::new_v4(),
        &Call::Query {
            actor: None,
            query: Query::History {
                channel: None,
                thread: None,
                page: Default::default(),
            },
        },
    )
    .unwrap();
    framing::write(&mut write, &request).await.unwrap();
    let status = tokio::time::timeout(
        Duration::from_secs(2),
        support::runtime::health(&mut author),
    )
    .await
    .unwrap();
    assert_eq!(status["statistics"]["messages"], 12);
    tokio::time::sleep(Duration::from_millis(500)).await;
    let status = support::runtime::health(&mut author).await;
    assert_eq!(
        status["runtime"]["requests"], 1,
        "only this status request remains active"
    );
    let mut partial = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), read.read_to_end(&mut partial))
        .await
        .unwrap()
        .unwrap();
    assert!(!partial.is_empty());
    assert!(
        !partial.ends_with(b"\n"),
        "the timed out page was not fully emitted"
    );
    drop(write);
    drop(author);
    r.stop().await;
}

#[tokio::test]
async fn a_slow_observer_is_disconnected_and_can_resume_from_its_last_complete_page() {
    let mut opts = options();
    // The same owner serves both the deliberately stalled stream and the
    // resumed reader. Exercise its real write deadline for the large pages.
    opts.io_timeout = agentchat_runtime::daemon::RuntimeOptions::default().io_timeout;
    let write_deadline = opts.io_timeout;
    let r = Running::new(opts).await;
    let context = r.context("slow-observer-author").await;
    let mut author = r.client().await;
    join(&mut author, &context, "alice").await;
    for _ in 0..220 {
        mutate(
            &mut author,
            &context,
            Mutation::Post {
                channel: Some("dev".into()),
                body: "\0".repeat(512),
            },
        )
        .await;
    }
    let (mut read, mut write) = checked_stream(&r).await;
    let request_id = Uuid::new_v4();
    let request = Request::new(
        request_id,
        &Call::SubscribeChanges {
            after: agentchat_core::read::Checkpoint::start(r.project.identity().id),
        },
    )
    .unwrap();
    framing::write(&mut write, &request).await.unwrap();
    // Consume only the subscribe response. No complete event page is read, so
    // replay still starts at zero. This also fences registration before polling.
    let accepted: serde_json::Value =
        tokio::time::timeout(write_deadline * 2, framing::read(&mut read))
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    assert_eq!(accepted["id"], request_id.to_string());
    assert!(accepted.get("result").is_some());
    tokio::time::timeout(write_deadline * 2, async {
        loop {
            if support::runtime::health(&mut author).await["runtime"]["observers"] == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let mut partial = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), read.read_to_end(&mut partial))
        .await
        .unwrap()
        .unwrap();
    assert!(
        !partial.is_empty(),
        "an event page must have been partially emitted"
    );
    assert!(!partial.ends_with(b"\n"));
    let mut resumed = r
        .client()
        .await
        .subscribe_changes(agentchat_core::read::Checkpoint::start(
            r.project.identity().id,
        ))
        .await
        .unwrap();
    let mut count = 0;
    while count < 220 {
        let page = resumed.next().await.unwrap();
        count += page
            .items
            .iter()
            .filter(|e| {
                matches!(
                    e.change,
                    agentchat_core::read::ViewerChangeKind::MessageCreated { .. }
                )
            })
            .count();
    }
    assert_eq!(count, 220);
    drop(resumed);
    drop(write);
    drop(author);
    r.stop().await;
}
