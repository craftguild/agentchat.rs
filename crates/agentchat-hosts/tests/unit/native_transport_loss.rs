//! Actual physical helper loss is a reconnect hint, never native/model proof.
use super::harness::Gateway;
use crate::claude::namespace::{OwnerNamespace, TransportLease};
use crate::claude::{observer::relay, process::ProcessIdentity};
use agentchat_protocol::framing;
use serde_json::{Value, json};
use std::{path::Path, time::Duration};
use tokio::{io::BufReader, time::timeout};

async fn event(reader: &mut BufReader<tokio::io::DuplexStream>) -> Value {
    timeout(Duration::from_secs(3), framing::read(reader))
        .await
        .unwrap()
        .unwrap()
        .unwrap()
}
async fn no_event(reader: &mut BufReader<tokio::io::DuplexStream>) {
    assert!(
        timeout(
            Duration::from_millis(600),
            framing::read::<_, Value>(reader)
        )
        .await
        .is_err()
    );
}
async fn namespace(home: &Path) -> OwnerNamespace {
    OwnerNamespace::open(home, ProcessIdentity::capture(std::process::id()).unwrap())
        .await
        .unwrap()
}
#[tokio::test]
async fn a_proved_helper_loss_emits_once_while_same_birth_exec_does_not() {
    let base = tempfile::tempdir().unwrap();
    let ns = namespace(&base.path().join("state")).await;
    let mut gateway = Gateway::spawn(
        ns.directory(),
        ns.id(),
        ns.owner().pid(),
        &base.path().join("ready"),
    );
    assert!(gateway.inherited.is_none());
    let (_, helper) = ns.watched_transport().await.unwrap().unwrap();
    assert_eq!(helper, Some(gateway.helper.clone()));
    let (output, input) = tokio::io::duplex(4096);
    let mut reader = BufReader::new(input);
    let relay_ns = ns.clone();
    let task = tokio::spawn(async move { relay::serve_managed(relay_ns, output).await });
    assert_eq!(
        event(&mut reader).await,
        json!({"event":"transport_changed","endpoint_nonce":gateway.endpoint.nonce})
    );
    gateway.exec();
    no_event(&mut reader).await;
    gateway.kill();
    assert_eq!(
        event(&mut reader).await,
        json!({"event":"transport_lost","endpoint_nonce":gateway.endpoint.nonce})
    );
    no_event(&mut reader).await;
    let replacement = TransportLease::bind_managed(&ns).await.unwrap();
    assert_eq!(
        event(&mut reader).await,
        json!({"event":"transport_changed","endpoint_nonce":replacement.nonce})
    );
    assert!(ns.owner().is_current());
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn replacing_a_proved_endpoint_with_a_legacy_copy_revokes_the_cached_loss_hint() {
    let base = tempfile::tempdir().unwrap();
    let ns = namespace(&base.path().join("state")).await;
    let mut gateway = Gateway::spawn(
        ns.directory(),
        ns.id(),
        ns.owner().pid(),
        &base.path().join("ready"),
    );
    assert!(gateway.inherited.is_none());
    let (output, input) = tokio::io::duplex(4096);
    let mut reader = BufReader::new(input);
    let relay_ns = ns.clone();
    let task = tokio::spawn(async move { relay::serve_managed(relay_ns, output).await });
    assert_eq!(event(&mut reader).await["event"], "transport_changed");
    let path = ns.directory().join(format!("gateway-{}.json", ns.id()));
    let legacy = serde_json::to_vec(&gateway.endpoint).unwrap();
    std::fs::write(&path, &legacy).unwrap();
    assert!(ns.watched_transport().await.unwrap().unwrap().1.is_none());
    no_event(&mut reader).await;
    gateway.kill();
    no_event(&mut reader).await;
    assert_eq!(std::fs::read(path).unwrap(), legacy);
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
}
