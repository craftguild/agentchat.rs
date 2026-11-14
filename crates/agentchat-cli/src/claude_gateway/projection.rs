//! Bounded change hints; the shared heartbeat still obtains actual host facts.
use super::Diagnostics;
use agentchat_core::ErrorCode;
use agentchat_mcp::{ConversationKey, Conversations};
use std::{
    sync::{Arc, Weak},
    time::Duration,
};
use tokio::{
    sync::watch,
    task::JoinHandle,
    time::{Instant, timeout_at},
};

pub(super) struct Projection {
    pending: watch::Sender<Option<ConversationKey>>,
    worker: Option<JoinHandle<()>>,
}
impl Projection {
    pub(super) fn new(roots: &Arc<Conversations>, diagnostics: Diagnostics) -> Self {
        let (pending, mut changes) = watch::channel(None);
        let roots = Arc::downgrade(roots);
        let worker = tokio::spawn(async move {
            while changes.changed().await.is_ok() {
                let key = *changes.borrow_and_update();
                let Some(key) = key else {
                    continue;
                };
                refresh(&roots, key, &diagnostics).await;
            }
        });
        Self {
            pending,
            worker: Some(worker),
        }
    }
    pub(super) fn request(&self, key: ConversationKey) {
        // One pending key coalesces bursts without spawning work per callback.
        // A queued old key can only fail the immutable lifecycle check.
        self.pending.send_replace(Some(key));
    }
    pub(super) async fn close(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.abort();
            let _ = worker.await;
        }
    }
}
impl Drop for Projection {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.abort();
        }
    }
}
async fn refresh(roots: &Weak<Conversations>, key: ConversationKey, diagnostics: &Diagnostics) {
    let Some(roots) = roots.upgrade() else {
        return;
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    let (outcome, error) = loop {
        match timeout_at(deadline, roots.refresh(key)).await {
            Ok(Ok(true)) => break ("refreshed", None),
            Ok(Ok(false)) => break ("unbound", None),
            Ok(Err(error))
                if error.retryable
                    && error.code == ErrorCode::Busy
                    && Instant::now() < deadline =>
            {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Ok(Err(error)) => break ("failed", Some(error.code)),
            Err(_) => break ("timeout", Some(ErrorCode::RuntimeUnavailable)),
        }
    };
    diagnostics.record(
        serde_json::json!({
            "event":"native_projection_refresh", "context_id":key.context_id,
            "generation":key.generation, "outcome":outcome, "error":error,
            "unix_ms":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d|d.as_millis()).unwrap_or(0),
        }),
    );
}
