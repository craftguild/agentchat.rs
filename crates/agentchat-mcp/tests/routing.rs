//! Synthetic host authority over actual SDK calls; no real profile is admitted.
mod support;

use agentchat_core::{Error, ErrorCode, Result};
use agentchat_mcp::{CallEvidence, CallVerifier, ConversationKey, Conversations, Server};
use async_trait::async_trait;
use rmcp::model::{CallToolRequestParams, CallToolResult, ClientRequest, Request};
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
struct Authority {
    proofs: Mutex<HashMap<String, (String, ConversationKey)>>,
}
impl Authority {
    fn grant(
        &self,
        key: ConversationKey,
        tool: &'static str,
        value: Value,
    ) -> CallToolRequestParams {
        let nonce = Uuid::new_v4().to_string();
        self.proofs
            .lock()
            .unwrap()
            .insert(nonce.clone(), (tool.into(), key));
        let mut request = params(tool, value);
        request.meta = Some(
            json!({"fixture/nativeCallId":nonce})
                .as_object()
                .unwrap()
                .clone()
                .into(),
        );
        request
    }
}
#[async_trait]
impl CallVerifier for Authority {
    async fn verify(&self, evidence: CallEvidence<'_>) -> Result<ConversationKey> {
        let proof = evidence
            .metadata
            .and_then(|m| m.get("fixture/nativeCallId"))
            .and_then(Value::as_str);
        let found = proof.and_then(|proof| self.proofs.lock().unwrap().remove(proof));
        match found {
            Some((tool, key)) if tool == evidence.tool_name => Ok(key),
            _ => Err(Error::new(
                ErrorCode::SessionRequired,
                "Synthetic missing/replayed/mismatched native proof.",
            )),
        }
    }
}
fn key(generation: u64) -> ConversationKey {
    ConversationKey {
        context_id: Uuid::new_v4(),
        generation,
    }
}

#[tokio::test]
async fn concurrent_native_call_waits_then_verifies_the_completed_binding() {
    let backend = Arc::new(Fake::default());
    let roots = Arc::new(Conversations::new(backend.clone()));
    let authority = Arc::new(Authority::default());
    let a = key(1);
    roots.activate(a).await.unwrap();
    let session = Session::from_server(Server::gateway(roots.clone(), authority.clone())).await;
    backend.block_join.store(true, Ordering::SeqCst);
    let joining = session
        .client
        .peer()
        .send_request_with_option(
            ClientRequest::CallToolRequest(Request::new(authority.grant(
                a,
                "agentchat_join",
                json!({"context_id":a.context_id}),
            ))),
            PeerRequestOptions::default(),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), backend.entered.notified())
        .await
        .unwrap();
    let reading = session
        .client
        .peer()
        .send_request_with_option(
            ClientRequest::CallToolRequest(Request::new(authority.grant(
                a,
                "agentchat_whoami",
                json!({}),
            ))),
            PeerRequestOptions::default(),
        )
        .await
        .unwrap();
    tokio::task::yield_now().await;
    assert_eq!(backend.calls.lock().unwrap().len(), 1);
    backend.release.add_permits(1);
    for pending in [joining, reading] {
        let response = tokio::time::timeout(Duration::from_secs(2), pending.await_response())
            .await
            .unwrap()
            .unwrap();
        let result: CallToolResult =
            serde_json::from_value(serde_json::to_value(response).unwrap()).unwrap();
        let result = envelope(result);
        assert!(result.get("result").is_some(), "{result}");
    }
    assert_eq!(backend.calls.lock().unwrap().len(), 2);
    session.close().await;
}
async fn call(session: &Session, request: CallToolRequestParams) -> Value {
    envelope(session.client.call_tool(request).await.unwrap())
}
async fn joined(session: &Session, authority: &Authority, key: ConversationKey) -> Value {
    call(
        session,
        authority.grant(key, "agentchat_join", json!({"context_id":key.context_id})),
    )
    .await
}

