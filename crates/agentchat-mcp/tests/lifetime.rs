//! A retired host owner closes SDK state rather than lending it to another root.
mod support;

use agentchat_core::{ErrorCode, Result};
use agentchat_mcp::{ConnectionLifetime, Server};
use agentchat_protocol::operations::{Attachment, Backend, Operation, Target};
use async_trait::async_trait;
use rmcp::{
    model::{ClientRequest, Request},
    service::PeerRequestOptions,
};
use serde_json::{Value, json};
use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};
use support::*;
use tokio::{
    io::AsyncReadExt,
    sync::{Notify, Semaphore},
};
use uuid::Uuid;

#[tokio::test]
async fn retirement_before_initialization_or_restore_never_invokes_a_backend() {
    let backend = Arc::new(Fake::default());
    let lifetime = ConnectionLifetime::default();
    let server = Server::with_lifetime(backend.clone(), &lifetime);
    lifetime.retire();
    let context_id = Uuid::new_v4();
    assert_eq!(
        server
            .restore(Target {
                context_id: Some(context_id),
                ..Target::default()
            })
            .await
            .unwrap_err()
            .code,
        ErrorCode::SessionRequired
    );
    let (mut client, socket) = tokio::io::duplex(8192);
    let (read, write) = tokio::io::split(socket);
    tokio::time::timeout(
        Duration::from_secs(2),
        agentchat_mcp::serve(server, read, write),
    )
    .await
    .unwrap()
    .unwrap();
    let mut byte = [0];
    assert_eq!(client.read(&mut byte).await.unwrap(), 0);
    assert!(backend.calls.lock().unwrap().is_empty());
    assert_eq!(backend.attached.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn retirement_drops_a_joined_attachment_and_cannot_rebind_the_old_server() {
    let backend = Arc::new(Fake::default());
    let lifetime = ConnectionLifetime::default();
    let session = Session::from_server(Server::with_lifetime(backend.clone(), &lifetime)).await;
    let first = Uuid::new_v4();
    assert!(session.join(first).await.get("result").is_some());
    assert_eq!(backend.attached.load(Ordering::SeqCst), 1);
    let Session {
        server,
        client,
        task,
    } = session;
    lifetime.retire();
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(backend.attached.load(Ordering::SeqCst), 0);
    assert!(
        client
            .call_tool(params("agentchat_whoami", json!({})))
            .await
            .is_err()
    );
    let other = Uuid::new_v4();
    assert_eq!(
        server
            .restore(Target {
                context_id: Some(other),
                ..Target::default()
            })
            .await
            .unwrap_err()
            .code,
        ErrorCode::SessionRequired
    );
    assert_eq!(backend.calls.lock().unwrap().len(), 1);
    // A fresh root gets a fresh SDK server and lifetime, never a reset of the
    // retired object's binding or unresolved request intent.
    let fresh_lifetime = ConnectionLifetime::default();
    let fresh = Session::from_server(Server::with_lifetime(backend.clone(), &fresh_lifetime)).await;
    assert!(fresh.join(other).await.get("result").is_some());
    assert_eq!(
        backend.calls.lock().unwrap().last().unwrap().1.context_id,
        Some(other)
    );
    fresh.close().await;
    assert_eq!(backend.attached.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn dropping_the_last_authority_closes_an_idle_connection_without_helper_renewal() {
    let backend = Arc::new(Fake::default());
    let lifetime = ConnectionLifetime::default();
    let another_authority = lifetime.clone();
    let session = Session::from_server(Server::with_lifetime(backend.clone(), &lifetime)).await;
    drop(lifetime);
    assert!(
        envelope(
            session
                .call("agentchat_doctor", json!({"project_path":"/explicit"}))
                .await
        )
        .get("result")
        .is_some()
    );
    drop(another_authority);
    let Session { task, .. } = session;
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(backend.calls.lock().unwrap().len(), 1);
    assert_eq!(backend.attached.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn retirement_cancels_a_blocked_join_without_retry_or_attachment() {
    let backend = Arc::new(Fake::default());
    backend.block_join.store(true, Ordering::SeqCst);
    let lifetime = ConnectionLifetime::default();
    let session = Session::from_server(Server::with_lifetime(backend.clone(), &lifetime)).await;
    let request_id = Uuid::new_v4();
    let context = Uuid::new_v4();
    let request = ClientRequest::CallToolRequest(Request::new(params(
        "agentchat_join",
        json!({"context_id":context,"request_id":request_id}),
    )));
    let handle = session
        .client
        .peer()
        .send_request_with_option(request, PeerRequestOptions::default())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), backend.entered.notified())
        .await
        .unwrap();
    lifetime.retire();
    let Session { server, task, .. } = session;
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    drop(handle);
    {
        let calls = backend.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, request_id);
        assert_eq!(calls[0].1.context_id, Some(context));
    }
    assert_eq!(backend.attached.load(Ordering::SeqCst), 0);
    assert_eq!(
        server
            .restore(Target {
                context_id: Some(context),
                ..Target::default()
            })
            .await
            .unwrap_err()
            .code,
        ErrorCode::SessionRequired
    );
    // A real submitted application operation may already have committed. This
    // proves connection cleanup, not rollback of an external durable effect.
}

struct LateAttach {
    backend: Arc<Fake>,
    entered: Notify,
    release: Semaphore,
}
#[async_trait]
impl Backend for LateAttach {
    async fn invoke(
        &self,
        id: Uuid,
        target: Target,
        operation: Operation,
    ) -> agentchat_protocol::envelope::Envelope<Value> {
        self.backend.invoke(id, target, operation).await
    }
    async fn attach(&self, target: Target) -> Result<Box<dyn Attachment>> {
        self.entered.notify_one();
        self.release.acquire().await.unwrap().forget();
        self.backend.attach(target).await
    }
}

#[tokio::test]
async fn retirement_during_restore_drops_the_late_attachment_without_joining() {
    let backend = Arc::new(Fake::default());
    let gate = Arc::new(LateAttach {
        backend: backend.clone(),
        entered: Notify::new(),
        release: Semaphore::new(0),
    });
    let lifetime = ConnectionLifetime::default();
    let server = Server::with_lifetime(gate.clone(), &lifetime);
    let restore = tokio::spawn(async move {
        server
            .restore(Target {
                context_id: Some(Uuid::new_v4()),
                ..Target::default()
            })
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), gate.entered.notified())
        .await
        .unwrap();
    lifetime.retire();
    gate.release.add_permits(1);
    assert_eq!(
        restore.await.unwrap().unwrap_err().code,
        ErrorCode::SessionRequired
    );
    assert_eq!(backend.attached.load(Ordering::SeqCst), 0);
    assert!(backend.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn retirement_unblocks_an_initial_handshake_already_waiting_for_a_frame() {
    use std::{
        pin::Pin,
        task::{Context, Poll},
    };
    use tokio::io::{AsyncRead, ReadBuf};
    struct Started<R> {
        inner: R,
        entered: Arc<Notify>,
    }
    impl<R: AsyncRead + Unpin> AsyncRead for Started<R> {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            self.entered.notify_one();
            Pin::new(&mut self.inner).poll_read(cx, buf)
        }
    }
    let backend = Arc::new(Fake::default());
    let lifetime = ConnectionLifetime::default();
    let (mut client, socket) = tokio::io::duplex(8192);
    let (read, write) = tokio::io::split(socket);
    let entered = Arc::new(Notify::new());
    let read = Started {
        inner: read,
        entered: entered.clone(),
    };
    let task = tokio::spawn(agentchat_mcp::serve(
        Server::with_lifetime(backend.clone(), &lifetime),
        read,
        write,
    ));
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    lifetime.retire();
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(client.read(&mut [0]).await.unwrap(), 0);
    assert!(backend.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn retirement_with_slow_stdout_preserves_request_and_write_bounds() {
    use agentchat_protocol::framing::{self, FramedReader};
    use tokio::io::{AsyncWriteExt, BufReader};
    let backend = Arc::new(Fake::default());
    *backend.result.lock().unwrap() = Some(json!({"items":[{"body":"x".repeat(65_536)}]}));
    let lifetime = ConnectionLifetime::default();
    let (client, socket) = tokio::io::duplex(8192);
    let (read, write) = tokio::io::split(socket);
    let task = tokio::spawn(agentchat_mcp::serve(
        Server::with_lifetime(backend.clone(), &lifetime),
        read,
        write,
    ));
    let (read, mut write) = tokio::io::split(client);
    let mut read = FramedReader::new(BufReader::new(read));
    framing::write(&mut write, &json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{
        "protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"retire-slow-test","version":"1"}
    }})).await.unwrap();
    let initialized: Value = read.read().await.unwrap().unwrap();
    assert!(initialized.get("result").is_some());
    framing::write(
        &mut write,
        &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
    )
    .await
    .unwrap();
    let sender = tokio::spawn(async move {
        for id in 1..=100 {
            if framing::write(
                &mut write,
                &json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{
                    "name":"agentchat_history","arguments":{"project_path":"/explicit"}
                }}),
            )
            .await
            .is_err()
            {
                break;
            }
        }
        let _ = write.shutdown().await;
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while backend.calls.lock().unwrap().len() < 32 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    lifetime.retire();
    // An already blocked stdout write retains the ordinary five-second I/O
    // deadline. Retirement cannot release credits into more business calls.
    tokio::time::timeout(Duration::from_secs(8), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    sender.await.unwrap();
    assert_eq!(backend.calls.lock().unwrap().len(), 32);
    assert_eq!(backend.attached.load(Ordering::SeqCst), 0);
}
