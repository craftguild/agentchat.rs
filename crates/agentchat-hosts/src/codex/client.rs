//! One bounded ordered native stream. Any failed request retires its connection.
mod input;
use super::{invalid, required, state, thread_id, unavailable};
use agentchat_core::{Error, ErrorCode, Result, ports::HostState};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};
use tokio::{net::UnixStream, time::timeout};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{Message, protocol::WebSocketConfig},
};

const LIMIT: usize = 1024 * 1024;
type Socket = WebSocketStream<UnixStream>;

/// Initialized control connection to an existing same-user shared server.
/// `&mut self` serializes both native APIs. The caller must also serialize
/// reconnections/retries for one recipient and retain a failed head before
/// advancing. Connection success never proves CLI liveness or admits a version.
pub struct Client {
    socket: Option<Socket>,
    version: String,
    counter: u64,
}
impl Client {
    /// Connect only to the supplied existing Unix socket; start no native server.
    /// Check the peer user and actual initialization version before any thread API.
    pub async fn connect(path: &Path, expected_version: &str) -> Result<Self> {
        if !path.is_absolute() || expected_version.is_empty() || expected_version.len() > 128 {
            return Err(invalid());
        }
        let socket = timeout(Duration::from_secs(5), async {
            let socket = UnixStream::connect(path).await.map_err(|_| unavailable())?;
            if socket.peer_cred().map_err(|_| unavailable())?.uid()
                != rustix::process::geteuid().as_raw()
            {
                return Err(required());
            }
            let config = WebSocketConfig::default()
                .max_message_size(Some(LIMIT))
                .max_frame_size(Some(LIMIT));
            tokio_tungstenite::client_async_with_config("ws://localhost/", socket, Some(config))
                .await
                .map(|(socket, _)| socket)
                .map_err(|_| unavailable())
        })
        .await
        .map_err(|_| unavailable())??;
        let mut client = Self {
            socket: Some(socket),
            version: expected_version.into(),
            counter: 0,
        };
        let initialized = client
            .request(
                "initialize",
                json!({
                    "clientInfo":{"name":"agentchat","title":"AgentChat","version":env!("CARGO_PKG_VERSION")},
                    "capabilities":{"experimentalApi":true}
                }),
            )
            .await
            .map_err(|mut error| {
                if error.code == ErrorCode::UnsupportedProtocol {
                    // The selected executable label is not a live observation.
                    error.details["observed_version"] = Value::Null;
                }
                error
            })?;
        let actual = initialized
            .get("userAgent")
            .and_then(Value::as_str)
            .and_then(|s| s.split_whitespace().next())
            .and_then(|s| s.rsplit('/').next())
            .filter(|version| !version.is_empty());
        let actual = client
            .decoded("initialize.result.userAgent", actual.ok_or_else(invalid))
            .map_err(|mut error| {
                // No server label was observed. Keep the selected label for
                // diagnostics without presenting it as a live observation.
                error.details["observed_version"] = Value::Null;
                error
            })?;
        if actual != expected_version {
            return Err(Error::new(
                ErrorCode::UnsupportedHost,
                "The shared Codex server version differs from the selected native executable.",
            ));
        }
        client.notification("initialized", json!({})).await?;
        Ok(client)
    }

    /// Actual initialized native version, without an automatic-wake claim.
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Create an exact root using inherited host defaults, without generation.
    /// One passive startup item materializes its resumable rollout. No model,
    /// sandbox, approval, collaboration or service policy override is supplied.
    pub async fn create_thread(&mut self, directory: &Path) -> Result<String> {
        if !directory.is_absolute() {
            return Err(invalid());
        }
        let text = directory.to_str().ok_or_else(invalid)?;
        let result = self.request("thread/start", json!({"cwd":text})).await?;
        let root = self
            .decoded(
                "thread/start.result.thread.id",
                result
                    .pointer("/thread/id")
                    .and_then(Value::as_str)
                    .filter(|id| thread_id(id))
                    .ok_or_else(invalid),
            )?
            .to_owned();
        self.inject(&root, "AgentChat launch context. Startup participation is managed outside the model. No task is requested; do not issue a join operation.").await?;
        Ok(root)
    }

