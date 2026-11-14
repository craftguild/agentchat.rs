//! A transport capability epoch cannot manufacture native participation.
mod support;
use agentchat_core::model::HostKind;
use agentchat_mcp::Server;
use serde_json::json;
use std::sync::Arc;
use support::{Fake, Session, envelope};

#[tokio::test]
async fn native_epoch_is_visible_to_the_sdk_without_binding_an_unbound_client() {
    let fake = Arc::new(Fake::default());
    let server =
        Server::new(fake.clone()).with_native_routing(HostKind::Codex, "owned-state".into());
    let session = Session::from_server(server).await;
    let info = serde_json::to_value(session.client.peer_info().unwrap().as_ref()).unwrap();
    assert_eq!(
        info.pointer("/capabilities/experimental/agentchat~1native_routing"),
        Some(&json!({"host":"codex","protocol":3,"namespace":"owned-state"}))
    );
    let who = envelope(session.call("agentchat_whoami", json!({})).await);
    // Actor-specific whoami needs verified identity even when a native epoch
    // is advertised. Capability information supplies no participation scope.
    assert_eq!(who["error"]["code"], "session_required");
    let leave = envelope(session.call("agentchat_leave", json!({})).await);
    assert_eq!(leave["error"]["code"], "session_required");
    assert!(fake.calls.lock().unwrap().is_empty());
    session.close().await;
}

#[tokio::test]
async fn ordinary_mcp_information_does_not_advertise_native_authority() {
    let session = Session::new(Arc::new(Fake::default())).await;
    let info = serde_json::to_value(session.client.peer_info().unwrap().as_ref()).unwrap();
    assert!(
        info.pointer("/capabilities/experimental/agentchat~1native_routing")
            .is_none()
    );
    session.close().await;
}
