use axum::{
    body::Bytes,
    http::{Request, StatusCode},
};
use http_body_util::{BodyExt, Full};
use hyper::{Response, body::Incoming, client::conn::http1};
use hyper_util::rt::TokioIo;
use serde_json::Value;
use std::net::SocketAddr;
use tokio::{net::TcpStream, task::JoinHandle};

pub struct Http {
    pub response: Response<Incoming>,
    connection: JoinHandle<()>,
    pending: Vec<u8>,
}
impl Http {
    pub async fn open(address: SocketAddr, query: &str, variables: Value, streaming: bool) -> Self {
        let socket = TcpStream::connect(address).await.unwrap();
        let (mut send, connection) = http1::handshake(TokioIo::new(socket)).await.unwrap();
        let connection = tokio::spawn(async move {
            let _ = connection.await;
        });
        let body =
            serde_json::to_vec(&serde_json::json!({"query":query,"variables":variables})).unwrap();
        let request = Request::post("/graphql")
            .header("host", address.to_string())
            .header("content-type", "application/json")
            .header(
                "accept",
                if streaming {
                    "text/event-stream"
                } else {
                    "application/graphql-response+json"
                },
            )
            .body(Full::new(Bytes::from(body)))
            .unwrap();
        let response = send.send_request(request).await.unwrap();
        Self {
            response,
            connection,
            pending: Vec::new(),
        }
    }
    pub async fn json(self) -> Value {
        self.json_status(StatusCode::OK).await
    }
    pub async fn json_status(mut self, expected: StatusCode) -> Value {
        let status = self.response.status();
        let mut bytes = Vec::new();
        while let Some(frame) = self.response.body_mut().frame().await {
            if let Ok(data) = frame.unwrap().into_data() {
                bytes.extend_from_slice(&data);
            }
            assert!(bytes.len() <= 1024 * 1024);
        }
        let value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(status, expected, "{value}");
        value
    }
    pub async fn event(&mut self) -> Option<(String, Option<Value>)> {
        loop {
            if let Some(end) = self.pending.windows(2).position(|part| part == b"\n\n") {
                let bytes: Vec<_> = self.pending.drain(..end + 2).collect();
                let frame = std::str::from_utf8(&bytes).unwrap();
                let event = frame.lines().find_map(|line| line.strip_prefix("event: "));
                let data = frame.lines().find_map(|line| line.strip_prefix("data: "));
                if let Some(event) = event {
                    return Some((
                        event.to_owned(),
                        data.map(|data| serde_json::from_str(data).unwrap()),
                    ));
                }
                continue;
            }
            let frame = self.response.body_mut().frame().await?;
            if let Ok(bytes) = frame.unwrap().into_data() {
                self.pending.extend_from_slice(&bytes);
            }
            assert!(self.pending.len() <= 2 * 1024 * 1024);
        }
    }
}
impl Drop for Http {
    fn drop(&mut self) {
        self.connection.abort();
    }
}
