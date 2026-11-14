//! A Join may synchronously replace its own verified native lifecycle.
mod support;
use agentchat_core::{Result, application::Mutation};
use agentchat_mcp::{CallEvidence, CallVerifier, ConversationKey, Conversations, Server};
use agentchat_protocol::{
    envelope::Envelope,
    operations::{Attachment, Backend, Operation, Target},
};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex, Weak, atomic::Ordering},
    time::Duration,
};
use support::*;
use uuid::Uuid;

struct Authority(Mutex<ConversationKey>);
#[async_trait]
impl CallVerifier for Authority {
    async fn verify(&self, _: CallEvidence<'_>) -> Result<ConversationKey> {
        Ok(*self.0.lock().unwrap())
    }
}
struct Replacing {
    fake: Arc<Fake>,
    roots: Mutex<Weak<Conversations>>,
    next: ConversationKey,
}
#[async_trait]
impl Backend for Replacing {
    async fn attach(&self, target: Target) -> Result<Box<dyn Attachment>> {
        self.fake.attach(target).await
    }
    async fn invoke(&self, id: Uuid, target: Target, operation: Operation) -> Envelope<Value> {
        if matches!(
            operation,
            Operation::Mutate {
                command: Mutation::Join { .. }
            }
        ) {
            let roots = self.roots.lock().unwrap().upgrade().unwrap();
            // Mirrors the actual Claude registration callback during explicit
            // rejoin. Replacement waits for the preceding slot's cleanup.
            roots.activate_same_conversation(self.next).await.unwrap();
            drop(roots);
            if self.fake.block_join.load(Ordering::SeqCst) {
                self.fake.entered.notify_one();
                self.fake.release.acquire().await.unwrap().forget();
            }
            return Envelope::new(
                id,
                Some(self.fake.project),
                Ok(json!({"agent_id":self.fake.agent,"context_id":self.next.context_id})),
            );
        }
        self.fake.invoke(id, target, operation).await
    }
}
#[tokio::test]
async fn join_returns_committed_receipt_after_its_native_callback_retires_the_old_slot() {
    let old = ConversationKey {
        context_id: Uuid::new_v4(),
        generation: 1,
    };
    let next = ConversationKey {
        context_id: Uuid::new_v4(),
        generation: 2,
    };
    let fake = Arc::new(Fake::default());
    let backend = Arc::new(Replacing {
        fake: fake.clone(),
        roots: Mutex::default(),
        next,
    });
    let roots = Arc::new(Conversations::new(backend.clone()));
    *backend.roots.lock().unwrap() = Arc::downgrade(&roots);
    let previous = roots.activate(old).await.unwrap();
    let authority = Arc::new(Authority(Mutex::new(old)));
    let session = Session::from_server(Server::gateway(roots.clone(), authority.clone())).await;
    let receipt = tokio::time::timeout(
        Duration::from_secs(2),
        session.call("agentchat_join", json!({"context_id":old.context_id})),
    )
    .await
    .unwrap();
    assert_eq!(
        envelope(receipt)["result"]["context_id"],
        next.context_id.to_string()
    );
    assert!(previous.restore().await.is_err());
    assert_eq!(fake.attached.load(Ordering::SeqCst), 1);
    *authority.0.lock().unwrap() = next;
    let read = envelope(
        session
            .call("agentchat_whoami", json!({"project_path":"/explicit"}))
            .await,
    );
    assert!(read.get("result").is_some(), "{read}");
    assert_eq!(
        fake.calls.lock().unwrap().last().unwrap().1.context_id,
        Some(next.context_id)
    );
    let posted = envelope(
        session
            .call("agentchat_post", json!({"body":"after explicit Join"}))
            .await,
    );
    assert!(posted.get("result").is_some(), "{posted}");
    assert_eq!(
        fake.calls.lock().unwrap().last().unwrap().1.context_id,
        Some(next.context_id)
    );
    session.close().await;
    assert_eq!(fake.attached.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_preserved_join_receipt_still_cancels_on_later_root_switch_close_or_control_loss() {
    use rmcp::{
        model::{ClientRequest, Request},
        service::PeerRequestOptions,
    };
    for action in 0..3 {
        let old = ConversationKey {
            context_id: Uuid::new_v4(),
            generation: 1,
        };
        let next = ConversationKey {
            context_id: Uuid::new_v4(),
            generation: 2,
        };
        let fake = Arc::new(Fake::default());
        fake.block_join.store(true, Ordering::SeqCst);
        let backend = Arc::new(Replacing {
            fake: fake.clone(),
            roots: Mutex::default(),
            next,
        });
        let mut roots = Some(Arc::new(Conversations::new(backend.clone())));
        *backend.roots.lock().unwrap() = Arc::downgrade(roots.as_ref().unwrap());
        roots.as_ref().unwrap().activate(old).await.unwrap();
        let authority = Arc::new(Authority(Mutex::new(old)));
        let session =
            Session::from_server(Server::gateway(roots.as_ref().unwrap().clone(), authority)).await;
        let request = params(
            "agentchat_join",
            json!({"context_id":old.context_id,"request_id":Uuid::new_v4()}),
        );
        let pending = session
            .client
            .peer()
            .send_request_with_option(
                ClientRequest::CallToolRequest(Request::new(request)),
                PeerRequestOptions::default(),
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), fake.entered.notified())
            .await
            .unwrap();
        match action {
            0 => {
                roots
                    .as_ref()
                    .unwrap()
                    .activate(ConversationKey {
                        context_id: Uuid::new_v4(),
                        generation: 3,
                    })
                    .await
                    .unwrap();
            }
            1 => roots.as_ref().unwrap().close().await,
            _ => drop(roots.take()),
        }
        assert!(
            tokio::time::timeout(Duration::from_secs(2), pending.await_response())
                .await
                .unwrap()
                .is_err()
        );
        assert_eq!(fake.attached.load(Ordering::SeqCst), 0);
        session.close().await;
    }
}
