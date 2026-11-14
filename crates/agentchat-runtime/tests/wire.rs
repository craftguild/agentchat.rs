//! Actual JSON-RPC identifiers, notifications, batch limits and malformed frames.

mod support;
use agentchat_protocol::{
    framing,
    rpc::{Call, Request},
};
use serde_json::{Value, json};
use std::time::Duration;
use support::runtime::{Running, join, mutate, options};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{
        UnixStream,
        unix::{OwnedReadHalf, OwnedWriteHalf},
    },
};
use uuid::Uuid;

struct Wire {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
}
impl Wire {
    async fn raw(r: &Running) -> Self {
        let stream = UnixStream::connect(&r.handle.paths().socket).await.unwrap();
        let (read, write) = stream.into_split();
        Self {
            reader: BufReader::new(read),
            writer: write,
        }
    }
    async fn connected(r: &Running) -> Self {
        let mut wire = Self::raw(r).await;
        wire.send(json!({"jsonrpc":"2.0","id":"hello","method":"handshake","params":{"version":1,"project_id":r.project.identity().id}})).await;
        assert_eq!(wire.next().await["id"], "hello");
        wire
    }
    async fn send(&mut self, value: Value) {
        framing::write(&mut self.writer, &value).await.unwrap();
    }
    async fn next(&mut self) -> Value {
        self.next_with_budget(Duration::from_secs(3)).await
    }
    async fn next_with_budget(&mut self, budget: Duration) -> Value {
        tokio::time::timeout(budget, framing::read::<_, Value>(&mut self.reader))
            .await
            .unwrap()
            .unwrap()
            .unwrap()
    }
}
fn health(id: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"internal.health"})
}

#[tokio::test]
async fn an_incompatible_component_handshake_cannot_pipeline_a_mutation() {
    let r = Running::new(options()).await;
    let context = r.context("compatible-original-participant").await;
    let mut client = r.client().await;
    join(&mut client, &context, "original").await;
    let before = support::runtime::health(&mut client).await;
    let mut incompatible = Wire::raw(&r).await;
    let hello = json!({"jsonrpc":"2.0","id":"future-image","method":"handshake",
        "params":{"version":999,"project_id":r.project.identity().id}});
    let mutation = Request::new(
        Uuid::new_v4(),
        &Call::Mutate {
            request_id: Uuid::new_v4(),
            actor: agentchat_protocol::rpc::ActorReference::Native {
                context_id: context.context_id,
            },
            command: agentchat_core::application::Mutation::Post {
                channel: Some("dev".into()),
                body: "must never be committed".into(),
            },
        },
    )
    .unwrap();
    // An otherwise valid retained actor cannot bypass failed compatibility by
    // queueing a business operation behind its unsupported initial handshake.
    let mut frames = Vec::new();
    framing::write(&mut frames, &hello).await.unwrap();
    framing::write(&mut frames, &mutation).await.unwrap();
    incompatible.writer.write_all(&frames).await.unwrap();
    let refusal = incompatible.next().await;
    assert_eq!(refusal["id"], "future-image");
    assert_eq!(refusal["error"]["data"]["code"], "unsupported_protocol");
    let mut trailing = Vec::new();
    let closed = tokio::time::timeout(
        Duration::from_secs(3),
        incompatible.reader.read_until(b'\n', &mut trailing),
    )
    .await
    .unwrap();
    assert!(
        matches!(closed, Ok(0))
            || closed.is_err_and(|error| error.kind() == std::io::ErrorKind::ConnectionReset)
    );
    assert!(trailing.is_empty());
    let after = support::runtime::health(&mut client).await;
    assert_eq!(after["statistics"], before["statistics"]);
    assert_eq!(after["statistics"]["messages"], 0);
    assert_eq!(after["statistics"]["joined"], 1);
    drop(incompatible);
    drop(client);
    r.stop().await;
}

#[tokio::test]
async fn numeric_string_and_null_ids_are_echoed_and_notifications_never_respond() {
    let r = Running::new(options()).await;
    let mut wire = Wire::connected(&r).await;
    for id in [json!(17), json!("17"), Value::Null] {
        wire.send(health(id.clone())).await;
        let reply = wire.next().await;
        assert_eq!(reply["id"], id);
        assert!(reply.get("result").is_some());
        assert!(reply.get("error").is_none());
    }
    let mut notification = health(Value::Null);
    notification.as_object_mut().unwrap().remove("id");
    wire.send(notification.clone()).await;
    notification["method"] = "no.such.method".into();
    wire.send(notification).await;
    wire.send(health(json!("barrier"))).await;
    assert_eq!(wire.next().await["id"], "barrier");
    drop(wire);
    r.stop().await;
}

#[tokio::test]
async fn finite_batches_preserve_per_request_errors_and_notification_silence() {
    let r = Running::new(options()).await;
    let mut wire = Wire::connected(&r).await;
    let mut notice = health(Value::Null);
    notice.as_object_mut().unwrap().remove("id");
    wire.send(json!([health(json!(1)),notice.clone(),{"jsonrpc":"2.0","method":"missing","id":"bad"},7,health(Value::Null)])).await;
    let replies = wire.next().await;
    let replies = replies.as_array().unwrap();
    assert_eq!(replies.len(), 4);
    assert_eq!(replies[0]["id"], 1);
    assert_eq!(replies[1]["error"]["code"], -32601);
    assert_eq!(replies[2]["error"]["code"], -32600);
    assert_eq!(replies[2]["id"], Value::Null);
    assert!(replies[3].get("result").is_some());
    wire.send(json!([notice.clone(), notice])).await;
    wire.send(health(json!("after-notices"))).await;
    assert_eq!(wire.next().await["id"], "after-notices");
    for batch in [json!([]), json!(vec![health(json!(1)); 33])] {
        wire.send(batch).await;
        assert_eq!(wire.next().await["error"]["code"], -32600);
    }
    drop(wire);
    r.stop().await;
}

