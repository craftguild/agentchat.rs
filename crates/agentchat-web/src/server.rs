//! Viewer listener/native resources, independent from shared runtime lifecycle.

use crate::{
    connections::{CloseOnDrop, Connections, LimitedListener},
    http,
};
use agentchat_core::{Error, ErrorCode, Result};
use agentchat_protocol::viewer::ViewerReader;
use axum::Router;
use chigiri::{Endpoint, ShutdownOptions, ShutdownOutcome};
use std::{
    future::{Future, IntoFuture},
    sync::Arc,
    time::Duration,
};
use tokio::{net::TcpListener, sync::oneshot};

/// One loopback-only server with embedded assets and a fixed injected project.
/// CLI composition owns its context-free runtime lease separately.
pub struct Viewer {
    listener: TcpListener,
    router: Router,
    endpoint: Endpoint<bool>,
    url: String,
    connections: Connections,
}
impl Viewer {
    /// Bind an ephemeral IPv4 loopback port. This neither initializes a project
    /// nor opens a browser; all source/build assets are already embedded.
    pub async fn bind(reader: Arc<dyn ViewerReader>) -> Result<Self> {
        Self::bind_with_context(reader, crate::ViewerContext::default()).await
    }
    /// Bind with immutable display metadata supplied by the executable. Metadata
    /// does not select a project or grant any filesystem/read capabilities.
    pub async fn bind_with_context(
        reader: Arc<dyn ViewerReader>,
        context: crate::ViewerContext,
    ) -> Result<Self> {
        Self::bind_with_writer(reader, None, context).await
    }
    /// Bind with an optional explicit user-action capability for this project.
    pub async fn bind_with_writer(
        reader: Arc<dyn ViewerReader>,
        writer: Option<Arc<dyn agentchat_protocol::viewer_write::ViewerWriter>>,
        context: crate::ViewerContext,
    ) -> Result<Self> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(io)?;
        let authority = listener.local_addr().map_err(io)?.to_string();
        let endpoint = crate::endpoint::endpoint_with_writer(reader, writer)
            .map_err(|_| failure("Viewer GraphQL configuration is invalid."))?;
        let router = http::router(endpoint.clone(), authority.clone(), context)
            .map_err(|_| failure("Viewer managed service could not start."))?;
        Ok(Self {
            listener,
            router,
            endpoint,
            url: format!("http://{authority}/"),
            connections: Connections::default(),
        })
    }
    /// Ready same-origin human URL, with no filesystem path or agent descriptor.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Serve until the supplied signal. Chigiri owns SDK supervision and scoped
    /// cancellation. After its bounded public hook, close only this viewer's
    /// native sockets and bound the native server's remaining cleanup.
    pub async fn serve(self, signal: impl Future<Output = ()> + Send + 'static) -> Result<()> {
        let _cleanup = CloseOnDrop(self.connections.clone());
        let (stopped, notified) = oneshot::channel();
        let endpoint = self.endpoint.clone();
        let connections = self.connections.clone();
        let hook = self.endpoint.shutdown_on(
            signal,
            ShutdownOptions::graceful_for(Duration::from_secs(2))
                .expect("fixed finite shutdown budget"),
        );
        let shutdown = async move {
            hook.await;
            connections.close();
            let _ = stopped.send(());
        };
        let native = axum::serve(
            LimitedListener::new(self.listener, self.connections),
            self.router,
        )
        .with_graceful_shutdown(shutdown)
        .into_future();
        tokio::pin!(native);
        let result = tokio::select! {
            result = &mut native => result.map_err(io),
            _ = notified => match tokio::time::timeout(Duration::from_secs(3), &mut native).await {
                Ok(result) => result.map_err(io),
                Err(_) => Err(failure("Viewer HTTP cleanup exceeded its deadline.")),
            },
        };
        // A native error may precede the supplied shutdown signal. The same
        // supported hook requests cancellation without stopping any shared host.
        endpoint
            .shutdown_on(async {}, ShutdownOptions::immediate())
            .await;
        if result.is_ok() && endpoint.shutdown_outcome() != Some(ShutdownOutcome::Drained) {
            return Err(failure("Viewer SDK did not confirm a complete drain."));
        }
        result
    }
}
fn io(error: std::io::Error) -> Error {
    Error::new(ErrorCode::IoError, error.to_string())
}
fn failure(message: &str) -> Error {
    Error::new(ErrorCode::RuntimeUnavailable, message)
}