#[tokio::test]
async fn native_doctor_metadata_retains_only_the_last_thirty_two_call_timings() {
    let backend = Arc::new(Fake::default());
    let roots = Arc::new(Conversations::new(backend.clone()));
    let authority = Arc::new(Authority::default());
    let a = key(1);
    roots.activate(a).await.unwrap();
    let session = Session::from_server(Server::gateway(roots.clone(), authority.clone())).await;
    assert!(
        joined(&session, &authority, a)
            .await
            .get("result")
            .is_some()
    );
    let mut ids = Vec::new();
    for _ in 0..35 {
        let id = Uuid::new_v4();
        ids.push(id.to_string());
        let result = call(
            &session,
            authority.grant(
                a,
                "agentchat_search",
                json!({
                    "request_id":id, "query":"PRIVATE_TEXT_MUST_NOT_ENTER_TIMINGS",
                }),
            ),
        )
        .await;
        assert!(result.get("result").is_some(), "{result}");
    }
    let doctor = session
        .client
        .call_tool(authority.grant(a, "agentchat_doctor", json!({})))
        .await
        .unwrap();
    let meta = doctor.meta.as_ref().unwrap();
    let timings = meta.0.get("agentchat.rs/native_call_timings").unwrap();
    assert_eq!(timings["capacity"], 32);
    assert_eq!(timings["overwritten"], 4);
    assert_eq!(timings["clock"], "process_local_durations");
    let samples = timings["samples"].as_array().unwrap();
    assert_eq!(
        samples
            .iter()
            .map(|v| v["request_id"].as_str().unwrap().to_string())
            .collect::<Vec<_>>(),
        ids[3..]
    );
    assert!(samples.iter().all(|v| v["verified"] == true
        && v["verification_us"].is_u64()
        && v["execution_us"].is_u64()));
    assert!(
        !serde_json::to_string(timings)
            .unwrap()
            .contains("PRIVATE_TEXT_MUST_NOT_ENTER_TIMINGS")
    );
    // Domain output stays CLI-compatible; diagnostics are transport metadata.
    assert!(
        doctor.structured_content.as_ref().unwrap()["result"]
            .get("native_call_timings")
            .is_none()
    );
    session.close().await;
}