#[tokio::test]
async fn large_batch_results_are_bounded_without_losing_successful_mutations() {
    let mut opts = options();
    opts.io_timeout = agentchat_runtime::daemon::RuntimeOptions::default().io_timeout;
    // This response follows two maximum-size queries and one durable mutation.
    // Its functional guard covers all three operations at the normal I/O budget.
    let batch_budget = opts.io_timeout * 3;
    let r = Running::new(opts).await;
    let context = r.context("batch-author").await;
    let mut author = r.client().await;
    join(&mut author, &context, "alice").await;
    for _ in 0..12 {
        mutate(
            &mut author,
            &context,
            agentchat_core::application::Mutation::Post {
                channel: Some("dev".into()),
                body: "z".repeat(65_536),
            },
        )
        .await;
    }
    let mut wire = Wire::connected(&r).await;
    let history = json!({"jsonrpc":"2.0","id":"large","method":"application.query","params":{"query":{"operation":"history"}}});
    let key = Uuid::new_v4();
    let mutation = Request::new(
        Uuid::new_v4(),
        &Call::Mutate {
            request_id: key,
            actor: agentchat_protocol::rpc::ActorReference::Native {
                context_id: context.context_id,
            },
            command: agentchat_core::application::Mutation::Post {
                channel: Some("dev".into()),
                body: "batch mutation".into(),
            },
        },
    )
    .unwrap();
    wire.send(json!([history.clone(), history, mutation])).await;
    let response = wire.next_with_budget(batch_budget).await;
    let bytes = serde_json::to_vec(&response).unwrap();
    assert!(bytes.len() < framing::MAX_FRAME_BYTES);
    assert!(response[0].get("result").is_some());
    assert_eq!(response[1]["error"]["code"], -32000);
    assert_eq!(response[2]["result"]["request_id"], key.to_string());
    wire.send(health(json!("check"))).await;
    assert_eq!(
        wire.next().await["result"]["result"]["statistics"]["messages"],
        13
    );
    drop(wire);
    drop(author);
    r.stop().await;
}

#[tokio::test]
async fn parse_invalid_request_method_and_parameter_errors_remain_distinct() {
    let r = Running::new(options()).await;
    let mut wire = Wire::connected(&r).await;
    for (request, code) in [
        (
            json!({"jsonrpc":"1.0","method":"application.query","id":1}),
            -32600,
        ),
        (json!({"jsonrpc":"2.0","method":"missing","id":2}), -32601),
        (
            json!({"jsonrpc":"2.0","method":"application.query","params":{"query":{"operation":"arbitrary"}},"id":3}),
            -32602,
        ),
    ] {
        wire.send(request).await;
        assert_eq!(wire.next().await["error"]["code"], code);
    }
    wire.writer.write_all(b"{\n").await.unwrap();
    assert_eq!(wire.next().await["error"]["code"], -32700);
    drop(wire);
    let mut first = Wire::raw(&r).await;
    first.writer.write_all(b"{\n").await.unwrap();
    assert_eq!(first.next().await["error"]["code"], -32700);
    drop(first);
    r.stop().await;
}

#[tokio::test]
async fn incomplete_handshakes_timeout_and_oversized_commands_are_closed() {
    let mut opts = options();
    opts.io_timeout = Duration::from_millis(100);
    let r = Running::new(opts).await;
    let mut incomplete = Wire::raw(&r).await;
    incomplete.writer.write_all(b"{\"jsonrpc\":").await.unwrap();
    let mut byte = [0];
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), incomplete.reader.read(&mut byte))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    let mut wire = Wire::connected(&r).await;
    let mut oversized = vec![b' '; framing::MAX_FRAME_BYTES];
    oversized.push(b'\n');
    wire.writer.write_all(&oversized).await.unwrap();
    assert_eq!(wire.next().await["error"]["code"], -32700);
    drop(wire);
    drop(incomplete);
    r.stop().await;
}

#[tokio::test]
async fn connection_capacity_is_bounded_and_a_disconnected_slot_is_reusable() {
    let r = Running::new(options()).await;
    let mut held = Vec::new();
    for _ in 0..32 {
        held.push(Wire::connected(&r).await);
    }
    let mut excess = Wire::raw(&r).await;
    let mut byte = [0];
    let read = tokio::time::timeout(Duration::from_secs(2), excess.reader.read(&mut byte))
        .await
        .unwrap();
    assert!(matches!(read, Ok(0) | Err(_)));
    drop(held.pop());
    tokio::time::sleep(Duration::from_millis(50)).await;
    let mut reused = Wire::connected(&r).await;
    reused.send(health(json!("reused"))).await;
    assert_eq!(reused.next().await["id"], "reused");
    drop(reused);
    drop(excess);
    drop(held);
    r.stop().await;
}
