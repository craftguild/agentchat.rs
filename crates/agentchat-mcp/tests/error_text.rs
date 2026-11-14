//! Real SDK clients must receive actionable errors even if they ignore structuredContent.
mod support;

use agentchat_core::{Error, ErrorCode, Result};
use agentchat_mcp::Server;
use agentchat_protocol::{
    envelope::Envelope,
    framing::MAX_FRAME_BYTES,
    operations::{Attachment, Backend, Operation, Target},
};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;
use support::{Fake, Session};
use uuid::Uuid;

fn text(result: &rmcp::model::CallToolResult) -> String {
    serde_json::to_value(result).unwrap()["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn unbound_mutation_error_is_actionable_without_structured_content() {
    let backend = Arc::new(Fake::default());
    let session = Session::new(backend.clone()).await;
    let request = Uuid::new_v4();
    let result = session
        .call(
            "agentchat_post",
            json!({"request_id":request,"project_path":"/explicit","body":"literal"}),
        )
        .await;
    assert_eq!(result.is_error, Some(true));
    let visible = text(&result);
    assert!(visible.contains("session_required"));
    assert!(visible.contains(&request.to_string()));
    assert!(visible.contains("retryable"));
    let complete = result.structured_content.unwrap();
    assert_eq!(complete["request_id"], request.to_string());
    assert_eq!(complete["error"]["code"], "session_required");
    assert!(backend.calls.lock().unwrap().is_empty());
    session.close().await;
}

struct Failing(Error);
#[async_trait]
impl Backend for Failing {
    async fn attach(&self, _: Target) -> Result<Box<dyn Attachment>> {
        Err(Error::new(
            ErrorCode::NotJoined,
            "This test never attaches.",
        ))
    }
    async fn invoke(&self, id: Uuid, _: Target, _: Operation) -> Envelope<Value> {
        Envelope::new(id, None, Err(self.0.clone()))
    }
}

#[tokio::test]
async fn unicode_error_summary_is_bounded_and_preserves_the_complete_single_envelope() {
    let message = format!("{}UNTRUNCATED_MESSAGE_TAIL", "界".repeat(4000));
    let mut error = Error::new(ErrorCode::Busy, message.clone());
    error.retryable = true;
    error.details = json!({"retained":"DETAILS_ONLY".repeat(60_000)});
    let session = Session::from_server(Server::new(Arc::new(Failing(error)))).await;
    let request = Uuid::new_v4();
    let result = session
        .call(
            "agentchat_doctor",
            json!({"request_id":request,"project_path":"/explicit"}),
        )
        .await;
    assert_eq!(result.is_error, Some(true));
    let visible = text(&result);
    assert!(visible.contains("busy"));
    assert!(visible.contains(&request.to_string()));
    assert!(visible.len() < 8192);
    assert!(!visible.contains("UNTRUNCATED_MESSAGE_TAIL"));
    assert!(!visible.contains("DETAILS_ONLY"));
    assert!(serde_json::to_vec(&result).unwrap().len() < MAX_FRAME_BYTES);
    let complete = result.structured_content.unwrap();
    assert_eq!(complete["error"]["message"], message);
    assert_eq!(complete["error"]["retryable"], true);
    assert_eq!(
        complete["error"]["details"]["retained"]
            .as_str()
            .unwrap()
            .len(),
        720_000
    );
    session.close().await;
}

#[tokio::test]
async fn missing_host_api_is_visible_to_a_skill_without_structured_content() {
    let mut error = Error::new(
        ErrorCode::UnsupportedProtocol,
        "The shared Codex server does not provide the required API surface.",
    );
    error.details = json!({
        "host":"codex",
        "host_version":"0.164.7",
        "observed_version":"0.164.7",
        "missing_api":"thread/read",
        "internal_detail":"must not be copied into the text summary",
    });
    let session = Session::from_server(Server::new(Arc::new(Failing(error)))).await;
    let request = Uuid::new_v4();
    let result = session
        .call(
            "agentchat_doctor",
            json!({"request_id":request,"project_path":"/explicit"}),
        )
        .await;
    assert_eq!(result.is_error, Some(true));
    let visible = text(&result);
    assert!(visible.contains("unsupported_protocol"));
    assert!(visible.contains("codex"));
    assert!(visible.contains("0.164.7"));
    assert!(visible.contains("thread/read"));
    assert!(!visible.contains("internal_detail"));
    let complete = result.structured_content.unwrap();
    assert_eq!(complete["error"]["details"]["missing_api"], "thread/read");
    session.close().await;
}