#[tokio::test]
async fn retained_transport_switches_attachments_without_lending_identity_to_the_new_root() {
    let backend = Arc::new(Fake::default());
    let roots = Arc::new(Conversations::new(backend.clone()));
    let authority = Arc::new(Authority::default());
    let a = key(1);
    let old = roots.activate(a).await.unwrap();
    let session = Session::from_server(Server::gateway(roots.clone(), authority.clone())).await;
    assert!(
        joined(&session, &authority, a)
            .await
            .get("result")
            .is_some()
    );
    assert_eq!(backend.attached.load(Ordering::SeqCst), 1);
    assert!(
        call(
            &session,
            authority.grant(a, "agentchat_post", json!({"body":"A task"}))
        )
        .await
        .get("result")
        .is_some()
    );
    let c = key(2);
    roots.activate(c).await.unwrap();
    assert_eq!(backend.attached.load(Ordering::SeqCst), 0);
    assert_eq!(
        old.restore().await.unwrap_err().code,
        ErrorCode::SessionRequired
    );
    let before = backend.calls.lock().unwrap().len();
    for (tool, args, code) in [
        (
            "agentchat_post",
            json!({"body":"C has not joined","project_path":"/explicit"}),
            "session_required",
        ),
        // A verified native actor still needs an explicit project on this
        // unbound connection; native capability alone supplies neither route.
        ("agentchat_whoami", json!({}), "invalid_argument"),
        (
            "agentchat_join",
            json!({"context_id":a.context_id}),
            "identity_conflict",
        ),
    ] {
        assert_eq!(
            call(&session, authority.grant(c, tool, args)).await["error"]["code"],
            code
        );
    }
    assert_eq!(backend.calls.lock().unwrap().len(), before);
    assert!(
        joined(&session, &authority, c)
            .await
            .get("result")
            .is_some()
    );
    assert!(
        call(&session, authority.grant(c, "agentchat_whoami", json!({})))
            .await
            .get("result")
            .is_some()
    );
    assert_eq!(
        backend.calls.lock().unwrap().last().unwrap().1.context_id,
        Some(c.context_id)
    );
    assert_eq!(
        session.client.list_tools(None).await.unwrap().tools.len(),
        23
    );
    session.close().await;
    assert_eq!(backend.attached.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn proof_is_required_on_every_call_and_arguments_cannot_supply_it() {
    let backend = Arc::new(Fake::default());
    let roots = Arc::new(Conversations::new(backend.clone()));
    let authority = Arc::new(Authority::default());
    let a = key(1);
    roots.activate(a).await.unwrap();
    let session = Session::from_server(Server::gateway(roots.clone(), authority.clone())).await;
    assert_eq!(
        session.client.list_tools(None).await.unwrap().tools.len(),
        23
    );
    assert!(backend.calls.lock().unwrap().is_empty());
    let id = Uuid::new_v4();
    let missing = call(
        &session,
        params(
            "agentchat_join",
            json!({"context_id":a.context_id,"request_id":id}),
        ),
    )
    .await;
    assert_eq!(missing["error"]["code"], "session_required");
    assert_eq!(missing["request_id"], id.to_string());
    let request = authority.grant(a, "agentchat_join", json!({"context_id":a.context_id}));
    assert!(
        call(&session, request.clone())
            .await
            .get("result")
            .is_some()
    );
    assert_eq!(
        call(&session, request).await["error"]["code"],
        "session_required"
    );
    assert_eq!(backend.calls.lock().unwrap().len(), 1);
    let mut wrong_tool = authority.grant(a, "agentchat_doctor", json!({}));
    wrong_tool.name = "agentchat_whoami".into();
    assert_eq!(
        call(&session, wrong_tool).await["error"]["code"],
        "session_required"
    );
    let stale = authority.grant(a, "agentchat_whoami", json!({}));
    roots.activate(key(2)).await.unwrap();
    assert_eq!(
        call(&session, stale).await["error"]["code"],
        "session_required"
    );
    assert_eq!(backend.calls.lock().unwrap().len(), 1);
    session.close().await;
}

#[tokio::test]
async fn lifecycle_generation_fences_replays_and_does_not_reset_a_live_binding() {
    let backend = Arc::new(Fake::default());
    let roots = Arc::new(Conversations::new(backend.clone()));
    let authority = Arc::new(Authority::default());
    assert_eq!(
        roots.activate(key(0)).await.err().unwrap().code,
        ErrorCode::SessionRequired
    );
    let a = key(1);
    roots.activate(a).await.unwrap();
    let session = Session::from_server(Server::gateway(roots.clone(), authority.clone())).await;
    assert!(
        joined(&session, &authority, a)
            .await
            .get("result")
            .is_some()
    );
    roots.activate(a).await.unwrap();
    assert_eq!(backend.attached.load(Ordering::SeqCst), 1);
    assert_eq!(
        roots.activate(key(1)).await.err().unwrap().code,
        ErrorCode::SessionRequired
    );
    let reused_descriptor = ConversationKey { generation: 2, ..a };
    assert_eq!(
        roots.activate(reused_descriptor).await.err().unwrap().code,
        ErrorCode::SessionRequired
    );
    let c = key(2);
    roots.activate(c).await.unwrap();
    assert_eq!(
        roots.activate(a).await.err().unwrap().code,
        ErrorCode::SessionRequired
    );
    roots.close().await;
    assert_eq!(
        roots.activate(key(3)).await.err().unwrap().code,
        ErrorCode::SessionRequired
    );
    session.close().await;
}

#[tokio::test]
async fn fresh_resume_restores_without_inviting_and_fork_restoration_can_fail_unjoined() {
    let backend = Arc::new(Fake::default());
    let roots = Arc::new(Conversations::new(backend.clone()));
    let authority = Arc::new(Authority::default());
    let a = key(1);
    roots.activate(a).await.unwrap();
    let session = Session::from_server(Server::gateway(roots.clone(), authority.clone())).await;
    assert!(
        joined(&session, &authority, a)
            .await
            .get("result")
            .is_some()
    );
    let resumed = key(2);
    roots
        .activate(resumed)
        .await
        .unwrap()
        .restore()
        .await
        .unwrap();
    assert_eq!(backend.calls.lock().unwrap().len(), 1);
    call(
        &session,
        authority.grant(resumed, "agentchat_whoami", json!({})),
    )
    .await;
    assert_eq!(
        backend.calls.lock().unwrap().last().unwrap().1.context_id,
        Some(resumed.context_id)
    );
    let fork = key(3);
    backend.attach_fails.store(true, Ordering::SeqCst);
    assert_eq!(
        roots
            .activate(fork)
            .await
            .unwrap()
            .restore()
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotJoined
    );
    assert_eq!(backend.attached.load(Ordering::SeqCst), 0);
    assert_eq!(
        call(
            &session,
            authority.grant(
                fork,
                "agentchat_post",
                json!({"body":"unjoined","project_path":"/explicit"})
            )
        )
        .await["error"]["code"],
        "session_required"
    );
    assert_eq!(backend.calls.lock().unwrap().len(), 2);
    session.close().await;
}

#[tokio::test]
async fn retirement_during_join_cancels_old_waiting_and_keeps_new_retry_intent_separate() {
    let backend = Arc::new(Fake::default());
    backend.block_join.store(true, Ordering::SeqCst);
    let roots = Arc::new(Conversations::new(backend.clone()));
    let authority = Arc::new(Authority::default());
    let a = key(1);
    let old = roots.activate(a).await.unwrap();
    let session = Session::from_server(Server::gateway(roots.clone(), authority.clone())).await;
    let original = Uuid::new_v4();
    let request = authority.grant(
        a,
        "agentchat_join",
        json!({"context_id":a.context_id,"request_id":original}),
    );
    let handle = session
        .client
        .peer()
        .send_request_with_option(
            ClientRequest::CallToolRequest(Request::new(request)),
            PeerRequestOptions::default(),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), backend.entered.notified())
        .await
        .unwrap();
    let c = key(2);
    tokio::time::timeout(Duration::from_secs(2), roots.activate(c))
        .await
        .unwrap()
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(2), handle.await_response())
            .await
            .unwrap()
            .is_err()
    );
    assert_eq!(
        old.restore().await.unwrap_err().code,
        ErrorCode::SessionRequired
    );
    backend.block_join.store(false, Ordering::SeqCst);
    let result = joined(&session, &authority, c).await;
    assert!(result.get("result").is_some());
    assert_ne!(result["request_id"], original.to_string());
    let calls = backend.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].0, original);
    assert_eq!(calls[0].1.context_id, Some(a.context_id));
    assert_eq!(calls[1].1.context_id, Some(c.context_id));
    session.close().await;
}

