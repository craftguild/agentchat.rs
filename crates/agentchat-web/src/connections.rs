//! Bound native socket owners separately from the SDK's scoped subscription work.

use axum::serve::Listener;
use std::{
    collections::HashMap,
    io,
    net::{Shutdown, SocketAddr},
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{TcpListener, TcpStream},
    sync::{OwnedSemaphorePermit, Semaphore},
};
use uuid::Uuid;

const MAX_CONNECTIONS: usize = 16;
#[derive(Default)]
struct State {
    closed: bool,
    sockets: HashMap<Uuid, std::net::TcpStream>,
}
#[derive(Clone, Default)]
pub(crate) struct Connections(Arc<Mutex<State>>);
impl Connections {
    pub fn close(&self) {
        let mut state = self.0.lock().expect("viewer socket registry");
        state.closed = true;
        for socket in state.sockets.values() {
            let _ = socket.shutdown(Shutdown::Both);
        }
    }
}
pub(crate) struct CloseOnDrop(pub Connections);
impl Drop for CloseOnDrop {
    fn drop(&mut self) {
        self.0.close();
    }
}

pub(crate) struct LimitedListener {
    listener: TcpListener,
    slots: Arc<Semaphore>,
    connections: Connections,
}
impl LimitedListener {
    pub fn new(listener: TcpListener, connections: Connections) -> Self {
        Self {
            listener,
            slots: Arc::new(Semaphore::new(MAX_CONNECTIONS)),
            connections,
        }
    }
    fn track(&self, socket: TcpStream, permit: OwnedSemaphorePermit) -> io::Result<Socket> {
        // The duplicate FD belongs only to this viewer, allowing bounded native
        // cleanup after the public SDK hook. No shared daemon/host is signalled.
        let native = socket.into_std()?;
        let control = native.try_clone()?;
        let stream = TcpStream::from_std(native)?;
        let id = Uuid::new_v4();
        let mut registry = self.connections.0.lock().expect("viewer socket registry");
        if registry.closed {
            let _ = control.shutdown(Shutdown::Both);
        }
        registry.sockets.insert(id, control);
        Ok(Socket {
            stream,
            id,
            connections: self.connections.clone(),
            _permit: permit,
        })
    }
}
impl Listener for LimitedListener {
    type Io = Socket;
    type Addr = SocketAddr;
    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            let permit = self
                .slots
                .clone()
                .acquire_owned()
                .await
                .expect("private admission remains open");
            match self.listener.accept().await {
                Ok((socket, address)) => {
                    if let Ok(socket) = self.track(socket, permit) {
                        return (socket, address);
                    }
                }
                Err(_) => {
                    drop(permit);
                }
            }
            // Back off native accept/FD errors instead of spinning on failure.
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }
    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.listener.local_addr()
    }
}
pub(crate) struct Socket {
    stream: TcpStream,
    id: Uuid,
    connections: Connections,
    _permit: OwnedSemaphorePermit,
}
impl Drop for Socket {
    fn drop(&mut self) {
        self.connections
            .0
            .lock()
            .expect("viewer socket registry")
            .sockets
            .remove(&self.id);
    }
}
impl AsyncRead for Socket {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}
impl AsyncWrite for Socket {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write(cx, bytes)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}
