//! Retained control reader; invokes no resume or explicit subscription operation.
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{error::Error, io::Write, time::Duration};
use tokio::{
    net::UnixStream,
    process::Command,
    time::{Instant, sleep, timeout},
};
use tokio_tungstenite::{
    WebSocketStream, client_async_with_config,
    tungstenite::{Message, protocol::WebSocketConfig},
};
use uuid::Uuid;

type Failure = Box<dyn Error>;
type Socket = WebSocketStream<UnixStream>;

#[tokio::main]
async fn main() -> Result<(), Failure> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 4 || args[0] != "--owned-thread" || args[2] != "--observe-seconds" {
        return Err("usage: g0_lifecycle_probe --owned-thread EXACT_DISPOSABLE_UUID --observe-seconds 1..120".into());
    }
    let thread = Uuid::parse_str(&args[1])?.to_string();
    let seconds = args[3].parse::<u64>()?;
    if !(1..=120).contains(&seconds) {
        return Err("observation must be 1 through 120 seconds".into());
    }
    let version = Command::new("codex")
        .args(["app-server", "daemon", "version"])
        .output()
        .await?;
    if !version.status.success() {
        return Err("shared-server version inspection failed".into());
    }
    let version: Value = serde_json::from_slice(&version.stdout)?;
    let path = version["socketPath"]
        .as_str()
        .ok_or("no running shared control socket")?;
    let stream = UnixStream::connect(path).await?;
    let (mut socket, _) = timeout(
        Duration::from_secs(10),
        client_async_with_config(
            "ws://localhost/",
            stream,
            Some(
                WebSocketConfig::default()
                    .max_message_size(Some(1_048_576))
                    .max_frame_size(Some(1_048_576)),
            ),
        ),
    )
    .await??;
    request(
        &mut socket,
        1,
        "initialize",
        json!({"clientInfo":{"name":"agentchat_g0_lifecycle_probe","version":"0.1.0"}}),
    )
    .await?;
    socket
        .send(Message::text(
            json!({"method":"initialized","params":{}}).to_string(),
        ))
        .await?;
    let start = Instant::now();
    let mut id = 2;
    loop {
        let value = request(
            &mut socket,
            id,
            "thread/read",
            json!({"threadId":thread,"includeTurns":false}),
        )
        .await?;
        let t = &value["thread"];
        if t["id"] != thread || !t["parentThreadId"].is_null() {
            return Err("response is not the exact owned root".into());
        }
        println!(
            "{}",
            json!({"thread_id":t["id"],"host_session_id":t["sessionId"],"status":t["status"],"forked_from_id":t["forkedFromId"],"elapsed_ms":start.elapsed().as_millis(),"observed_unix_ms":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis(),"control_connection_retained":true,"resume_or_subscription_api_invoked":false,"g0_admitted":false})
        );
        std::io::stdout().flush()?;
        if start.elapsed() >= Duration::from_secs(seconds) {
            break;
        }
        id += 1;
        sleep(Duration::from_secs(1)).await;
    }
    socket.close(None).await?;
    Ok(())
}
async fn request(
    socket: &mut Socket,
    id: u64,
    method: &str,
    params: Value,
) -> Result<Value, Failure> {
    socket
        .send(Message::text(
            json!({"id":id,"method":method,"params":params}).to_string(),
        ))
        .await?;
    timeout(Duration::from_secs(10), async {
        loop {
            let message = socket
                .next()
                .await
                .ok_or("shared control socket closed")??;
            if let Message::Text(text) = message {
                let value: Value = serde_json::from_str(&text)?;
                if value["id"] == id {
                    if value.get("error").is_some() {
                        return Err(format!("{value}").into());
                    }
                    return Ok(value["result"].clone());
                }
                if value.get("id").is_some() && value.get("method").is_some() {
                    return Err(
                        "interactive request received; this probe never answers approvals".into(),
                    );
                }
            }
        }
    })
    .await?
}