    /// Read metadata only, without resume, subscription or presence renewal.
    pub async fn observe(&mut self, root: &str) -> Result<HostState> {
        if !thread_id(root) {
            return Err(invalid());
        }
        let result = self
            .request("thread/read", json!({"threadId":root,"includeTurns":false}))
            .await?;
        self.decoded(
            "thread/read.result.thread.status",
            state::decode(root, result),
        )
    }

    /// Inspect only the exact thread's existing AGENTCHAT MCP connection/catalog. A
    /// connected current-version inventory is integration health, not frontend
    /// readiness, conversation authority, participation or automatic-wake
    /// admission. Missing/starting/failed inventory cannot complete startup.
    /// A timeout retires this connection without assuming success or failure of
    /// the host's own initialization; a later independent observation may retry.
    pub async fn integration_ready(&mut self, root: &str) -> Result<bool> {
        self.integration_ready_scoped(root, None).await
    }
    /// Also require the effective native MCP entry to use the selected AGENTCHAT
    /// namespace. A healthy entry configured for another home cannot release
    /// this launch's first input or supply its membership operations.
    pub async fn integration_ready_for(&mut self, root: &str, namespace: &str) -> Result<bool> {
        self.integration_ready_scoped(root, Some(namespace)).await
    }
    /// Read exact native scope and owned MCP readiness for direct startup. This
    /// is combined with the actual launched child and shared StartLaunch; cached
    /// metadata alone is never process authority or completed participation.
    pub async fn ready(
        &mut self,
        root: &str,
        directory: &Path,
        namespace: &str,
    ) -> Result<super::CodexReady> {
        if !thread_id(root) || !directory.is_absolute() {
            return Err(invalid());
        }
        let result = self
            .request("thread/read", json!({"threadId":root,"includeTurns":false}))
            .await?;
        // Keep malformed native controls distinct from a missing cwd. Neither
        // can authorize startup, and diagnostics must name the failing surface.
        let state = self.decoded(
            "thread/read.result.thread.status",
            state::decode(root, result.clone()),
        )?;
        let version = self.version.clone();
        let ready = self.decoded(
            "thread/read.result.thread.cwd",
            super::CodexReady::decode(root, directory, &version, result, state),
        )?;
        if !self.integration_ready_for(root, namespace).await? {
            return Err(super::ready::waiting());
        }
        Ok(ready)
    }
    /// Read a successful foreground transition target without trusting the CLI
    /// process cwd. The native-reported absolute cwd is returned for canonical
    /// project validation before any target descriptor can be published.
    pub async fn ready_observed(
        &mut self,
        root: &str,
        namespace: &str,
    ) -> Result<super::CodexReady> {
        let ready = self.observe_target(root).await?;
        if !self.integration_ready_for(root, namespace).await? {
            return Err(super::ready::waiting());
        }
        Ok(ready)
    }
    /// Read only the exact successful transition target and its native cwd.
    /// Same-project activation must separately require scoped MCP readiness.
    pub async fn observe_target(&mut self, root: &str) -> Result<super::CodexReady> {
        if !thread_id(root) {
            return Err(invalid());
        }
        let result = self
            .request("thread/read", json!({"threadId":root,"includeTurns":false}))
            .await?;
        let state = self.decoded(
            "thread/read.result.thread.status",
            state::decode(root, result.clone()),
        )?;
        let version = self.version.clone();
        let ready = self.decoded(
            "thread/read.result.thread.cwd",
            super::CodexReady::decode_observed(root, &version, result, state),
        )?;
        Ok(ready)
    }
    async fn integration_ready_scoped(
        &mut self,
        root: &str,
        namespace: Option<&str>,
    ) -> Result<bool> {
        if !thread_id(root) {
            return Err(invalid());
        }
        let result = self
            .request(
                "mcpServerStatus/list",
                json!({"threadId":root,"serverName":"agentchat","detail":"toolsAndAuthOnly","limit":1}),
            )
            .await?;
        self.decoded(
            "mcpServerStatus/list.result.data",
            super::integration::decode(&result, namespace),
        )
    }

