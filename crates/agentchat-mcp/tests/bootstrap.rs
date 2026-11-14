//! Synthetic native authority over real SDK routing; no host is admitted.
mod support;

use agentchat_core::{Error, ErrorCode, Result};
use agentchat_mcp::{
    BootstrapKey, CallEvidence, CallScope, CallVerifier, ConversationKey, Conversations, Server,
};
use agentchat_protocol::{
    envelope::Envelope,
    operations::{Attachment, Backend, InitializationIdentity, Operation, Target},
};
use async_trait::async_trait;
use rmcp::model::{CallToolRequestParams, ClientRequest, Request};
use rmcp::service::PeerRequestOptions;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, atomic::Ordering},
    time::Duration,
};
use support::*;
use tokio::sync::{Notify, Semaphore};
use uuid::Uuid;

#[derive(Default)]
struct Authority(Mutex<HashMap<String, (String, CallScope)>>);
impl Authority {
    fn grant(&self, scope: CallScope, tool: &'static str, args: Value) -> CallToolRequestParams {
        let id = Uuid::new_v4().to_string();
        self.0
            .lock()
            .unwrap()
            .insert(id.clone(), (tool.into(), scope));
        let mut request = params(tool, args);
        request.meta = Some(
            json!({"fixture/nativeCallId":id})
                .as_object()
                .unwrap()
                .clone()
                .into(),
        );
        request
    }
}
fn unavailable() -> Error {
    Error::new(
        ErrorCode::SessionRequired,
        "Synthetic native proof unavailable.",
    )
}
#[async_trait]
impl CallVerifier for Authority {
    async fn verify(&self, _: CallEvidence<'_>) -> Result<ConversationKey> {
        Err(unavailable())
    }
    async fn verify_scope(&self, e: CallEvidence<'_>) -> Result<CallScope> {
        let id = e
            .metadata
            .and_then(|v| v.get("fixture/nativeCallId"))
            .and_then(Value::as_str)
            .ok_or_else(unavailable)?;
        let (tool, key) = self.0.lock().unwrap().remove(id).ok_or_else(unavailable)?;
        if tool != e.tool_name {
            return Err(unavailable());
        }
        Ok(key)
    }
}
fn identity() -> InitializationIdentity {
    InitializationIdentity::new(
        agentchat_core::model::HostKind::Claude,
        "fixture-native-root".into(),
    )
    .unwrap()
}
fn boot(generation: u64) -> BootstrapKey {
    BootstrapKey {
        lifecycle_id: Uuid::new_v4(),
        generation,
    }
}
fn bound(generation: u64) -> ConversationKey {
    ConversationKey {
        context_id: Uuid::new_v4(),
        generation,
    }
}
async fn call(
    s: &Session,
    a: &Authority,
    key: CallScope,
    tool: &'static str,
    args: Value,
) -> Value {
    envelope(s.client.call_tool(a.grant(key, tool, args)).await.unwrap())
}

