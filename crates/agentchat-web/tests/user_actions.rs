//! Same-origin write admission and maximum escaped payloads through real HTTP.
mod support;
use agentchat_core::Result;
use agentchat_protocol::viewer_write::{CreatedChannel, PostedMessage, ViewerWriter};
use agentchat_web::{Viewer, ViewerContext};
use async_trait::async_trait;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use support::reader::{Fixture, Reader};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::oneshot,
};
use uuid::Uuid;

#[derive(Default)]
struct Writer {
    calls: AtomicUsize,
    bytes: AtomicUsize,
}
#[async_trait]
impl ViewerWriter for Writer {
    async fn create_channel(&self, _: Uuid, name: String) -> Result<CreatedChannel> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(CreatedChannel {
            channel_id: Uuid::nil(),
            name,
        })
    }
    async fn post(&self, _: Uuid, _: String, body: String) -> Result<PostedMessage> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.bytes.store(body.len(), Ordering::SeqCst);
        Ok(PostedMessage {
            message_id: Uuid::nil(),
            warnings: Vec::new(),
        })
    }
    async fn reply(&self, id: Uuid, _: Uuid, body: String) -> Result<PostedMessage> {
        self.post(id, "dev".into(), body).await
    }
}
async fn request(authority: &str, headers: &str, body: &str) -> String {
    let mut socket = TcpStream::connect(authority).await.unwrap();
    let request = format!(
        "POST /graphql HTTP/1.1\r\nHost: {authority}\r\n{headers}Accept: application/graphql-response+json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    socket.write_all(request.as_bytes()).await.unwrap();
    let mut output = Vec::new();
    socket.read_to_end(&mut output).await.unwrap();
    String::from_utf8(output).unwrap()
}

#[tokio::test]
async fn only_same_origin_json_can_write_and_escaped_maximum_body_is_complete() {
    let writer = Arc::new(Writer::default());
    let viewer = Viewer::bind_with_writer(
        Arc::new(Reader(Fixture::new())),
        Some(writer.clone()),
        ViewerContext::default(),
    )
    .await
    .unwrap();
    let authority = viewer
        .url()
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_string();
    let (stop, receive) = oneshot::channel();
    let task = tokio::spawn(viewer.serve(async {
        let _ = receive.await;
    }));
    let body = serde_json::json!({"query":"mutation($id:ID!,$body:String!){postMessage(requestId:$id,channel:\"dev\",body:$body){id:messageId warnings{code handle}}}","variables":{"id":Uuid::new_v4(),"body":"\0".repeat(65536)}}).to_string();
    let small = serde_json::json!({"query":"mutation($id:ID!){postMessage(requestId:$id,channel:\"dev\",body:\"hello\"){messageId}}","variables":{"id":Uuid::new_v4()}}).to_string();
    let denied = request(&authority, "Content-Type: application/json\r\n", &small).await;
    assert!(denied.contains("errors"));
    assert_eq!(writer.calls.load(Ordering::SeqCst), 0);
    let denied = request(
        &authority,
        "Origin: http://attacker.invalid\r\nContent-Type: application/json\r\n",
        &small,
    )
    .await;
    assert!(denied.starts_with("HTTP/1.1 403"));
    assert_eq!(writer.calls.load(Ordering::SeqCst), 0);
    let result = request(
        &authority,
        &format!("Origin: http://{authority}\r\nContent-Type: application/json\r\n"),
        &body,
    )
    .await;
    assert!(result.starts_with("HTTP/1.1 200"), "{result}");
    assert!(!result.contains("errors"), "{result}");
    assert_eq!(writer.calls.load(Ordering::SeqCst), 1);
    assert_eq!(writer.bytes.load(Ordering::SeqCst), 65536);
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
}
