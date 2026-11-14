//! Synthetic startup readiness/cancellation tests; no real host is admitted.
#[path = "../src/claude_gateway/diagnostics.rs"]
mod diagnostics;
#[allow(dead_code, unused_imports)]
#[path = "../examples/g0_gateway/protocol.rs"]
mod protocol;
#[path = "../src/claude_gateway/restoration.rs"]
mod restoration;
use diagnostics::Diagnostics;

use agentchat_core::{Error, ErrorCode, Result};
use agentchat_mcp::{ConversationKey, Conversations};
use agentchat_protocol::{
    envelope::Envelope,
    operations::{Attachment, Backend, Binding, Operation, Target},
};
use async_trait::async_trait;
use restoration::Restoration;
use serde_json::Value;
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Notify, Semaphore};
use uuid::Uuid;

struct Evidence(std::path::PathBuf);
impl diagnostics::Trace for Evidence {
    fn record(&self, event: Value) {
        protocol::audit(&self.0, event).unwrap();
    }
}
fn diagnostic(path: &std::path::Path) -> Diagnostics {
    Diagnostics::new(Some(Arc::new(Evidence(path.into()))))
}
struct Fake {
    attempts: AtomicUsize,
    blocked: AtomicBool,
    entered: Notify,
    released: Semaphore,
    inflight: Arc<AtomicUsize>,
    held: Arc<AtomicUsize>,
    outcomes: Mutex<VecDeque<ErrorCode>>,
}
impl Default for Fake {
    fn default() -> Self {
        Self {
            attempts: AtomicUsize::new(0),
            blocked: AtomicBool::new(false),
            entered: Notify::new(),
            released: Semaphore::new(0),
            inflight: Arc::default(),
            held: Arc::default(),
            outcomes: Mutex::default(),
        }
    }
}
struct Counted(Arc<AtomicUsize>);
impl Drop for Counted {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
struct Held {
    binding: Binding,
    _counted: Counted,
}
#[async_trait]
impl Attachment for Held {
    fn binding(&self) -> &Binding {
        &self.binding
    }
    async fn check(&self) -> Result<()> {
        Ok(())
    }
    async fn refresh(&self) -> Result<()> {
        panic!("Restoration tests must not implicitly refresh attachments");
    }
}
#[async_trait]
impl Backend for Fake {
    async fn attach(&self, target: Target) -> Result<Box<dyn Attachment>> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        self.inflight.fetch_add(1, Ordering::SeqCst);
        let _inflight = Counted(self.inflight.clone());
        self.entered.notify_one();
        if self.blocked.load(Ordering::SeqCst) {
            self.released.acquire().await.unwrap().forget();
        }
        if let Some(code) = self.outcomes.lock().unwrap().pop_front() {
            return Err(Error::new(
                code,
                "Synthetic host readiness/participation result.",
            ));
        }
        self.held.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(Held {
            binding: Binding {
                context_id: target.context_id.unwrap(),
                project_id: Uuid::new_v4(),
                agent_id: Uuid::new_v4(),
            },
            _counted: Counted(self.held.clone()),
        }))
    }
    async fn invoke(&self, _: Uuid, _: Target, _: Operation) -> Envelope<Value> {
        panic!("Restoration must never invoke join or any ordinary operation");
    }
}
fn key(generation: u64) -> ConversationKey {
    ConversationKey {
        generation,
        context_id: Uuid::new_v4(),
    }
}
async fn result(directory: &std::path::Path, key: ConversationKey) -> Value {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Ok(log) = std::fs::read_to_string(directory.join("gateway-events.jsonl")) {
                for line in log.lines() {
                    let v: Value = serde_json::from_str(line).unwrap();
                    if v["context_id"] == key.context_id.to_string() {
                        return v;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap()
}
async fn entered(fake: &Fake) {
    tokio::time::timeout(Duration::from_secs(2), fake.entered.notified())
        .await
        .unwrap();
}

#[tokio::test]
async fn readiness_retry_restores_once_and_repeated_registration_does_not_reset_it() {
    let directory = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake::default());
    fake.outcomes
        .lock()
        .unwrap()
        .push_back(ErrorCode::SessionRequired);
    let roots = Conversations::new(fake.clone());
    let key = key(1);
    let conversation = roots.activate(key).await.unwrap();
    let mut owner = Restoration::default();
    owner
        .start(conversation.clone(), diagnostic(directory.path()), false)
        .await;
    let report = result(directory.path(), key).await;
    assert_eq!(report["outcome"], "restored");
    assert_eq!(report["attempts"], 2);
    owner
        .start(conversation, diagnostic(directory.path()), false)
        .await;
    tokio::task::yield_now().await;
    assert_eq!(fake.attempts.load(Ordering::SeqCst), 2);
    assert_eq!(fake.held.load(Ordering::SeqCst), 1);
    roots.close().await;
    owner.close().await;
    assert_eq!(fake.held.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn pending_managed_activation_waits_for_its_already_authorized_membership() {
    let directory = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake::default());
    fake.outcomes
        .lock()
        .unwrap()
        .extend([ErrorCode::NotJoined, ErrorCode::SessionRequired]);
    let roots = Conversations::new(fake.clone());
    let key = key(1);
    let conversation = roots.activate(key).await.unwrap();
    let mut owner = Restoration::default();
    owner
        .start(conversation.clone(), diagnostic(directory.path()), true)
        .await;
    let report = result(directory.path(), key).await;
    assert_eq!(report["outcome"], "restored");
    assert_eq!(report["attempts"], 3);
    assert_eq!(report["startup_readiness_pending"], true);
    owner
        .start(conversation, diagnostic(directory.path()), true)
        .await;
    tokio::task::yield_now().await;
    assert_eq!(fake.attempts.load(Ordering::SeqCst), 3);
    assert_eq!(fake.held.load(Ordering::SeqCst), 1);
    // Fake::invoke panics on Join and every ordinary operation. The only
    // observed operations above are shared Attach attempts, never invitations.
    roots.close().await;
    owner.close().await;
    assert_eq!(fake.held.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn known_unjoined_same_root_is_terminal_and_never_implicitly_invited_or_reattempted() {
    let directory = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake::default());
    fake.outcomes
        .lock()
        .unwrap()
        .push_back(ErrorCode::NotJoined);
    let roots = Conversations::new(fake.clone());
    let key = key(1);
    let conversation = roots.activate(key).await.unwrap();
    let mut owner = Restoration::default();
    owner
        .start(conversation.clone(), diagnostic(directory.path()), false)
        .await;
    let report = result(directory.path(), key).await;
    assert_eq!(report["outcome"], "unjoined");
    assert_eq!(report["startup_readiness_pending"], false);
    owner
        .start(conversation, diagnostic(directory.path()), false)
        .await;
    tokio::task::yield_now().await;
    assert_eq!(fake.attempts.load(Ordering::SeqCst), 1);
    assert_eq!(fake.held.load(Ordering::SeqCst), 0);
    roots.close().await;
    owner.close().await;
}

#[tokio::test]
async fn replacement_cancels_old_readiness_before_attaching_a_fresh_generation() {
    let directory = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake::default());
    fake.blocked.store(true, Ordering::SeqCst);
    let roots = Conversations::new(fake.clone());
    let old = roots.activate(key(1)).await.unwrap();
    let mut owner = Restoration::default();
    owner.start(old, diagnostic(directory.path()), false).await;
    entered(&fake).await;
    assert_eq!(fake.inflight.load(Ordering::SeqCst), 1);
    let fresh = key(2);
    let conversation = roots.activate(fresh).await.unwrap();
    fake.blocked.store(false, Ordering::SeqCst);
    owner
        .start(conversation, diagnostic(directory.path()), false)
        .await;
    assert_eq!(result(directory.path(), fresh).await["outcome"], "restored");
    assert_eq!(fake.inflight.load(Ordering::SeqCst), 0);
    assert_eq!(fake.attempts.load(Ordering::SeqCst), 2);
    assert_eq!(fake.held.load(Ordering::SeqCst), 1);
    roots.close().await;
    owner.close().await;
    assert_eq!(fake.held.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn exact_retirement_and_owner_drop_cancel_pending_readiness() {
    let directory = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake::default());
    fake.blocked.store(true, Ordering::SeqCst);
    let roots = Conversations::new(fake.clone());
    let current = key(1);
    let mut owner = Restoration::default();
    owner
        .start(
            roots.activate(current).await.unwrap(),
            diagnostic(directory.path()),
            false,
        )
        .await;
    entered(&fake).await;
    owner.retire(key(2)).await;
    assert_eq!(fake.inflight.load(Ordering::SeqCst), 1);
    roots.retire(current).await.unwrap();
    owner.retire(current).await;
    assert_eq!(fake.inflight.load(Ordering::SeqCst), 0);
    owner
        .start(
            roots.activate(key(2)).await.unwrap(),
            diagnostic(directory.path()),
            false,
        )
        .await;
    entered(&fake).await;
    drop(owner);
    tokio::time::timeout(Duration::from_secs(2), async {
        while fake.inflight.load(Ordering::SeqCst) != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    roots.close().await;
    assert_eq!(fake.held.load(Ordering::SeqCst), 0);
}
