//! Saturated product HTTP must leave real mutations, native input and another viewer independent.
#[path = "../../agentchat-runtime/tests/native_workload/host.rs"]
mod host;
mod support;
#[path = "viewer_slow_consumer/tracking.rs"]
mod tracking;
#[path = "viewer_slow_consumer/wire.rs"]
mod wire;

use agentchat_core::{
    Error, ErrorCode, Result,
    application::Mutation,
    delivery::{
        HostReceipt, MessageDispatch, MessageInput, MessagePort, MessageProgress, NoticeDispatch,
    },
    model::{Context, ReadKind},
    read::{Checkpoint, requests},
    time::SystemClock,
};
use agentchat_protocol::rpc::{Call, Client, success};
use agentchat_runtime::{
    daemon::{RuntimeOptions, Server},
    operations::{Launcher, Operations},
    project::Project,
};
use agentchat_web::Viewer;
use async_trait::async_trait;
use serde_json::{Value, json};
use std::{
    collections::{BTreeSet, HashMap},
    net::SocketAddr,
    path::Path,
    sync::{Arc, Mutex, atomic::Ordering},
    time::Duration,
};
use support::Fixture;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpSocket, TcpStream},
    sync::oneshot,
};
use uuid::Uuid;
use wire::Http;

const INITIAL: usize = 64;
const QUERY: &str = "subscription($after:CheckpointInput!){changes(after:$after){after{seq}scan{seq}items{seq message{id body bodyHtml}}}}";

