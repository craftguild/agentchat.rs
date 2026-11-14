//! Explicit disposable-thread wake probe; never an automatic host admission.

use std::{error::Error, time::Duration};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
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

/// Only explicitly supplied, test-owned root IDs may receive this fixture text.
/// Keep that CLI visibly attached: loaded/idle metadata alone is not a complete
/// frontend-liveness proof during the shared server's post-detach grace period.
#[tokio::main]
async fn main() -> Result<(), Failure> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if !(args.len() == 2 || args.len() == 3 && args[2] == "--active") || args[0] != "--owned-thread"
    {
        return Err(
            "usage: g0_wake_probe --owned-thread EXACT_DISPOSABLE_ROOT_UUID [--active]".into(),
        );
    }
    let active = args.len() == 3;
    let thread = Uuid::parse_str(&args[1])?.to_string();
    let info = Command::new("codex")
        .args(["app-server", "daemon", "version"])
        .output()
        .await?;
    let info: Value = serde_json::from_slice(&info.stdout)?;
    let path = info["socketPath"]
        .as_str()
        .ok_or("shared server is not running")?;
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
        json!({"clientInfo":{"name":"agentchat_g0_wake_probe","version":"0.1.0"}}),
    )
    .await?;
    socket
        .send(Message::text(
            json!({"method":"initialized","params":{}}).to_string(),
        ))
        .await?;
    let before = request(
        &mut socket,
        2,
        "thread/read",
        json!({"threadId":thread,"includeTurns":false}),
    )
    .await?;
    if before["thread"]["id"] != thread
        || before["thread"]["status"]["type"] != "idle"
        || !before["thread"]["parentThreadId"].is_null()
    {
        return Err("exact disposable thread is not loaded and idle; no wake was sent".into());
    }
    let marker = format!("AGENTCHAT_CODEX_WAKE_{}", Uuid::new_v4().simple());
    println!(
        "{}",
        json!({"thread_id":thread,"marker":marker,"active_probe":active,"phase":"before_submission","g0_admitted":false})
    );
    // Omit model, permission, sandbox and collaboration overrides. The existing
    // test conversation remains the authority for its settings. No resume or
    // subscription is used to turn an offline conversation into a live one.
    let text = if active {
        "Disposable original task: do not use tools, modify files, or launch agents. Write one 100-word paragraph about transactional storage, surrounded by the exact markers AGENTCHAT_TASK_BEGIN and AGENTCHAT_TASK_END. Preserve and finish this original task if additional context arrives.".to_owned()
    } else {
        format!(
            "Disposable agentchat control probe. Do not use tools, modify files, or launch agents. Output exactly {marker}."
        )
    };
    let accepted = request(
        &mut socket,
        3,
        "turn/start",
        json!({"threadId":thread,"input":[{
            "type":"text","text":text,"text_elements":[]
        }]}),
    )
    .await?;
    let turn = accepted["turn"]["id"]
        .as_str()
        .ok_or("host omitted accepted turn ID")?
        .to_owned();
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut id = 4;
    if active {
        sleep(Duration::from_millis(250)).await;
        let running = request(
            &mut socket,
            4,
            "thread/read",
            json!({"threadId":thread,"includeTurns":true}),
        )
        .await?;
        if running["thread"]["status"]["type"] != "active"
            || !running["thread"]["turns"].as_array().is_some_and(|turns| {
                turns
                    .iter()
                    .any(|t| t["id"] == turn && t["status"] == "inProgress")
            })
        {
            return Err(
                "original fixture task already left its active turn; no steer was sent".into(),
            );
        }
        let steered = request(&mut socket,5,"turn/steer",json!({"threadId":thread,"expectedTurnId":turn,"input":[{
            "type":"text","text":format!("Additional agentchat probe context: append the exact marker {marker} while preserving and finishing the original AGENTCHAT_TASK_BEGIN/AGENTCHAT_TASK_END task. Do not use tools."),"text_elements":[]
        }]})).await?;
        if steered["turnId"] != turn {
            return Err("steer did not retain the expected original turn".into());
        }
        id = 6;
    }
    loop {
        if Instant::now() >= deadline {
            return Err("accepted wake lacked actual model receipt before deadline".into());
        }
        let result = request(
            &mut socket,
            id,
            "thread/read",
            json!({"threadId":thread,"includeTurns":true}),
        )
        .await?;
        id += 1;
        if let Some(turns) = result["thread"]["turns"].as_array()
            && let Some(found) = turns.iter().find(|t| t["id"] == turn)
        {
            if found["status"] == "failed" || found["status"] == "interrupted" {
                return Err("host turn failed or was interrupted; no model receipt claim".into());
            }
            let received = found["items"].as_array().is_some_and(|items| {
                items.iter().any(|item| {
                    item["type"] == "agentMessage"
                        && item["text"].as_str().is_some_and(|s| s.contains(&marker))
                })
            });
            if found["status"] == "completed" && received {
                if active
                    && !found["items"].as_array().is_some_and(|items| {
                        let text = items
                            .iter()
                            .filter(|item| item["type"] == "agentMessage")
                            .filter_map(|item| item["text"].as_str())
                            .collect::<String>();
                        text.contains("AGENTCHAT_TASK_BEGIN") && text.contains("AGENTCHAT_TASK_END")
                    })
                {
                    return Err("steering displaced the original fixture task".into());
                }
                for field in ["model", "reasoningEffort"] {
                    if result["thread"][field] != before["thread"][field] {
                        return Err("host model settings changed during probe".into());
                    }
                }
                println!(
                    "{}",
                    json!({"thread_id":thread,"turn_id":turn,"marker":marker,"actual_model_receipt":true,"active_probe":active,"model_and_effort_preserved":true,"g0_admitted":false})
                );
                socket.close(None).await?;
                return Ok(());
            }
        }
        sleep(Duration::from_millis(250)).await;
    }
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
            let incoming = socket.next().await.ok_or("shared connection closed")??;
            if let Message::Text(text) = incoming {
                let value: Value = serde_json::from_str(&text)?;
                if value["id"] == id {
                    if value.get("error").is_some() {
                        return Err(format!("{value}").into());
                    }
                    return Ok(value["result"].clone());
                }
                if value.get("id").is_some() && value.get("method").is_some() {
                    return Err(
                        "host requested an interactive decision; probe does not answer approvals"
                            .into(),
                    );
                }
            }
        }
    })
    .await?
}