    async fn inject(&mut self, root: &str, text: &str) -> Result<()> {
        self.request("thread/inject_items", json!({"threadId":root,"items":[{"type":"message","role":"user","content":[{"type":"input_text","text":text}]}]})).await.map(|_| ())
    }
    fn decoded<T>(&mut self, api: &str, value: Result<T>) -> Result<T> {
        match value {
            Ok(value) => Ok(value),
            Err(mut error) => {
                // A syntactically matching response can still leave acceptance or
                // exact-root attribution unknown. Never reuse that connection for
                // a successor body after its receipt/identity failed validation.
                self.socket = None;
                if error.code == ErrorCode::UnsupportedProtocol {
                    error.details = json!({
                        "host":"codex",
                        "host_version":self.version,
                        "observed_version":self.version,
                        "missing_api":api,
                        "compatibility":"best_effort_api_surface",
                    });
                }
                Err(error)
            }
        }
    }
    async fn notification(&mut self, method: &str, params: Value) -> Result<()> {
        let mut socket = self.socket.take().ok_or_else(unavailable)?;
        timeout(
            Duration::from_secs(5),
            socket.send(Message::Text(
                json!({"method":method,"params":params}).to_string().into(),
            )),
        )
        .await
        .map_err(|_| unavailable())?
        .map_err(|_| unavailable())?;
        self.socket = Some(socket);
        Ok(())
    }
    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        self.counter = self.counter.checked_add(1).ok_or_else(invalid)?;
        let id = self.counter;
        let message = serde_json::to_string(&json!({"id":id,"method":method,"params":params}))
            .map_err(|_| invalid())?;
        if message.len() > LIMIT {
            return Err(invalid());
        }
        let mut socket = self.socket.take().ok_or_else(unavailable)?;
        // Taking ownership retires a timed-out/cancelled stream. A later request
        // cannot consume a delayed predecessor's reply and silently overtake it.
        let result = timeout(
            Duration::from_secs(5),
            exchange(&mut socket, id, message, method, &self.version),
        )
        .await
        .map_err(|_| unavailable())??;
        self.socket = Some(socket);
        Ok(result)
    }
}
async fn exchange(
    socket: &mut Socket,
    id: u64,
    text: String,
    method: &str,
    observed_version: &str,
) -> Result<Value> {
    socket
        .send(Message::Text(text.into()))
        .await
        .map_err(|_| unavailable())?;
    loop {
        let message = socket
            .next()
            .await
            .ok_or_else(unavailable)?
            .map_err(|_| unavailable())?;
        match message {
            Message::Text(text) => {
                let row: Value = serde_json::from_str(&text).map_err(|_| invalid())?;
                if row.get("method").is_some() {
                    if row.get("id").is_some() {
                        return Err(required());
                    }
                    tokio::task::yield_now().await;
                    continue;
                }
                if row.get("id").and_then(Value::as_u64) != Some(id) {
                    return Err(invalid());
                }
                if let Some(error) = row.get("error") {
                    let mut failure = match error.get("code").and_then(Value::as_i64) {
                        Some(-32001) => Error::new(
                            ErrorCode::Busy,
                            "The shared Codex server rejected an overloaded request.",
                        ),
                        Some(code @ (-32601 | -32602)) => {
                            surface(observed_version, method, Some(code))
                        }
                        _ => unavailable(),
                    };
                    failure.retryable = !matches!(
                        error.get("code").and_then(Value::as_i64),
                        Some(-32601 | -32602)
                    );
                    return Err(failure);
                }
                return row
                    .get("result")
                    .cloned()
                    .ok_or_else(|| surface(observed_version, &format!("{method}.result"), None));
            }
            Message::Ping(payload) => socket
                .send(Message::Pong(payload))
                .await
                .map_err(|_| unavailable())?,
            Message::Pong(_) => {}
            _ => return Err(unavailable()),
        }
    }
}

fn surface(version: &str, api: &str, code: Option<i64>) -> Error {
    let mut error = Error::new(
        ErrorCode::UnsupportedProtocol,
        "The shared Codex server does not provide the required API surface.",
    );
    error.details = json!({
        "host":"codex",
        "host_version":version,
        "observed_version":version,
        "missing_api":api,
        "compatibility":"best_effort_api_surface",
    });
    if let Some(code) = code {
        error.details["api_error_code"] = json!(code);
    }
    error
}