#[tokio::test]
async fn bootstrap_keeps_explicit_path_initialization_available_without_binding_or_activation() {
    let backend = Arc::new(Fake::default());
    let roots = Arc::new(Conversations::new(backend.clone()));
    let authority = Arc::new(Authority::default());
    let key = boot(1);
    roots.activate_bootstrap(key, identity()).await.unwrap();
    let s = Session::from_server(Server::gateway(roots.clone(), authority.clone())).await;
    assert_eq!(s.client.list_tools(None).await.unwrap().tools.len(), 23);
    assert!(backend.calls.lock().unwrap().is_empty());
    assert_eq!(
        call(
            &s,
            &authority,
            CallScope::Bootstrap(key),
            "agentchat_init",
            json!({})
        )
        .await["error"]["code"],
        "invalid_argument"
    );
    let request = Uuid::new_v4();
    let initialized = call(
        &s,
        &authority,
        CallScope::Bootstrap(key),
        "agentchat_init",
        json!({"project_path":"/explicit","request_id":request}),
    )
    .await;
    assert!(initialized.get("result").is_some());
    assert_eq!(initialized["request_id"], request.to_string());
    assert!(
        call(
            &s,
            &authority,
            CallScope::Bootstrap(key),
            "agentchat_doctor",
            json!({"project_path":"/explicit"})
        )
        .await
        .get("result")
        .is_some()
    );
    let calls = backend.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 2);
    assert!(matches!(calls[0].2, Operation::Initialize { .. }));
    assert_eq!(calls[0].1.initialization_identity, Some(identity()));
    assert!(calls[1].1.initialization_identity.is_none());
    assert!(
        calls
            .iter()
            .all(|(_, target, _)| target.context_id.is_none()
                && target.expected_project_id.is_none())
    );
    assert_eq!(backend.attached.load(Ordering::SeqCst), 0);
    // An ordinary verified native root receives actionable managed-launch
    // guidance without inventing or borrowing a product context UUID.
    let missing = s
        .client
        .call_tool(authority.grant(
            CallScope::Bootstrap(key),
            "agentchat_join",
            json!({"project_path":"/explicit"}),
        ))
        .await
        .unwrap();
    let missing = envelope(missing);
    assert_eq!(missing["error"]["code"], "session_required");
    assert_eq!(missing["error"]["retryable"], false);
    assert_eq!(
        missing["error"]["details"]["reason"],
        "managed_launch_required"
    );
    assert_eq!(
        missing["error"]["details"]["required_command"],
        "agentchat launch claude"
    );
    assert_eq!(backend.calls.lock().unwrap().len(), 2);
    s.close().await;
}

#[tokio::test]
async fn bootstrap_never_borrows_a_supplied_context_or_grants_agent_mutations() {
    let backend = Arc::new(Fake::default());
    let roots = Arc::new(Conversations::new(backend.clone()));
    let authority = Arc::new(Authority::default());
    let key = boot(1);
    roots.activate_bootstrap(key, identity()).await.unwrap();
    let s = Session::from_server(Server::gateway(roots.clone(), authority.clone())).await;
    for tool in ["agentchat_init", "agentchat_whoami", "agentchat_join"] {
        assert_eq!(
            call(
                &s,
                &authority,
                CallScope::Bootstrap(key),
                tool,
                json!({"project_path":"/explicit","context_id":Uuid::new_v4()})
            )
            .await["error"]["code"],
            if tool == "agentchat_join" {
                "session_required"
            } else {
                "identity_conflict"
            }
        );
    }
    for (tool, args) in [
        (
            "agentchat_post",
            json!({"project_path":"/explicit","body":"not invited"}),
        ),
        ("agentchat_leave", json!({"project_path":"/explicit"})),
        (
            "agentchat_read",
            json!({"project_path":"/explicit","message_ids":[Uuid::new_v4()]}),
        ),
    ] {
        assert_eq!(
            call(&s, &authority, CallScope::Bootstrap(key), tool, args).await["error"]["code"],
            "session_required"
        );
    }
    assert!(backend.calls.lock().unwrap().is_empty());
    assert_eq!(backend.attached.load(Ordering::SeqCst), 0);
    s.close().await;
}

#[tokio::test]
async fn every_bootstrap_call_needs_fresh_proof_and_cannot_activate_its_own_lifecycle() {
    let backend = Arc::new(Fake::default());
    let roots = Arc::new(Conversations::new(backend.clone()));
    let authority = Arc::new(Authority::default());
    let key = boot(1);
    let s = Session::from_server(Server::gateway(roots.clone(), authority.clone())).await;
    assert_eq!(
        call(
            &s,
            &authority,
            CallScope::Bootstrap(key),
            "agentchat_init",
            json!({"project_path":"/explicit"})
        )
        .await["error"]["code"],
        "session_required"
    );
    roots.activate_bootstrap(key, identity()).await.unwrap();
    assert_eq!(
        envelope(
            s.call("agentchat_init", json!({"project_path":"/explicit"}))
                .await
        )["error"]["code"],
        "session_required"
    );
    let request = authority.grant(
        CallScope::Bootstrap(key),
        "agentchat_init",
        json!({"project_path":"/explicit"}),
    );
    assert!(
        envelope(s.client.call_tool(request.clone()).await.unwrap())
            .get("result")
            .is_some()
    );
    assert_eq!(
        envelope(s.client.call_tool(request).await.unwrap())["error"]["code"],
        "session_required"
    );
    let mut wrong = authority.grant(
        CallScope::Bootstrap(key),
        "agentchat_whoami",
        json!({"project_path":"/explicit"}),
    );
    wrong.name = "agentchat_init".into();
    assert_eq!(
        envelope(s.client.call_tool(wrong).await.unwrap())["error"]["code"],
        "session_required"
    );
    assert_eq!(backend.calls.lock().unwrap().len(), 1);
    s.close().await;
}

