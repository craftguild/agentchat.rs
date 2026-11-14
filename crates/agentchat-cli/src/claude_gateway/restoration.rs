//! One bounded lifecycle-owned restoration attempt; no implicit invitation.
use super::Diagnostics;
use agentchat_core::ErrorCode;
use agentchat_mcp::{Conversation, ConversationKey};
use std::time::Duration;
use tokio::{
    task::JoinHandle,
    time::{Instant, timeout_at},
};

#[derive(Default)]
pub(super) struct Restoration {
    key: Option<ConversationKey>,
    worker: Option<JoinHandle<()>>,
}
impl Restoration {
    pub(super) async fn start(
        &mut self,
        conversation: Conversation,
        diagnostics: Diagnostics,
        pending_launch: bool,
    ) {
        let key = conversation.key();
        if self.key == Some(key) {
            // Record callbacks repeat activation. Neither a completed unjoined
            // attempt nor a later explicit leave may start another restoration.
            return;
        }
        self.close().await;
        self.key = Some(key);
        self.worker = Some(tokio::spawn(async move {
            let deadline = Instant::now() + Duration::from_secs(15);
            let mut attempts = 0u16;
            let (outcome, error) = loop {
                attempts += 1;
                match timeout_at(deadline, conversation.restore()).await {
                    Ok(Ok(())) => break ("restored", None),
                    Ok(Err(error)) if error.code == ErrorCode::NotJoined => {
                        // This flag is derived only from exact pre-commit
                        // startup or a durable distinct-root departure. Once an
                        // original root has started, same-root Leave/recovery
                        // supplies false and remains terminal below.
                        if !pending_launch {
                            break ("unjoined", Some(error.code));
                        }
                        if Instant::now() >= deadline || attempts >= 150 {
                            break ("timeout", Some(error.code));
                        }
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                    Ok(Err(error))
                        if matches!(
                            error.code,
                            ErrorCode::SessionRequired | ErrorCode::RuntimeUnavailable
                        ) || error.retryable && error.code == ErrorCode::Busy =>
                    {
                        if Instant::now() >= deadline || attempts >= 150 {
                            break ("timeout", Some(error.code));
                        }
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                    Ok(Err(error)) => break ("failed", Some(error.code)),
                    Err(_) => break ("timeout", Some(ErrorCode::SessionRequired)),
                }
            };
            diagnostics.record(serde_json::json!({
                "event":"native_attachment_restore", "context_id":key.context_id,
                "generation":key.generation, "outcome":outcome, "error":error,
                "attempts":attempts,"startup_readiness_pending":pending_launch
            }));
        }));
    }
    pub(super) async fn retire(&mut self, key: ConversationKey) {
        if self.key == Some(key) {
            self.close().await;
        }
    }
    pub(super) async fn close(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.abort();
            // Await cancellation before a replacement can start. The immutable
            // Server separately fences any already-committed late attachment.
            let _ = worker.await;
        }
    }
}
impl Drop for Restoration {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.abort();
        }
    }
}