struct HeldVerifier {
    key: ConversationKey,
    entered: Notify,
    release: Semaphore,
}

#[tokio::test]
async fn verification_deadline_is_retryable_and_does_not_invoke_business_work() {
    let backend = Arc::new(Fake::default());
    let roots = Arc::new(Conversations::new(backend.clone()));
    let a = key(1);
    roots.activate(a).await.unwrap();
    let verifier = Arc::new(HeldVerifier {
        key: a,
        entered: Notify::new(),
        release: Semaphore::new(0),
    });
    let session = Session::from_server(Server::gateway(roots.clone(), verifier.clone())).await;
    let request = Uuid::new_v4();
    let args = json!({"context_id":a.context_id,"request_id":request});
    let result = tokio::time::timeout(
        Duration::from_secs(7),
        call(&session, params("agentchat_join", args.clone())),
    )
    .await
    .unwrap();
    assert_eq!(result["error"]["code"], "busy");
    assert_eq!(result["error"]["retryable"], true);
    assert_eq!(result["request_id"], request.to_string());
    assert!(backend.calls.lock().unwrap().is_empty());
    // Deadline cancellation leaves no authority/result behind. Retry the same
    // business key only after fresh verification succeeds in the current root.
    verifier.release.add_permits(1);
    let retried = call(&session, params("agentchat_join", args)).await;
    assert!(retried.get("result").is_some(), "{retried}");
    assert_eq!(backend.calls.lock().unwrap().len(), 1);
    session.close().await;
}
#[async_trait]
impl CallVerifier for HeldVerifier {
    async fn verify(&self, _: CallEvidence<'_>) -> Result<ConversationKey> {
        self.entered.notify_one();
        self.release.acquire().await.unwrap().forget();
        Ok(self.key)
    }
}
#[tokio::test]
async fn a_late_verifier_result_cannot_route_to_a_replacement_conversation() {
    let backend = Arc::new(Fake::default());
    let roots = Arc::new(Conversations::new(backend.clone()));
    let a = key(1);
    roots.activate(a).await.unwrap();
    let verifier = Arc::new(HeldVerifier {
        key: a,
        entered: Notify::new(),
        release: Semaphore::new(0),
    });
    let session = Session::from_server(Server::gateway(roots.clone(), verifier.clone())).await;
    let pending = session
        .client
        .peer()
        .send_request_with_option(
            ClientRequest::CallToolRequest(Request::new(params(
                "agentchat_join",
                json!({"context_id":a.context_id}),
            ))),
            PeerRequestOptions::default(),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), verifier.entered.notified())
        .await
        .unwrap();
    roots.activate(key(2)).await.unwrap();
    verifier.release.add_permits(1);
    let response = pending.await_response().await.unwrap();
    let result: CallToolResult =
        serde_json::from_value(serde_json::to_value(response).unwrap()).unwrap();
    assert_eq!(envelope(result)["error"]["code"], "session_required");
    assert!(backend.calls.lock().unwrap().is_empty());
    session.close().await;
}