#[tokio::test]
async fn bootstrap_and_project_attachments_share_one_monotonic_retirement_fence() {
    let backend = Arc::new(Fake::default());
    let roots = Arc::new(Conversations::new(backend.clone()));
    let authority = Arc::new(Authority::default());
    let first = boot(1);
    roots.activate_bootstrap(first, identity()).await.unwrap();
    assert!(
        roots
            .activate_bootstrap(
                first,
                InitializationIdentity::new(
                    agentchat_core::model::HostKind::Claude,
                    "different-native-root".into()
                )
                .unwrap()
            )
            .await
            .is_err()
    );
    assert_eq!(
        roots
            .refresh(ConversationKey {
                context_id: first.lifecycle_id,
                generation: first.generation
            })
            .await
            .unwrap_err()
            .code,
        ErrorCode::SessionRequired
    );
    roots.activate_bootstrap(first, identity()).await.unwrap();
    assert!(roots.activate_bootstrap(boot(0), identity()).await.is_err());
    assert!(roots.activate(bound(1)).await.is_err());
    assert!(
        roots
            .activate(ConversationKey {
                context_id: first.lifecycle_id,
                generation: 2
            })
            .await
            .is_err()
    );
    let s = Session::from_server(Server::gateway(roots.clone(), authority.clone())).await;
    let joined = bound(2);
    let old = roots.activate(joined).await.unwrap();
    assert_eq!(
        call(
            &s,
            &authority,
            CallScope::Bootstrap(first),
            "agentchat_init",
            json!({"project_path":"/explicit"})
        )
        .await["error"]["code"],
        "session_required"
    );
    assert!(
        call(
            &s,
            &authority,
            CallScope::Conversation(joined),
            "agentchat_join",
            json!({"context_id":joined.context_id})
        )
        .await
        .get("result")
        .is_some()
    );
    assert_eq!(backend.attached.load(Ordering::SeqCst), 1);
    let cleared = boot(3);
    roots.activate_bootstrap(cleared, identity()).await.unwrap();
    assert_eq!(backend.attached.load(Ordering::SeqCst), 0);
    assert_eq!(
        old.restore().await.unwrap_err().code,
        ErrorCode::SessionRequired
    );
    assert!(!roots.retire_bootstrap(first).await.unwrap());
    assert!(roots.retire_bootstrap(cleared).await.unwrap());
    assert!(roots.activate_bootstrap(cleared, identity()).await.is_err());
    assert_eq!(
        call(
            &s,
            &authority,
            CallScope::Bootstrap(cleared),
            "agentchat_whoami",
            json!({"project_path":"/explicit"})
        )
        .await["error"]["code"],
        "session_required"
    );
    roots.activate_bootstrap(boot(4), identity()).await.unwrap();
    s.close().await;
    assert!(roots.activate_bootstrap(boot(5), identity()).await.is_err());
}

