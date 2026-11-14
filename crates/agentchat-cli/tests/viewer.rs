//! Actual CLI -> runtime/RPC -> SeaORM -> managed GraphQL/embedded-assets path.
mod support;

use agentchat_core::{application::Mutation, model::Context, read::requests};
use agentchat_protocol::rpc::{Call, Client, success};
use agentchat_runtime::daemon::RuntimePaths;
use http_body_util::{BodyExt, Full};
use hyper::{
    Request, Response,
    body::{Bytes, Incoming},
    client::conn::http1,
};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use std::{collections::BTreeSet, net::SocketAddr, time::Duration};
use support::Fixture;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    net::TcpStream,
    task::JoinHandle,
};
use uuid::Uuid;

struct Http {
    response: Response<Incoming>,
    task: JoinHandle<()>,
    pending: Vec<u8>,
}
impl Http {
    async fn open(address: SocketAddr, query: &str, variables: Value, stream: bool) -> Self {
        let socket = TcpStream::connect(address).await.unwrap();
        let (mut send, connection) = http1::handshake(TokioIo::new(socket)).await.unwrap();
        let task = tokio::spawn(async move {
            let _ = connection.await;
        });
        let request = Request::post("/graphql")
            .header("host", address.to_string())
            .header("content-type", "application/json")
            .header(
                "accept",
                if stream {
                    "text/event-stream"
                } else {
                    "application/graphql-response+json"
                },
            )
            .body(Full::new(Bytes::from(
                serde_json::to_vec(&json!({"query":query,"variables":variables})).unwrap(),
            )))
            .unwrap();
        let response = send.send_request(request).await.unwrap();
        assert_eq!(response.status(), 200);
        Self {
            response,
            task,
            pending: Vec::new(),
        }
    }
    async fn json(mut self) -> Value {
        let bytes = self.response.body_mut().collect().await.unwrap().to_bytes();
        assert!(bytes.len() <= 1024 * 1024);
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(value.get("errors").is_none(), "{value}");
        value["data"].clone()
    }
    async fn next(&mut self) -> Option<Value> {
        loop {
            if let Some(end) = self.pending.windows(2).position(|bytes| bytes == b"\n\n") {
                let frame: Vec<_> = self.pending.drain(..end + 2).collect();
                let frame = std::str::from_utf8(&frame).unwrap();
                if frame.lines().any(|line| line == "event: complete") {
                    return None;
                }
                if let Some(data) = frame.lines().find_map(|line| line.strip_prefix("data: ")) {
                    let value: Value = serde_json::from_str(data).unwrap();
                    assert!(value.get("errors").is_none(), "{value}");
                    return Some(value["data"]["changes"].clone());
                }
                continue;
            }
            let frame = self.response.body_mut().frame().await?.unwrap();
            if let Ok(bytes) = frame.into_data() {
                self.pending.extend_from_slice(&bytes);
            }
            assert!(self.pending.len() <= 2 * 1024 * 1024);
        }
    }
}
impl Drop for Http {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn mutate(client: &mut Client, actor: &Context, mutation: Mutation) -> Value {
    let id = Uuid::new_v4();
    success(
        client
            .call(
                id,
                Call::Mutate {
                    request_id: id,
                    actor: agentchat_protocol::rpc::ActorReference::Native {
                        context_id: actor.context_id,
                    },
                    command: mutation,
                },
            )
            .await
            .unwrap(),
    )
    .unwrap()
}
async fn post(client: &mut Client, actor: &Context, body: String) -> Value {
    mutate(
        client,
        actor,
        Mutation::Post {
            channel: Some("dev".into()),
            body,
        },
    )
    .await
}
async fn next(stream: &mut Http) -> Value {
    tokio::time::timeout(Duration::from_secs(10), stream.next())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn viewer_refuses_uninitialized_projects_and_pagination_without_initialization() {
    let fixture = Fixture::new();
    let before: Vec<_> = std::fs::read_dir(&fixture.workspace).unwrap().collect();
    assert!(before.is_empty());
    for arguments in [
        vec!["viewer"],
        vec!["viewer", "--limit", "50"],
        vec!["viewer", "--after", "0"],
    ] {
        let (code, result) = fixture.run(&arguments).await;
        assert_ne!(code, 0);
        assert!(result["error"].is_object());
    }
    assert_eq!(std::fs::read_dir(&fixture.workspace).unwrap().count(), 0);
    assert_eq!(std::fs::read_dir(&fixture.config).unwrap().count(), 0);
}

#[tokio::test]
async fn viewer_cli_serves_real_retained_windows_and_live_changes_without_participating() {
    let fixture = Fixture::new();
    let project = fixture.initialize().await;
    let (handle, task) = fixture.server(&project).await;
    let socket = RuntimePaths::for_project(&project).socket;
    let mut client = Client::connect(&socket, project.identity().id)
        .await
        .unwrap();
    let actor = fixture.context("viewer-author").await;
    mutate(
        &mut client,
        &actor,
        Mutation::Join {
            name: Some("author".into()),
        },
    )
    .await;
    let root = post(&mut client, &actor, "Old root".into()).await["message_id"]
        .as_str()
        .unwrap()
        .to_owned();
    for n in 0..64 {
        post(&mut client, &actor, format!("Root {n:03}")).await;
    }
    let nul = "\0".repeat(65_536);
    let unicode = "💜".repeat(16_384);
    post(&mut client, &actor, nul.clone()).await;
    post(&mut client, &actor, unicode.clone()).await;
    let baseline = client.health().await.unwrap();
    let instance = client.instance_id();
    let mut command = fixture.command(&["viewer", "--json"]);
    command.env("AGENTCHAT_CONTEXT", Uuid::new_v4().to_string());
    let mut child = command.spawn().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap()).lines();
    let ready = tokio::time::timeout(Duration::from_secs(10), output.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let ready: Value = serde_json::from_str(&ready).unwrap();
    assert_eq!(ready["version"], 1);
    assert_eq!(ready["project_id"], project.identity().id.to_string());
    let url = ready["result"]["url"].as_str().unwrap();
    let address: SocketAddr = url
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .parse()
        .unwrap();
    assert_eq!(address.ip(), std::net::Ipv4Addr::LOCALHOST);
    let agents = Http::open(
        address,
        "{agents{items{id handle joined host}nextCursor scanCursor head}}",
        json!({}),
        false,
    )
    .await
    .json()
    .await;
    assert_eq!(agents["agents"]["items"].as_array().unwrap().len(), 1);
    assert_eq!(agents["agents"]["items"][0]["handle"], "author");
    assert_eq!(agents["agents"]["items"][0]["joined"], true);
    assert_eq!(agents["agents"]["items"][0]["host"], "codex");
    let query = "query($before:String,$snapshot:CheckpointInput,$limit:Int!){roots(channel:\"dev\",before:$before,snapshot:$snapshot,limit:$limit){snapshot{projectId seq eventId}items{id body bodyHtml rootId replyCount}nextBefore}}";
    let mut bodies = Vec::new();
    let mut before = Value::Null;
    let mut snapshot = Value::Null;
    while bodies.len() < 50 {
        let page = Http::open(
            address,
            query,
            json!({"before":before,"snapshot":snapshot,"limit":50-bodies.len()}),
            false,
        )
        .await
        .json()
        .await;
        let window = &page["roots"];
        if !snapshot.is_null() {
            assert_eq!(window["snapshot"], snapshot);
        }
        snapshot = window["snapshot"].clone();
        before = window["nextBefore"].clone();
        for message in window["items"].as_array().unwrap() {
            assert!(message["rootId"].is_null());
            bodies.push(message["body"].as_str().unwrap().to_owned());
        }
        assert!(bodies.len() == 50 || !before.is_null());
    }
    assert!(bodies.contains(&nul));
    assert!(bodies.contains(&unicode));
    let mut observer = Client::connect(&socket, project.identity().id)
        .await
        .unwrap();
    let initial = success(
        observer
            .read(Uuid::new_v4(), None, requests::ViewerHead)
            .await
            .unwrap(),
    )
    .unwrap();
    drop(observer);
    let subscription = "subscription($after:CheckpointInput!){changes(after:$after){after{seq}scan{seq}head{seq}hasMore items{seq eventId kind channel{name}message{id body threadId replyCount}}}}";
    let mut stream = Http::open(address, subscription, json!({"after":{"projectId":initial.project_id,"seq":initial.seq,"eventId":initial.event_id}}), true).await;
    let mut writer = Client::connect(&socket, project.identity().id)
        .await
        .unwrap();
    let producer = tokio::spawn(async move {
        mutate(
            &mut writer,
            &actor,
            Mutation::ChannelCreate {
                name: "review".into(),
            },
        )
        .await;
        for n in 0..65 {
            post(&mut writer, &actor, format!("Burst {n:03}")).await;
        }
        mutate(
            &mut writer,
            &actor,
            Mutation::Reply {
                root_message_id: Uuid::parse_str(&root).unwrap(),
                body: "Old root reply".into(),
                read_message_ids: Vec::new(),
            },
        )
        .await;
        root
    });
    let mut identities = BTreeSet::new();
    let mut cursor = initial.seq.get().to_string();
    let mut reply = false;
    let mut channel = false;
    while identities.len() < 67 {
        let batch = next(&mut stream).await;
        assert_eq!(batch["after"]["seq"], cursor);
        cursor = batch["scan"]["seq"].as_str().unwrap().to_owned();
        for item in batch["items"].as_array().unwrap() {
            assert!(identities.insert(item["eventId"].as_str().unwrap().to_owned()));
            channel |= item["channel"]["name"] == "review";
            if item["message"]["body"] == "Old root reply" {
                reply = true;
                assert_eq!(item["message"]["replyCount"], "1");
            }
        }
    }
    let root = producer.await.unwrap();
    assert!(reply && channel);
    let pinned = Http::open(
        address,
        "query($root:ID!){pinnedRoot(channel:\"dev\",rootId:$root){root{id body replyCount}}}",
        json!({"root":root}),
        false,
    )
    .await
    .json()
    .await;
    assert_eq!(pinned["pinnedRoot"]["root"]["replyCount"], "1");
    let pid = child.id().unwrap().to_string();
    assert!(
        tokio::process::Command::new("kill")
            .args(["-INT", &pid])
            .status()
            .await
            .unwrap()
            .success()
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(6), child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    drop(stream);
    let status = client.health().await.unwrap();
    assert_eq!(client.instance_id(), instance);
    assert!(!task.is_finished());
    assert_eq!(
        status.application.statistics.agents,
        baseline.application.statistics.agents
    );
    assert_eq!(status.application.statistics.read_receipts, 0);
    assert!(
        serde_json::to_value(&status.application.statistics)
            .unwrap()
            .get("acknowledged_deliveries")
            .is_none()
    );
    tokio::time::timeout(Duration::from_secs(3), async {
        while client.health().await.unwrap().runtime.activity.observers != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    handle.shutdown();
    task.await.unwrap().unwrap();
}
