//! Read-only shared-server feasibility probe; this is not a host admission test.

use std::{error::Error, time::Duration};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::{net::UnixStream, process::Command, time::timeout};
use tokio_tungstenite::{
    client_async_with_config,
    tungstenite::{Message, protocol::WebSocketConfig},
};
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let thread = std::env::args()
        .nth(1)
        .ok_or("supply the exact disposable conversation UUID")?;
    Uuid::parse_str(&thread)?;
    // Resolve the running daemon without starting a second server or changing
    // configuration. A stored thread is never resumed or subscribed by this probe.
    let version = Command::new("codex")
        .args(["app-server", "daemon", "version"])
        .output()
        .await?;
    let info: Value = serde_json::from_slice(&version.stdout)?;
    let socket = info["socketPath"]
        .as_str()
        .ok_or("no running shared control socket")?;
    let transport = UnixStream::connect(socket).await?;
    // The installed control socket speaks WebSocket over Unix, not JSONL. The
    // CLI's raw byte proxy does not perform the HTTP Upgrade on a client's behalf.
    let (mut ws, _) = timeout(
        Duration::from_secs(10),
        client_async_with_config(
            "ws://localhost/",
            transport,
            Some(
                WebSocketConfig::default()
                    .max_message_size(Some(1_048_576))
                    .max_frame_size(Some(1_048_576)),
            ),
        ),
    )
    .await??;
    for request in [
        json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"agentchat_g0_probe","version":"0.1.0"}}}),
        json!({"method":"initialized","params":{}}),
        json!({"id":2,"method":"thread/read","params":{"threadId":thread,"includeTurns":false}}),
    ] {
        ws.send(Message::text(request.to_string())).await?;
        let Some(expected_id) = request.get("id") else {
            continue;
        };
        let response = timeout(Duration::from_secs(10), async {
            loop {
                let message = ws
                    .next()
                    .await
                    .ok_or("control connection closed before response")??;
                let Message::Text(line) = message else {
                    continue;
                };
                let value: Value = serde_json::from_str(&line)?;
                if value.get("id") == Some(expected_id) {
                    return Ok::<_, Box<dyn Error>>(value);
                }
            }
        })
        .await??;
        if response.get("error").is_some() {
            return Err(format!("{response}").into());
        }
        if *expected_id == 2 {
            let t = &response["result"]["thread"];
            println!(
                "{}",
                json!({"thread_id":t["id"],"status":t["status"],"source":t["source"]})
            );
        }
    }
    ws.close(None).await?;
    Ok(())
}
