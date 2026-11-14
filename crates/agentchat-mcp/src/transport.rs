//! SDK dispatch over bounded, cancellation-safe newline frames.

use crate::Server;
use agentchat_core::{Error, ErrorCode, Result};
use agentchat_protocol::framing::{self, FramedReader};
use rmcp::{
    RoleServer, ServiceExt,
    model::{ClientNotification, JsonRpcMessage, RequestId},
    service::{RxJsonRpcMessage, TxJsonRpcMessage},
    transport::Transport,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, AsyncWriteExt, BufReader},
    sync::{Mutex as AsyncMutex, OwnedSemaphorePermit, Semaphore, watch},
};

type Incoming = RxJsonRpcMessage<RoleServer>;
type Outgoing = TxJsonRpcMessage<RoleServer>;
type Credits = Arc<Mutex<HashMap<RequestId, OwnedSemaphorePermit>>>;
const DEADLINE: Duration = Duration::from_secs(5);

struct Bounded<R, W> {
    reader: FramedReader<BufReader<R>>,
    writer: Arc<AsyncMutex<Option<W>>>,
    pending: Option<Incoming>,
    credits: Credits,
    available: Arc<Semaphore>,
    failed: bool,
    closed: watch::Sender<bool>,
    sends: Arc<Semaphore>,
}
impl<R: AsyncRead + Unpin, W> Bounded<R, W> {
    fn new(reader: R, writer: W) -> Self {
        Self {
            reader: FramedReader::new(BufReader::new(reader)),
            writer: Arc::new(AsyncMutex::new(Some(writer))),
            pending: None,
            credits: Arc::default(),
            available: Arc::new(Semaphore::new(32)),
            failed: false,
            closed: watch::channel(false).0,
            sends: Arc::new(Semaphore::new(32)),
        }
    }
}
impl<R, W> Transport<RoleServer> for Bounded<R, W>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    type Error = Error;
    fn send(&mut self, message: Outgoing) -> impl Future<Output = Result<()>> + Send + 'static {
        let writer = self.writer.clone();
        let credits = self.credits.clone();
        let closed_signal = self.closed.clone();
        let capacity = self.sends.clone().try_acquire_owned();
        async move {
            let Ok(_capacity) = capacity else {
                closed_signal.send_replace(true);
                return Err(closed());
            };
            let id = match &message {
                JsonRpcMessage::Response(response) => Some(response.id.clone()),
                JsonRpcMessage::Error(error) => error.id.clone(),
                _ => None,
            };
            let result = tokio::time::timeout(DEADLINE, async {
                let mut guard = writer.lock().await;
                framing::write(guard.as_mut().ok_or_else(closed)?, &message).await
            })
            .await
            .unwrap_or_else(|_| Err(closed()));
            if result.is_err() {
                closed_signal.send_replace(true);
            }
            if result.is_ok()
                && let Some(id) = id
            {
                credits.lock().expect("MCP request credit lock").remove(&id);
            }
            result
        }
    }
    async fn receive(&mut self) -> Option<Incoming> {
        if self.failed || *self.closed.borrow() {
            return None;
        }
        if self.pending.is_none() {
            let mut closed = self.closed.subscribe();
            if *closed.borrow() {
                self.failed = true;
                return None;
            }
            let next = tokio::select! {
                next = self.reader.read() => next,
                _ = closed.changed() => { self.failed = true; return None; }
            };
            self.pending = match next {
                Ok(Some(message)) => Some(message),
                _ => {
                    self.failed = true;
                    return None;
                }
            };
        }
        if let Some(JsonRpcMessage::Notification(notification)) = &self.pending
            && let ClientNotification::CancelledNotification(cancelled) = &notification.notification
            && let Some(id) = &cancelled.params.request_id
        {
            // The SDK suppresses responses for cancelled requests. Release the
            // inbound credit here; bounded outgoing sends still cap slow writes.
            self.credits
                .lock()
                .expect("MCP request credit lock")
                .remove(id);
        }
        if let Some(JsonRpcMessage::Request(request)) = &self.pending {
            let id = request.id.clone();
            if self
                .credits
                .lock()
                .expect("MCP request credit lock")
                .contains_key(&id)
            {
                self.failed = true;
                return None;
            }
            // Keep the decoded request in self while awaiting capacity: the SDK
            // may cancel receive when a response becomes ready. Neither a frame
            // nor a request permit can disappear at that cancellation boundary.
            let mut closed = self.closed.subscribe();
            if *closed.borrow() {
                self.failed = true;
                return None;
            }
            let permit = tokio::select! {
                permit = self.available.clone().acquire_owned() => permit.ok()?,
                _ = closed.changed() => { self.failed = true; return None; }
            };
            self.credits
                .lock()
                .expect("MCP request credit lock")
                .insert(id, permit);
        }
        self.pending.take()
    }
    async fn close(&mut self) -> Result<()> {
        self.failed = true;
        self.available.close();
        self.pending = None;
        self.credits
            .lock()
            .expect("MCP request credit lock")
            .clear();
        tokio::time::timeout(DEADLINE, async {
            if let Some(mut writer) = self.writer.lock().await.take() {
                writer.shutdown().await.map_err(|_| closed())?;
            }
            Ok(())
        })
        .await
        .map_err(|_| closed())?
    }
}

/// Serve one SDK connection with 1 MiB frames and at most 32 pending requests.
/// EOF, invalid framing and transport closure release its owned attachment.
pub async fn serve<R, W>(server: Server, reader: R, writer: W) -> Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    run(server, reader, writer, false).await
}

/// Serve MCP on standard input/output; stdout contains SDK protocol frames only.
pub async fn stdio(server: Server) -> Result<()> {
    let (reader, writer) = crate::stdio::streams()
        .map_err(|_| Error::new(ErrorCode::IoError, "MCP standard streams are unavailable."))?;
    run(server, reader, writer, true).await
}

async fn run<R, W>(server: Server, reader: R, writer: W, signals: bool) -> Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let retirement = server.retirement();
    tokio::pin!(retirement);
    let started = tokio::select! {
        biased;
        _ = &mut retirement => { server.clear().await; return Ok(()); },
        started = tokio::time::timeout(DEADLINE, server.clone().serve(Bounded::new(reader, writer))) => started,
    };
    let result = match started {
        Ok(Ok(running)) => {
            let cancel = running.cancellation_token();
            let waiting = running.waiting();
            tokio::pin!(waiting);
            tokio::select! {
                biased;
                _ = &mut retirement => {
                    cancel.cancel(); waiting.await.map(|_| ()).map_err(|_| closed())
                }
                result = &mut waiting => result.map(|_| ()).map_err(|_| closed()),
                _ = tokio::signal::ctrl_c(), if signals => {
                    cancel.cancel(); waiting.await.map(|_| ()).map_err(|_| closed())
                }
            }
        }
        _ => Err(Error::new(
            ErrorCode::RuntimeUnavailable,
            "MCP initialization failed or timed out.",
        )),
    };
    server.clear().await;
    result
}
fn closed() -> Error {
    Error::new(
        ErrorCode::IoError,
        "MCP transport closed or exceeded its write deadline.",
    )
}