struct HeldInit {
    inner: Arc<Fake>,
    entered: Notify,
    release: Semaphore,
}
#[async_trait]
impl Backend for HeldInit {
    async fn invoke(&self, id: Uuid, target: Target, operation: Operation) -> Envelope<Value> {
        let hold = matches!(operation, Operation::Initialize { .. });
        let result = self.inner.invoke(id, target, operation).await;
        if hold {
            self.entered.notify_one();
            self.release.acquire().await.unwrap().forget();
        }
        result
    }
    async fn attach(&self, target: Target) -> Result<Box<dyn Attachment>> {
        self.inner.attach(target).await
    }
}
#[tokio::test]
async fn retirement_fences_an_initialization_response_without_lending_it_to_the_new_context() {
    let inner = Arc::new(Fake::default());
    let backend = Arc::new(HeldInit {
        inner: inner.clone(),
        entered: Notify::new(),
        release: Semaphore::new(0),
    });
    let roots = Arc::new(Conversations::new(backend.clone()));
    let authority = Arc::new(Authority::default());
    let key = boot(1);
    roots.activate_bootstrap(key, identity()).await.unwrap();
    let s = Session::from_server(Server::gateway(roots.clone(), authority.clone())).await;
    let id = Uuid::new_v4();
    let pending = s
        .client
        .peer()
        .send_request_with_option(
            ClientRequest::CallToolRequest(Request::new(authority.grant(
                CallScope::Bootstrap(key),
                "agentchat_init",
                json!({"project_path":"/explicit","request_id":id}),
            ))),
            PeerRequestOptions::default(),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), backend.entered.notified())
        .await
        .unwrap();
    roots.activate(bound(2)).await.unwrap();
    assert!(pending.await_response().await.is_err());
    assert_eq!(inner.calls.lock().unwrap().len(), 1);
    assert_eq!(inner.calls.lock().unwrap()[0].0, id);
    assert_eq!(
        call(
            &s,
            &authority,
            CallScope::Bootstrap(key),
            "agentchat_init",
            json!({"project_path":"/explicit","request_id":id})
        )
        .await["error"]["code"],
        "session_required"
    );
    s.close().await;
}

#[tokio::test]
async fn losing_host_control_retires_bootstrap_authority_while_the_sdk_transport_stays_open() {
    let backend = Arc::new(Fake::default());
    let roots = Arc::new(Conversations::new(backend.clone()));
    let authority = Arc::new(Authority::default());
    let key = boot(1);
    roots.activate_bootstrap(key, identity()).await.unwrap();
    let s = Session::from_server(Server::gateway(roots.clone(), authority.clone())).await;
    drop(roots);
    assert_eq!(
        call(
            &s,
            &authority,
            CallScope::Bootstrap(key),
            "agentchat_init",
            json!({"project_path":"/explicit"})
        )
        .await["error"]["code"],
        "session_required"
    );
    assert!(backend.calls.lock().unwrap().is_empty());
    assert_eq!(s.client.list_tools(None).await.unwrap().tools.len(), 23);
    s.close().await;
}

#[tokio::test]
async fn cancelled_bootstrap_activation_retains_its_initializer_identity_before_retry() {
    use std::{future::Future, task::Poll};
    let backend = Arc::new(Fake::default());
    let roots = Arc::new(Conversations::new(backend.clone()));
    roots.activate_bootstrap(boot(1), identity()).await.unwrap();
    let next = boot(2);
    let mut transition = Box::pin(roots.activate_bootstrap(next, identity()));
    let first = std::future::poll_fn(|cx| Poll::Ready(transition.as_mut().poll(cx))).await;
    assert!(first.is_pending());
    drop(transition);
    let foreign = InitializationIdentity::new(
        agentchat_core::model::HostKind::Claude,
        "another-root".into(),
    )
    .unwrap();
    assert_eq!(
        roots
            .activate_bootstrap(next, foreign)
            .await
            .unwrap_err()
            .code,
        ErrorCode::SessionRequired
    );
    roots.activate_bootstrap(next, identity()).await.unwrap();
    let authority = Arc::new(Authority::default());
    let session = Session::from_server(Server::gateway(roots.clone(), authority.clone())).await;
    assert!(
        call(
            &session,
            &authority,
            CallScope::Bootstrap(next),
            "agentchat_init",
            json!({"project_path":"/explicit"})
        )
        .await
        .get("result")
        .is_some()
    );
    assert_eq!(
        backend.calls.lock().unwrap()[0].1.initialization_identity,
        Some(identity())
    );
    session.close().await;
}
