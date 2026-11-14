//! Cancelled SDK calls retain semantic join intent and release transport bounds.
mod support;
use rmcp::{
    model::{ClientRequest, Request},
    service::PeerRequestOptions,
};
use serde_json::json;
use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};
use support::*;
use uuid::Uuid;

#[tokio::test]
async fn cancellation_keeps_exact_join_intent_and_does_not_exhaust_credits() {
    let backend = Arc::new(Fake::default());
    backend.block_join.store(true, Ordering::SeqCst);
    let session = Session::new(backend.clone()).await;
    let context = Uuid::new_v4();
    for _ in 0..40 {
        let request = ClientRequest::CallToolRequest(Request::new(params(
            "agentchat_join",
            json!({"context_id":context}),
        )));
        let handle = session
            .client
            .peer()
            .send_request_with_option(request, PeerRequestOptions::default())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), backend.entered.notified())
            .await
            .unwrap();
        let concurrent = envelope(
            session
                .call("agentchat_join", json!({"context_id":Uuid::new_v4()}))
                .await,
        );
        assert_eq!(concurrent["error"]["code"], "busy");
        handle.cancel(None).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let switched = envelope(
                    session
                        .call("agentchat_join", json!({"context_id":Uuid::new_v4()}))
                        .await,
                );
                match switched["error"]["code"].as_str() {
                    Some("busy") => tokio::task::yield_now().await,
                    Some("identity_conflict") => break,
                    _ => panic!("Cancelled join lost its immutable intent: {switched}"),
                }
            }
        })
        .await
        .unwrap();
    }
    backend.block_join.store(false, Ordering::SeqCst);
    let result = session.join(context).await;
    assert!(result.get("result").is_some(), "{result}");
    {
        let calls = backend.calls.lock().unwrap();
        assert_eq!(calls.len(), 41);
        assert!(
            calls
                .iter()
                .all(|(id, target, _)| id.to_string() == result["request_id"]
                    && target.context_id == Some(context))
        );
    }
    session.close().await;
}

#[tokio::test]
async fn cancelled_join_compares_canonical_arguments_when_retrying_the_original_intent() {
    let backend = Arc::new(Fake::default());
    backend.block_join.store(true, Ordering::SeqCst);
    let session = Session::new(backend.clone()).await;
    let context = Uuid::new_v4();
    let id = Uuid::new_v4();
    let request = ClientRequest::CallToolRequest(Request::new(params(
        "agentchat_join",
        json!({"context_id":context,"request_id":id,"name":"Alpha"}),
    )));
    let pending = session
        .client
        .peer()
        .send_request_with_option(request, PeerRequestOptions::default())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), backend.entered.notified())
        .await
        .unwrap();
    pending.cancel(None).await.unwrap();
    backend.block_join.store(false, Ordering::SeqCst);
    let retried = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let value = envelope(
                session
                    .call(
                        "agentchat_join",
                        json!({"context_id":context,"request_id":id,"name":"alpha"}),
                    )
                    .await,
            );
            if value["error"]["code"] == "busy" {
                tokio::task::yield_now().await;
                continue;
            }
            return value;
        }
    })
    .await
    .unwrap();
    assert!(retried.get("result").is_some(), "{retried}");
    let calls = backend.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 2);
    for (request, target, operation) in calls {
        assert_eq!(request, id);
        assert_eq!(target.context_id, Some(context));
        assert!(
            matches!(operation,agentchat_protocol::operations::Operation::Mutate {
            command:agentchat_core::application::Mutation::Join {name:Some(name)}
        } if name=="alpha")
        );
    }
    session.close().await;
}