struct Native {
    bodies: Mutex<Vec<MessageInput>>,
    generations: HashMap<Uuid, Uuid>,
}
#[async_trait]
impl MessagePort for Native {
    async fn present_message(
        &self,
        context: &Context,
        d: &MessageDispatch,
    ) -> Result<MessageProgress> {
        assert_eq!(context.host_version, "fixture-only");
        assert_eq!(context.os_profile, "synthetic-not-admitted");
        assert!(!d.awaiting_acceptance);
        assert!(d.input.auto_read);
        assert_eq!(self.generations[&context.context_id], d.input.generation);
        assert_eq!(MessageInput::from_text(&d.input.text()?)?, d.input);
        let mut bodies = self.bodies.lock().unwrap();
        if let Some(last) = bodies.last() {
            assert!(last.seq < d.input.seq);
        }
        assert!(bodies.iter().all(|m| m.delivery_id != d.input.delivery_id));
        bodies.push(d.input.clone());
        Ok(MessageProgress::Accepted(HostReceipt::default()))
    }
    async fn present_notice(
        &self,
        context: &Context,
        _: &NoticeDispatch,
    ) -> Result<MessageProgress> {
        assert_eq!(context.host_version, "fixture-only");
        Ok(MessageProgress::Accepted(HostReceipt::default()))
    }
}
struct NoLaunch;
#[async_trait]
impl Launcher for NoLaunch {
    async fn launch(&self, _: &Project, _: &Path) -> Result<()> {
        Err(Error::new(
            ErrorCode::RuntimeUnavailable,
            "Only the owned fixture runtime may start.",
        ))
    }
}
async fn post(client: &mut Client, actor: &Context, body: String) {
    success(
        client
            .call(
                Uuid::new_v4(),
                Call::Mutate {
                    actor: agentchat_protocol::rpc::ActorReference::Native {
                        context_id: actor.context_id,
                    },
                    request_id: Uuid::new_v4(),
                    command: Mutation::Post {
                        channel: Some("dev".into()),
                        body,
                    },
                },
            )
            .await
            .unwrap(),
    )
    .unwrap();
}
async fn automatic_receipts(client: &mut Client, inputs: &[MessageInput]) {
    for input in inputs {
        let detail = success(
            client
                .read(
                    Uuid::new_v4(),
                    None,
                    requests::Message {
                        message_id: input.message_id,
                        receipts: Default::default(),
                    },
                )
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(detail.receipts.items.len(), 1);
        let row = &detail.receipts.items[0];
        let delivery = row.delivery.as_ref().unwrap();
        assert_eq!(delivery.delivery_id, input.delivery_id);
        assert!(delivery.history.delivered_at.is_some());
        assert_eq!(row.reads.len(), 1);
        assert_eq!(row.reads[0].kind, ReadKind::Automatic);
        assert_eq!(Some(row.reads[0].at), delivery.history.delivered_at);
    }
}
fn checkpoint(p: Checkpoint) -> Value {
    json!({"after":{"projectId":p.project_id,"seq":p.seq,"eventId":p.event_id}})
}
fn address(viewer: &Viewer) -> SocketAddr {
    viewer
        .url()
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .parse()
        .unwrap()
}
async fn unread(address: SocketAddr, after: Checkpoint) -> TcpStream {
    let socket = TcpSocket::new_v4().unwrap();
    socket.set_recv_buffer_size(1024).unwrap();
    let mut socket = socket.connect(address).await.unwrap();
    let body = json!({"query":QUERY,"variables":checkpoint(after)}).to_string();
    socket.write_all(format!("POST /graphql HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/json\r\nAccept: text/event-stream\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
    let mut headers = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !headers.ends_with(b"\r\n\r\n") {
            headers.push(socket.read_u8().await.unwrap());
            assert!(headers.len() <= 16 * 1024);
        }
    })
    .await
    .unwrap();
    assert!(headers.starts_with(b"HTTP/1.1 200"));
    // No SSE/body bytes are read, so the only applied checkpoint remains after.
    socket
}
async fn collect(stream: &mut Http, after: Checkpoint, expected: &[String]) {
    let mut cursor = after.seq.get();
    let mut ids = BTreeSet::new();
    let mut bodies = Vec::new();
    while bodies.len() < expected.len() {
        let (event, value) = tokio::time::timeout(Duration::from_secs(10), stream.event())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(event, "next");
        let value = value.unwrap();
        assert!(value.get("errors").is_none(), "{value}");
        let batch = &value["data"]["changes"];
        assert_eq!(
            batch["after"]["seq"]
                .as_str()
                .unwrap()
                .parse::<i64>()
                .unwrap(),
            cursor
        );
        let scan = batch["scan"]["seq"]
            .as_str()
            .unwrap()
            .parse::<i64>()
            .unwrap();
        for item in batch["items"].as_array().unwrap() {
            let seq = item["seq"].as_str().unwrap().parse::<i64>().unwrap();
            assert!(seq > cursor && seq <= scan);
            let m = &item["message"];
            if !m.is_null() {
                assert!(ids.insert(m["id"].as_str().unwrap().to_owned()));
                assert!(!m["bodyHtml"].as_str().unwrap().is_empty());
                bodies.push(m["body"].as_str().unwrap().to_owned());
            }
            cursor = seq;
        }
        cursor = scan;
    }
    assert_eq!(bodies, expected);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stalled_rendered_sse_isolated_from_writes_native_input_and_a_second_viewer_then_replays() {
    let f = Fixture::new();
    let project = f.initialize().await;
    let contexts = [
        f.context("slow-author").await,
        f.context("slow-reader").await,
    ];
    let host = Arc::new(host::ManagedHost::new(&contexts));
    let native = Arc::new(Native {
        bodies: Mutex::new(Vec::new()),
        generations: host.generations(),
    });
    let server = Server::bind(
        f.manager.clone(),
        project.clone(),
        host,
        Arc::new(SystemClock),
        RuntimeOptions::default(),
    )
    .await
    .unwrap()
    .with_native_messages(native.clone());
    let handle = server.handle();
    let runtime = tokio::spawn(server.run());
    let mut author = Client::connect(&handle.paths().socket, project.identity().id)
        .await
        .unwrap();
    let mut receiver = Client::connect(&handle.paths().socket, project.identity().id)
        .await
        .unwrap();
    for (client, context, name) in [
        (&mut author, &contexts[0], "author"),
        (&mut receiver, &contexts[1], "reader"),
    ] {
        success(
            client
                .call(
                    Uuid::new_v4(),
                    Call::StartLaunch {
                        context_id: context.context_id,
                        name: Some(name.into()),
                    },
                )
                .await
                .unwrap(),
        )
        .unwrap();
        success(
            client
                .call(
                    Uuid::new_v4(),
                    Call::Attach {
                        context_id: context.context_id,
                    },
                )
                .await
                .unwrap(),
        )
        .unwrap();
    }
    let mut control = Client::connect(&handle.paths().socket, project.identity().id)
        .await
        .unwrap();
    let after = success(
        control
            .read(Uuid::new_v4(), None, requests::ViewerHead)
            .await
            .unwrap(),
    )
    .unwrap();
    let prefix = "@reader maximum **GFM** ";
    let maximum = prefix.to_owned() + &"\0".repeat(65_536 - prefix.len());
    let mut expected = vec![maximum; INITIAL];
    for body in &expected {
        post(&mut author, &contexts[0], body.clone()).await;
    }
    tokio::time::timeout(Duration::from_secs(10), async {
        while native.bodies.lock().unwrap().len() != INITIAL {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let live_after = success(
        control
            .read(Uuid::new_v4(), None, requests::ViewerHead)
            .await
            .unwrap(),
    )
    .unwrap();
    let operations = Operations::new(f.manager.clone(), Arc::new(NoLaunch));
    let slow_session = operations.viewer(f.workspace.clone()).await.unwrap();
    let fast_session = operations.viewer(f.workspace.clone()).await.unwrap();
    let progress = Arc::new(tracking::Progress::default());
    let slow_viewer = Viewer::bind(Arc::new(tracking::Tracked {
        reader: slow_session.reader(),
        progress: progress.clone(),
    }))
    .await
    .unwrap();
    let fast_viewer = Viewer::bind(fast_session.reader()).await.unwrap();
    let slow_address = address(&slow_viewer);
    let fast_address = address(&fast_viewer);
    let (stop_slow, receive_slow) = oneshot::channel();
    let (stop_fast, receive_fast) = oneshot::channel();
    let slow_task = tokio::spawn(slow_viewer.serve(async {
        let _ = receive_slow.await;
    }));
    let fast_task = tokio::spawn(fast_viewer.serve(async {
        let _ = receive_fast.await;
    }));
    let stalled = unread(slow_address, after).await;
    // Real unread TCP plus a stopped typed-source demand proves saturation,
    // rather than calling an idle subscription a slow-consumer test.
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let before = progress.messages.load(Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(100)).await;
            let now = progress.messages.load(Ordering::SeqCst);
            assert!(
                progress.alive.load(Ordering::SeqCst),
                "slow source ended before overlap was observed"
            );
            if (2..INITIAL).contains(&now) && now == before {
                break;
            }
        }
    })
    .await
    .unwrap();
    let stopped_at = progress.messages.load(Ordering::SeqCst);
    let mut fast = Http::open(fast_address, QUERY, checkpoint(live_after), true).await;
    let probe = "@reader Independent mutation while rendered SSE is saturated".to_owned();
    post(&mut author, &contexts[0], probe.clone()).await;
    expected.push(probe.clone());
    let query = Http::open(fast_address, "{project{id}}", json!({}), false)
        .await
        .json()
        .await;
    assert_eq!(
        query["data"]["project"]["id"],
        project.identity().id.to_string()
    );
    collect(&mut fast, live_after, &[probe]).await;
    tokio::time::timeout(Duration::from_secs(3), async {
        while native.bodies.lock().unwrap().len() != expected.len() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        progress.alive.load(Ordering::SeqCst),
        "fixture lost the saturated-source overlap before independent progress completed"
    );
    assert_eq!(
        native
            .bodies
            .lock()
            .unwrap()
            .iter()
            .map(|m| m.body.clone())
            .collect::<Vec<_>>(),
        expected
    );
    let status = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let status = control.health().await.unwrap();
            if status.application.statistics.read_receipts == expected.len() as u64 {
                break status;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(status.application.statistics.agents, 2);
    let native_read_count = status.application.statistics.read_receipts;
    // The unchanged product no-progress/queue-age budgets must release the
    // unconsumed source even while its TCP consumer remains open and unread.
    tokio::time::timeout(Duration::from_secs(15), async {
        while progress.alive.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    drop(stalled);
    let mut resumed = Http::open(slow_address, QUERY, checkpoint(after), true).await;
    collect(&mut resumed, after, &expected).await;
    drop(resumed);
    drop(fast);
    stop_slow.send(()).unwrap();
    stop_fast.send(()).unwrap();
    slow_task.await.unwrap().unwrap();
    fast_task.await.unwrap().unwrap();
    slow_session.close().await;
    fast_session.close().await;
    tokio::time::timeout(Duration::from_secs(3), async {
        while control.health().await.unwrap().runtime.activity.observers != 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(!runtime.is_finished());
    let status = control.health().await.unwrap();
    assert_eq!(
        status.application.statistics.messages,
        expected.len() as u64
    );
    assert_eq!(
        status.application.statistics.read_receipts,
        native_read_count
    );
    let inputs = native.bodies.lock().unwrap().clone();
    automatic_receipts(&mut control, &inputs).await;
    println!(
        "SLOW_CONSUMER_COMPLETE {}",
        json!({"scope":"actual product HTTP/runtime/SeaORM and synthetic native input; no real host/model claim","maximum_body_bytes":65_536,"initial_messages":INITIAL,"source_stopped_after_messages":stopped_at,"complete_fast_messages":1,"complete_replayed_messages":expected.len(),"native_complete_bodies":inputs.len(),"viewers":2,"participant_count":2,"automatic_reads":native_read_count,"explicit_reads":0,"viewer_receipt_delta":0,"observers_after_close":0,"shared_runtime_survived_viewer_shutdown":true})
    );
    handle.shutdown();
    runtime.await.unwrap().unwrap();
}
