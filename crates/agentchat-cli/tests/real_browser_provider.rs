//! Controlled real SQLite/RPC/CLI browser fixture; no product mutation endpoint.
#[path = "viewer_browser/fixture.rs"]
mod fixture;
mod support;

use agentchat_core::application::Mutation;
use agentchat_protocol::rpc::{Call, Client, success};
use agentchat_runtime::daemon::RuntimePaths;
use fixture::BrowserFixture;
use serde::Deserialize;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
enum Control {
    Root {
        body: String,
        count: Option<usize>,
    },
    Reply {
        root: Uuid,
        body: String,
    },
    Channel {
        name: String,
    },
    Participants {
        count: usize,
    },
    Membership {
        index: Option<usize>,
        channel: String,
        joined: bool,
    },
    Restart,
    StopViewer,
    Status,
    Stop,
}
async fn mutate(fixture: &mut BrowserFixture, command: Mutation) -> Value {
    let request_id = Uuid::new_v4();
    success(
        fixture
            .client
            .call(
                request_id,
                Call::Mutate {
                    request_id,
                    actor: agentchat_protocol::rpc::ActorReference::Native {
                        context_id: fixture.actor.context_id,
                    },
                    command,
                },
            )
            .await
            .unwrap(),
    )
    .unwrap()
}
async fn root(fixture: &mut BrowserFixture, body: String) -> Uuid {
    let result = mutate(
        fixture,
        Mutation::Post {
            channel: Some("dev".into()),
            body,
        },
    )
    .await;
    Uuid::parse_str(result["message_id"].as_str().unwrap()).unwrap()
}
async fn reply(fixture: &mut BrowserFixture, root_message_id: Uuid, body: String) {
    mutate(
        fixture,
        Mutation::Reply {
            root_message_id,
            body,
            read_message_ids: Vec::new(),
        },
    )
    .await;
}

async fn participants(fixture: &mut BrowserFixture, count: usize) {
    assert!(count <= 64);
    while fixture.participants.len() > count {
        let context = fixture.participants.pop().unwrap();
        participant_mutation(fixture, context.context_id, Mutation::Leave).await;
    }
    while fixture.participants.len() < count {
        let name = format!("extra-{}", &Uuid::new_v4().simple().to_string()[..12]);
        let context = fixture
            .fixture
            .context(&format!("{name}-{}", Uuid::new_v4()))
            .await;
        participant_mutation(
            fixture,
            context.context_id,
            Mutation::Join { name: Some(name) },
        )
        .await;
        fixture.created_participants += 1;
        fixture.participants.push(context);
    }
}

async fn participant_mutation(fixture: &BrowserFixture, context_id: Uuid, command: Mutation) {
    // Each synthetic participant owns an exact-context RPC connection.
    let mut client = Client::connect(
        RuntimePaths::for_project(&fixture.project).socket,
        fixture.project.identity().id,
    )
    .await
    .unwrap();
    let request_id = Uuid::new_v4();
    success(
        client
            .call(
                request_id,
                Call::Mutate {
                    request_id,
                    actor: agentchat_protocol::rpc::ActorReference::Native { context_id },
                    command,
                },
            )
            .await
            .unwrap(),
    )
    .unwrap();
}

#[tokio::test]
#[ignore = "Explicit browser integration owns stdin and disposable runtime/CLI children"]
async fn real_browser_provider() {
    assert_eq!(
        std::env::var("AGENTCHAT_WEB_BROWSER_PROVIDER").as_deref(),
        Ok("1")
    );
    let mut fixture = BrowserFixture::new().await;
    let joined = mutate(
        &mut fixture,
        Mutation::Join {
            name: Some("author".into()),
        },
    )
    .await;
    fixture.author_id = Uuid::parse_str(joined["agent_id"].as_str().unwrap()).unwrap();
    for n in 0..120 {
        let id = root(&mut fixture, format!("Root {n:03}\n\nHello **AgentChat**.")).await;
        fixture.roots.push(id);
    }
    fixture.thread_root = fixture.roots[119];
    fixture.old_root = fixture.roots[70];
    let thread = fixture.thread_root;
    for n in 0..75 {
        reply(&mut fixture, thread, format!("Reply {n:03}")).await;
    }
    let url = fixture.start_viewer().await;
    println!("AGENTCHAT_BROWSER_URL={url}");
    let mut input = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = input.next_line().await.unwrap() {
        assert!(line.len() <= 1024 * 1024);
        let command: Control = serde_json::from_str(&line).unwrap();
        match command {
            Control::Stop => break,
            Control::Root { body, count } => {
                assert!(body.len() <= 65_536);
                for _ in 0..count.unwrap_or(1).min(100) {
                    root(&mut fixture, body.clone()).await;
                }
            }
            Control::Reply { root, body } => reply(&mut fixture, root, body).await,
            Control::Channel { name } => {
                mutate(&mut fixture, Mutation::ChannelCreate { name }).await;
            }
            Control::Membership {
                index,
                channel,
                joined,
            } => {
                let context = index
                    .map(|i| fixture.participants[i].context_id)
                    .unwrap_or(fixture.actor.context_id);
                let command = if joined {
                    Mutation::ChannelJoin { name: channel }
                } else {
                    Mutation::ChannelLeave { name: channel }
                };
                participant_mutation(&fixture, context, command).await;
            }
            Control::Restart => fixture.restart().await,
            Control::Participants { count } => participants(&mut fixture, count).await,
            Control::StopViewer => fixture.stop_viewer().await,
            Control::Status => {}
        }
        println!("AGENTCHAT_BROWSER_CONTROL={}", fixture.status().await);
    }
    fixture.close().await;
}
