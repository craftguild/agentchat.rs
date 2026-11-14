//! Actual private files/sockets, synthetic native locators; never host admission.
use agentchat_cli::claude_gateway::records::{self, Endpoint};
#[path = "../src/claude_gateway/endpoint.rs"]
mod endpoint;
use serde_json::{Value, json};
use std::{
    os::unix::fs::{MetadataExt, PermissionsExt},
    time::Duration,
};
use tokio::net::{UnixListener, UnixStream};
use uuid::Uuid;
async fn bind(path: &std::path::Path, scope: Uuid) -> agentchat_core::Result<endpoint::Lease> {
    let native = agentchat_hosts::claude::process::ProcessIdentity::capture(std::process::id())?;
    endpoint::bind(path, scope, &native, None).await
}

fn root() -> tempfile::TempDir {
    tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap()
}
#[test]
fn records_are_private_atomic_and_bounded_before_replacement() {
    let r = root();
    let p = r.path().join("literal ' $(not-a-shell).json");
    let expected = json!({"value":"日本語\nline"});
    records::write(&p, &expected).unwrap();
    assert_eq!(records::read::<Value>(&p).unwrap(), expected);
    assert_eq!(std::fs::metadata(&p).unwrap().mode() & 0o777, 0o600);
    let before = std::fs::read(&p).unwrap();
    assert!(records::write(&p, &json!({"value":"x".repeat(65537)})).is_err());
    assert_eq!(std::fs::read(&p).unwrap(), before);
    assert_eq!(std::fs::read_dir(r.path()).unwrap().count(), 1);
}
#[test]
fn unsafe_links_modes_and_oversize_records_preserve_foreign_data() {
    let r = root();
    let victim = r.path().join("victim");
    records::write(&victim, &json!({"kept":true})).unwrap();
    let before = std::fs::read(&victim).unwrap();
    let link = r.path().join("symlink");
    std::os::unix::fs::symlink(&victim, &link).unwrap();
    assert!(records::read::<Value>(&link).is_err());
    assert!(records::write(&link, &json!(null)).is_err());
    assert_eq!(std::fs::read(&victim).unwrap(), before);
    let hard = r.path().join("hardlink");
    std::fs::hard_link(&victim, &hard).unwrap();
    assert!(records::read::<Value>(&hard).is_err());
    assert!(records::write(&hard, &json!(null)).is_err());
    std::fs::remove_file(hard).unwrap();
    std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(records::read::<Value>(&victim).is_err());
    std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(&victim, vec![b'x'; 65537]).unwrap();
    assert!(records::read::<Value>(&victim).is_err());
}
#[tokio::test]
async fn a_second_gateway_cannot_replace_the_live_physical_transport() {
    let r = root();
    let owner = Uuid::new_v4();
    let lease = bind(r.path(), owner).await.unwrap();
    let before: Endpoint = records::read(&records::endpoint(r.path(), owner)).unwrap();
    assert!(bind(r.path(), owner).await.is_err());
    let now: Endpoint = records::read(&records::endpoint(r.path(), owner)).unwrap();
    assert_eq!(now.nonce, before.nonce);
    let _client = UnixStream::connect(&now.socket).await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), lease.listener.accept())
        .await
        .unwrap()
        .unwrap();
    drop(lease);
    assert!(!records::endpoint(r.path(), owner).exists());
    let next = bind(r.path(), owner).await.unwrap();
    assert_ne!(next.nonce, before.nonce);
}
#[tokio::test]
async fn late_cleanup_preserves_an_atomically_replaced_publication_even_with_the_old_nonce() {
    let r = root();
    let owner = Uuid::new_v4();
    let lease = bind(r.path(), owner).await.unwrap();
    let p = records::endpoint(r.path(), owner);
    let publication: Endpoint = records::read(&p).unwrap();
    let socket_parent = publication.socket.parent().unwrap().to_owned();
    let before = std::fs::metadata(&p).unwrap().ino();
    records::write(&p, &publication).unwrap();
    assert_ne!(before, std::fs::metadata(&p).unwrap().ino());
    drop(lease);
    assert!(p.exists());
    assert!(!socket_parent.exists());
    assert_eq!(
        records::read::<Endpoint>(&p).unwrap().nonce,
        publication.nonce
    );
}
#[tokio::test]
async fn a_live_foreign_endpoint_is_preserved_after_obtaining_an_unowned_lock() {
    let r = root();
    let sockets = root();
    let socket = sockets.path().join("foreign.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let owner = Uuid::new_v4();
    let p = records::endpoint(r.path(), owner);
    let nonce = Uuid::new_v4();
    records::write(
        &p,
        &Endpoint {
            ownership: None,
            socket: socket.clone(),
            nonce,
        },
    )
    .unwrap();
    assert!(bind(r.path(), owner).await.is_err());
    assert_eq!(records::read::<Endpoint>(&p).unwrap().nonce, nonce);
    // Refusing an unproved locator must not probe its external path. Explicit
    // fixture I/O independently proves that the preserved listener still works.
    let _client = UnixStream::connect(&socket).await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), listener.accept())
        .await
        .unwrap()
        .unwrap();
    assert!(socket.exists());
}
#[tokio::test]
async fn an_unproved_closed_predecessor_is_readable_but_not_replaced() {
    let r = root();
    let owner = Uuid::new_v4();
    let p = records::endpoint(r.path(), owner);
    let value = Endpoint {
        ownership: None,
        socket: r.path().join("gone.sock"),
        nonce: Uuid::new_v4(),
    };
    records::write(&p, &value).unwrap();
    let before = std::fs::read(&p).unwrap();
    let original = std::fs::metadata(&p).unwrap().ino();
    assert_eq!(records::read::<Endpoint>(&p).unwrap(), value);
    assert!(bind(r.path(), owner).await.is_err());
    let lock = r.path().join(format!("gateway-{owner}.lock"));
    let inode = std::fs::metadata(&lock).unwrap().ino();
    assert!(bind(r.path(), owner).await.is_err());
    assert_eq!(std::fs::read(&p).unwrap(), before);
    assert_eq!(std::fs::metadata(&p).unwrap().ino(), original);
    assert_eq!(std::fs::metadata(&lock).unwrap().ino(), inode);
    // This fixture authored the unproved old record. Product recovery must not
    // silently adopt its external path; real killed-helper recovery has its own
    // separate process test. Explicit fixture removal permits a fresh bind.
    assert!(agentchat_hosts::claude::metadata::remove_if(&p, &value).unwrap());
    let lease = bind(r.path(), owner).await.unwrap();
    assert_ne!(lease.nonce, value.nonce);
    drop(lease);
    let _next = bind(r.path(), owner).await.unwrap();
    assert_eq!(std::fs::metadata(lock).unwrap().ino(), inode);
}
