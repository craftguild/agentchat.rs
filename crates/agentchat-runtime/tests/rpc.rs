//! Real socket dispatch retains shared application semantics and retry results.

mod support;
use agentchat_core::{
    ErrorCode,
    application::Mutation,
    read::{PageRequest, Query},
};
use agentchat_protocol::{
    envelope::Outcome,
    rpc::{Call, Client},
};
use support::runtime::{Running, attach, join, mutate, options, query};
use uuid::Uuid;

#[tokio::test]
async fn typed_rpc_isolates_same_directory_conversations_and_rejects_rebinding() {
    let r = Running::new(options()).await;
    let a = r.context("exact-a").await;
    let b = r.context("exact-b").await;
    let mut ca = r.client().await;
    let mut cb = r.client().await;
    assert!(attach(&mut ca, &a).await["agent"].is_null());
    assert_eq!(
        support::runtime::health(&mut cb).await["statistics"]["agents"],
        0
    );
    let ja = join(&mut ca, &a, "alice").await;
    let jb = join(&mut cb, &b, "bob").await;
    assert_ne!(ja["agent_id"], jb["agent_id"]);
    let error = ca.native_binding(b.context_id).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::IdentityConflict);
    let message = mutate(
        &mut ca,
        &a,
        Mutation::Post {
            channel: Some("dev".into()),
            body: "hello @bob".into(),
        },
    )
    .await;
    assert!(message["message_id"].is_string());
    let inbox = query(
        &mut cb,
        Some(b.context_id),
        Query::Inbox {
            page: PageRequest::default(),
        },
    )
    .await;
    assert!(!inbox["items"].as_array().unwrap().is_empty());
    let head = support::runtime::health(&mut ca).await["head"].clone();
    let history = query(
        &mut ca,
        None,
        Query::History {
            channel: None,
            thread: None,
            page: PageRequest::default(),
        },
    )
    .await;
    assert_eq!(history["items"].as_array().unwrap().len(), 1);
    assert_eq!(support::runtime::health(&mut ca).await["head"], head);
    assert_eq!(
        query(
            &mut cb,
            Some(b.context_id),
            Query::Inbox {
                page: PageRequest::default()
            }
        )
        .await,
        inbox
    );
    drop(ca);
    drop(cb);
    r.stop().await;
}

#[tokio::test]
async fn semantic_retry_survives_reconnect_with_a_different_rpc_identifier() {
    let r = Running::new(options()).await;
    let context = r.context("retry-root").await;
    let mut client = r.client().await;
    join(&mut client, &context, "alice").await;
    let key = Uuid::new_v4();
    let call = Call::Mutate {
        request_id: key,
        actor: agentchat_protocol::rpc::ActorReference::Native {
            context_id: context.context_id,
        },
        command: Mutation::Post {
            channel: Some("dev".into()),
            body: "only once".into(),
        },
    };
    let first = client.call(Uuid::new_v4(), call.clone()).await.unwrap();
    assert_eq!(first.request_id, key);
    drop(client);
    let mut retry = r.client().await;
    let second = retry.call(Uuid::new_v4(), call.clone()).await.unwrap();
    assert_eq!(
        serde_json::to_value(&first).unwrap(),
        serde_json::to_value(&second).unwrap()
    );
    let status = support::runtime::health(&mut retry).await;
    assert_eq!(status["statistics"]["messages"], 1);
    let conflict = Call::Mutate {
        request_id: key,
        actor: agentchat_protocol::rpc::ActorReference::Native {
            context_id: context.context_id,
        },
        command: Mutation::Post {
            channel: Some("dev".into()),
            body: "different".into(),
        },
    };
    assert_eq!(
        retry.call(Uuid::new_v4(), conflict).await.unwrap_err().code,
        ErrorCode::IdempotencyConflict
    );
    assert!(matches!(second.outcome, Outcome::Success { .. }));
    drop(retry);
    r.stop().await;
}

#[tokio::test]
async fn namespace_handshake_mismatch_and_stale_context_are_refused() {
    let r = Running::new(options()).await;
    let error = Client::connect(&r.handle.paths().socket, Uuid::new_v4())
        .await
        .err()
        .unwrap();
    assert_eq!(error.code, ErrorCode::ProjectBindingMismatch);
    let mut client = r.client().await;
    assert_eq!(
        client
            .native_binding(Uuid::new_v4())
            .await
            .unwrap_err()
            .code,
        ErrorCode::SessionRequired
    );
    // A failed call does not rebind this still-unbound connection.
    let result = client.health().await.unwrap();
    assert_eq!(result.application.statistics.agents, 0);
    drop(client);
    r.stop().await;
}