#[tokio::test]
async fn eof_closes_retained_attachments_and_dropping_control_cannot_keep_them_alive() {
    let backend = Arc::new(Fake::default());
    let roots = Arc::new(Conversations::new(backend.clone()));
    let authority = Arc::new(Authority::default());
    let a = key(1);
    let retained = roots.activate(a).await.unwrap();
    let session = Session::from_server(Server::gateway(roots.clone(), authority.clone())).await;
    assert!(
        joined(&session, &authority, a)
            .await
            .get("result")
            .is_some()
    );
    session.close().await;
    assert_eq!(backend.attached.load(Ordering::SeqCst), 0);
    assert_eq!(
        retained.restore().await.unwrap_err().code,
        ErrorCode::SessionRequired
    );
    assert_eq!(
        roots.activate(key(2)).await.err().unwrap().code,
        ErrorCode::SessionRequired
    );
    // A separately owned control also retires on last-owner loss, despite a
    // retained public Conversation clone and without a physical SDK server.
    let other = Conversations::new(backend.clone());
    let retained = other.activate(key(1)).await.unwrap();
    retained.restore().await.unwrap();
    assert_eq!(backend.attached.load(Ordering::SeqCst), 1);
    drop(other);
    tokio::time::timeout(Duration::from_secs(2), async {
        while backend.attached.load(Ordering::SeqCst) != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        retained.restore().await.unwrap_err().code,
        ErrorCode::SessionRequired
    );
}

#[tokio::test]
async fn cancelled_transition_retains_its_fence_and_cleanup_without_a_historical_root_map() {
    use std::{future::Future, task::Poll};
    let backend = Arc::new(Fake::default());
    let roots = Arc::new(Conversations::new(backend.clone()));
    let authority = Arc::new(Authority::default());
    let a = key(1);
    let old = roots.activate(a).await.unwrap();
    let session = Session::from_server(Server::gateway(roots.clone(), authority.clone())).await;
    assert!(
        joined(&session, &authority, a)
            .await
            .get("result")
            .is_some()
    );
    let c = key(2);
    let mut transition = Box::pin(roots.activate(c));
    // Poll once without yielding to the new cleanup task: retire has happened,
    // but awaiting its JoinHandle is the deterministic cancellation boundary.
    let first = std::future::poll_fn(|cx| Poll::Ready(transition.as_mut().poll(cx))).await;
    assert!(first.is_pending());
    drop(transition);
    assert_eq!(
        old.restore().await.unwrap_err().code,
        ErrorCode::SessionRequired
    );
    assert_eq!(
        roots.activate(a).await.err().unwrap().code,
        ErrorCode::SessionRequired
    );
    assert_eq!(
        roots.activate(key(2)).await.err().unwrap().code,
        ErrorCode::SessionRequired
    );
    tokio::time::timeout(Duration::from_secs(2), async {
        while backend.attached.load(Ordering::SeqCst) != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    roots.activate(c).await.unwrap();
    assert!(
        joined(&session, &authority, c)
            .await
            .get("result")
            .is_some()
    );
    assert_eq!(backend.attached.load(Ordering::SeqCst), 1);
    session.close().await;
}

#[tokio::test]
async fn losing_native_control_retires_a_joined_root_while_the_sdk_transport_stays_open() {
    let backend = Arc::new(Fake::default());
    let roots = Arc::new(Conversations::new(backend.clone()));
    let authority = Arc::new(Authority::default());
    let a = key(1);
    let retained = roots.activate(a).await.unwrap();
    let session = Session::from_server(Server::gateway(roots.clone(), authority.clone())).await;
    assert!(
        joined(&session, &authority, a)
            .await
            .get("result")
            .is_some()
    );
    drop(roots);
    tokio::time::timeout(Duration::from_secs(2), async {
        while backend.attached.load(Ordering::SeqCst) != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        retained.restore().await.unwrap_err().code,
        ErrorCode::SessionRequired
    );
    let before = backend.calls.lock().unwrap().len();
    assert_eq!(
        call(&session, authority.grant(a, "agentchat_whoami", json!({}))).await["error"]["code"],
        "session_required"
    );
    assert_eq!(backend.calls.lock().unwrap().len(), before);
    assert_eq!(
        session.client.list_tools(None).await.unwrap().tools.len(),
        23
    );
    session.close().await;
}

#[tokio::test]
async fn native_verification_timeout_fails_closed_without_an_application_call() {
    let backend = Arc::new(Fake::default());
    let roots = Arc::new(Conversations::new(backend.clone()));
    let a = key(1);
    roots.activate(a).await.unwrap();
    let verifier = Arc::new(HeldVerifier {
        key: a,
        entered: Notify::new(),
        release: Semaphore::new(0),
    });
    let session = Session::from_server(Server::gateway(roots.clone(), verifier)).await;
    let result = tokio::time::timeout(
        Duration::from_secs(7),
        session.call("agentchat_whoami", json!({"project_path":"/explicit"})),
    )
    .await
    .unwrap();
    let timed_out = envelope(result);
    assert_eq!(timed_out["error"]["code"], "busy");
    assert_eq!(timed_out["error"]["retryable"], true);
    assert!(backend.calls.lock().unwrap().is_empty());
    session.close().await;
}

struct ForeignRestore {
    backend: Arc<Fake>,
    context: Uuid,
}
#[async_trait]
impl agentchat_protocol::operations::Backend for ForeignRestore {
    async fn invoke(
        &self,
        id: Uuid,
        target: agentchat_protocol::operations::Target,
        operation: agentchat_protocol::operations::Operation,
    ) -> agentchat_protocol::envelope::Envelope<Value> {
        self.backend.invoke(id, target, operation).await
    }
    async fn attach(
        &self,
        mut target: agentchat_protocol::operations::Target,
    ) -> Result<Box<dyn agentchat_protocol::operations::Attachment>> {
        target.context_id = Some(self.context);
        self.backend.attach(target).await
    }
}
#[tokio::test]
async fn restoration_checks_the_returned_attachment_identity_before_capturing_it() {
    let backend = Arc::new(Fake::default());
    let server = Server::new(Arc::new(ForeignRestore {
        backend: backend.clone(),
        context: Uuid::new_v4(),
    }));
    assert_eq!(
        server
            .restore(agentchat_protocol::operations::Target::default())
            .await
            .unwrap_err()
            .code,
        ErrorCode::SessionRequired
    );
    let result = server
        .restore(agentchat_protocol::operations::Target {
            context_id: Some(Uuid::new_v4()),
            ..Default::default()
        })
        .await;
    assert_eq!(result.unwrap_err().code, ErrorCode::IdentityConflict);
    assert_eq!(backend.attached.load(Ordering::SeqCst), 0);
    let session = Session::from_server(server).await;
    assert_eq!(
        envelope(session.call("agentchat_whoami", json!({})).await)["error"]["code"],
        "session_required"
    );
    assert!(backend.calls.lock().unwrap().is_empty());
    session.close().await;
}

#[tokio::test]
async fn native_root_end_retires_only_its_attachment_without_closing_the_transport() {
    let backend = Arc::new(Fake::default());
    let roots = Arc::new(Conversations::new(backend.clone()));
    let authority = Arc::new(Authority::default());
    let a = key(1);
    let old = roots.activate(a).await.unwrap();
    let session = Session::from_server(Server::gateway(roots.clone(), authority.clone())).await;
    assert!(
        joined(&session, &authority, a)
            .await
            .get("result")
            .is_some()
    );
    assert!(roots.retire(a).await.unwrap());
    assert_eq!(backend.attached.load(Ordering::SeqCst), 0);
    assert_eq!(
        old.restore().await.unwrap_err().code,
        ErrorCode::SessionRequired
    );
    assert!(roots.activate(a).await.is_err());
    let c = key(2);
    roots.activate(c).await.unwrap();
    assert!(!roots.retire(a).await.unwrap());
    assert!(
        joined(&session, &authority, c)
            .await
            .get("result")
            .is_some()
    );
    assert_eq!(backend.attached.load(Ordering::SeqCst), 1);
    assert_eq!(
        session.client.list_tools(None).await.unwrap().tools.len(),
        23
    );
    session.close().await;
}

#[tokio::test]
async fn a_verified_unjoined_read_uses_its_exact_descriptor_without_implicit_participation() {
    let backend = Arc::new(Fake::default());
    let roots = Arc::new(Conversations::new(backend.clone()));
    let authority = Arc::new(Authority::default());
    let a = key(1);
    roots.activate(a).await.unwrap();
    let session = Session::from_server(Server::gateway(roots.clone(), authority.clone())).await;
    assert!(
        call(
            &session,
            authority.grant(a, "agentchat_whoami", json!({"project_path":"/explicit"}))
        )
        .await
        .get("result")
        .is_some()
    );
    assert_eq!(
        backend.calls.lock().unwrap().last().unwrap().1.context_id,
        Some(a.context_id)
    );
    assert_eq!(backend.attached.load(Ordering::SeqCst), 0);
    let before = backend.calls.lock().unwrap().len();
    assert_eq!(
        call(
            &session,
            authority.grant(
                a,
                "agentchat_post",
                json!({"project_path":"/explicit","body":"still unjoined"})
            )
        )
        .await["error"]["code"],
        "session_required"
    );
    assert_eq!(backend.calls.lock().unwrap().len(), before);
    session.close().await;
}
