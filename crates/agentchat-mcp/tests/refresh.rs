//! A native change hint rechecks only an existing immutable attachment.
use agentchat_core::{Error, ErrorCode, Result};
use agentchat_mcp::{ConversationKey, Conversations};
use agentchat_protocol::{
    envelope::Envelope,
    operations::{Attachment, Backend, Binding, Operation, Target},
};
use async_trait::async_trait;
use serde_json::Value;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::Notify;
use uuid::Uuid;

#[derive(Default)]
struct Facts {
    attached: AtomicUsize,
    refreshes: AtomicUsize,
    inflight: AtomicUsize,
    blocked: AtomicBool,
    failed: AtomicBool,
    entered: Notify,
}
struct Fake(Arc<Facts>);
struct Held {
    binding: Binding,
    facts: Arc<Facts>,
}
struct Inflight(Arc<Facts>);
impl Drop for Inflight {
    fn drop(&mut self) {
        self.0.inflight.fetch_sub(1, Ordering::SeqCst);
    }
}
#[async_trait]
impl Attachment for Held {
    fn binding(&self) -> &Binding {
        &self.binding
    }
    async fn check(&self) -> Result<()> {
        if self.facts.failed.load(Ordering::SeqCst) {
            return Err(Error::new(
                ErrorCode::SessionRequired,
                "Synthetic native authority ended.",
            ));
        }
        Ok(())
    }
    async fn refresh(&self) -> Result<()> {
        self.facts.refreshes.fetch_add(1, Ordering::SeqCst);
        self.facts.inflight.fetch_add(1, Ordering::SeqCst);
        let _inflight = Inflight(self.facts.clone());
        self.facts.entered.notify_one();
        if self.facts.blocked.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        if self.facts.failed.load(Ordering::SeqCst) {
            return Err(Error::new(
                ErrorCode::SessionRequired,
                "Synthetic native authority ended.",
            ));
        }
        Ok(())
    }
}
#[async_trait]
impl Backend for Fake {
    async fn attach(&self, target: Target) -> Result<Box<dyn Attachment>> {
        self.0.attached.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(Held {
            binding: Binding {
                project_id: Uuid::new_v4(),
                agent_id: Uuid::new_v4(),
                context_id: target.context_id.unwrap(),
            },
            facts: self.0.clone(),
        }))
    }
    async fn invoke(&self, _: Uuid, _: Target, _: Operation) -> Envelope<Value> {
        panic!("A native change hint must never invoke a public operation or implicit join");
    }
}
fn key(generation: u64) -> ConversationKey {
    ConversationKey {
        context_id: Uuid::new_v4(),
        generation,
    }
}

#[tokio::test]
async fn hints_are_inert_before_binding_and_refuse_old_generations_after_replacement() {
    let facts = Arc::new(Facts::default());
    let roots = Conversations::new(Arc::new(Fake(facts.clone())));
    let a = key(1);
    let current = roots.activate(a).await.unwrap();
    assert!(!roots.refresh(a).await.unwrap());
    assert_eq!(facts.attached.load(Ordering::SeqCst), 0);
    current.restore().await.unwrap();
    // Native verification can have observed NotJoined before an explicit Join
    // attached the same slot. Rechecking it must not request a second attachment.
    current.restore().await.unwrap();
    assert_eq!(facts.attached.load(Ordering::SeqCst), 1);
    assert_eq!(facts.refreshes.load(Ordering::SeqCst), 0);
    assert!(roots.refresh(a).await.unwrap());
    assert_eq!(facts.refreshes.load(Ordering::SeqCst), 1);
    let c = key(2);
    roots.activate(c).await.unwrap();
    assert_eq!(
        roots.refresh(a).await.unwrap_err().code,
        ErrorCode::SessionRequired
    );
    assert!(!roots.refresh(c).await.unwrap());
    assert_eq!(facts.attached.load(Ordering::SeqCst), 1);
    assert_eq!(facts.refreshes.load(Ordering::SeqCst), 1);
    roots.close().await;
    assert_eq!(
        roots.refresh(c).await.unwrap_err().code,
        ErrorCode::SessionRequired
    );
}

#[tokio::test]
async fn refresh_propagates_authority_loss_without_restoration() {
    let facts = Arc::new(Facts::default());
    let roots = Conversations::new(Arc::new(Fake(facts.clone())));
    let a = key(1);
    let retained = roots.activate(a).await.unwrap();
    retained.restore().await.unwrap();
    facts.failed.store(true, Ordering::SeqCst);
    assert_eq!(
        retained.restore().await.unwrap_err().code,
        ErrorCode::SessionRequired
    );
    assert_eq!(
        roots.refresh(a).await.unwrap_err().code,
        ErrorCode::SessionRequired
    );
    assert_eq!(facts.attached.load(Ordering::SeqCst), 1);
    roots.close().await;
}

#[tokio::test]
async fn retirement_cancels_a_pending_refresh_before_activating_another_root() {
    let facts = Arc::new(Facts::default());
    let roots = Arc::new(Conversations::new(Arc::new(Fake(facts.clone()))));
    let a = key(1);
    roots.activate(a).await.unwrap().restore().await.unwrap();
    facts.blocked.store(true, Ordering::SeqCst);
    let copied = roots.clone();
    let pending = tokio::spawn(async move { copied.refresh(a).await });
    tokio::time::timeout(Duration::from_secs(2), facts.entered.notified())
        .await
        .unwrap();
    assert_eq!(facts.inflight.load(Ordering::SeqCst), 1);
    // No permit is released by the fake backend. Only retirement can unblock.
    let c = key(2);
    tokio::time::timeout(Duration::from_secs(2), roots.activate(c))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        pending.await.unwrap().unwrap_err().code,
        ErrorCode::SessionRequired
    );
    assert_eq!(facts.inflight.load(Ordering::SeqCst), 0);
    assert!(!roots.refresh(c).await.unwrap());
    roots.close().await;
}
