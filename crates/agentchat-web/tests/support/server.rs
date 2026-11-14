//! Shared owned loopback schema service for HTTP receipt and stream regressions.
use super::reader::{Fixture, Reader};
use agentchat_web::endpoint;
use chigiri::{Endpoint, ShutdownOptions, ShutdownOutcome};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};

pub struct Server {
    pub address: SocketAddr,
    pub endpoint: Endpoint<bool>,
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<std::io::Result<()>>,
}
impl Server {
    pub async fn open(fixture: Arc<Fixture>, options: Option<ShutdownOptions>) -> Self {
        let endpoint = endpoint(Arc::new(Reader(fixture))).unwrap();
        let app = axum::Router::new()
            .route_service("/graphql", chigiri_axum::service(endpoint.clone()).unwrap());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, receive) = oneshot::channel();
        let native = endpoint.clone();
        let task = tokio::spawn(async move {
            if let Some(options) = options {
                axum::serve(listener, app)
                    .with_graceful_shutdown(native.shutdown_on(
                        async {
                            let _ = receive.await;
                        },
                        options,
                    ))
                    .await
            } else {
                axum::serve(listener, app).await
            }
        });
        Self {
            address,
            endpoint,
            stop: Some(stop),
            task,
        }
    }
    pub async fn finish(mut self, hooked: bool) -> Option<ShutdownOutcome> {
        if hooked {
            self.stop.take().unwrap().send(()).unwrap();
            tokio::time::timeout(Duration::from_secs(5), &mut self.task)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        } else {
            self.task.abort();
            let _ = self.task.await;
        }
        self.endpoint.shutdown_outcome()
    }
}
